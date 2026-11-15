//! Finish an agent: commit its changes, push its branch, open one draft pull
//! request, record it, and remove the worktree only when the agent created it.

use std::path::Path;
use std::process::Command;

use serde::Serialize;
use serde_json::Value;

use super::worktree::{self, git};
use super::{process, run_ok, view, AgentView, Runtime};
use crate::domain::task_agent::commands::AgentChange;
use crate::domain::task_agent::events::AgentState;
use crate::task_managers::{TaskError, TaskManagers, PROVIDER};

#[derive(Debug, Clone, Serialize)]
pub struct DoneResult {
    pub agent: AgentView,
    pub pr_url: String,
    /// True when the agent's own worktree was removed (never a supplied dir).
    pub worktree_removed: bool,
    /// Set when removing the worktree failed after the PR was recorded.
    pub cleanup_error: Option<String>,
}

fn is_url(text: &str) -> bool {
    text.starts_with("https://") && !text.contains(char::is_whitespace)
}

/// The last `https://` line `gh pr create` printed.
fn pr_url(stdout: &str) -> Option<String> {
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| is_url(line))
        .map(str::to_string)
}

/// `OWNER/NAME` of a github.com remote URL (https, ssh or scp-like form),
/// else `None`. Only the two validated names leave this function: never the
/// URL or a credential in it.
pub(crate) fn github_repo(url: &str) -> Option<String> {
    let url = url.trim();
    if url.contains(char::is_whitespace) {
        return None;
    }
    let lower = url.to_ascii_lowercase();
    let scheme = ["https://", "http://", "ssh://", "git://"]
        .iter()
        .find(|scheme| lower.starts_with(**scheme));
    let (host, path) = match scheme {
        Some(scheme) => {
            let (authority, path) = url[scheme.len()..].split_once('/')?;
            let host = match authority.split_once('@') {
                Some((user, host)) if !user.is_empty() => host,
                Some(_) => return None,
                None => authority,
            };
            let host = match host.split_once(':') {
                Some((host, port))
                    if (1..=5).contains(&port.len())
                        && port.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    host
                }
                Some(_) => return None,
                None => host,
            };
            (host, path)
        }
        None => {
            let (left, path) = url.split_once(':')?;
            let host = match left.split_once('@') {
                Some((user, host)) if !user.is_empty() && !user.contains('/') => host,
                Some(_) => return None,
                None => left,
            };
            (host, path)
        }
    };
    if !host.eq_ignore_ascii_case("github.com") {
        return None;
    }
    let (owner, name) = path.strip_suffix('/').unwrap_or(path).split_once('/')?;
    let name = match name.len().checked_sub(4) {
        Some(cut)
            if cut > 0
                && name.is_char_boundary(cut)
                && name[cut..].eq_ignore_ascii_case(".git") =>
        {
            &name[..cut]
        }
        _ => name,
    };
    let owner_ok = (1..=39).contains(&owner.len())
        && owner.as_bytes()[0].is_ascii_alphanumeric()
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let name_ok = (1..=100).contains(&name.len())
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    (owner_ok && name_ok).then(|| format!("{owner}/{name}"))
}

/// The repository the branch is pushed to, so gh never picks another remote
/// (a fork's upstream).
fn origin_repo(rt: &Runtime, tree: &Path) -> Result<String, TaskError> {
    git(rt, tree, &["config", "--get", "remote.origin.url"])
        .ok()
        .and_then(|url| github_repo(&url))
        .ok_or_else(|| {
            TaskError::Conflict(
                "the origin remote is not a github.com repository URL; nothing was pushed".into(),
            )
        })
}

/// The open pull request of `branch`, when one exists already.
fn existing_pr(
    rt: &Runtime,
    tree: &Path,
    target: &str,
    branch: &str,
) -> Result<Option<String>, TaskError> {
    let mut gh = Command::new(&rt.gh);
    gh.current_dir(tree).args([
        "pr", "list", "--head", branch, "--repo", target, "--state", "open", "--json", "url",
        "--limit", "1",
    ]);
    let stdout = run_ok(gh, "gh pr list")?;
    let rows: Value = serde_json::from_str(stdout.trim())
        .map_err(|_| TaskError::Internal("gh pr list printed invalid JSON".into()))?;
    Ok(rows[0]["url"]
        .as_str()
        .filter(|url| is_url(url))
        .map(str::to_string))
}

/// Commit, push and open the draft pull request. Every step tolerates a
/// retry: a clean tree is not committed again, a pushed branch pushes
/// nothing, and an existing pull request is recorded instead of a second one.
pub async fn done(tm: &TaskManagers, rt: &Runtime, code: &str) -> Result<DoneResult, TaskError> {
    tm.refresh()?;
    let record = tm.live_by_code(code)?;
    if matches!(
        record.state,
        AgentState::Running | AgentState::TakeoverRequested
    ) && process::alive(record.pid)
    {
        return Err(TaskError::Conflict(
            "the headless turn is still running; wait for it or stop the agent".into(),
        ));
    }
    let tree = Path::new(&record.worktree);
    if !tree.is_dir() {
        return Err(TaskError::Conflict(format!(
            "the agent's directory {} is gone",
            record.worktree
        )));
    }
    let branch = record.branch.clone().ok_or_else(|| {
        TaskError::Conflict(
            "the agent's directory is on a detached HEAD; check out a branch".into(),
        )
    })?;
    // Never commit to or push the default branch, from a supplied directory
    // or the agent's own worktree; an unknown default branch refuses too.
    let default = worktree::default_branch(rt, tree)?;
    let current = worktree::current_branch(rt, tree)?;
    if current.as_deref() != Some(branch.as_str()) || branch == default {
        return Err(TaskError::Conflict(format!(
            "the directory must be on a feature branch (recorded {branch}); switch to one first"
        )));
    }
    let target = origin_repo(rt, tree)?;
    let label = format!("{PROVIDER} item {}", record.item_id);
    if !git(rt, tree, &["status", "--porcelain"])?.trim().is_empty() {
        git(rt, tree, &["add", "-A"])?;
        git(
            rt,
            tree,
            &[
                "commit",
                "-m",
                &format!("Task Managers agent work for {label}"),
            ],
        )?;
    }
    git(rt, tree, &["push", "-u", "origin", &branch])?;
    let url = match existing_pr(rt, tree, &target, &branch)? {
        Some(url) => url,
        None => {
            let mut gh = Command::new(&rt.gh);
            gh.current_dir(tree).args([
                "pr",
                "create",
                "--draft",
                "--repo",
                &target,
                "--head",
                &branch,
                "--title",
                &format!("Task Managers agent: {label}"),
                "--body",
                &format!(
                    "Draft opened by a Task Managers agent ({}) for {label}.",
                    record.agent_kind.as_str()
                ),
            ]);
            let created = run_ok(gh, "gh pr create").and_then(|stdout| {
                pr_url(&stdout).ok_or_else(|| {
                    TaskError::Internal("gh pr create printed no pull request URL".into())
                })
            });
            match created {
                Ok(url) => url,
                // gh may fail or time out after the pull request exists.
                Err(error) => match existing_pr(rt, tree, &target, &branch) {
                    Ok(Some(url)) => url,
                    _ => return Err(error),
                },
            }
        }
    };
    let after = tm
        .change_agent(
            code,
            AgentChange::Done {
                pr_url: url.clone(),
            },
        )
        .await?;
    let (worktree_removed, cleanup_error) = if record.created_worktree {
        match worktree::remove(rt, tree, None, false) {
            Ok(()) => (true, None),
            Err(error) => (false, Some(error.to_string())),
        }
    } else {
        (false, None)
    };
    Ok(DoneResult {
        agent: view(after, rt.ssh.as_deref()),
        pr_url: url,
        worktree_removed,
        cleanup_error,
    })
}
