//! Any other source: a command that prints `{"issues": [...]}` (or a bare
//! array) in the issue shape documented in docs/observability-heatmap.md.

use crate::model::{clean_level, clean_title, clean_url, Frame, Issue};
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const OUTPUT_LIMIT: u64 = 8 * 1024 * 1024;

#[derive(Deserialize)]
struct Row {
    id: Value,
    title: String,
    #[serde(default)]
    level: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    count: u64,
    #[serde(default)]
    users: u64,
    #[serde(default)]
    first_seen: Option<String>,
    #[serde(default)]
    last_seen: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    frames: Vec<Frame>,
}

pub fn poll(argv: &[String], timeout: Duration, source: &str) -> Result<Vec<Issue>> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {}", argv[0]))?;
    let mut stdout = child.stdout.take().context("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(OUTPUT_LIMIT).read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{} did not finish within {}s", argv[0], timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    let out = reader.join().unwrap_or_default();
    if !status.success() {
        bail!("{} exited with {status}", argv[0]);
    }
    parse(&out, source)
}

pub fn parse(out: &[u8], source: &str) -> Result<Vec<Issue>> {
    let value: Value = serde_json::from_slice(out).context("the command printed invalid JSON")?;
    let rows = value.get("issues").unwrap_or(&value);
    let rows: Vec<Row> = serde_json::from_value(rows.clone())
        .context("the command's issues have the wrong shape")?;
    Ok(rows
        .into_iter()
        .filter_map(|row| {
            let id = match &row.id {
                Value::String(s) if !s.is_empty() => s.chars().take(128).collect(),
                Value::Number(n) => n.to_string(),
                _ => return None,
            };
            let status = match row.status.as_deref() {
                Some("resolved") => "resolved",
                Some("ignored") => "ignored",
                _ => "open",
            };
            Some(Issue {
                key: format!("{source}#{id}"),
                source: source.to_owned(),
                id,
                title: clean_title(&row.title),
                level: clean_level(row.level.as_deref().unwrap_or("error")),
                status: status.to_owned(),
                count: row.count,
                users: row.users,
                first_seen: row.first_seen,
                last_seen: row.last_seen,
                url: clean_url(row.url.as_deref()),
                frames: row.frames.into_iter().take(50).collect(),
                buckets: Vec::new(),
                frames_checked: false,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_print_issues_and_failures_are_reported() {
        let json = r#"{"issues":[{"id":1,"title":"Boom","level":"warn","count":3,"frames":[{"path":"src/app/a.php","line":3}]}]}"#;
        let issues = poll(
            &["printf".into(), "%s".into(), json.into()],
            Duration::from_secs(10),
            "command:0",
        )
        .unwrap();
        assert_eq!(issues[0].key, "command:0#1");
        assert_eq!(issues[0].level, "warning");
        assert!(issues[0].frames[0].in_app);
        assert!(poll(&["false".into()], Duration::from_secs(10), "c").is_err());
        let slow = poll(&["sleep".into(), "5".into()], Duration::from_secs(1), "c");
        assert!(slow.unwrap_err().to_string().contains("did not finish"));
        assert!(parse(b"not json", "c").is_err());
        assert_eq!(parse(br#"[{"id":"a","title":"t"}]"#, "c").unwrap().len(), 1);
    }
}
