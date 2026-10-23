//! The one issue shape every provider is normalised into. Only what the heat
//! layer needs is kept: no messages beyond a short title, no request data, no
//! user identifiers.

use serde::{Deserialize, Serialize};

const TITLE_LIMIT: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub path: String,
    #[serde(default)]
    pub line: Option<u64>,
    #[serde(default = "yes")]
    pub in_app: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Issue {
    /// `<source>#<id>`, unique within a target.
    pub key: String,
    pub source: String,
    pub id: String,
    pub title: String,
    /// `fatal`, `error`, `warning` or `info`.
    pub level: String,
    /// `open`, `resolved` or `ignored`; only open issues count.
    pub status: String,
    /// Events in the provider's window (or received, for webhooks).
    pub count: u64,
    pub users: u64,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
    pub url: Option<String>,
    /// Stack frames, innermost first.
    #[serde(default)]
    pub frames: Vec<Frame>,
    /// Hourly `(unix seconds, events)` buckets, when the provider has them.
    #[serde(default)]
    pub buckets: Vec<(i64, u64)>,
    /// The provider was already asked for this issue's frames (even if it had none).
    #[serde(default)]
    pub frames_checked: bool,
}

/// UUIDs and email addresses in titles often identify a person; replace them.
pub fn scrub(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let uuid_at = |i: usize| {
        i + 36 <= chars.len()
            && (i == 0 || !chars[i - 1].is_ascii_alphanumeric())
            && chars[i..i + 36].iter().enumerate().all(|(k, c)| {
                if matches!(k, 8 | 13 | 18 | 23) {
                    *c == '-'
                } else {
                    c.is_ascii_hexdigit()
                }
            })
    };
    let local = |c: char| c.is_ascii_alphanumeric() || "._%+-".contains(c);
    let domain = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-';
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if uuid_at(i) {
            out.push_str("<id>");
            i += 36;
            continue;
        }
        if chars[i] == '@' {
            let start = out.chars().rev().take_while(|c| local(*c)).count();
            let end = chars[i + 1..].iter().take_while(|c| domain(**c)).count();
            let host: String = chars[i + 1..i + 1 + end].iter().collect();
            if start > 0 && host.contains('.') {
                for _ in 0..start {
                    out.pop();
                }
                out.push_str("<email>");
                i += 1 + end;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// One line, at most 200 characters, without identifiers.
pub fn clean_title(raw: &str) -> String {
    let line = scrub(raw.lines().next().unwrap_or("").trim());
    let line = line.as_str();
    let mut title: String = line.chars().take(TITLE_LIMIT).collect();
    if line.chars().count() > TITLE_LIMIT {
        title.push('…');
    }
    if title.is_empty() {
        "(untitled)".to_owned()
    } else {
        title
    }
}

pub fn clean_level(raw: &str) -> String {
    match raw.to_ascii_lowercase().as_str() {
        "fatal" | "critical" | "emergency" | "alert" => "fatal",
        "warning" | "warn" | "notice" => "warning",
        "info" | "debug" | "log" | "sample" => "info",
        _ => "error",
    }
    .to_owned()
}

/// Only http(s) links reach the dashboard.
pub fn clean_url(raw: Option<&str>) -> Option<String> {
    raw.filter(|url| url.starts_with("https://") || url.starts_with("http://"))
        .map(|url| url.chars().take(500).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn titles_levels_and_links_are_bounded() {
        assert_eq!(clean_title("Boom\nsecret stack line"), "Boom");
        let long = "x".repeat(300);
        assert_eq!(clean_title(&long).chars().count(), 201);
        assert_eq!(clean_title("  "), "(untitled)");
        assert_eq!(
            clean_title(
                "Member 4ab61b31-2fd8-45f7-a07c-faf7dd2ff2a1 billing (ops@example.com) at a@b"
            ),
            "Member <id> billing (<email>) at a@b"
        );
        assert_eq!(clean_level("CRITICAL"), "fatal");
        assert_eq!(clean_level("warn"), "warning");
        assert_eq!(clean_level("whatever"), "error");
        assert_eq!(clean_url(Some("javascript:alert(1)")), None);
        assert_eq!(
            clean_url(Some("https://sentry.io/x")).as_deref(),
            Some("https://sentry.io/x")
        );
    }
}
