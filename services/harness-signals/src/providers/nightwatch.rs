//! Laravel Nightwatch, two ways. Polling: after a one-time OAuth sign-in the
//! service reads issues from Nightwatch's MCP server, like any MCP client.
//! Webhooks: Nightwatch signs and pushes `issue.opened`, `reopened`,
//! `resolved` and `ignored`, carrying the exception's class, file and line;
//! each opened or reopened delivery counts as one occurrence.

use crate::model::{clean_title, clean_url, Frame, Issue};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::collections::BTreeMap;

pub const SOURCE: &str = "nightwatch";

/// Run `work` against Nightwatch's MCP server, refreshing the access token and
/// retrying once when the server refuses it.
pub fn with_client<T>(
    credentials: &std::path::Path,
    mut work: impl FnMut(&mut crate::mcp::Client) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    let attempt =
        |force: bool, work: &mut dyn FnMut(&mut crate::mcp::Client) -> anyhow::Result<T>| {
            let (token, url) = crate::oauth::access_token(credentials, force)?;
            let mut client = crate::mcp::Client::connect(&url, &token)?;
            work(&mut client)
        };
    match attempt(false, &mut work) {
        Err(error) if error.downcast_ref::<crate::mcp::Unauthorized>().is_some() => {
            attempt(true, &mut work)
        }
        other => other,
    }
}

/// `Nightwatch-Signature`: hex HMAC-SHA256 of the raw body, compared in constant time.
pub fn verify(secret: &str, body: &[u8], signature: &str) -> bool {
    let signature = signature.trim();
    let signature = signature.strip_prefix("sha256=").unwrap_or(signature);
    let Ok(expected) = hex::decode(signature) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

#[derive(Debug, PartialEq)]
pub enum Outcome {
    Updated(String),
    Ignored(&'static str),
}

/// Apply one delivery to the target's issues.
pub fn apply(
    issues: &mut BTreeMap<String, Issue>,
    delivery: &Value,
    environment: Option<&str>,
    application_id: Option<&str>,
) -> Outcome {
    let event = delivery.get("event").and_then(Value::as_str).unwrap_or("");
    let payload = delivery.get("payload").unwrap_or(&Value::Null);
    if let Some(wanted) = application_id {
        if payload.get("application_id").and_then(Value::as_str) != Some(wanted) {
            return Outcome::Ignored("another application");
        }
    }
    let Some(issue) = payload.get("issue") else {
        return Outcome::Ignored("no issue");
    };
    let id = match issue.get("id") {
        Some(Value::String(s)) if !s.is_empty() && s.len() <= 64 => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Outcome::Ignored("no issue id"),
    };
    let key = format!("{SOURCE}#{id}");
    let at = delivery
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
    match event {
        "issue.resolved" | "issue.ignored" => {
            let status = if event == "issue.resolved" {
                "resolved"
            } else {
                "ignored"
            };
            match issues.get_mut(&key) {
                Some(known) => {
                    known.status = status.to_owned();
                    Outcome::Updated(key)
                }
                None => Outcome::Ignored("unknown issue"),
            }
        }
        "issue.opened" | "issue.reopened" => {
            if let Some(wanted) = environment {
                let name = payload.pointer("/environment/name").and_then(Value::as_str);
                if name.is_some_and(|n| n != wanted) {
                    return Outcome::Ignored("another environment");
                }
            }
            let details = issue.get("details").unwrap_or(&Value::Null);
            let text = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
            let handled = details
                .get("handled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let title = text(issue, "title")
                .or_else(|| text(details, "class"))
                .unwrap_or_default();
            let frames: Vec<Frame> = text(details, "file")
                .map(|path| Frame {
                    path,
                    line: details.get("line").and_then(Value::as_u64),
                    in_app: true,
                })
                .into_iter()
                .collect();
            let entry = issues.entry(key.clone()).or_insert_with(|| Issue {
                key: key.clone(),
                source: SOURCE.to_owned(),
                id: id.clone(),
                title: String::new(),
                level: String::new(),
                status: "open".to_owned(),
                count: 0,
                users: 0,
                first_seen: Some(at.clone()),
                last_seen: None,
                url: None,
                frames: Vec::new(),
                buckets: Vec::new(),
                frames_checked: false,
            });
            entry.title = clean_title(&title);
            entry.level = if handled { "warning" } else { "error" }.to_owned();
            entry.status = "open".to_owned();
            entry.count += 1;
            entry.last_seen = Some(at);
            entry.url = clean_url(issue.get("url").and_then(Value::as_str)).or(entry.url.take());
            if !frames.is_empty() {
                entry.frames = frames;
            }
            Outcome::Updated(key)
        }
        _ => Outcome::Ignored("event not tracked"),
    }
}

#[cfg(test)]
pub mod fixtures {
    pub fn delivery(event: &str, environment: &str) -> String {
        format!(
            r#"{{"event":"{event}","timestamp":"2026-10-23T11:58:00Z","payload":{{
              "webhook_id":"w","application_id":"app-1","organization_id":"o",
              "issue":{{"id":"9f1","ref":"NW-12","type":"exception","title":"ErrorException: Undefined index","status":"open","priority":"high",
                "url":"https://nightwatch.laravel.com/issues/9f1",
                "details":{{"class":"ErrorException","message":"Undefined index: secret","file":"/var/www/html/src/app/Http/Controller.php","line":20,"handled":false}}}},
              "environment":{{"id":"e","name":"{environment}"}}}}}}"#
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, body: &[u8]) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    #[test]
    fn signatures_are_checked_against_the_raw_body() {
        let body = b"{\"event\":\"issue.opened\"}";
        let good = sign("s3cret", body);
        assert!(verify("s3cret", body, &good));
        assert!(verify("s3cret", body, &format!("sha256={good}")));
        assert!(!verify("other", body, &good));
        assert!(!verify("s3cret", b"{}", &good));
        assert!(!verify("s3cret", body, "not-hex"));
    }

    #[test]
    fn deliveries_open_count_and_close_issues() {
        let mut issues = BTreeMap::new();
        let parse = |s: String| serde_json::from_str::<Value>(&s).unwrap();
        let opened = parse(fixtures::delivery("issue.opened", "production"));
        assert_eq!(
            apply(&mut issues, &opened, Some("production"), Some("app-1")),
            Outcome::Updated("nightwatch#9f1".into())
        );
        apply(
            &mut issues,
            &parse(fixtures::delivery("issue.reopened", "production")),
            Some("production"),
            None,
        );
        let issue = &issues["nightwatch#9f1"];
        assert_eq!(issue.count, 2);
        assert_eq!(issue.level, "error");
        assert_eq!(issue.title, "ErrorException: Undefined index");
        assert_eq!(issue.frames[0].line, Some(20));
        let stored = serde_json::to_string(issue).unwrap();
        assert!(
            !stored.contains("secret"),
            "the exception message is not kept"
        );

        assert_eq!(
            apply(
                &mut issues,
                &parse(fixtures::delivery("issue.opened", "staging")),
                Some("production"),
                None
            ),
            Outcome::Ignored("another environment")
        );
        assert_eq!(
            apply(&mut issues, &opened, None, Some("app-2")),
            Outcome::Ignored("another application")
        );
        apply(
            &mut issues,
            &parse(fixtures::delivery("issue.resolved", "production")),
            None,
            None,
        );
        assert_eq!(issues["nightwatch#9f1"].status, "resolved");
    }
}
