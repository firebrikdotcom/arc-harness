//! Git worktrees for agents: `<repo>-tma-<hex>` beside the repository, on
//! branch `tm/basecamp-<item id>-<hex>`. No item text in any name.

use std::path::{Path, PathBuf};
use std::process::Command;

use rand::Rng;

use super::done::github_repo;
use super::{path_str, run_ok, Runtime};
use crate::task_managers::{TaskError, PROVIDER};

pub(crate) fn git(rt: &Runtime, dir: &Path, args: &[&str]) -> Result<String, TaskError> {
    let mut command = Command::new(&rt.git);
    command.arg("-C").arg(dir).args(args);
    // An inherited GIT_DIR and its kin would answer for every directory, whatever -C says.
    for name in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_COMMON_DIR",
        "GIT_INDEX_FILE",
        "GIT_CEILING_DIRECTORIES",
    ] {
        command.env_remove(name);
    }
    run_ok(
        command,
        &format!("git {}", args.first().copied().unwrap_or("")),
    )
}

/// A new worktree and branch for `item_id`, based on the repository's HEAD.
pub fn create(
    rt: &Runtime,
    repo_path: &Path,
    item_id: &str,
) -> Result<(PathBuf, String), TaskError> {
    if !repo_path.is_absolute() || !repo_path.is_dir() {
        return Err(TaskError::Usage(format!(
            "the repository path {} is not an existing absolute directory",
            repo_path.display()
        )));
    }
    if !is_checkout(rt, repo_path) {
        return Err(TaskError::Usage(format!(
            "the repository path {} is not a git repository",
            repo_path.display()
        )));
    }
    let base = repo_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| TaskError::Usage("the repository path has no name".into()))?;
    let parent = repo_path
        .parent()
        .ok_or_else(|| TaskError::Usage("the repository path has no parent".into()))?;
    let mut rng = rand::thread_rng();
    for _ in 0..16 {
        let hex = format!("{:06x}", rng.gen::<u32>() & 0x00ff_ffff);
        let path = parent.join(format!("{base}-tma-{hex}"));
        if path.exists() {
            continue;
        }
        let branch = format!("tm/{PROVIDER}-{item_id}-{hex}");
        git(
            rt,
            repo_path,
            &["worktree", "add", "-b", &branch, &path_str(&path)?, "HEAD"],
        )?;
        return Ok((path, branch));
    }
    Err(TaskError::Internal(
        "could not pick a free worktree name".into(),
    ))
}

/// The checked-out branch, `None` on a detached HEAD.
pub fn current_branch(rt: &Runtime, dir: &Path) -> Result<Option<String>, TaskError> {
    let branch = git(rt, dir, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let branch = branch.trim();
    Ok((!branch.is_empty() && branch != "HEAD").then(|| branch.to_string()))
}

/// The canonical git common directory of `dir`: the one a repository's main
/// checkout and all of its linked worktrees share.
pub fn common_dir(rt: &Runtime, dir: &Path) -> Result<PathBuf, TaskError> {
    let common = git(
        rt,
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    PathBuf::from(common.trim())
        .canonicalize()
        .map_err(|e| TaskError::Internal(format!("could not resolve the git directory: {e}")))
}

/// The origin a checkout fetches from, reduced to what two clones of one
/// repository share: `OWNER/NAME` on github.com, else the URL without a
/// trailing `/` or `.git`. Compared only, never shown.
fn origin_identity(rt: &Runtime, dir: &Path) -> Option<String> {
    let url = git(rt, dir, &["config", "--get", "remote.origin.url"]).ok()?;
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    Some(github_repo(url).unwrap_or_else(|| {
        let plain = url.trim_end_matches('/');
        plain.strip_suffix(".git").unwrap_or(plain).to_string()
    }))
}

/// Whether `path` is a checkout's own top level. A plain directory inside some
/// other checkout is not: it would otherwise stand for that whole checkout.
fn is_checkout(rt: &Runtime, path: &Path) -> bool {
    git(rt, path, &["rev-parse", "--show-toplevel"])
        .ok()
        .and_then(|top| PathBuf::from(top.trim()).canonicalize().ok())
        .is_some_and(|top| path.canonicalize().ok() == Some(top))
}

/// Refuse `dir` unless it is the checkout at `repo_path` (the allowlisted
/// repository a person knows as `label`) or a linked worktree of it. Another
/// clone of the same origin is not the repository: it is named as such.
pub fn require_in_repo(
    rt: &Runtime,
    dir: &Path,
    repo_path: &Path,
    label: &str,
) -> Result<(), TaskError> {
    let checkout = repo_path.display();
    let not_git = || {
        TaskError::Usage(format!(
            "the repository path of {label} ({checkout}) is not a git repository"
        ))
    };
    if !is_checkout(rt, repo_path) {
        return Err(not_git());
    }
    let expected = common_dir(rt, repo_path).map_err(|_| not_git())?;
    let found = common_dir(rt, dir).ok();
    if found.as_ref() == Some(&expected) {
        return Ok(());
    }
    let origin = origin_identity(rt, repo_path);
    if found.is_some() && origin.is_some() && origin_identity(rt, dir) == origin {
        return Err(TaskError::Usage(format!(
            "dir is a separate clone of {label}; use the checkout at {checkout} or a linked worktree of it"
        )));
    }
    Err(TaskError::Usage(format!(
        "dir must be the checkout of {label} at {checkout} or a linked worktree of it"
    )))
}

/// The default branch `origin/HEAD` names. Unresolved is a refusal, never a
/// guess: nothing may be pushed while the default branch is unknown.
pub fn default_branch(rt: &Runtime, dir: &Path) -> Result<String, TaskError> {
    git(rt, dir, &["rev-parse", "--abbrev-ref", "origin/HEAD"])
        .ok()
        .and_then(|name| {
            name.trim()
                .strip_prefix("origin/")
                .filter(|branch| !branch.is_empty() && *branch != "HEAD")
                .map(str::to_string)
        })
        .ok_or_else(|| {
            TaskError::Conflict(
                "the default branch of origin is not known here; run `git remote set-head origin -a` in the repository, then retry".into(),
            )
        })
}

/// Remove a worktree the agent created; `delete_branch` also drops its branch
/// (used when a start fails before anything was recorded).
pub fn remove(
    rt: &Runtime,
    worktree: &Path,
    branch: Option<&str>,
    delete_branch: bool,
) -> Result<(), TaskError> {
    let common = git(
        rt,
        worktree,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    let common = PathBuf::from(common.trim());
    let mut command = Command::new(&rt.git);
    command
        .arg("--git-dir")
        .arg(&common)
        .args(["worktree", "remove", "--force"])
        .arg(worktree);
    run_ok(command, "git worktree remove")?;
    if let (true, Some(branch)) = (delete_branch, branch) {
        let mut command = Command::new(&rt.git);
        command
            .arg("--git-dir")
            .arg(&common)
            .args(["branch", "-D", branch]);
        run_ok(command, "git branch -D")?;
    }
    Ok(())
}
