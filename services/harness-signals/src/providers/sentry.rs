//! Sentry: unresolved issues seen in the last 24 hours, with hourly counts,
//! and the stack frames of each issue's latest event (fetched once per issue).

use crate::model::{clean_level, clean_title, clean_url, Frame, Issue};
use anyhow::{bail, Context, Result};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// Latest-event lookups per poll, so a burst of new issues cannot exhaust the rate limit.
const FRAME_FETCHES: usize = 20;

pub struct Sentry<'a> {
    pub base_url: &'a str,
    pub org: &'a str,
    pub project: &'a str,
    pub token: String,
    pub limit: u32,
    pub environment: Option<&'a str>,
    pub source: String,
}

impl Sentry<'_> {
    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(30))
            .redirects(0)
            .build()
    }

    fn get(&self, agent: &ureq::Agent, url: &str, query: &[(&str, &str)]) -> Result<Value> {
        let mut request = agent
            .get(url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/json");
        for (key, value) in query {
            request = request.query(key, value);
        }
        match request.call() {
            Ok(response) => response
                .into_json()
                .context("Sentry answered with invalid JSON"),
            // Never echo the response body: it can carry event data.
            Err(ureq::Error::Status(code, _)) => bail!("Sentry answered HTTP {code} for {url}"),
            Err(ureq::Error::Transport(error)) => {
                bail!("could not reach Sentry ({})", error.kind())
            }
        }
    }

    /// Current issues for this source; `known` supplies frames fetched earlier.
    pub fn poll(&self, known: &BTreeMap<String, Issue>) -> Result<Vec<Issue>> {
        let agent = Self::agent();
        let base = self.base_url.trim_end_matches('/');
        let url = format!(
            "{base}/api/0/projects/{}/{}/issues/",
            self.org, self.project
        );
        let limit = self.limit.to_string();
        let mut query = vec![
            ("query", "is:unresolved lastSeen:-24h"),
            ("statsPeriod", "24h"),
            ("limit", limit.as_str()),
        ];
        if let Some(environment) = self.environment {
            query.push(("environment", environment));
        }
        let listed = self.get(&agent, &url, &query)?;
        let Some(rows) = listed.as_array() else {
            bail!("Sentry issue list was not an array");
        };
        let mut fetched = 0;
        let mut issues = Vec::new();
        for row in rows {
            let Some(mut issue) = parse_issue(row, &self.source) else {
                continue;
            };
            if let Some(old) = known.get(&issue.key).filter(|old| old.frames_checked) {
                issue.frames = old.frames.clone();
                issue.frames_checked = true;
            } else if fetched < FRAME_FETCHES {
                fetched += 1;
                let event_url = format!(
                    "{base}/api/0/organizations/{}/issues/{}/events/latest/",
                    self.org, issue.id
                );
                // A failed lookup is retried on the next poll; an event without frames is not.
                match self.get(&agent, &event_url, &[]) {
                    Ok(event) => {
                        issue.frames = event_frames(&event);
                        issue.frames_checked = true;
                    }
                    Err(error) => eprintln!("harness-signals: {}: {error:#}", self.source),
                }
            }
            if issue.frames.is_empty() {
                if let Some(file) = row.pointer("/metadata/filename").and_then(Value::as_str) {
                    issue.frames.push(Frame {
                        path: file.to_owned(),
                        line: None,
                        in_app: true,
                    });
                }
            }
            issues.push(issue);
        }
        Ok(issues)
    }
}

fn number(value: Option<&Value>) -> u64 {
    match value {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
        Some(Value::String(s)) => s.parse().unwrap_or(0),
        _ => 0,
    }
}

pub fn parse_issue(row: &Value, source: &str) -> Option<Issue> {
    let id = row.get("id")?.as_str()?;
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let text = |key: &str| row.get(key).and_then(Value::as_str);
    let buckets: Vec<(i64, u64)> = row
        .pointer("/stats/24h")
        .and_then(Value::as_array)
        .map(|points| {
            points
                .iter()
                .filter_map(|p| Some((p.get(0)?.as_i64()?, p.get(1)?.as_u64()?)))
                .collect()
        })
        .unwrap_or_default();
    let window: u64 = buckets.iter().map(|(_, n)| n).sum();
    let status = match text("status").unwrap_or("unresolved") {
        "resolved" => "resolved",
        "ignored" | "muted" => "ignored",
        _ => "open",
    };
    Some(Issue {
        key: format!("{source}#{id}"),
        source: source.to_owned(),
        id: id.to_owned(),
        title: clean_title(text("title").unwrap_or("")),
        level: clean_level(text("level").unwrap_or("error")),
        status: status.to_owned(),
        count: if buckets.is_empty() {
            number(row.get("count"))
        } else {
            window
        },
        users: number(row.get("userCount")),
        first_seen: text("firstSeen").map(str::to_owned),
        last_seen: text("lastSeen").map(str::to_owned),
        url: clean_url(text("permalink")),
        frames: Vec::new(),
        buckets,
        frames_checked: false,
    })
}

/// Frames of the event's most recent exception (or its thread stack), innermost first.
pub fn event_frames(event: &Value) -> Vec<Frame> {
    let entries = event
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let stack = entries
        .iter()
        .find(|e| e.get("type").and_then(Value::as_str) == Some("exception"))
        .and_then(|e| e.pointer("/data/values").and_then(Value::as_array))
        .and_then(|values| {
            values
                .iter()
                .rev()
                .find_map(|v| v.pointer("/stacktrace/frames").and_then(Value::as_array))
        })
        .or_else(|| {
            entries
                .iter()
                .find(|e| e.get("type").and_then(Value::as_str) == Some("stacktrace"))
                .and_then(|e| e.pointer("/data/frames").and_then(Value::as_array))
        });
    stack
        .map(|frames| {
            frames
                .iter()
                .rev()
                .filter_map(|f| {
                    let path = f
                        .get("absPath")
                        .and_then(Value::as_str)
                        .filter(|p| !p.is_empty())
                        .or_else(|| f.get("filename").and_then(Value::as_str))?;
                    Some(Frame {
                        path: path.to_owned(),
                        line: f.get("lineNo").and_then(Value::as_u64),
                        in_app: f.get("inApp").and_then(Value::as_bool).unwrap_or(false),
                    })
                })
                .take(50)
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
pub mod fixtures {
    pub const ISSUES: &str = r#"[
      {"id": "101", "title": "QueryException: SQLSTATE[HY000]\nsecret details", "level": "error",
       "status": "unresolved", "count": "999", "userCount": 7,
       "firstSeen": "2026-10-23T10:00:00Z", "lastSeen": "2026-10-23T11:55:00Z",
       "permalink": "https://acme.sentry.io/issues/101/",
       "metadata": {"filename": "src/models/User.php"},
       "stats": {"24h": [[1792744800, 3], [1792748400, 12]]}},
      {"id": "102", "title": "Deprecated call", "level": "warning", "status": "unresolved",
       "count": "4", "userCount": 0, "firstSeen": "2026-10-01T00:00:00Z", "lastSeen": "2026-10-23T09:00:00Z",
       "permalink": "https://acme.sentry.io/issues/102/"},
      {"id": "../x", "title": "bad id"}
    ]"#;

    pub const EVENT: &str = r#"{"entries": [
      {"type": "request", "data": {"url": "https://example.com/?token=secret"}},
      {"type": "exception", "data": {"values": [
        {"type": "PDOException", "stacktrace": {"frames": [{"filename": "vendor/a.php", "inApp": false}]}},
        {"type": "QueryException", "stacktrace": {"frames": [
          {"absPath": "/var/www/html/vendor/laravel/x.php", "inApp": false, "lineNo": 1},
          {"absPath": "/var/www/html/src/app/Http/Controller.php", "inApp": true, "lineNo": 20},
          {"absPath": "/var/www/html/src/models/User.php", "inApp": true, "lineNo": 42}
        ]}}
      ]}}
    ]}"#;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issues_and_frames_parse_from_sentry_shapes() {
        let rows: Value = serde_json::from_str(fixtures::ISSUES).unwrap();
        let issues: Vec<_> = rows
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|r| parse_issue(r, "sentry:acme/api"))
            .collect();
        assert_eq!(issues.len(), 2);
        let first = &issues[0];
        assert_eq!(first.key, "sentry:acme/api#101");
        assert_eq!(first.title, "QueryException: SQLSTATE[HY000]");
        assert_eq!(first.count, 15, "window count comes from the 24h buckets");
        assert_eq!(first.users, 7);
        assert_eq!(first.status, "open");
        assert_eq!(issues[1].level, "warning");
        assert_eq!(issues[1].count, 4);

        let frames = event_frames(&serde_json::from_str(fixtures::EVENT).unwrap());
        assert_eq!(frames[0].path, "/var/www/html/src/models/User.php");
        assert_eq!(frames[0].line, Some(42));
        assert!(frames[0].in_app);
        assert_eq!(frames.len(), 3);
        assert!(!frames[2].in_app);
    }
}
