//! Polling Nightwatch through its MCP server. Only the read tools are ever
//! called: `list_applications`, `list_environments`, `list_issues` and
//! `get_issue`. Their answers are Markdown; only the fields the heat needs are
//! read from them (titles, timestamps, occurrence counts, stack frame paths).

use super::nightwatch::{with_client, SOURCE};
use crate::mcp::Client;
use crate::model::{clean_title, Frame, Issue};
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

const READ_TOOLS: [&str; 4] = [
    "list_applications",
    "list_environments",
    "list_issues",
    "get_issue",
];
/// Detail lookups per poll; the rest are filled in on later polls.
const DETAIL_FETCHES: usize = 15;
const PAGES: usize = 8;

pub struct Poll<'a> {
    pub credentials: &'a Path,
    pub application: Option<&'a str>,
    pub environment: Option<&'a str>,
    pub limit: usize,
}

fn read(client: &mut Client, tool: &str, arguments: Value) -> Result<String> {
    debug_assert!(READ_TOOLS.contains(&tool));
    if !READ_TOOLS.contains(&tool) {
        bail!("refusing to call {tool}: only read tools are allowed");
    }
    match client.call(tool, arguments)? {
        Value::String(text) => Ok(text),
        other => Ok(other.to_string()),
    }
}

/// `- **Name** (ID: `uuid`, Region: us-east-2)` lines: (name, id, region).
pub fn parse_named_ids(text: &str) -> Vec<(String, String, Option<String>)> {
    text.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("- **")?;
            let (name, rest) = rest.split_once("**")?;
            let id = rest.split_once("ID: `")?.1.split('`').next()?;
            let region = rest
                .split_once("Region: ")
                .map(|(_, r)| r.trim_end_matches(')').trim().to_owned());
            Some((name.trim().to_owned(), id.to_owned(), region))
        })
        .collect()
}

#[derive(Debug, PartialEq)]
pub struct Listed {
    pub reference: String,
    pub title: String,
    pub kind: String,
    pub first_seen: Option<String>,
    pub last_seen: Option<String>,
}

fn field(block: &str, name: &str) -> Option<String> {
    let marker = format!("- **{name}:** ");
    block
        .lines()
        .find_map(|line| line.trim().strip_prefix(&marker))
        .map(|value| value.split(" (").next().unwrap_or(value).trim().to_owned())
}

/// One `list_issues` page: its issues and the next cursor.
pub fn parse_list(text: &str) -> (Vec<Listed>, Option<String>) {
    let issues = text
        .split("\n### #")
        .skip(1)
        .filter_map(|block| {
            let (heading, body) = block.split_once('\n').unwrap_or((block, ""));
            let (reference, title) = heading.split_once(' ').unwrap_or((heading, ""));
            if reference.is_empty() || !reference.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            Some(Listed {
                reference: reference.to_owned(),
                title: title.trim().to_owned(),
                kind: field(body, "Type").unwrap_or_default().to_ascii_lowercase(),
                first_seen: field(body, "First seen"),
                last_seen: field(body, "Last seen"),
            })
        })
        .collect();
    let cursor = text
        .lines()
        .find_map(|line| line.trim().strip_prefix("next_cursor:"))
        .map(|c| c.trim().to_owned())
        .filter(|c| !c.is_empty() && c.len() <= 2048);
    (issues, cursor)
}

#[derive(Debug, Default, PartialEq)]
pub struct Detail {
    pub count: Option<u64>,
    pub users: Option<u64>,
    pub frames: Vec<Frame>,
}

fn section<'a>(text: &'a str, heading: &str) -> &'a str {
    let Some(start) = text.find(&format!("## {heading}")) else {
        return "";
    };
    let body = &text[start..];
    let body = body.split_once('\n').map_or("", |(_, b)| b);
    match body.find("\n## ") {
        Some(end) => &body[..end],
        None => body,
    }
}

fn backticked_number(block: &str, label: &str) -> Option<u64> {
    block
        .lines()
        .find(|line| line.contains(label))
        .and_then(|line| line.split('`').nth(1))
        .and_then(|n| n.replace(',', "").parse().ok())
}

fn frame(location: &str) -> Option<Frame> {
    let location = location.trim().trim_matches('`');
    if location.is_empty() {
        return None;
    }
    let (path, line) = match location.rsplit_once(':') {
        Some((path, line)) if line.bytes().all(|b| b.is_ascii_digit()) && !line.is_empty() => {
            (path, line.parse().ok())
        }
        _ => (location, None),
    };
    let vendor = path.starts_with("vendor/") || path.contains("/vendor/");
    Some(Frame {
        path: path.to_owned(),
        line,
        in_app: !vendor,
    })
}

/// The parts of a `get_issue` answer the heat needs.
pub fn parse_detail(text: &str) -> Detail {
    let stats = section(text, "Occurrence Statistics");
    let mut frames: Vec<Frame> = section(text, "Stack Trace")
        .lines()
        .filter_map(|line| line.trim().strip_prefix("at "))
        .filter_map(frame)
        .take(50)
        .collect();
    if frames.is_empty() {
        let summary = section(text, "Exception Summary");
        if let Some(file) = summary
            .lines()
            .find_map(|line| line.trim().strip_prefix("- File: "))
            .and_then(frame)
        {
            frames.push(file);
        }
    }
    Detail {
        count: backticked_number(stats, "Occurrences (last 24 hours)"),
        users: backticked_number(stats, "Users affected"),
        frames,
    }
}

fn level(listed: &Listed) -> &'static str {
    if listed.title.contains("FatalError") {
        "fatal"
    } else if listed.kind == "exception" || listed.kind.is_empty() {
        "error"
    } else {
        // route, job, command and scheduled task issues are thresholds, not crashes.
        "warning"
    }
}

fn pick<'a>(
    rows: &'a [(String, String, Option<String>)],
    wanted: &str,
    what: &str,
) -> Result<&'a (String, String, Option<String>)> {
    rows.iter()
        .find(|(name, id, _)| id == wanted || name.eq_ignore_ascii_case(wanted))
        .with_context(|| {
            let names: Vec<_> = rows.iter().map(|(n, _, _)| n.as_str()).collect();
            format!(
                "no Nightwatch {what} named {wanted:?} (available: {})",
                names.join(", ")
            )
        })
}

impl Poll<'_> {
    pub fn poll(&self, known: &BTreeMap<String, Issue>) -> Result<Vec<Issue>> {
        with_client(self.credentials, |client| self.poll_with(client, known))
    }

    fn poll_with(
        &self,
        client: &mut Client,
        known: &BTreeMap<String, Issue>,
    ) -> Result<Vec<Issue>> {
        let apps = parse_named_ids(&read(client, "list_applications", json!({}))?);
        let app = match self.application {
            Some(wanted) => pick(&apps, wanted, "application")?,
            None if apps.len() == 1 => &apps[0],
            None => bail!(
                "this Nightwatch account has {} applications; set \"application\" in signals.json",
                apps.len()
            ),
        };
        let mut arguments = json!({"application_id": app.1, "status": "open"});
        let mut link = None;
        if let Some(wanted) = self.environment {
            let envs = parse_named_ids(&read(
                client,
                "list_environments",
                json!({"application_id": app.1}),
            )?);
            let env = pick(&envs, wanted, "environment")?;
            arguments["environment_id"] = Value::String(env.1.clone());
            let region = env
                .2
                .as_deref()
                .and_then(|r| r.split('-').next())
                .unwrap_or("us");
            link = Some(format!(
                "https://nightwatch.laravel.com/{region}/environments/{}/issues",
                env.1
            ));
        }
        let mut listed = Vec::new();
        for _ in 0..PAGES {
            let (page, cursor) = parse_list(&read(client, "list_issues", arguments.clone())?);
            listed.extend(page);
            match cursor {
                Some(cursor) if listed.len() < self.limit => {
                    arguments["cursor"] = Value::String(cursor)
                }
                _ => break,
            }
        }
        listed.truncate(self.limit);

        let mut fetched = 0;
        let mut issues = Vec::with_capacity(listed.len());
        for row in listed {
            let key = format!("{SOURCE}#{}", row.reference);
            let previous = known.get(&key).filter(|old| old.frames_checked);
            let mut issue = Issue {
                key: key.clone(),
                source: SOURCE.to_owned(),
                id: row.reference.clone(),
                title: clean_title(&row.title),
                level: level(&row).to_owned(),
                status: "open".to_owned(),
                count: previous.map_or(1, |old| old.count),
                users: previous.map_or(0, |old| old.users),
                first_seen: row.first_seen.clone(),
                last_seen: row.last_seen.clone(),
                url: link
                    .as_ref()
                    .map(|base| format!("{base}/{}", row.reference)),
                frames: previous.map(|old| old.frames.clone()).unwrap_or_default(),
                buckets: Vec::new(),
                frames_checked: previous.is_some(),
            };
            // Details change only when the issue recurs; re-read them then.
            let stale = previous.is_none_or(|old| old.last_seen != row.last_seen);
            if stale && fetched < DETAIL_FETCHES {
                fetched += 1;
                let mut detail_args = json!({"application_id": app.1, "identifier": row.reference});
                if let Some(env) = arguments.get("environment_id") {
                    detail_args["environment_id"] = env.clone();
                }
                match read(client, "get_issue", detail_args) {
                    Ok(text) => {
                        let detail = parse_detail(&text);
                        issue.count = detail.count.unwrap_or(issue.count).max(1);
                        issue.users = detail.users.unwrap_or(issue.users);
                        issue.frames = detail.frames;
                        issue.frames_checked = true;
                    }
                    Err(error) => {
                        crate::poll::log(&format!("nightwatch #{}: {error:#}", row.reference))
                    }
                }
            }
            issues.push(issue);
        }
        Ok(issues)
    }
}

#[cfg(test)]
pub mod fixtures {
    pub const APPS: &str = "## Applications\n\n- **Acme** (ID: `a1111111-1111-4111-8111-111111111111`, Organization: Acme Inc)\n- **Other** (ID: `b2222222-2222-4222-8222-222222222222`, Organization: Acme Inc)";
    pub const ENVS: &str = "## Environments for Acme\n\n- **develop** (ID: `e0000000-0000-4000-8000-000000000001`, Region: us-east-2)\n- **Production** (ID: `e0000000-0000-4000-8000-000000000002`, Region: us-east-2)";
    pub const LIST_1: &str = "## Issues for Acme\n\nShowing 2 open issues, most recently seen first.\n\n### #509 Illuminate\\Contracts\\Cache\\LockTimeoutException\n\n- **Type:** Exception\n- **Priority:** No Priority\n- **First seen:** 2026-10-22T19:03:22+00:00 (21 hours ago)\n- **Last seen:** 2026-10-23T15:42:08+00:00 (50 minutes ago)\n\n### #388 App\\Services\\Admin\\MemberException: Member 4ab61b31-2fd8-45f7-a07c-faf7dd2ff2a1 state changed (ops@example.com)\n\n- **Type:** Exception\n- **Priority:** No Priority\n- **First seen:** 2026-08-15T12:12:48+00:00 (2 months ago)\n- **Last seen:** 2026-10-23T15:46:42+00:00 (46 minutes ago)\n\n---\n\nnext_cursor: eyJwYWdlIjoyfQ\n\nThere are more issues available.";
    pub const LIST_2: &str = "## Issues for Acme\n\n### #12 GET /slow-route\n\n- **Type:** Route\n- **First seen:** 2026-10-23T10:00:00+00:00 (6 hours ago)\n- **Last seen:** 2026-10-23T15:00:00+00:00 (1 hour ago)\n";
    pub const DETAIL: &str = "## Exception Summary\n\n- Class: `Illuminate\\Contracts\\Cache\\LockTimeoutException`\n- Message: `secret message`\n- File: `app/Services/Billing/Lock.php:113`\n\n## Job\n\n### Authenticated User\n\n- ID: `0003a026-19c1-4ef2-bf6e-d792b2fc1f77`\n\n## Stack Trace (most recent call first)\n\n- [0] App\\Services\\Billing\\Lock->execute()\n    at app/Services/Billing/Lock.php:113\n- [1] App\\Services\\Quiltt\\Api->deleteProfile()\n    at app/Services/Quiltt/Api.php:137\n- [2] Illuminate\\Container\\BoundMethod::call()\n    at vendor/laravel/framework/src/Illuminate/Container/BoundMethod.php:36\n\n## Code Context\n\n```php\n113: $lock->block(5);\n```\n\n## Occurrence Statistics\n\n- First seen: `2026-10-22T19:03:22+00:00`\n- Occurrences (last 24 hours): `21`\n- Occurrences (last 7 days): `21`\n- Users affected: `18`\n\n## Deploy\n\n- Last seen deploy: `depl-1`\n";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_answers_parse_into_issue_fields() {
        let apps = parse_named_ids(fixtures::APPS);
        assert_eq!(
            apps[0],
            (
                "Acme".into(),
                "a1111111-1111-4111-8111-111111111111".into(),
                None
            )
        );
        let envs = parse_named_ids(fixtures::ENVS);
        assert_eq!(envs[1].0, "Production");
        assert_eq!(envs[1].2.as_deref(), Some("us-east-2"));
        assert_eq!(
            pick(&envs, "production", "environment").unwrap().1,
            "e0000000-0000-4000-8000-000000000002"
        );
        assert!(pick(&envs, "qa", "environment")
            .unwrap_err()
            .to_string()
            .contains("develop, Production"));

        let (issues, cursor) = parse_list(fixtures::LIST_1);
        assert_eq!(cursor.as_deref(), Some("eyJwYWdlIjoyfQ"));
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].reference, "509");
        assert_eq!(issues[0].kind, "exception");
        assert_eq!(
            issues[0].last_seen.as_deref(),
            Some("2026-10-23T15:42:08+00:00")
        );
        let (route, none) = parse_list(fixtures::LIST_2);
        assert_eq!(
            (route[0].kind.as_str(), level(&route[0])),
            ("route", "warning")
        );
        assert!(none.is_none());

        let detail = parse_detail(fixtures::DETAIL);
        assert_eq!((detail.count, detail.users), (Some(21), Some(18)));
        assert_eq!(detail.frames.len(), 3);
        assert_eq!(detail.frames[0].path, "app/Services/Billing/Lock.php");
        assert_eq!(detail.frames[0].line, Some(113));
        assert!(!detail.frames[2].in_app);
        let no_stack = parse_detail("## Exception Summary\n\n- File: `app/X.php:9`\n");
        assert_eq!(no_stack.frames[0].path, "app/X.php");
    }

    #[test]
    fn only_read_tools_may_be_called() {
        for tool in READ_TOOLS {
            assert!(!tool.contains("update") && !tool.contains("comment"));
        }
    }
}
