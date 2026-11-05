//! `harness-signals jobs ...`: prints one JSON document. A failure prints
//! `{"error": {"code", "message"}}` and exits 3 (not found), 4 (conflict),
//! 5 (invalid), 6 (quota) or 1 (anything else).

use super::runner::{self, Context};
use super::settings::Settings;
use super::{actions, JobError, JobResult};
use crate::config;
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

pub const USAGE: &str = "       harness-signals jobs list [--target ID]
       harness-signals jobs show --target ID --job FINGERPRINT
       harness-signals jobs analyze --target ID --issue KEY --card CARD [--reanalyze]
       harness-signals jobs stop|takeover|handback --target ID --job FINGERPRINT
       harness-signals jobs cleanup --target ID --job FINGERPRINT [--force-detached]
       harness-signals jobs difit start|stop --target ID --job FINGERPRINT
       harness-signals jobs run [--once]";

#[derive(Default)]
struct Args {
    action: Vec<String>,
    target: Option<String>,
    job: Option<String>,
    issue: Option<String>,
    card: Option<String>,
    reanalyze: bool,
    force_detached: bool,
    once: bool,
}

fn parse(args: Vec<String>) -> JobResult<Args> {
    let mut out = Args::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let mut value = |name: &str| {
            args.next()
                .ok_or_else(|| JobError::Invalid(format!("{name} needs a value")))
        };
        match arg.as_str() {
            "--target" => out.target = Some(value("--target")?),
            "--job" => out.job = Some(value("--job")?),
            "--issue" => out.issue = Some(value("--issue")?),
            "--card" => out.card = Some(value("--card")?),
            "--reanalyze" => out.reanalyze = true,
            "--force-detached" => out.force_detached = true,
            "--once" => out.once = true,
            other if !other.starts_with('-') && out.action.len() < 2 => {
                out.action.push(other.to_owned())
            }
            other => return Err(JobError::Invalid(format!("unexpected argument {other:?}"))),
        }
    }
    Ok(out)
}

fn need<'a>(value: &'a Option<String>, name: &str) -> JobResult<&'a str> {
    value
        .as_deref()
        .ok_or_else(|| JobError::Invalid(format!("{name} is required")))
}

/// Run one `jobs` command; returns the process exit code.
pub fn main(db_root: &Path, config_path: &Path, args: Vec<String>) -> i32 {
    let loaded = config::load(config_path);
    let (config, config_error) = match loaded {
        Ok(config) => (config, None),
        Err(error) => (None, Some(format!("{error:#}"))),
    };
    let settings = Settings::from_config(config.as_ref());
    let ctx = Context {
        db_root,
        config: config.as_ref(),
        settings: &settings,
    };
    let result =
        parse(args).and_then(|args| dispatch(&ctx, config_path, config_error.as_deref(), &args));
    match result {
        Ok(Value::Null) => 0,
        Ok(value) => {
            println!(
                "{}",
                serde_json::to_string_pretty(&value).unwrap_or_default()
            );
            0
        }
        Err(error) => {
            println!(
                "{}",
                json!({"error": {"code": error.code(), "message": error.message()}})
            );
            error.exit_code()
        }
    }
}

fn dispatch(
    ctx: &Context,
    config_path: &Path,
    config_error: Option<&str>,
    args: &Args,
) -> JobResult<Value> {
    let action: Vec<&str> = args.action.iter().map(String::as_str).collect();
    let target = || need(&args.target, "--target");
    let job = || need(&args.job, "--job");
    match action.as_slice() {
        ["list"] => actions::list_all(ctx, args.target.as_deref()),
        ["show"] => actions::show(ctx, target()?, job()?),
        ["analyze"] => {
            if let Some(error) = config_error {
                return Err(JobError::Invalid(format!(
                    "signals.json is invalid: {error}"
                )));
            }
            let (job, created) = actions::analyze(
                ctx,
                target()?,
                need(&args.issue, "--issue")?,
                need(&args.card, "--card")?,
                args.reanalyze,
            )?;
            if created {
                runner::kick(ctx, config_path);
            }
            let show = actions::show(ctx, &job.target_id, &job.fingerprint)?;
            Ok(json!({"job": show["job"], "created": created}))
        }
        ["stop"] => Ok(json!({"job": actions::stop(ctx, target()?, job()?)?})),
        ["takeover"] => {
            let (job, commands) = actions::takeover(ctx, target()?, job()?)?;
            Ok(json!({"job": job, "resume_commands": commands}))
        }
        ["handback"] => {
            let job = actions::handback(ctx, target()?, job()?)?;
            runner::kick(ctx, config_path);
            let show = actions::show(ctx, &job.target_id, &job.fingerprint)?;
            Ok(json!({"job": show["job"]}))
        }
        ["cleanup"] => {
            let (job, done) = actions::cleanup(ctx, target()?, job()?, args.force_detached)?;
            Ok(json!({"job": job, "done": done}))
        }
        ["difit", "start"] => Ok(json!({"job": actions::difit_start(ctx, target()?, job()?)?})),
        ["difit", "stop"] => Ok(json!({"job": actions::difit_stop(ctx, target()?, job()?)?})),
        ["run"] => {
            let spawner = runner::process_spawner(ctx.db_root.to_owned(), config_path.to_owned());
            loop {
                runner::reap();
                let report = runner::run_once(ctx, &spawner)?;
                runner::record_tick(ctx.db_root);
                if args.once {
                    return Ok(report);
                }
                std::thread::sleep(Duration::from_secs(15));
            }
        }
        ["supervise"] => {
            runner::supervise(ctx, target()?, job()?)?;
            Ok(Value::Null)
        }
        _ => Err(JobError::Invalid(format!("usage:\n{USAGE}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_takes_force_detached() {
        let argv = |a: &[&str]| a.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let args = parse(argv(&[
            "cleanup",
            "--target",
            "t",
            "--job",
            "f",
            "--force-detached",
        ]))
        .unwrap();
        assert!(args.force_detached && args.action == ["cleanup"]);
        assert!(!parse(argv(&["cleanup"])).unwrap().force_detached);
        assert!(USAGE.contains("cleanup --target ID --job FINGERPRINT [--force-detached]"));
    }
}
