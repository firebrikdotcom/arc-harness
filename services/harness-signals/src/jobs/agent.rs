//! The two agent profiles, both read-only and without MCP servers:
//!
//! - Claude: only the Read, Grep and Glob tools, `--permission-mode dontAsk`
//!   (anything needing approval is denied, so nothing can stall on a prompt),
//!   no user/project/local settings, and a strict, empty MCP configuration.
//!   The session id is preassigned with `--session-id`.
//! - Codex: `--sandbox read-only`, approval policy `never`, no MCP servers.
//!   Codex has no preassignment; background runs read `thread_id` from the
//!   first `thread.started` JSONL event and the id is never invented.
//!
//! Replies are taken from the launcher's output or the agent's own transcript
//! (found by session id) and passed to one marker parser.

use super::{packet, AgentKind};
use serde_json::Value;
use std::path::{Path, PathBuf};

const EMPTY_MCP: &str = r#"{"mcpServers":{}}"#;
const CLAUDE_TOOLS: &str = "Read,Grep,Glob";

fn claude_profile() -> Vec<String> {
    [
        "--tools",
        CLAUDE_TOOLS,
        "--permission-mode",
        "dontAsk",
        "--setting-sources",
        "",
        "--strict-mcp-config",
        "--mcp-config",
        EMPTY_MCP,
    ]
    .map(str::to_owned)
    .to_vec()
}

fn codex_config() -> Vec<String> {
    ["-c", "approval_policy=\"never\"", "-c", "mcp_servers={}"]
        .map(str::to_owned)
        .to_vec()
}

/// Interactive arguments for a Herdr tab (the prompt is sent separately).
pub fn interactive_args(kind: AgentKind, session: Option<&str>) -> Vec<String> {
    match kind {
        AgentKind::Claude => {
            let mut args = Vec::new();
            if let Some(session) = session {
                args.extend(["--session-id".to_owned(), session.to_owned()]);
            }
            args.extend(claude_profile());
            args.extend(["--append-system-prompt".to_owned(), packet::policy()]);
            args
        }
        AgentKind::Codex => {
            let mut args: Vec<String> = ["--sandbox", "read-only", "--ask-for-approval", "never"]
                .map(str::to_owned)
                .to_vec();
            args.extend(["-c".to_owned(), "mcp_servers={}".to_owned()]);
            args
        }
    }
}

/// Arguments after the executable for a background run. `resume` continues
/// an existing session instead of starting one.
pub fn background_args(
    kind: AgentKind,
    worktree: &Path,
    session: &str,
    resume: bool,
    prompt: &str,
) -> Vec<String> {
    match kind {
        AgentKind::Claude => {
            let mut args = vec![
                if resume { "--resume" } else { "--session-id" }.to_owned(),
                session.to_owned(),
            ];
            args.extend(claude_profile());
            args.extend(["--append-system-prompt".to_owned(), packet::policy()]);
            args.extend(["-p", "--output-format", "json"].map(str::to_owned));
            args.push(prompt.to_owned());
            args
        }
        AgentKind::Codex if resume => {
            let mut args: Vec<String> = ["exec", "resume", session].map(str::to_owned).to_vec();
            args.extend(codex_config());
            args.extend(["-c", "sandbox_mode=\"read-only\"", "--json"].map(str::to_owned));
            args.push(format!("{}\n\n{prompt}", packet::policy()));
            args
        }
        AgentKind::Codex => {
            let mut args = vec![
                "exec".to_owned(),
                "--cd".to_owned(),
                worktree.display().to_string(),
            ];
            args.extend(["--sandbox", "read-only"].map(str::to_owned));
            args.extend(codex_config());
            args.push("--json".to_owned());
            args.push(format!("{}\n\n{prompt}", packet::policy()));
            args
        }
    }
}

/// The Codex thread id from a `thread.started` JSONL line.
pub fn thread_started(line: &str) -> Option<String> {
    let event: Value = serde_json::from_str(line.trim()).ok()?;
    (event["type"] == "thread.started")
        .then(|| event["thread_id"].as_str().map(str::to_owned))
        .flatten()
        .filter(|id| super::settings::safe_token(id))
}

fn texts(content: &Value) -> Vec<String> {
    match content {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items
            .iter()
            .filter(|c| matches!(c["type"].as_str(), Some("text" | "output_text")))
            .filter_map(|c| c["text"].as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    }
}

/// Assistant replies, in order, from one JSON value of any supported format:
/// Claude `-p --output-format json`, Claude transcripts, `codex exec --json`
/// events and Codex rollout files. User and system text is never included.
fn replies_of(value: &Value, out: &mut Vec<String>) {
    if let Some(result) = value["result"]
        .as_str()
        .filter(|_| value["type"] == "result" || value.get("session_id").is_some())
    {
        out.push(result.to_owned());
        return;
    }
    if value["type"] == "assistant" {
        out.extend(texts(&value["message"]["content"]));
        return;
    }
    if value["type"] == "item.completed" && value["item"]["type"] == "agent_message" {
        if let Some(text) = value["item"]["text"].as_str() {
            out.push(text.to_owned());
        }
        return;
    }
    let payload = &value["payload"];
    if value["type"] == "response_item"
        && payload["type"] == "message"
        && payload["role"] == "assistant"
    {
        out.extend(texts(&payload["content"]));
    } else if value["type"] == "event_msg" && payload["type"] == "task_complete" {
        if let Some(text) = payload["last_agent_message"].as_str() {
            out.push(text.to_owned());
        }
    }
}

/// Assistant replies from output that is one JSON document or JSONL.
pub fn replies(output: &str) -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(value) = serde_json::from_str::<Value>(output.trim()) {
        replies_of(&value, &mut out);
        return out;
    }
    for line in output.lines() {
        if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
            replies_of(&value, &mut out);
        }
    }
    out
}

#[derive(Clone, Copy, PartialEq)]
enum Last {
    Nothing,
    Working,
    Replied,
    Answered,
}

/// Whether a session transcript ends between steps: no tool call is open and
/// the last event is a finished assistant message or an answered tool result.
pub fn step_boundary(transcript: &str) -> bool {
    let mut open = std::collections::BTreeSet::new();
    let mut last = Last::Nothing;
    for line in transcript.lines() {
        let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
            continue;
        };
        let payload = &value["payload"];
        match (value["type"].as_str(), payload["type"].as_str()) {
            (Some("assistant"), _) => {
                let mut called = false;
                for block in value["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_use" {
                        open.insert(block["id"].as_str().unwrap_or("").to_owned());
                        called = true;
                    }
                }
                last = if called { Last::Working } else { Last::Replied };
            }
            (Some("user"), _) => {
                let mut answered = false;
                for block in value["message"]["content"].as_array().into_iter().flatten() {
                    if block["type"] == "tool_result" {
                        open.remove(block["tool_use_id"].as_str().unwrap_or(""));
                        answered = true;
                    }
                }
                last = if answered {
                    Last::Answered
                } else {
                    Last::Working
                };
            }
            (Some("response_item"), Some(kind)) => {
                let call = payload["call_id"].as_str().unwrap_or("").to_owned();
                if kind.ends_with("_call_output") {
                    open.remove(&call);
                    last = Last::Answered;
                } else if kind.ends_with("_call") {
                    open.insert(call);
                    last = Last::Working;
                } else if kind == "message" && payload["role"] == "assistant" {
                    last = Last::Replied;
                } else {
                    last = Last::Working;
                }
            }
            (Some("event_msg"), Some("task_complete")) => last = Last::Replied,
            (Some("event_msg"), Some("task_started" | "user_message")) => last = Last::Working,
            _ => {}
        }
    }
    open.is_empty() && matches!(last, Last::Replied | Last::Answered)
}

/// Claude's project bucket name for a working directory.
pub(super) fn claude_bucket(cwd: &Path) -> String {
    cwd.display()
        .to_string()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// How much of a long project bucket name Claude keeps before its hash suffix.
const CLAUDE_BUCKET_PREFIX: usize = 150;

/// The transcript of a session, located by its id and the job's worktree.
pub fn transcript(kind: AgentKind, home: &Path, worktree: &Path, session: &str) -> Option<PathBuf> {
    if !super::settings::safe_token(session) {
        return None;
    }
    match kind {
        AgentKind::Claude => {
            let projects = home.join(".claude/projects");
            let mut dirs = vec![worktree.to_owned()];
            if let Ok(real) = worktree.canonicalize() {
                dirs.push(real);
            }
            let file = format!("{session}.jsonl");
            let buckets: Vec<String> = dirs.iter().map(|dir| claude_bucket(dir)).collect();
            if let Some(exact) = buckets
                .iter()
                .map(|bucket| projects.join(bucket).join(&file))
                .find(|path| path.is_file())
            {
                return Some(exact);
            }
            // Claude shortens long bucket names and appends a hash; the session id is unique.
            let found: Vec<PathBuf> = read_dirs(&projects)
                .into_iter()
                .map(|dir| dir.join(&file))
                .filter(|path| path.is_file())
                .collect();
            let prefixed = |path: &PathBuf| {
                let name = path
                    .parent()
                    .and_then(Path::file_name)
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                buckets.iter().any(|bucket| {
                    name.starts_with(&bucket[..bucket.len().min(CLAUDE_BUCKET_PREFIX)])
                })
            };
            found
                .iter()
                .find(|path| prefixed(path))
                .cloned()
                .or_else(|| (found.len() == 1).then(|| found[0].clone()))
        }
        AgentKind::Codex => {
            let suffix = format!("-{session}.jsonl");
            let sessions = home.join(".codex/sessions");
            let mut found = None;
            for year in read_dirs(&sessions) {
                for month in read_dirs(&year) {
                    for day in read_dirs(&month) {
                        for entry in std::fs::read_dir(&day).into_iter().flatten().flatten() {
                            let name = entry.file_name().to_string_lossy().into_owned();
                            if name.starts_with("rollout-") && name.ends_with(&suffix) {
                                found = Some(entry.path());
                            }
                        }
                    }
                }
            }
            found
        }
    }
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir() && !p.is_symlink())
        .collect();
    dirs.sort();
    dirs
}
