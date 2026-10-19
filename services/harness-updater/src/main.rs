//! One-way, CI-gated distribution. Never writes the developer checkout.
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    env,
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::{fs::symlink, io::AsRawFd},
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Deserialize, Serialize)]
struct Config {
    repository: String,
    url: String,
    branch: String,
    workflows: Vec<String>,
    developer_root: PathBuf,
    state: PathBuf,
    git: PathBuf,
    gh: Option<PathBuf>,
    python: PathBuf,
    #[serde(default = "interval")]
    poll_seconds: u64,
}
fn interval() -> u64 {
    60
}
impl Config {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.state.is_absolute() && self.developer_root.is_absolute(),
            "State and developer paths must be absolute"
        );
        ensure!(
            self.state != self.developer_root && !self.state.starts_with(&self.developer_root),
            "Managed state must be outside the developer checkout"
        );
        ensure!(
            !self.workflows.is_empty(),
            "At least one required CI workflow must be configured"
        );
        ensure!(
            !self.branch.starts_with('-') && !self.branch.is_empty(),
            "Invalid branch"
        );
        ensure!(
            self.repository.split('/').count() == 2 && !self.repository.starts_with('-'),
            "Repository must be owner/name"
        );
        ensure!(!self.url.starts_with('-'), "Invalid remote URL");
        if self.url.starts_with("https://github.com/") {
            ensure!(
                self.url.trim_end_matches(".git").trim_end_matches('/')
                    == format!("https://github.com/{}", self.repository),
                "CI repository must match the source remote"
            );
        }
        ensure!(
            self.poll_seconds >= 10,
            "Poll interval must be at least ten seconds"
        );
        for p in [&self.git, &self.python].into_iter().chain(self.gh.iter()) {
            ensure!(
                p.is_absolute() && p.is_file(),
                "Missing absolute tool path: {}",
                p.display()
            );
        }
        Ok(())
    }
}
fn stamp() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos()
}
fn atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension(format!("tmp-{}-{}", std::process::id(), stamp()));
    let mut f = OpenOptions::new().create_new(true).write(true).open(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    File::open(path.parent().context("Missing parent")?)?.sync_all()?;
    Ok(())
}
fn lock(path: &Path, exclusive: bool, nonblocking: bool) -> Result<Option<File>> {
    unsafe extern "C" {
        fn flock(fd: i32, operation: i32) -> i32;
    }
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    let op = (if exclusive { 2 } else { 1 }) | (if nonblocking { 4 } else { 0 });
    if unsafe { flock(file.as_raw_fd(), op) } != 0 {
        let e = std::io::Error::last_os_error();
        if nonblocking && e.kind() == std::io::ErrorKind::WouldBlock {
            return Ok(None);
        }
        return Err(e.into());
    }
    Ok(Some(file))
}
// Capture into files: verbose subprocesses cannot fill a pipe and deadlock.
fn output(cfg: &Config, mut cmd: Command) -> Result<String> {
    let prefix = cfg
        .state
        .join(format!("command-{}-{}", std::process::id(), stamp()));
    let out = prefix.with_extension("out");
    let err = prefix.with_extension("err");
    cmd.stdout(File::create(&out)?)
        .stderr(File::create(&err)?)
        .stdin(Stdio::null());
    let result = (|| -> Result<String> {
        let mut child = cmd.spawn().context("Cannot start updater dependency")?;
        let began = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait()? {
                break s;
            }
            if began.elapsed() > Duration::from_secs(120) {
                child.kill()?;
                child.wait()?;
                bail!("Updater dependency timed out");
            }
            thread::sleep(Duration::from_millis(50));
        };
        ensure!(
            status.success(),
            "Updater dependency failed: {}",
            fs::read_to_string(&err)?
                .chars()
                .take(1500)
                .collect::<String>()
        );
        Ok(fs::read_to_string(&out)?.trim().to_owned())
    })();
    let _ = fs::remove_file(out);
    let _ = fs::remove_file(err);
    result
}
fn git(cfg: &Config, args: &[&str]) -> Result<String> {
    let mut c = Command::new(&cfg.git);
    c.args(args).env("GIT_TERMINAL_PROMPT", "0");
    output(cfg, c)
}
fn sha(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|c| c.is_ascii_hexdigit())
}
fn ci(cfg: &Config, revision: &str) -> Result<bool> {
    for workflow in &cfg.workflows {
        let authenticated = cfg.gh.as_ref().map(|gh| {
            let mut cmd = Command::new(gh);
            cmd.args([
                "run",
                "list",
                "--repo",
                &cfg.repository,
                "--commit",
                revision,
                "--branch",
                &cfg.branch,
                "--event",
                "push",
                "--workflow",
                workflow,
                "--limit",
                "100",
                "--json",
                "headSha,status,conclusion,databaseId",
            ]);
            output(cfg, cmd)
        });
        let rows: Vec<Value> = match authenticated {
            Some(Ok(body)) => serde_json::from_str(&body)?,
            other => {
                // Public repositories need no copied credentials. Private/API errors fail closed.
                ensure!(
                    cfg.url.starts_with("https://github.com/"),
                    "CI lookup failed: {:?}",
                    other
                );
                let url = format!("https://api.github.com/repos/{}/actions/workflows/{}/runs?head_sha={}&branch={}&event=push&per_page=100", cfg.repository, encode(workflow), revision, encode(&cfg.branch));
                let mut cmd = Command::new("curl");
                cmd.args([
                    "--fail",
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "30",
                    "-H",
                    "Accept: application/vnd.github+json",
                    &url,
                ]);
                let body: Value = serde_json::from_str(&output(cfg, cmd)?)?;
                body["workflow_runs"]
                    .as_array()
                    .context("Invalid GitHub workflow response")?
                    .clone()
            }
        };
        let latest = rows
            .iter()
            .filter(|r| r["headSha"].as_str().or(r["head_sha"].as_str()) == Some(revision))
            .max_by_key(|r| r["databaseId"].as_u64().or(r["id"].as_u64()).unwrap_or(0));
        if !latest.is_some_and(|r| r["status"] == "completed" && r["conclusion"] == "success") {
            return Ok(false);
        }
    }
    Ok(true)
}
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn read_link(path: &Path) -> Option<PathBuf> {
    fs::read_link(path).ok()
}
fn swap(path: &Path, target: &Path) -> Result<()> {
    let temp = path.with_extension(format!("link-{}-{}", std::process::id(), stamp()));
    symlink(target, &temp)?;
    fs::rename(&temp, path)?;
    File::open(path.parent().unwrap())?.sync_all()?;
    Ok(())
}
fn status(cfg: &Config, stage: &str, detail: &str) -> Result<()> {
    atomic(
        &cfg.state.join("status.json"),
        &serde_json::to_vec_pretty(
            &json!({"stage":stage,"detail":detail,"current":read_link(&cfg.state.join("current")),"previous":read_link(&cfg.state.join("previous")),"override":read_link(&cfg.state.join("override")),"checked_at":SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs()}),
        )?,
    )
}
fn environment(cfg: &Config, cmd: &mut Command) {
    let db = cfg.developer_root.join(".harness-db");
    cmd.env("HARNESS_CONTROL_DB_ROOT", &db)
        .env("HARNESS_DEVELOPER_ROOT", &cfg.developer_root);
    for (key, value) in [
        ("HARNESS_DB_ROOT", db.clone()),
        ("HARNESS_WORKFLOW_STATE", db.join("workflow-state.sqlite")),
        ("HARNESS_AUDIT_OUTBOX", db.join("audit-outbox.sqlite")),
        ("HARNESS_DISPATCH_DB", db.join("dispatch.sqlite")),
        ("HARNESS_SESSION_SLIM_STATE", db.join("session-slim.json")),
    ] {
        cmd.env(key, value);
    }
    cmd.env_remove("HARNESS_ROOT")
        .env("HARNESS_RUNTIME_ACTIVE", "1");
}
fn prepare(cfg: &Config, revision: &str) -> Result<PathBuf> {
    let releases = cfg.state.join("releases");
    fs::create_dir_all(&releases)?;
    let dest = releases.join(revision);
    if dest.join(".ready").is_file() {
        return Ok(dest);
    }
    // Interrupted preparations are disposable; source and active releases are never removed.
    let stage = releases.join(format!("staging-{}-{}", std::process::id(), stamp()));
    fs::create_dir(&stage)?;
    let archive = stage.join("source.tar");
    let cache = cfg.state.join("repository.git");
    git(
        cfg,
        &[
            "--git-dir",
            cache.to_str().unwrap(),
            "archive",
            "--format=tar",
            "--output",
            archive.to_str().unwrap(),
            revision,
        ],
    )?;
    let mut tar = Command::new("tar");
    tar.args([
        "-xf",
        archive.to_str().unwrap(),
        "-C",
        stage.to_str().unwrap(),
    ]);
    output(cfg, tar)?;
    fs::remove_file(&archive)?;
    ensure!(
        stage.join("scripts/harness").is_file() && stage.join("scripts/install-hooks.sh").is_file(),
        "Release is not a harness installation"
    );
    let mut smoke = Command::new("sh");
    smoke.arg(stage.join("scripts/harness")).arg("--help");
    environment(cfg, &mut smoke);
    output(cfg, smoke)?;
    atomic(&stage.join(".ready"), revision.as_bytes())?;
    if dest.exists() {
        bail!(
            "Incomplete existing release {}; inspect before retrying",
            dest.display()
        );
    }
    fs::rename(&stage, &dest)?;
    Ok(dest)
}
fn activate(cfg: &Config, release: &Path) -> Result<bool> {
    let Some(_operations) = lock(&cfg.state.join("operations.lock"), true, true)? else {
        status(
            cfg,
            "waiting",
            "A harness operation is running; activation deferred",
        )?;
        return Ok(false);
    };
    if cfg.state.join("override").exists() {
        status(
            cfg,
            "override",
            "Development override is active; activation paused",
        )?;
        return Ok(false);
    }
    let current = cfg.state.join("current");
    if let Some(old) = read_link(&current) {
        if old == release {
            return Ok(true);
        }
        if old.starts_with(cfg.state.join("releases")) && old.join(".ready").is_file() {
            swap(&cfg.state.join("previous"), &old)?;
        }
    }
    swap(&current, release)?;
    status(cfg, "active", "Verified published version activated")?;
    Ok(true)
}
fn check(cfg: &Config) -> Result<()> {
    let Some(_update) = lock(&cfg.state.join("update.lock"), true, true)? else {
        return Ok(());
    };
    if cfg.state.join("override").exists() {
        return status(
            cfg,
            "override",
            "Development override is active; automatic updates paused",
        );
    }
    let cache = cfg.state.join("repository.git");
    if !cache.exists() {
        git(cfg, &["init", "--bare", cache.to_str().unwrap()])?;
        git(
            cfg,
            &[
                "--git-dir",
                cache.to_str().unwrap(),
                "remote",
                "add",
                "origin",
                &cfg.url,
            ],
        )?;
    }
    ensure!(
        git(
            cfg,
            &[
                "--git-dir",
                cache.to_str().unwrap(),
                "config",
                "--get",
                "remote.origin.url"
            ]
        )? == cfg.url,
        "Cache remote differs from configured repository"
    );
    git(
        cfg,
        &[
            "--git-dir",
            cache.to_str().unwrap(),
            "fetch",
            "--no-tags",
            "origin",
            &format!("+refs/heads/{}:refs/heads/published", cfg.branch),
        ],
    )?;
    let revision = git(
        cfg,
        &[
            "--git-dir",
            cache.to_str().unwrap(),
            "rev-parse",
            "refs/heads/published^{commit}",
        ],
    )?;
    ensure!(sha(&revision), "Invalid published revision");
    if read_link(&cfg.state.join("current")) == Some(cfg.state.join("releases").join(&revision)) {
        return status(cfg, "active", "Already using the latest published commit");
    }
    if !ci(cfg, &revision)? {
        return status(
            cfg,
            "waiting_ci",
            "Published commit has not passed every configured CI workflow",
        );
    }
    let release = prepare(cfg, &revision)?;
    activate(cfg, &release)?;
    Ok(())
}
fn execute(cfg: &Config, args: &[String]) -> Result<i32> {
    ensure!(!args.is_empty(), "exec requires a release-relative script");
    let _operations = lock(&cfg.state.join("operations.lock"), false, false)?;
    let root = read_link(&cfg.state.join("current"))
        .context("No active release; run check first")?
        .canonicalize()?;
    let rel = Path::new(&args[0]);
    ensure!(
        rel.components().all(|c| matches!(c, Component::Normal(_))),
        "Script must be a relative path without traversal"
    );
    let file = root.join(rel).canonicalize()?;
    ensure!(
        file.starts_with(&root) && file.is_file(),
        "Script escapes active release"
    );
    let mut command = if file.extension().is_some_and(|x| x == "py") {
        let mut c = Command::new(&cfg.python);
        c.arg(&file);
        c
    } else {
        Command::new(&file)
    };
    // @root arguments pin nested hook paths to the same immutable release.
    for arg in &args[1..] {
        if let Some(p) = arg.strip_prefix("@root/") {
            ensure!(
                Path::new(p)
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
                "Invalid nested path"
            );
            command.arg(root.join(p));
        } else {
            command.arg(arg);
        }
    }
    environment(cfg, &mut command);
    command.env("HARNESS_ROOT", &root);
    Ok(command.status()?.code().unwrap_or(1))
}
fn main() {
    if let Err(error) = run() {
        eprintln!("harness-updater: {error:#}");
        std::process::exit(1);
    }
}
fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    ensure!(args.len()>=3 && args[0]=="--config", "Usage: harness-updater --config FILE check|daemon|status|rollback|override PATH|resume|exec SCRIPT [ARGS]");
    let cfg: Config = serde_json::from_slice(&fs::read(&args[1])?)?;
    cfg.validate()?;
    fs::create_dir_all(&cfg.state)?;
    match args[2].as_str() {
        "check" => {
            if let Err(e) = check(&cfg) {
                status(&cfg, "error", &format!("{e:#}"))?;
                return Err(e);
            }
        }
        "daemon" => loop {
            if let Err(e) = check(&cfg) {
                eprintln!("harness-updater: {e:#}");
                let _ = status(&cfg, "error", &format!("{e:#}"));
            }
            thread::sleep(Duration::from_secs(cfg.poll_seconds));
        },
        "status" => println!(
            "{}",
            fs::read_to_string(cfg.state.join("status.json"))
                .unwrap_or_else(|_| "{\"stage\":\"not_checked\"}".into())
        ),
        "exec" => std::process::exit(execute(&cfg, &args[3..])?),
        "override" => {
            let _u = lock(&cfg.state.join("update.lock"), true, false)?;
            let _o = lock(&cfg.state.join("operations.lock"), true, false)?;
            let p =
                Path::new(args.get(3).context("override needs a checkout path")?).canonicalize()?;
            ensure!(
                p.join("scripts/harness").is_file(),
                "Not a harness checkout"
            );
            if let Some(old) = read_link(&cfg.state.join("current")) {
                if old.starts_with(cfg.state.join("releases")) && old.join(".ready").is_file() {
                    swap(&cfg.state.join("previous"), &old)?;
                }
            }
            swap(&cfg.state.join("override"), &p)?;
            swap(&cfg.state.join("current"), &p)?;
            status(&cfg, "override", "Development override enabled")?;
        }
        "resume" => {
            let _u = lock(&cfg.state.join("update.lock"), true, false)?;
            let _o = lock(&cfg.state.join("operations.lock"), true, false)?;
            let p = cfg.state.join("override");
            if p.exists() {
                fs::remove_file(p)?;
            }
            if let Some(current) = read_link(&cfg.state.join("current")) {
                if !current.starts_with(cfg.state.join("releases")) {
                    let previous = read_link(&cfg.state.join("previous"))
                        .context("No published release to resume")?;
                    swap(&cfg.state.join("current"), &previous)?;
                }
            }
            status(
                &cfg,
                "active",
                "Development override cleared; next poll checks for updates",
            )?;
        }
        "rollback" => {
            let _u = lock(&cfg.state.join("update.lock"), true, false)?;
            let _o = lock(&cfg.state.join("operations.lock"), true, false)?;
            let previous = read_link(&cfg.state.join("previous")).context("No previous release")?;
            ensure!(
                previous.join(".ready").is_file(),
                "Previous release is incomplete"
            );
            swap(&cfg.state.join("override"), &previous)?;
            swap(&cfg.state.join("current"), &previous)?;
            status(&cfg, "override", "Rolled back; updates paused until resume")?;
        }
        _ => bail!("Unknown updater command"),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_full_commit_ids() {
        assert!(sha(&"a".repeat(40)));
        assert!(!sha("main"));
        assert!(!sha(&"g".repeat(40)));
    }
    #[test]
    fn encodes_url_components() {
        assert_eq!(encode("release/v1 test"), "release%2Fv1%20test");
    }
    #[test]
    fn shared_operation_blocks_activation_lock() {
        let path = env::temp_dir().join(format!("harness-lock-{}-{}", std::process::id(), stamp()));
        let shared = lock(&path, false, false).unwrap();
        assert!(lock(&path, true, true).unwrap().is_none());
        drop(shared);
        assert!(lock(&path, true, true).unwrap().is_some());
        fs::remove_file(path).unwrap();
    }
}
