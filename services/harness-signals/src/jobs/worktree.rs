//! Each job's own git worktree, on branch `nw/<first 12 hex of the fingerprint>`,
//! under `<signals>/worktrees/<fingerprint>`. It is kept after the job ends so a
//! person can resume; `jobs cleanup` removes it on request.

use super::{worktrees_dir, Job, JobError, JobResult, WorktreeRef};
use crate::store::Target;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// git with the project's hooks disabled and no prompts.
fn git(dir: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-c")
        .arg("core.hooksPath=/dev/null")
        .arg("-C")
        .arg(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null());
    command
}

fn run(mut command: Command) -> Result<String> {
    let out = command.output().context("running git")?;
    if !out.status.success() {
        bail!(
            "git failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn ok(mut command: Command) -> bool {
    command
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A branch name git accepts and that cannot be read as an option.
fn valid_branch(root: &Path, name: &str) -> bool {
    if name.is_empty() || name.starts_with('-') || name.len() > 200 {
        return false;
    }
    let mut command = git(root);
    command.args(["check-ref-format", "--branch", name]);
    ok(command)
}

/// The board's `default_branch`, else the remote HEAD, else the current branch.
pub fn base_branch(target: &Target) -> Result<String> {
    if let Some(branch) = target
        .board()
        .and_then(|b| b["default_branch"].as_str().map(str::to_owned))
    {
        if valid_branch(&target.root, &branch) {
            return Ok(branch);
        }
    }
    let mut command = git(&target.root);
    command.args([
        "symbolic-ref",
        "--quiet",
        "--short",
        "refs/remotes/origin/HEAD",
    ]);
    command.stderr(Stdio::null());
    if let Ok(remote) = run(command) {
        let branch = remote.strip_prefix("origin/").unwrap_or(&remote).to_owned();
        if valid_branch(&target.root, &branch) {
            return Ok(branch);
        }
    }
    let mut command = git(&target.root);
    command.args(["rev-parse", "--abbrev-ref", "HEAD"]);
    let current = run(command).context("the project has no current branch")?;
    if current != "HEAD" && valid_branch(&target.root, &current) {
        return Ok(current);
    }
    bail!("could not tell the project's base branch: set default_branch on its board")
}

pub fn planned(target: &Target, fp: &str, base: String) -> WorktreeRef {
    WorktreeRef {
        path: worktrees_dir(target).join(fp).display().to_string(),
        branch: format!("nw/{}", &fp[..12]),
        base_branch: base,
        base_commit: None,
    }
}

fn commit_of(root: &Path, rev: &str) -> Option<String> {
    let mut command = git(root);
    command
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{rev}^{{commit}}"),
        ])
        .stderr(Stdio::null());
    run(command).ok().filter(|sha| !sha.is_empty())
}

fn same_path(a: &Path, b: &Path) -> bool {
    a == b || matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// Worktrees other than `ours` that have `branch` checked out.
fn checked_out_elsewhere(root: &Path, branch: &str, ours: &Path) -> Result<Vec<PathBuf>> {
    let mut command = git(root);
    command.args(["worktree", "list", "--porcelain"]);
    let listing = run(command)?;
    let wanted = format!("branch refs/heads/{branch}");
    Ok(listing
        .split("\n\n")
        .filter(|block| block.lines().any(|line| line == wanted))
        .filter_map(|block| block.lines().find_map(|l| l.strip_prefix("worktree ")))
        .map(PathBuf::from)
        .filter(|path| !same_path(path, ours))
        .collect())
}

/// The branch may carry this job only when its tip is the job's base commit,
/// or descends from it through commits that exist on no other branch, remote
/// or tag (made on this branch, in the job's worktree).
fn branch_is_this_jobs(root: &Path, branch: &str, base_commit: &str) -> Result<(), String> {
    let tip = commit_of(root, &format!("refs/heads/{branch}"))
        .ok_or_else(|| format!("branch {branch} has no commit"))?;
    if tip == base_commit {
        return Ok(());
    }
    let mut ancestor = git(root);
    ancestor.args(["merge-base", "--is-ancestor", base_commit, &tip]);
    if !ok(ancestor) {
        return Err(format!(
            "branch {branch} is at {} and does not descend from the job's base commit {}",
            &tip[..12.min(tip.len())],
            &base_commit[..12.min(base_commit.len())]
        ));
    }
    let count = |only_here: bool| -> Option<String> {
        let mut command = git(root);
        command.args(["rev-list", "--count", &tip, &format!("^{base_commit}")]);
        if only_here {
            command.args([
                "--not",
                &format!("--exclude={branch}"),
                "--branches",
                "--remotes",
                "--tags",
            ]);
        }
        run(command).ok()
    };
    match (count(false), count(true)) {
        (Some(all), Some(own)) if all == own => Ok(()),
        _ => Err(format!(
            "branch {branch} carries commits after the job's base commit that also exist elsewhere, so they were not made by this job"
        )),
    }
}

fn reuse_refused(job: &Job, why: &str) -> anyhow::Error {
    anyhow::anyhow!(
        "{why}; refusing to reuse it. Run `jobs cleanup --target {} --job {}` (it keeps a branch with commits of its own: delete or rename that branch yourself), then analyze again",
        job.target_id,
        job.fingerprint
    )
}

/// Refuse a branch this job cannot prove is its own.
fn check_reuse(target: &Target, job: &Job, path: &Path) -> Result<()> {
    let branch = &job.worktree.branch;
    let elsewhere = checked_out_elsewhere(&target.root, branch, path)?;
    if let Some(other) = elsewhere.first() {
        return Err(reuse_refused(
            job,
            &format!("branch {branch} is checked out in {}", other.display()),
        ));
    }
    let Some(base_commit) = job.worktree.base_commit.as_deref() else {
        return Err(reuse_refused(
            job,
            &format!("branch {branch} already exists but this job never recorded creating it"),
        ));
    };
    branch_is_this_jobs(&target.root, branch, base_commit).map_err(|why| reuse_refused(job, &why))
}

/// The worktree a job runs in, and the commit its branch started from.
pub struct Ensured {
    pub path: PathBuf,
    pub base_commit: String,
}

/// The job's worktree path, refused unless it is exactly `<worktrees>/<fingerprint>`.
pub fn checked_path(target: &Target, job: &Job) -> JobResult<PathBuf> {
    let expected = worktrees_dir(target).join(&job.fingerprint);
    if Path::new(&job.worktree.path) != expected {
        return Err(JobError::Invalid(
            "the job's worktree is outside the harness worktree root".into(),
        ));
    }
    if let (Ok(real), Ok(root)) = (
        expected.canonicalize(),
        worktrees_dir(target).canonicalize(),
    ) {
        if real.parent() != Some(root.as_path()) {
            return Err(JobError::Invalid(
                "the job's worktree resolves outside the harness worktree root".into(),
            ));
        }
    }
    Ok(expected)
}

fn dirty(path: &Path) -> Result<bool> {
    let mut command = git(path);
    command.args(["status", "--porcelain", "--untracked-files=all"]);
    Ok(!run(command)?.is_empty())
}

fn is_worktree_root(path: &Path) -> bool {
    let mut command = git(path);
    command.args(["rev-parse", "--show-toplevel"]);
    match (run(command), path.canonicalize()) {
        (Ok(top), Ok(real)) => Path::new(&top).canonicalize().ok() == Some(real),
        _ => false,
    }
}

/// Create the worktree, or reuse the job's own clean worktree and branch.
/// An existing branch is reused only when `check_reuse` proves it is this job's.
pub fn ensure(target: &Target, job: &Job) -> Result<Ensured> {
    let path = checked_path(target, job).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let branch = &job.worktree.branch;
    let base = &job.worktree.base_branch;
    anyhow::ensure!(
        valid_branch(&target.root, base) && valid_branch(&target.root, branch),
        "unsafe branch name"
    );
    let reused = |path: PathBuf| -> Result<Ensured> {
        check_reuse(target, job, &path)?;
        let base_commit = job.worktree.base_commit.clone().unwrap_or_default();
        Ok(Ensured { path, base_commit })
    };
    if path.exists() {
        if !is_worktree_root(&path) {
            bail!("{} exists but is not a git worktree", path.display());
        }
        let mut head = git(&path);
        head.args(["symbolic-ref", "--quiet", "HEAD"]);
        if run(head).ok().as_deref() != Some(format!("refs/heads/{branch}").as_str()) {
            return Err(reuse_refused(
                job,
                &format!("the job's worktree is not on branch {branch}"),
            ));
        }
        if dirty(&path)? {
            bail!("the job's worktree has uncommitted changes; clean it or run jobs cleanup before reanalyzing");
        }
        return reused(path);
    }
    crate::store::private_dir(&worktrees_dir(target))?;
    let mut command = git(&target.root);
    command.args(["worktree", "add", "--quiet"]);
    // The path is absolute and both refs were checked, so none can be read as an option.
    if commit_of(&target.root, &format!("refs/heads/{branch}")).is_some() {
        let ensured = reused(path.clone())?;
        command.arg(&path).arg(branch);
        run(command).with_context(|| format!("creating the worktree for {}", job.fp8()))?;
        return Ok(ensured);
    }
    let start = if commit_of(&target.root, base).is_some() {
        base.clone()
    } else {
        format!("origin/{base}")
    };
    let base_commit = commit_of(&target.root, &start)
        .with_context(|| format!("the base branch {base} has no commit"))?;
    command.args(["-b", branch]).arg(&path).arg(&base_commit);
    run(command).with_context(|| format!("creating the worktree for {}", job.fp8()))?;
    Ok(Ensured { path, base_commit })
}

/// Remove the worktree and, when it has no commits of its own, its branch.
pub fn cleanup(target: &Target, job: &Job) -> Result<Vec<String>> {
    let path = checked_path(target, job).map_err(|e| anyhow::anyhow!(e.to_string()))?;
    let mut done = Vec::new();
    if path.exists() {
        let mut command = git(&target.root);
        command.args(["worktree", "remove"]).arg(&path);
        run(command).context("removing the worktree (it may have uncommitted changes)")?;
        done.push("worktree removed".to_owned());
    }
    let branch = &job.worktree.branch;
    let mut exists = git(&target.root);
    exists.args([
        "show-ref",
        "--verify",
        "--quiet",
        &format!("refs/heads/{branch}"),
    ]);
    if ok(exists) {
        let mut count = git(&target.root);
        count.args([
            "rev-list",
            "--count",
            &format!(
                "{}..{branch}",
                job.worktree
                    .base_commit
                    .as_deref()
                    .unwrap_or(&job.worktree.base_branch)
            ),
        ]);
        if run(count).ok().as_deref() == Some("0") {
            let mut delete = git(&target.root);
            delete.args(["branch", "-D", branch]);
            run(delete)?;
            done.push("branch deleted".to_owned());
        } else {
            done.push("branch kept: it has commits of its own".to_owned());
        }
    }
    Ok(done)
}
