//! From issues to the heat layer: each open issue is attributed to the cards
//! that own its stack frames, scored by level, recent volume and reach, and
//! decayed over time. `heat.json` is what `board.py overlay` hands the board.

use crate::config::{PathMap, Project};
use crate::glob::owners;
use crate::model::Issue;
use crate::store::{PollStatus, Target};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

const PER_CARD: usize = 5;
const UNMAPPED: usize = 10;
/// Stack frames kept per issue in the compact `issues` list.
const FRAMES: usize = 12;
/// The share a card gets when it is on the error's path but not where it was raised.
const CALLER_WEIGHT: f64 = 0.3;
const NEW_HOURS: i64 = 24;

/// The time scores decay to. Tests pin it to the fixtures' time so their scores never age.
#[cfg(not(test))]
fn clock() -> DateTime<Utc> {
    Utc::now()
}
#[cfg(test)]
fn clock() -> DateTime<Utc> {
    "2026-10-23T12:00:00Z".parse().unwrap()
}

fn level_weight(level: &str) -> f64 {
    match level {
        "fatal" => 8.0,
        "error" => 5.0,
        "warning" => 1.0,
        _ => 0.2,
    }
}

fn parse_time(value: Option<&str>) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value?)
        .ok()
        .map(|t| t.with_timezone(&Utc))
}

fn decay(age_minutes: f64, half_life: f64) -> f64 {
    0.5_f64.powf(age_minutes.max(0.0) / half_life)
}

/// Level × log(recent events) × reach. Recent events decay with the half-life.
pub fn score(issue: &Issue, now: DateTime<Utc>, half_life: f64) -> f64 {
    let activity = if issue.buckets.iter().any(|(_, n)| *n > 0) {
        issue
            .buckets
            .iter()
            .map(|(at, n)| {
                let age = (now.timestamp() - at) as f64 / 60.0;
                *n as f64 * decay(age, half_life)
            })
            .sum()
    } else {
        let age = parse_time(issue.last_seen.as_deref())
            .map_or(0.0, |t| (now - t).num_seconds() as f64 / 60.0);
        issue.count.max(1) as f64 * decay(age, half_life)
    };
    level_weight(&issue.level) * (1.0 + activity).ln() * (1.0 + (1.0 + issue.users as f64).ln())
}

/// A deployed or frame path as a repository path, or `None` when it is empty.
fn repo_path(raw: &str, map: &[PathMap], root: &Path) -> Option<String> {
    let mut path = raw.trim().to_owned();
    for prefix in ["file://", "app:///", "webpack:///"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            path = rest.to_owned();
        }
    }
    if let Some(rule) = map.iter().find(|rule| path.starts_with(&rule.from)) {
        path = format!("{}{}", rule.to, &path[rule.from.len()..]);
    } else if let Some(rest) = root.to_str().and_then(|r| path.strip_prefix(r)) {
        path = rest.to_owned();
    }
    let path = path
        .trim_start_matches("./")
        .trim_start_matches('/')
        .to_owned();
    (!path.is_empty()).then_some(path)
}

/// A frame path after the deploy-path rules, and whether it is still absolute.
fn mapped_path(raw: &str, map: &[PathMap], root: &Path) -> (String, bool) {
    let mut path = raw.trim().replace('\\', "/");
    for prefix in ["file://", "app:///", "webpack:///"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            path = rest.to_owned();
        }
    }
    if let Some(rule) = map.iter().find(|rule| path.starts_with(&rule.from)) {
        return (format!("{}{}", rule.to, &path[rule.from.len()..]), false);
    }
    if let Some(rest) = root
        .to_str()
        .and_then(|r| path.strip_prefix(r))
        .filter(|rest| rest.starts_with('/'))
    {
        return (rest.to_owned(), false);
    }
    let absolute = path.starts_with('/') || path.as_bytes().get(1) == Some(&b':');
    (path, absolute)
}

/// A frame path as a clean repository-relative path: `/` separators, no `.`
/// segments, never `..`. An absolute path counts only when a card owns one of
/// its suffixes, the same fallback the heat mapping uses.
pub fn normalized_path(raw: &str, board: &Value, map: &[PathMap], root: &Path) -> Option<String> {
    let (path, absolute) = mapped_path(raw, map, root);
    let segments: Vec<&str> = path
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    if segments.is_empty() || segments.contains(&"..") {
        return None;
    }
    let relative = segments.join("/");
    if !absolute {
        return Some(relative);
    }
    let mut rest = relative.as_str();
    loop {
        if !owners(board, rest).is_empty() {
            return Some(rest.to_owned());
        }
        match rest.split_once('/') {
            Some((_, tail)) if !tail.is_empty() => rest = tail,
            _ => return None,
        }
    }
}

/// The innermost in-app frame with a usable path and line: an issue's root cause.
pub fn root_frame(
    issue: &Issue,
    board: &Value,
    map: &[PathMap],
    root: &Path,
) -> Option<(String, u64)> {
    issue.frames.iter().filter(|f| f.in_app).find_map(|f| {
        let line = f.line.filter(|l| *l > 0)?;
        Some((normalized_path(&f.path, board, map, root)?, line))
    })
}

/// Cards that own `path`; when none does, retry with leading directories
/// dropped, so `/srv/app/releases/42/app/Models/User.php` still finds the card
/// owning `app/Models/User.php` without a path_map entry.
fn path_owners(board: &Value, path: &str) -> Vec<String> {
    let mut rest = path;
    loop {
        let found = owners(board, rest);
        if !found.is_empty() {
            return found;
        }
        match rest.split_once('/') {
            Some((_, tail)) if !tail.is_empty() => rest = tail,
            _ => return Vec::new(),
        }
    }
}

/// `(card, weight)`: full weight for the cards owning the innermost frame that
/// has an owner, a smaller share for cards further up the stack.
pub fn attribute(issue: &Issue, board: &Value, map: &[PathMap], root: &Path) -> Vec<(String, f64)> {
    let in_app: Vec<_> = issue.frames.iter().filter(|f| f.in_app).collect();
    let frames = if in_app.is_empty() {
        issue.frames.iter().collect()
    } else {
        in_app
    };
    let mut cards: Vec<(String, f64)> = Vec::new();
    for frame in frames {
        let Some(path) = repo_path(&frame.path, map, root) else {
            continue;
        };
        let weight = if cards.is_empty() { 1.0 } else { CALLER_WEIGHT };
        for card in path_owners(board, &path) {
            if !cards.iter().any(|(id, _)| *id == card) {
                cards.push((card, weight));
            }
        }
    }
    cards
}

/// Where an issue sits on a card, for the board's Analyze action.
struct Origin<'a> {
    target: &'a str,
    card: Option<&'a str>,
    root: Option<&'a (String, u64)>,
}

fn summary(
    issue: &Issue,
    now: DateTime<Utc>,
    contribution: f64,
    weight: f64,
    origin: &Origin,
) -> Value {
    let new = parse_time(issue.first_seen.as_deref())
        .is_some_and(|t| now - t < chrono::Duration::hours(NEW_HOURS));
    let fingerprint = origin
        .card
        .zip(origin.root)
        .map(|(card, (path, line))| crate::jobs::fingerprint(origin.target, card, path, *line));
    json!({
        "key": issue.key,
        "root_frame": origin.root.map(|(path, line)| json!({"path": path, "line": line})),
        "fingerprint": fingerprint,
        "title": issue.title,
        "level": issue.level,
        "count": issue.count,
        "users": issue.users,
        "last_seen": issue.last_seen,
        "url": issue.url,
        "source": issue.source,
        "new": new,
        "share": if weight < 1.0 { "caller" } else { "raised" },
        "score": round(contribution),
    })
}

/// One open issue in compact form for the board's ticker and issue sheet:
/// the card that raised it (owner of the innermost owned frame), the same
/// fingerprint the Analyze action uses, and up to [`FRAMES`] repository paths.
fn compact(
    issue: &Issue,
    now: DateTime<Utc>,
    origin: &Origin,
    map: &[PathMap],
    root: &Path,
    board: &Value,
) -> Value {
    let new = parse_time(issue.first_seen.as_deref())
        .is_some_and(|t| now - t < chrono::Duration::hours(NEW_HOURS));
    let card = origin.card;
    let fingerprint = card
        .zip(origin.root)
        .map(|(card, (path, line))| crate::jobs::fingerprint(origin.target, card, path, *line));
    let frames: Vec<Value> = issue
        .frames
        .iter()
        .take(FRAMES)
        .map(|f| {
            let path = normalized_path(&f.path, board, map, root)
                .unwrap_or_else(|| mapped_path(&f.path, map, root).0);
            json!({"path": path, "line": f.line, "in_app": f.in_app})
        })
        .collect();
    json!({
        "key": issue.key,
        "fingerprint": fingerprint,
        "title": issue.title,
        "level": issue.level,
        "count": issue.count,
        "users": issue.users,
        "first_seen": issue.first_seen,
        "last_seen": issue.last_seen,
        "url": issue.url,
        "source": issue.source,
        "new": new,
        "card": card,
        "frames": frames,
    })
}

/// The Nightwatch Logs page for a Nightwatch issue, narrowed to a period that
/// covers the issue's first sighting and to text from its title. Built from the
/// issue's own link, so the region and environment are Nightwatch's.
fn logs_url(issue: &Issue, now: DateTime<Utc>) -> Option<String> {
    if issue.source != "nightwatch" {
        return None;
    }
    let url = issue.url.as_deref()?;
    let rest = url.strip_prefix("https://nightwatch.laravel.com/")?;
    let at = rest.find("/environments/")? + "/environments/".len();
    let env = rest[at..].split('/').next()?;
    if env.is_empty() || !env.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-') {
        return None;
    }
    let hours = parse_time(issue.first_seen.as_deref()).map_or(i64::MAX, |t| (now - t).num_hours());
    let period = [(1, "1h"), (24, "24h"), (168, "7d"), (336, "14d")]
        .iter()
        .find(|(limit, _)| hours < *limit)
        .map_or("30d", |(_, p)| p);
    // A slow-route title carries its threshold, which no log line repeats.
    let title = issue
        .title
        .split(" exceeded ")
        .next()
        .unwrap_or(&issue.title);
    let search: String = title.chars().take(100).collect();
    Some(format!(
        "https://nightwatch.laravel.com/{}{env}/logs?period={period}&search={}",
        &rest[..at],
        crate::oauth::encode(search.trim())
    ))
}

fn round(value: f64) -> f64 {
    (value * 1000.0).round() / 1000.0
}

pub fn compute(
    target: &Target,
    project: &Project,
    issues: &BTreeMap<String, Issue>,
    polls: &BTreeMap<String, PollStatus>,
    now: DateTime<Utc>,
    half_life: f64,
) -> Value {
    let board = target.board().unwrap_or_else(|| json!({"nodes": []}));
    let mut cards: BTreeMap<String, (f64, Vec<(f64, Value)>)> = BTreeMap::new();
    let mut unmapped: Vec<(f64, Value)> = Vec::new();
    let mut all: Vec<Value> = Vec::new();
    let mut open = 0;
    for issue in issues.values().filter(|i| i.status == "open") {
        open += 1;
        let base = score(issue, now, half_life);
        let owners = attribute(issue, &board, &project.path_map, &target.root);
        let root = root_frame(issue, &board, &project.path_map, &target.root);
        let raised = owners.first().map(|(card, _)| card.as_str());
        let origin = Origin {
            target: &target.id,
            card: raised,
            root: root.as_ref(),
        };
        let logs = project.logs.as_ref().and_then(|_| logs_url(issue, now));
        let with_logs = |mut item: Value| {
            if let Some(url) = &logs {
                item["logs_url"] = json!(url);
            }
            item
        };
        all.push(with_logs(compact(
            issue,
            now,
            &origin,
            &project.path_map,
            &target.root,
            &board,
        )));
        if owners.is_empty() {
            let origin = Origin {
                target: &target.id,
                card: None,
                root: root.as_ref(),
            };
            unmapped.push((base, with_logs(summary(issue, now, base, 1.0, &origin))));
            continue;
        }
        for (card, weight) in owners {
            let contribution = base * weight;
            let origin = Origin {
                target: &target.id,
                card: Some(&card),
                root: root.as_ref(),
            };
            let item = with_logs(summary(issue, now, contribution, weight, &origin));
            let entry = cards.entry(card).or_default();
            entry.0 += contribution;
            entry.1.push((contribution, item));
        }
    }
    let hottest = cards.values().map(|(s, _)| *s).fold(0.0_f64, f64::max);
    let by_score = |a: &(f64, Value), b: &(f64, Value)| b.0.total_cmp(&a.0);
    let cards: serde_json::Map<String, Value> = cards
        .into_iter()
        .map(|(id, (total, mut list))| {
            list.sort_by(by_score);
            let count = list.len();
            let heat = if hottest > 0.0 { total / hottest } else { 0.0 };
            let top: Vec<Value> = list.into_iter().take(PER_CARD).map(|(_, v)| v).collect();
            (
                id,
                json!({"score": round(total), "heat": round(heat), "issues": count, "top": top}),
            )
        })
        .collect();
    unmapped.sort_by(by_score);
    // Newest first, the order the board's ticker shows.
    all.sort_by(|a, b| {
        let at = |v: &Value| parse_time(v["last_seen"].as_str());
        at(b).cmp(&at(a))
    });
    let unmapped_count = unmapped.len();
    json!({
        "version": 1,
        "project": project.name,
        "generated_at": now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "half_life_minutes": half_life,
        "totals": {"open": open, "unmapped": unmapped_count, "cards": cards.len()},
        "cards": cards,
        "unmapped": unmapped.into_iter().take(UNMAPPED).map(|(_, v)| v).collect::<Vec<_>>(),
        "issues": all,
        "sources": polls,
        "logs": project.logs.as_ref().map(|l| json!({"skill": l.skill, "environment": l.environment})),
    })
}

/// Recompute and store `heat.json` for one project.
pub fn refresh(target: &Target, project: &Project, half_life: f64) -> anyhow::Result<Value> {
    let heat = compute(
        target,
        project,
        &target.issues(),
        &target.polls(),
        clock(),
        half_life,
    );
    crate::store::write_json(&target.signals().join("heat.json"), &heat)?;
    Ok(heat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Frame;

    fn issue(level: &str, count: u64, users: u64, last_seen: &str, frames: &[&str]) -> Issue {
        Issue {
            key: format!("t#{level}{count}"),
            source: "t".into(),
            id: "1".into(),
            title: "Boom".into(),
            level: level.into(),
            status: "open".into(),
            count,
            users,
            first_seen: Some(last_seen.into()),
            last_seen: Some(last_seen.into()),
            url: None,
            frames: frames
                .iter()
                .map(|p| Frame {
                    path: (*p).into(),
                    line: Some(1),
                    in_app: true,
                })
                .collect(),
            buckets: vec![],
            frames_checked: false,
        }
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-10-23T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn scores_rank_level_volume_reach_and_recency() {
        let fresh = "2026-10-23T11:59:00Z";
        let error = score(&issue("error", 10, 0, fresh, &[]), now(), 180.0);
        assert!(score(&issue("fatal", 10, 0, fresh, &[]), now(), 180.0) > error);
        assert!(score(&issue("warning", 10, 0, fresh, &[]), now(), 180.0) < error);
        assert!(score(&issue("error", 100, 0, fresh, &[]), now(), 180.0) > error);
        assert!(score(&issue("error", 10, 50, fresh, &[]), now(), 180.0) > error);
        let stale = score(
            &issue("error", 10, 0, "2026-10-22T12:00:00Z", &[]),
            now(),
            180.0,
        );
        assert!(stale < error / 2.0);
        // Hourly buckets: recent events outweigh the same number long ago.
        let mut recent = issue("error", 0, 0, fresh, &[]);
        recent.buckets = vec![(now().timestamp() - 600, 10)];
        let mut old = recent.clone();
        old.buckets = vec![(now().timestamp() - 86_400, 10)];
        assert!(score(&recent, now(), 180.0) > score(&old, now(), 180.0));
    }

    #[test]
    fn frames_map_through_path_rules_and_fall_back_to_suffixes() {
        let board = json!({"nodes": [{"id": "app", "owns": ["src/app/"]}, {"id": "models", "owns": ["src/models/*.php"]}]});
        let map = vec![PathMap {
            from: "/var/www/html/".into(),
            to: "".into(),
        }];
        let root = Path::new("/home/me/project");
        let raised = issue(
            "error",
            1,
            0,
            "2026-10-23T11:00:00Z",
            &[
                "/var/www/html/src/models/User.php",
                "/srv/rel/42/src/app/Http.php",
                "vendor/x.php",
            ],
        );
        assert_eq!(
            attribute(&raised, &board, &map, root),
            vec![
                ("models".to_owned(), 1.0),
                ("app".to_owned(), CALLER_WEIGHT)
            ]
        );
        let local = issue(
            "error",
            1,
            0,
            "2026-10-23T11:00:00Z",
            &["/home/me/project/src/app/a.php"],
        );
        assert_eq!(
            attribute(&local, &board, &[], root),
            vec![("app".to_owned(), 1.0)]
        );
        let nowhere = issue("error", 1, 0, "2026-10-23T11:00:00Z", &["lib/x.js"]);
        assert!(attribute(&nowhere, &board, &[], root).is_empty());
    }

    #[test]
    fn root_frames_are_normalised_repository_paths() {
        let board = json!({"nodes": [{"id": "app", "owns": ["src/app/"]}]});
        let map = vec![PathMap {
            from: "/var/www/html/".into(),
            to: "".into(),
        }];
        let root = Path::new("/home/me/project");
        let norm = |raw: &str| normalized_path(raw, &board, &map, root);
        assert_eq!(
            norm("/var/www/html/./src//app/a.php").as_deref(),
            Some("src/app/a.php")
        );
        assert_eq!(norm("src\\app\\a.php").as_deref(), Some("src/app/a.php"));
        assert_eq!(
            norm("/home/me/project/lib/x.js").as_deref(),
            Some("lib/x.js")
        );
        assert_eq!(
            norm("/srv/rel/42/src/app/a.php").as_deref(),
            Some("src/app/a.php")
        );
        assert_eq!(
            norm("webpack:///./src/app/a.ts").as_deref(),
            Some("src/app/a.ts")
        );
        for bad in [
            "/etc/passwd",
            "src/../../x",
            "",
            "./",
            "C:/x/y.php",
            "/home/me/project2/a.php",
        ] {
            assert_eq!(norm(bad), None, "{bad}");
        }
        let mut i = issue(
            "error",
            1,
            0,
            "2026-10-23T11:00:00Z",
            &["vendor/x.php", "/etc/x", "src/app/b.php"],
        );
        i.frames[0].in_app = false;
        assert_eq!(
            root_frame(&i, &board, &map, root),
            Some(("src/app/b.php".into(), 1))
        );
        i.frames[2].line = None;
        assert_eq!(root_frame(&i, &board, &map, root), None);
    }

    #[test]
    fn compute_normalises_heat_and_lists_unmapped_issues() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _) = crate::store::testing::db_with_target(dir.path());
        let target = crate::store::resolve(&db, "demo-1").unwrap();
        let project: Project = serde_json::from_value(json!({"name": "demo", "target": "demo-1", "providers": [{"type": "command", "argv": ["x"]}]})).unwrap();
        let mut issues = BTreeMap::new();
        let mut add = |i: Issue| {
            issues.insert(i.key.clone(), i);
        };
        add(issue(
            "fatal",
            50,
            5,
            "2026-10-23T11:50:00Z",
            &["src/models/User.php"],
        ));
        add(issue(
            "warning",
            1,
            0,
            "2026-10-23T11:50:00Z",
            &["src/app/a.php"],
        ));
        add(issue(
            "error",
            3,
            0,
            "2026-10-23T11:50:00Z",
            &["lib/other.js"],
        ));
        let mut closed = issue("fatal", 999, 9, "2026-10-23T11:50:00Z", &["src/app/a.php"]);
        closed.key = "t#closed".into();
        closed.status = "resolved".into();
        add(closed);
        let heat = compute(&target, &project, &issues, &BTreeMap::new(), now(), 180.0);
        assert_eq!(
            heat["totals"],
            json!({"open": 3, "unmapped": 1, "cards": 2})
        );
        assert_eq!(heat["cards"]["models"]["heat"], 1.0);
        let app = heat["cards"]["app"]["heat"].as_f64().unwrap();
        assert!(app > 0.0 && app < 0.2, "{app}");
        assert_eq!(heat["cards"]["models"]["top"][0]["new"], true);
        assert_eq!(heat["unmapped"][0]["title"], "Boom");
    }

    #[test]
    fn marked_projects_link_nightwatch_issues_to_their_logs() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _) = crate::store::testing::db_with_target(dir.path());
        let target = crate::store::resolve(&db, "demo-1").unwrap();
        let env = "a148f4b8-8b4e-4900-a47d-ffa949e2ce6d";
        let mut slow = issue("warning", 1, 0, "2026-10-23T11:00:00Z", &[]);
        slow.source = "nightwatch".into();
        slow.title = "GET|HEAD /api/v2/transactions exceeded 1,500ms threshold".into();
        slow.first_seen = Some("2026-10-20T12:00:00Z".into());
        slow.url = Some(format!(
            "https://nightwatch.laravel.com/us/environments/{env}/issues/526"
        ));
        let mut other = issue("error", 2, 0, "2026-10-23T11:00:00Z", &[]);
        other.url = Some("https://sentry.io/issues/1".into());
        let issues: BTreeMap<String, Issue> = [slow, other]
            .into_iter()
            .map(|i| (i.key.clone(), i))
            .collect();
        let marked: Project = serde_json::from_value(json!({"name": "demo", "target": "demo-1",
            "logs": {"skill": "lothar-nightwatch-logs", "environment": "production"},
            "providers": [{"type": "command", "argv": ["x"]}]}))
        .unwrap();
        let heat = compute(&target, &marked, &issues, &BTreeMap::new(), now(), 180.0);
        assert_eq!(
            heat["logs"],
            json!({"skill": "lothar-nightwatch-logs", "environment": "production"})
        );
        let by_key = |list: &Value, key: &str| {
            list.as_array()
                .unwrap()
                .iter()
                .find(|i| i["key"] == key)
                .cloned()
                .unwrap()
        };
        let expected = format!("https://nightwatch.laravel.com/us/environments/{env}/logs?period=7d&search=GET%7CHEAD%20%2Fapi%2Fv2%2Ftransactions");
        assert_eq!(by_key(&heat["issues"], "t#warning1")["logs_url"], expected);
        assert_eq!(
            by_key(&heat["unmapped"], "t#warning1")["logs_url"],
            expected
        );
        assert!(by_key(&heat["issues"], "t#error2")
            .get("logs_url")
            .is_none());
        let plain: Project = serde_json::from_value(json!({"name": "demo", "target": "demo-1",
            "providers": [{"type": "command", "argv": ["x"]}]}))
        .unwrap();
        let heat = compute(&target, &plain, &issues, &BTreeMap::new(), now(), 180.0);
        assert!(heat["logs"].is_null());
        assert!(by_key(&heat["issues"], "t#warning1")
            .get("logs_url")
            .is_none());
    }

    #[test]
    fn issues_list_every_open_issue_compactly_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let (db, _) = crate::store::testing::db_with_target(dir.path());
        let target = crate::store::resolve(&db, "demo-1").unwrap();
        let project: Project = serde_json::from_value(json!({"name": "demo", "target": "demo-1", "path_map": [{"from": "/var/www/html/", "to": ""}], "providers": [{"type": "command", "argv": ["x"]}]})).unwrap();
        let mut issues = BTreeMap::new();
        let mut raised = issue(
            "error",
            2,
            1,
            "2026-10-23T11:50:00Z",
            &[
                "/var/www/html/vendor/laravel/x.php",
                "/var/www/html/src/models/User.php",
                "src/app/a.php",
            ],
        );
        raised.frames[0].in_app = false;
        raised.first_seen = Some("2026-10-20T11:50:00Z".into());
        let mut slow = issue("warning", 1, 0, "2026-10-23T11:55:00Z", &[]);
        slow.title = "GET /x exceeded 1,000ms threshold".into();
        let mut deep = issue("error", 9, 0, "2026-10-23T10:00:00Z", &["lib/other.js"; 20]);
        deep.key = "t#deep".into();
        let mut closed = issue("fatal", 1, 0, "2026-10-23T11:59:00Z", &["src/app/a.php"]);
        closed.key = "t#closed".into();
        closed.status = "resolved".into();
        for i in [raised, slow, deep, closed] {
            issues.insert(i.key.clone(), i);
        }
        // More open issues than the per-card and unmapped lists keep: all are listed.
        for n in 0..(UNMAPPED + 3) {
            let mut extra = issue("error", 1, 0, "2026-10-23T09:00:00Z", &["lib/x.js"]);
            extra.key = format!("t#extra{n}");
            issues.insert(extra.key.clone(), extra);
        }
        let heat = compute(&target, &project, &issues, &BTreeMap::new(), now(), 180.0);
        let list = heat["issues"].as_array().unwrap();
        assert_eq!(list.len(), 3 + UNMAPPED + 3);
        assert_eq!(heat["unmapped"].as_array().unwrap().len(), UNMAPPED);
        assert!(list.iter().all(|i| i["key"] != "t#closed"));
        let keys: Vec<&str> = list
            .iter()
            .take(3)
            .map(|i| i["key"].as_str().unwrap())
            .collect();
        assert_eq!(keys, ["t#warning1", "t#error2", "t#deep"]);
        let first = &list[1];
        assert_eq!(first["card"], "models");
        assert_eq!(first["new"], false);
        assert_eq!(first["users"], 1);
        assert_eq!(first["first_seen"], "2026-10-20T11:50:00Z");
        assert_eq!(
            first["frames"],
            json!([
                {"path": "vendor/laravel/x.php", "line": 1, "in_app": false},
                {"path": "src/models/User.php", "line": 1, "in_app": true},
                {"path": "src/app/a.php", "line": 1, "in_app": true},
            ])
        );
        // The same fingerprint the card's own issue list offers to Analyze.
        let on_card = &heat["cards"]["models"]["top"][0];
        assert_eq!(first["fingerprint"], on_card["fingerprint"]);
        assert!(first["fingerprint"].as_str().unwrap().len() == 64);
        assert_eq!(list[0]["card"], Value::Null);
        assert_eq!(list[0]["fingerprint"], Value::Null);
        assert_eq!(list[0]["frames"], json!([]));
        assert_eq!(list[2]["frames"].as_array().unwrap().len(), FRAMES);
        assert_eq!(list[2]["card"], Value::Null);
    }
}
