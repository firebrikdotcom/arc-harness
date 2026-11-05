//! Herdr launch adapter, used only when `issue_agents.herdr` is enabled in
//! `signals.json`. Every call is an argv with a timeout; replies are parsed
//! strictly and a malformed one fails the launch.
//!
//!   herdr tab create --workspace W --cwd WORKTREE --label issue-<fp8> --no-focus
//!       (result.tab.tab_id, result.root_pane.pane_id)
//!   herdr agent start issue-<fp8> --kind claude|codex --pane PANE -- <agent args>
//!   herdr agent prompt issue-<fp8> PROMPT --wait --until done --until idle --until blocked --timeout MS
//!   herdr agent get issue-<fp8>     (result.agent.agent_status; result.agent.agent_session.value is the session id)
//!
//! Stop and timeout interrupt the agent and confirm it settled:
//!
//!   herdr agent send-keys issue-<fp8> esc
//!   herdr agent wait issue-<fp8> --until idle --until done --timeout MS
//!   herdr tab close TAB                    (only when it did not go idle)
//!   herdr pane process-info --pane PANE    (the pane must be gone after the close)
//!
//! Observation only reads: `agent get`, and `pane process-info` when Herdr no
//! longer knows the agent (`error.code` ending in `not_found`).

use super::settings::safe_token;
use super::{AgentKind, HerdrTab};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const CALL_TIMEOUT: Duration = Duration::from_secs(60);
const OUTPUT_LIMIT: usize = 1 << 20;

pub struct Herdr<'a> {
    pub argv: &'a [String],
}

/// What Herdr says about a job's agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    /// Herdr knows the agent; its `agent_status`.
    Present(String),
    /// The agent is gone; how that was seen.
    Gone(String),
    /// Herdr could not tell (not running, timed out, an unexpected reply).
    Unknown(String),
}

fn failed(value: &Value) -> bool {
    value.get("ok") == Some(&Value::Bool(false)) || value.get("error").is_some_and(|e| !e.is_null())
}

fn not_found(value: &Value) -> bool {
    value["error"]["code"]
        .as_str()
        .is_some_and(|code| code.ends_with("not_found"))
}

/// A Herdr reply as `(exit ok, JSON)`. Herdr prints its error JSON to stderr
/// with exit 1, so a failed call whose stdout is not JSON is read from stderr.
pub(crate) fn parse_reply(
    ok: bool,
    stdout: &str,
    stderr: &str,
) -> std::result::Result<(bool, Value), String> {
    if let Ok(value) = serde_json::from_str::<Value>(stdout.trim()) {
        return Ok((ok, value));
    }
    if ok {
        return Err("printed invalid JSON".into());
    }
    serde_json::from_str::<Value>(stderr.trim())
        .map(|value| (false, value))
        .map_err(|_| format!("failed: {}", super::bounded(stderr, 300)))
}

/// Wait for a child with a deadline, collecting bounded stdout.
pub fn finish(mut child: Child, timeout: Duration) -> Result<(bool, String, String)> {
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let out = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = stdout.as_mut() {
            let _ = pipe.take(OUTPUT_LIMIT as u64).read_to_end(&mut buffer);
        }
        buffer
    });
    let err = std::thread::spawn(move || {
        let mut buffer = Vec::new();
        if let Some(pipe) = stderr.as_mut() {
            let _ = pipe.take(64 * 1024).read_to_end(&mut buffer);
        }
        buffer
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    let stdout = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err.join().unwrap_or_default()).into_owned();
    Ok((status.success(), stdout, stderr))
}

fn find<'v>(value: &'v Value, path: &[&str]) -> Option<&'v Value> {
    let direct = path.iter().try_fold(value, |v, key| v.get(*key));
    direct.or_else(|| path.iter().try_fold(&value["result"], |v, key| v.get(*key)))
}

fn token(value: &Value, path: &[&str]) -> Result<String> {
    let found = find(value, path)
        .and_then(Value::as_str)
        .filter(|v| safe_token(v));
    found
        .map(str::to_owned)
        .with_context(|| format!("herdr reply has no valid {}", path.join(".")))
}

impl Herdr<'_> {
    fn command(&self, args: &[String]) -> Command {
        let mut command = Command::new(&self.argv[0]);
        command
            .args(&self.argv[1..])
            .args(args)
            .env(
                "PATH",
                super::settings::child_path(std::path::Path::new(&self.argv[0]).parent()),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    /// Start a call; the caller decides how long to wait.
    pub fn spawn(&self, args: &[String]) -> Result<Child> {
        self.command(args).spawn().context("starting herdr")
    }

    fn call(&self, args: &[String], timeout: Duration) -> Result<Value> {
        let (ok, stdout, stderr) = finish(self.spawn(args)?, timeout)
            .with_context(|| format!("herdr {}", args[..2.min(args.len())].join(" ")))?;
        if !ok {
            bail!(
                "herdr {} failed: {}",
                args[..2.min(args.len())].join(" "),
                super::bounded(&stderr, 300)
            );
        }
        let value: Value = serde_json::from_str(stdout.trim()).with_context(|| {
            format!(
                "herdr {} printed invalid JSON",
                args[..2.min(args.len())].join(" ")
            )
        })?;
        if value.get("ok") == Some(&Value::Bool(false))
            || value.get("error").is_some_and(|e| !e.is_null())
        {
            bail!(
                "herdr {} reported an error: {}",
                args[..2.min(args.len())].join(" "),
                super::bounded(&value["error"].to_string(), 300)
            );
        }
        Ok(value)
    }

    pub fn create_tab(&self, workspace: &str, cwd: &str, label: &str) -> Result<HerdrTab> {
        let args = [
            "tab",
            "create",
            "--workspace",
            workspace,
            "--cwd",
            cwd,
            "--label",
            label,
            "--no-focus",
        ]
        .map(str::to_owned);
        let reply = self.call(&args, CALL_TIMEOUT)?;
        let tab_id = token(&reply, &["tab", "tab_id"])
            .or_else(|_| token(&reply, &["root_pane", "tab_id"]))
            .context("herdr tab create printed no valid tab id")?;
        Ok(HerdrTab {
            tab_id,
            pane_id: token(&reply, &["root_pane", "pane_id"])?,
        })
    }

    pub fn start_agent(
        &self,
        name: &str,
        kind: AgentKind,
        pane: &str,
        agent_args: &[String],
    ) -> Result<()> {
        let mut args: Vec<String> = [
            "agent",
            "start",
            name,
            "--kind",
            kind.name(),
            "--pane",
            pane,
        ]
        .map(str::to_owned)
        .to_vec();
        args.push("--".to_owned());
        args.extend(agent_args.iter().cloned());
        self.call(&args, CALL_TIMEOUT + Duration::from_secs(30))
            .map(|_| ())
    }

    /// `prompt --wait` returns when the agent is done, idle or blocked after this prompt.
    pub fn prompt_args(name: &str, prompt: &str, timeout: Duration) -> Vec<String> {
        let mut args: Vec<String> = ["agent", "prompt", name, prompt]
            .map(str::to_owned)
            .to_vec();
        args.extend(
            [
                "--wait",
                "--until",
                "done",
                "--until",
                "idle",
                "--until",
                "blocked",
                "--timeout",
            ]
            .map(str::to_owned),
        );
        args.push(timeout.as_millis().to_string());
        args
    }

    /// A call judged by its exit status; a JSON reply that reports an error fails it too.
    fn call_ok(&self, args: &[String], timeout: Duration) -> Result<()> {
        let what = args[..2.min(args.len())].join(" ");
        let (ok, stdout, stderr) =
            finish(self.spawn(args)?, timeout).with_context(|| format!("herdr {what}"))?;
        if !ok {
            bail!("herdr {what} failed: {}", super::bounded(&stderr, 300));
        }
        if let Ok(value) = serde_json::from_str::<Value>(stdout.trim()) {
            if value.get("ok") == Some(&Value::Bool(false))
                || value.get("error").is_some_and(|e| !e.is_null())
            {
                bail!(
                    "herdr {what} reported an error: {}",
                    super::bounded(&value["error"].to_string(), 300)
                );
            }
        }
        Ok(())
    }

    /// `herdr agent send-keys NAME KEY...`, e.g. `esc` to interrupt the current turn.
    pub fn send_keys(&self, name: &str, keys: &[&str]) -> Result<()> {
        let mut args = vec!["agent".to_owned(), "send-keys".to_owned(), name.to_owned()];
        args.extend(keys.iter().map(|k| (*k).to_owned()));
        self.call_ok(&args, CALL_TIMEOUT)
    }

    /// `herdr agent wait NAME --until S... --timeout MS`.
    pub fn wait(&self, name: &str, until: &[&str], timeout: Duration) -> Result<()> {
        let mut args = vec!["agent".to_owned(), "wait".to_owned(), name.to_owned()];
        for state in until {
            args.extend(["--until".to_owned(), (*state).to_owned()]);
        }
        args.extend(["--timeout".to_owned(), timeout.as_millis().to_string()]);
        self.call_ok(&args, timeout + Duration::from_secs(10))
    }

    /// `herdr tab close TAB_ID`.
    pub fn close_tab(&self, tab_id: &str) -> Result<()> {
        anyhow::ensure!(safe_token(tab_id), "invalid Herdr tab id");
        self.call_ok(
            &["tab".to_owned(), "close".to_owned(), tab_id.to_owned()],
            CALL_TIMEOUT,
        )
    }

    /// Whether `herdr pane process-info --pane PANE` still finds the pane.
    pub fn pane_exists(&self, pane_id: &str) -> bool {
        safe_token(pane_id)
            && self
                .call_ok(
                    &[
                        "pane".to_owned(),
                        "process-info".to_owned(),
                        "--pane".to_owned(),
                        pane_id.to_owned(),
                    ],
                    CALL_TIMEOUT,
                )
                .is_ok()
    }

    /// A call's JSON reply whatever its exit status: Herdr reports a missing
    /// agent or pane as `{"error": {"code": "..._not_found"}}` and exits 1.
    fn reply(&self, args: &[String]) -> Result<(bool, Value)> {
        let what = args[..2.min(args.len())].join(" ");
        let (ok, stdout, stderr) =
            finish(self.spawn(args)?, CALL_TIMEOUT).with_context(|| format!("herdr {what}"))?;
        parse_reply(ok, &stdout, &stderr).map_err(|why| anyhow::anyhow!("herdr {what} {why}"))
    }

    /// Whether the job's agent is still in Herdr, judged only by reading:
    /// `agent get`, then `pane process-info` when Herdr no longer knows the agent.
    pub fn presence(&self, name: &str, pane: &str, kind: AgentKind) -> Presence {
        let get = ["agent", "get", name].map(str::to_owned);
        let value = match self.reply(&get) {
            Ok((true, value)) if !failed(&value) => {
                return match find(&value, &["agent"]) {
                    Some(agent) => Presence::Present(
                        ["agent_status", "status", "state"]
                            .iter()
                            .find_map(|key| agent[*key].as_str())
                            .unwrap_or("unknown")
                            .to_owned(),
                    ),
                    None => Presence::Unknown("herdr agent get printed no agent".into()),
                }
            }
            Ok((_, value)) => value,
            Err(error) => return Presence::Unknown(format!("{error:#}")),
        };
        if !not_found(&value) {
            return Presence::Unknown(format!(
                "herdr agent get reported {}",
                super::bounded(&value["error"].to_string(), 200)
            ));
        }
        if !safe_token(pane) {
            return Presence::Gone(format!("Herdr no longer has agent {name}"));
        }
        let info = ["pane", "process-info", "--pane", pane].map(str::to_owned);
        match self.reply(&info) {
            Ok((_, value)) if not_found(&value) => Presence::Gone(format!(
                "Herdr no longer has agent {name} and its tab is gone (pane {pane} not found)"
            )),
            Ok((true, value)) if !failed(&value) => {
                let runs_agent = find(&value, &["process_info", "foreground_processes"])
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .any(|p| {
                        [&p["name"], &p["argv0"]]
                            .iter()
                            .any(|v| v.as_str() == Some(kind.name()))
                    });
                if runs_agent {
                    Presence::Unknown(format!(
                        "Herdr no longer lists agent {name}, but pane {pane} still runs {}",
                        kind.name()
                    ))
                } else {
                    Presence::Gone(format!(
                        "Herdr no longer has agent {name}; its pane {pane} no longer runs {}",
                        kind.name()
                    ))
                }
            }
            _ => Presence::Gone(format!("Herdr no longer has agent {name}")),
        }
    }

    /// `(state, session id)` from `herdr agent get`.
    pub fn get(&self, name: &str) -> Result<(String, Option<String>)> {
        let reply = self.call(
            &["agent".to_owned(), "get".to_owned(), name.to_owned()],
            CALL_TIMEOUT,
        )?;
        let agent = find(&reply, &["agent"]).context("herdr agent get printed no agent")?;
        let state = ["agent_status", "status", "state"]
            .iter()
            .find_map(|key| agent[*key].as_str())
            .unwrap_or("unknown")
            .to_owned();
        let session = agent["agent_session"]["value"]
            .as_str()
            .filter(|v| safe_token(v))
            .map(str::to_owned);
        Ok((state, session))
    }
}
