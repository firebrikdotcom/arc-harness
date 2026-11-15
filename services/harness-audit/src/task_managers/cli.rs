//! `harness-audit tasks attach|agent ...`: the agent commands, run without the
//! server (the command buses are built over the same database).
//!
//! Output is JSON on stdout; errors are `{"error": {"code", "message"}}`.
//! Exits: 0 ok, 1 internal, 2 usage, 4 not found, 6 conflict.
//! `attach --print` prints the resume command, then the SSH variant if any.

use std::path::PathBuf;

use serde_json::{json, Value};

use super::agents::{self, Runtime, StartRequest};
use super::{valid_code, TaskError, TaskManagers};
use crate::domain::task_agent::events::AgentKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentAction {
    List,
    Status,
    Stop,
    Handback,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TasksCommand {
    Attach {
        code: String,
        print: bool,
        force: bool,
    },
    Start {
        item: String,
        repo: Option<String>,
        dir: Option<PathBuf>,
        agent: AgentKind,
    },
    Agent {
        action: AgentAction,
        code: Option<String>,
    },
}

pub const USAGE: &str = "usage: harness-audit tasks attach CODE [--print] [--force]
       harness-audit tasks agent start --item ID [--repo NAME] [--dir PATH] [--agent claude|codex]
       harness-audit tasks agent list|status|stop|handback|done [--code C]";

fn code(value: &str) -> Result<String, String> {
    if valid_code(value) {
        Ok(value.to_string())
    } else {
        Err(format!(
            "invalid code {value:?}; codes are tm- and 6 lowercase hex digits"
        ))
    }
}

fn parse_start(rest: &[&str]) -> Result<TasksCommand, String> {
    let (mut item, mut repo, mut dir, mut agent) = (None, None, None, None);
    let mut rest = rest.iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?
            .to_string();
        let slot = match *flag {
            "--item" => &mut item,
            "--repo" => &mut repo,
            "--dir" => &mut dir,
            "--agent" => &mut agent,
            other => return Err(format!("unexpected argument {other:?}")),
        };
        if slot.replace(value).is_some() {
            return Err(format!("{flag} given twice"));
        }
    }
    let agent = match agent.as_deref() {
        None => AgentKind::Claude,
        Some(kind) => AgentKind::parse(kind).ok_or("--agent is claude or codex")?,
    };
    Ok(TasksCommand::Start {
        item: item.ok_or("start needs --item ID")?,
        repo,
        dir: dir.map(PathBuf::from),
        agent,
    })
}

/// Parse the arguments after `tasks`.
pub fn parse(args: &[String]) -> Result<TasksCommand, String> {
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    match args.as_slice() {
        ["attach", rest @ ..] => {
            let (mut found, mut print, mut force) = (None, false, false);
            for arg in rest {
                match *arg {
                    "--print" if !print => print = true,
                    "--force" if !force => force = true,
                    value if !value.starts_with('-') && found.is_none() => {
                        found = Some(code(value)?)
                    }
                    other => return Err(format!("unexpected argument {other:?}")),
                }
            }
            let code = found.ok_or("attach needs a CODE")?;
            Ok(TasksCommand::Attach { code, print, force })
        }
        ["agent", "start", rest @ ..] => parse_start(rest),
        ["agent", action, rest @ ..] => {
            let action = match *action {
                "list" => AgentAction::List,
                "status" => AgentAction::Status,
                "stop" => AgentAction::Stop,
                "handback" => AgentAction::Handback,
                "done" => AgentAction::Done,
                other => return Err(format!("unknown agent action {other:?}")),
            };
            let code = match rest {
                [] => None,
                ["--code", value] => Some(code(value)?),
                _ => return Err("expected at most --code C".into()),
            };
            if code.is_none() && action != AgentAction::List {
                return Err("this action needs --code C".into());
            }
            Ok(TasksCommand::Agent { action, code })
        }
        _ => Err("expected attach or agent".into()),
    }
}

/// Exit code for an error kind.
pub fn exit_code(error: &TaskError) -> i32 {
    match error {
        TaskError::Usage(_) => 2,
        TaskError::NotFound(_) => 4,
        TaskError::Conflict(_) => 6,
        TaskError::Internal(_) => 1,
    }
}

fn fail(error: &TaskError) -> i32 {
    println!(
        "{}",
        json!({"error": {"code": error.kind(), "message": error.to_string()}})
    );
    exit_code(error)
}

/// Execute a parsed command and return its JSON document. `Attach` here only
/// takes the session over and reports the commands; [`run`] also execs it.
pub async fn execute(
    tm: &TaskManagers,
    rt: &Runtime,
    command: TasksCommand,
) -> Result<Value, TaskError> {
    Ok(match command {
        TasksCommand::Attach { code, force, .. } => {
            let plan = agents::attach(tm, rt, &code, force).await?;
            json!({"agent": agents::view(plan.record.clone(), rt.ssh.as_deref()), "commands": plan.commands})
        }
        TasksCommand::Start {
            item,
            repo,
            dir,
            agent,
        } => {
            let view = agents::start(
                tm,
                rt,
                StartRequest {
                    item_id: item,
                    repo,
                    dir,
                    agent_kind: agent,
                },
            )
            .await?;
            json!({"agent_id": view.agent_id, "code": view.record.code, "state": view.record.state, "agent": view})
        }
        TasksCommand::Agent { action, code } => {
            let code = code.unwrap_or_default();
            match action {
                AgentAction::List => json!({"agents": agents::list(tm, rt)?}),
                AgentAction::Status => json!({"agent": agents::status(tm, rt, &code)?}),
                AgentAction::Stop => json!({"agent": agents::stop(tm, rt, &code).await?}),
                AgentAction::Handback => {
                    json!({"agent": agents::handback(tm, rt, &code).await?})
                }
                AgentAction::Done => serde_json::to_value(agents::done(tm, rt, &code).await?)
                    .map_err(|e| TaskError::Internal(e.to_string()))?,
            }
        }
    })
}

/// Run `tasks ...`; returns the process exit code.
pub async fn run(args: &[String]) -> i32 {
    let command = match parse(args) {
        Ok(command) => command,
        Err(message) => {
            eprintln!("{USAGE}");
            return fail(&TaskError::Usage(message));
        }
    };
    let url = std::env::var("DATABASE_URL").unwrap_or_else(|_| "database/database.sqlite".into());
    let tm = match TaskManagers::open(&url).await {
        Ok(tm) => tm,
        Err(error) => return fail(&error),
    };
    let mut rt = Runtime::from_env();
    // The SSH destination may come from task-managers.json; best effort.
    rt.resolve_ssh();
    if let TasksCommand::Attach { code, print, force } = &command {
        return match agents::attach(&tm, &rt, code, *force).await {
            Ok(plan) if *print => {
                println!("{}", plan.commands.local);
                if let Some(ssh) = &plan.commands.ssh {
                    println!("{ssh}");
                }
                0
            }
            Ok(plan) => {
                let error = agents::attach::exec(&plan);
                fail(&TaskError::Internal(format!(
                    "could not start the session: {error}"
                )))
            }
            Err(error) => fail(&error),
        };
    }
    match execute(&tm, &rt, command).await {
        Ok(document) => {
            println!("{document}");
            0
        }
        Err(error) => fail(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn parses_attach_start_and_agent() {
        assert_eq!(
            parse(&args("attach tm-0a1b2c --print --force")),
            Ok(TasksCommand::Attach {
                code: "tm-0a1b2c".into(),
                print: true,
                force: true
            })
        );
        assert_eq!(
            parse(&args("agent stop --code tm-0a1b2c")),
            Ok(TasksCommand::Agent {
                action: AgentAction::Stop,
                code: Some("tm-0a1b2c".into())
            })
        );
        assert_eq!(
            parse(&args("agent start --item 12 --agent codex --dir /tmp")),
            Ok(TasksCommand::Start {
                item: "12".into(),
                repo: None,
                dir: Some(PathBuf::from("/tmp")),
                agent: AgentKind::Codex
            })
        );
        assert!(parse(&args("agent start --agent gpt --item 1")).is_err());
        assert!(parse(&args("agent start")).is_err());
        assert!(parse(&args("agent list")).is_ok());
        assert!(parse(&args("agent stop")).is_err());
        assert!(parse(&args("attach TM-0A1B2C")).is_err());
        assert!(parse(&args("attach")).is_err());
        assert!(parse(&args("bogus")).is_err());
    }

    #[test]
    fn exit_codes_are_distinct() {
        assert_eq!(exit_code(&TaskError::Usage(String::new())), 2);
        assert_eq!(exit_code(&TaskError::NotFound(String::new())), 4);
        assert_eq!(exit_code(&TaskError::Conflict(String::new())), 6);
        assert_eq!(exit_code(&TaskError::Internal(String::new())), 1);
    }
}
