//! Harness targets and the files this service keeps for each one, all under
//! `<db>/targets/<id>/signals/`. Nothing is written inside a project.

use crate::model::Issue;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Serialises every read-modify-write of the signals files in this process
/// (the poller and the webhook receiver both write them).
static WRITE: Mutex<()> = Mutex::new(());

pub fn lock() -> MutexGuard<'static, ()> {
    WRITE.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Debug, Clone)]
pub struct Target {
    pub id: String,
    pub root: PathBuf,
    pub dir: PathBuf,
}

impl Target {
    pub fn signals(&self) -> PathBuf {
        self.dir.join("signals")
    }

    pub fn board(&self) -> Option<Value> {
        let text = std::fs::read_to_string(self.dir.join("board/board.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn issues(&self) -> BTreeMap<String, Issue> {
        read_json(&self.signals().join("issues.json")).unwrap_or_default()
    }

    pub fn save_issues(&self, issues: &BTreeMap<String, Issue>) -> Result<()> {
        write_json(&self.signals().join("issues.json"), issues)
    }

    /// Replace everything one source contributed with its latest list.
    pub fn replace_source(&self, source: &str, fresh: Vec<Issue>) -> Result<usize> {
        let mut issues = self.issues();
        issues.retain(|_, issue| issue.source != source);
        let count = fresh.len();
        for issue in fresh {
            issues.insert(issue.key.clone(), issue);
        }
        self.save_issues(&issues)?;
        Ok(count)
    }

    /// Forget issues and poll records of sources no longer configured.
    pub fn retain_sources(&self, sources: &std::collections::BTreeSet<String>) -> Result<()> {
        let mut issues = self.issues();
        let before = issues.len();
        issues.retain(|_, issue| sources.contains(&issue.source));
        if issues.len() != before {
            self.save_issues(&issues)?;
        }
        let path = self.signals().join("poll.json");
        let mut polls: BTreeMap<String, PollStatus> = read_json(&path).unwrap_or_default();
        let before = polls.len();
        polls.retain(|source, _| sources.contains(source));
        if polls.len() != before {
            write_json(&path, &polls)?;
        }
        Ok(())
    }

    pub fn record_poll(&self, source: &str, status: PollStatus) -> Result<()> {
        let path = self.signals().join("poll.json");
        let mut polls: BTreeMap<String, PollStatus> = read_json(&path).unwrap_or_default();
        polls.insert(source.to_owned(), status);
        write_json(&path, &polls)
    }

    pub fn polls(&self) -> BTreeMap<String, PollStatus> {
        read_json(&self.signals().join("poll.json")).unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PollStatus {
    pub kind: String,
    pub ok: bool,
    pub at: String,
    pub issues: usize,
    #[serde(default)]
    pub error: Option<String>,
}

/// `HARNESS_DB_ROOT`, else this checkout's `.harness-db`.
pub fn default_db_root() -> PathBuf {
    std::env::var_os("HARNESS_DB_ROOT").map_or_else(
        || PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.harness-db")),
        PathBuf::from,
    )
}

fn read_state(path: &Path) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|line| line.split_once('='))
        .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
        .collect()
}

/// A target id, or a path inside a registered target (the deepest root wins).
pub fn resolve(db_root: &Path, reference: &str) -> Result<Target> {
    let mut rows = Vec::new();
    if let Ok(entries) = std::fs::read_dir(db_root.join("targets")) {
        for entry in entries.flatten() {
            let state = read_state(&entry.path().join("target.state"));
            if let (Some(id), Some(root)) = (state.get("TARGET_ID"), state.get("TARGET_ROOT")) {
                if !id.is_empty() && !root.is_empty() && crate::config::slug(id) {
                    rows.push(Target {
                        id: id.clone(),
                        root: PathBuf::from(root),
                        dir: entry.path(),
                    });
                }
            }
        }
    }
    if let Some(target) = rows.iter().find(|t| t.id == reference) {
        return Ok(target.clone());
    }
    let wanted = crate::config::expand(reference);
    if let Ok(path) = wanted.canonicalize() {
        let best = rows
            .iter()
            .filter_map(|t| t.root.canonicalize().ok().map(|root| (root, t)))
            .filter(|(root, _)| path.starts_with(root))
            .max_by_key(|(root, _)| root.as_os_str().len());
        if let Some((_, target)) = best {
            return Ok(target.clone());
        }
    }
    bail!("no registered harness target for {reference}")
}

/// Remove the signals of every target this machine no longer tracks, so a
/// project dropped from the configuration stops showing heat. Returns their ids.
pub fn prune(db_root: &Path, tracked: &std::collections::BTreeSet<String>) -> Vec<String> {
    let mut removed = Vec::new();
    let Ok(entries) = std::fs::read_dir(db_root.join("targets")) else {
        return removed;
    };
    for entry in entries.flatten() {
        let id = entry.file_name().to_string_lossy().into_owned();
        let signals = entry.path().join("signals");
        if !tracked.contains(&id) && signals.is_dir() && std::fs::remove_dir_all(&signals).is_ok() {
            removed.push(id);
        }
    }
    removed.sort();
    removed
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

/// Atomic: a reader never sees a half-written file.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let dir = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
pub mod testing {
    use std::path::{Path, PathBuf};

    /// A DB root with one target whose board has `app` (src/app/) and `models` (src/models/*.php).
    pub fn db_with_target(base: &Path) -> (PathBuf, PathBuf) {
        let db = base.join("db");
        let project = base.join("project");
        std::fs::create_dir_all(project.join("src/app")).unwrap();
        let target = db.join("targets/demo-1");
        std::fs::create_dir_all(target.join("board")).unwrap();
        std::fs::write(
            target.join("target.state"),
            format!("TARGET_ID=demo-1\nTARGET_ROOT={}\n", project.display()),
        )
        .unwrap();
        std::fs::write(
            target.join("board/board.json"),
            serde_json::json!({"nodes": [
                {"id": "app", "owns": ["src/app/"]},
                {"id": "models", "owns": ["src/models/*.php"]}
            ]})
            .to_string(),
        )
        .unwrap();
        (db, project)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_resolve_by_id_or_path_and_sources_are_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let (db, project) = testing::db_with_target(dir.path());
        assert_eq!(resolve(&db, "demo-1").unwrap().id, "demo-1");
        assert_eq!(
            resolve(&db, project.join("src/app").to_str().unwrap())
                .unwrap()
                .id,
            "demo-1"
        );
        assert!(resolve(&db, "nope").is_err());

        let target = resolve(&db, "demo-1").unwrap();
        let issue = |source: &str, id: &str| Issue {
            key: format!("{source}#{id}"),
            source: source.to_owned(),
            id: id.to_owned(),
            title: "t".into(),
            level: "error".into(),
            status: "open".into(),
            count: 1,
            users: 0,
            first_seen: None,
            last_seen: None,
            url: None,
            frames: vec![],
            buckets: vec![],
            frames_checked: false,
        };
        target
            .replace_source("a", vec![issue("a", "1"), issue("a", "2")])
            .unwrap();
        target.replace_source("b", vec![issue("b", "1")]).unwrap();
        target.replace_source("a", vec![issue("a", "3")]).unwrap();
        let keys: Vec<_> = target.issues().into_keys().collect();
        assert_eq!(keys, ["a#3", "b#1"]);
        assert!(target.signals().join("issues.json").exists());
        assert!(!project.join("signals").exists());

        let tracked: std::collections::BTreeSet<String> = ["demo-1".to_owned()].into();
        assert!(prune(&db, &tracked).is_empty());
        assert_eq!(prune(&db, &Default::default()), ["demo-1"]);
        assert!(!target.signals().exists());
        assert!(
            target.dir.join("board/board.json").exists(),
            "only signals are pruned"
        );
    }
}
