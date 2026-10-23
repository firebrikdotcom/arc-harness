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
/// The share a card gets when it is on the error's path but not where it was raised.
const CALLER_WEIGHT: f64 = 0.3;
const NEW_HOURS: i64 = 24;

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

fn summary(issue: &Issue, now: DateTime<Utc>, contribution: f64, weight: f64) -> Value {
    let new = parse_time(issue.first_seen.as_deref())
        .is_some_and(|t| now - t < chrono::Duration::hours(NEW_HOURS));
    json!({
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
    let mut open = 0;
    for issue in issues.values().filter(|i| i.status == "open") {
        open += 1;
        let base = score(issue, now, half_life);
        let owners = attribute(issue, &board, &project.path_map, &target.root);
        if owners.is_empty() {
            unmapped.push((base, summary(issue, now, base, 1.0)));
            continue;
        }
        for (card, weight) in owners {
            let contribution = base * weight;
            let entry = cards.entry(card).or_default();
            entry.0 += contribution;
            entry
                .1
                .push((contribution, summary(issue, now, contribution, weight)));
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
    let unmapped_count = unmapped.len();
    json!({
        "version": 1,
        "project": project.name,
        "generated_at": now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        "half_life_minutes": half_life,
        "totals": {"open": open, "unmapped": unmapped_count, "cards": cards.len()},
        "cards": cards,
        "unmapped": unmapped.into_iter().take(UNMAPPED).map(|(_, v)| v).collect::<Vec<_>>(),
        "sources": polls,
    })
}

/// Recompute and store `heat.json` for one project.
pub fn refresh(target: &Target, project: &Project, half_life: f64) -> anyhow::Result<Value> {
    let heat = compute(
        target,
        project,
        &target.issues(),
        &target.polls(),
        Utc::now(),
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
}
