//! What an Analyze agent is told, and what it must answer. The context packet
//! is built only from the normalised issue and the board card, scrubbed and
//! bounded; the answer is one JSON object between fixed markers, validated
//! and rendered to the diagnosis Markdown the dashboard shows.

use super::{bounded, Job};
use crate::config::Project;
use crate::heat::normalized_path;
use crate::store::Target;
use anyhow::{bail, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

pub const START: &str = "<<<ISSUE_DIAGNOSIS_JSON>>>";
pub const END: &str = "<<<END_ISSUE_DIAGNOSIS_JSON>>>";
const PACKET_LIMIT: usize = 24 * 1024;
const LIST_LIMIT: usize = 30;
const TEXT_LIMIT: usize = 500;
const TITLE_LIMIT: usize = 200;
const FIELD_LIMIT: usize = 4000;
const TOTAL_LIMIT: usize = 12 * 1024;

pub const SECTIONS: [(&str, &str); 6] = [
    ("likely_cause", "Likely cause"),
    ("code_involved", "Code involved"),
    ("reproduce", "Reproduce"),
    ("suggested_fix", "Suggested fix"),
    ("blast_radius", "Blast radius"),
    ("confidence", "Confidence"),
];

/// The fixed instruction every Analyze agent receives.
pub fn policy() -> String {
    format!(
        "You are a read-only Analyze agent investigating one production error in this repository. \
Investigate only: read code and reason about it. Do not modify, create or delete files, do not run \
commands that change anything, and do not contact or change any external system (no git writes, \
issue trackers, deployments or browsers). When you are done, your final reply must contain exactly \
one JSON object between the line {START} and the line {END}, with exactly these six string fields: \
likely_cause, code_involved, reproduce, suggested_fix, blast_radius, confidence. Keep each field \
under 4000 characters. Write nothing else between the markers."
    )
}

fn text(value: &Value, limit: usize) -> String {
    bounded(value.as_str().unwrap_or_default(), limit)
}

fn list(value: &Value, item: impl Fn(&Value) -> Option<String>) -> Vec<String> {
    value
        .as_array()
        .map(|a| a.iter().filter_map(&item).take(LIST_LIMIT).collect())
        .unwrap_or_default()
}

/// A link without its query string or fragment.
/// An issue link without its query string or fragment, which may carry tokens.
pub fn bare_url(url: Option<&str>) -> Option<String> {
    let url = crate::model::clean_url(url)?;
    Some(url.split(['?', '#']).next().unwrap_or_default().to_owned())
}

/// The bounded context for one job, as compact JSON text (at most 24 KiB).
pub fn build(target: &Target, project: Option<&Project>, job: &Job) -> String {
    let board = target.board().unwrap_or_else(|| json!({"nodes": []}));
    let card = board["nodes"]
        .as_array()
        .and_then(|nodes| nodes.iter().find(|n| n["id"] == job.card_id.as_str()))
        .cloned()
        .unwrap_or(Value::Null);
    let issue = target.issues().remove(&job.issue_key);
    let map = project.map(|p| p.path_map.as_slice()).unwrap_or_default();
    let path_of = |v: &Value| {
        v.as_str()
            .or_else(|| v["path"].as_str())
            .map(|p| bounded(p, TEXT_LIMIT))
    };
    let frames: Vec<Value> = issue
        .as_ref()
        .map(|i| {
            i.frames
                .iter()
                .filter(|f| f.in_app)
                .filter_map(|f| {
                    normalized_path(&f.path, &board, map, Path::new(&target.root))
                        .map(|p| json!({"path": p, "line": f.line}))
                })
                .take(LIST_LIMIT)
                .collect()
        })
        .unwrap_or_default();
    let mut packet = json!({
        "issue": {
            "key": bounded(&job.issue_key, TITLE_LIMIT),
            "title": bounded(issue.as_ref().map_or(job.issue_title.as_str(), |i| i.title.as_str()), TITLE_LIMIT),
            "severity": issue.as_ref().map(|i| i.level.clone()),
            "count": issue.as_ref().map(|i| i.count),
            "first_seen": issue.as_ref().and_then(|i| i.first_seen.clone()),
            "last_seen": issue.as_ref().and_then(|i| i.last_seen.clone()),
            "url": bare_url(issue.as_ref().and_then(|i| i.url.as_deref()).or(job.issue_url.as_deref())),
        },
        "card": {
            "id": bounded(&job.card_id, TITLE_LIMIT),
            "title": text(&card["title"], TITLE_LIMIT),
            "summary": text(&card["summary"], TEXT_LIMIT),
            "responsibility": text(&card["responsibility"], TEXT_LIMIT),
            "owns": list(&card["owns"], |v| v.as_str().map(|s| bounded(s, TEXT_LIMIT))),
            "files": list(&card["files"], path_of),
            "tests": list(&card["tests"], |v| v.as_str().map(|s| bounded(s, TEXT_LIMIT))),
        },
        "root_frame": {"path": job.root_frame.path, "line": job.root_frame.line},
        "in_app_frames": frames,
        "repository": "The current directory is a dedicated git worktree of the project at its base branch; paths are relative to it.",
        "constraints": [
            "Read-only: change no file and no external system.",
            "Answer only with the diagnosis between the markers, in the final reply.",
        ],
    });
    // Shed the longest lists first until the packet fits.
    for key in [("card", "files"), ("card", "tests"), ("card", "owns")] {
        if packet.to_string().len() <= PACKET_LIMIT {
            break;
        }
        packet[key.0][key.1] = json!([]);
    }
    while packet.to_string().len() > PACKET_LIMIT {
        let Some(frames) = packet["in_app_frames"]
            .as_array_mut()
            .filter(|f| !f.is_empty())
        else {
            break;
        };
        frames.pop();
    }
    packet.to_string()
}

/// The first prompt of a job: the task, then the packet.
pub fn prompt(packet: &str) -> String {
    format!(
        "Investigate this production error and diagnose its root cause. Context (JSON):\n{packet}\n\n\
Finish with the diagnosis JSON between {START} and {END}."
    )
}

/// The prompt that resumes a handed-back session.
pub fn continuation() -> String {
    format!("Continue the investigation where it stopped. Finish with the diagnosis JSON between {START} and {END}.")
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Diagnosis {
    pub likely_cause: String,
    pub code_involved: String,
    pub reproduce: String,
    pub suggested_fix: String,
    pub blast_radius: String,
    pub confidence: String,
}

impl Diagnosis {
    pub fn fields(&self) -> [&str; 6] {
        [
            &self.likely_cause,
            &self.code_involved,
            &self.reproduce,
            &self.suggested_fix,
            &self.blast_radius,
            &self.confidence,
        ]
    }
}

/// The diagnosis in an agent's final reply: the last reply that has the
/// markers must hold exactly one marked JSON object with the six fields.
pub fn parse(replies: &[String]) -> Result<Diagnosis> {
    let Some(reply) = replies.iter().rev().find(|r| r.contains(START)) else {
        bail!("the agent's reply has no diagnosis between {START} and {END}");
    };
    if reply.matches(START).count() != 1 || reply.matches(END).count() != 1 {
        bail!("the agent's reply must contain exactly one diagnosis block");
    }
    let body = reply
        .split(START)
        .nth(1)
        .and_then(|rest| rest.split(END).next());
    let Some(body) = body.filter(|_| reply.find(START) < reply.find(END)) else {
        bail!("the diagnosis markers are out of order");
    };
    let body = body.trim();
    let body = body
        .strip_prefix("```json")
        .or_else(|| body.strip_prefix("```"))
        .and_then(|b| b.strip_suffix("```"))
        .unwrap_or(body)
        .trim();
    let diagnosis: Diagnosis = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("the diagnosis is not the required JSON object: {e}"))?;
    let mut total = 0;
    for ((name, _), value) in SECTIONS.iter().zip(diagnosis.fields()) {
        let value = value.trim();
        if value.is_empty() {
            bail!("the diagnosis field {name} is empty");
        }
        if value.chars().count() > FIELD_LIMIT {
            bail!("the diagnosis field {name} is longer than {FIELD_LIMIT} characters");
        }
        total += value.len();
    }
    if total > TOTAL_LIMIT {
        bail!("the diagnosis is larger than 12 KiB");
    }
    Ok(diagnosis)
}

/// The stored Markdown: fixed headings, the job, and when it was written.
pub fn render(job: &Job, diagnosis: &Diagnosis, at: &str) -> String {
    let mut out = format!("# Diagnosis\n\nJob: {}\nGenerated: {at}\n", job.fingerprint);
    for ((_, heading), value) in SECTIONS.iter().zip(diagnosis.fields()) {
        out.push_str(&format!(
            "\n## {heading}\n\n{}\n",
            value.trim().replace("\n## ", "\n  ## ")
        ));
    }
    out
}

/// `(heading, text)` sections of a stored diagnosis, for the dashboard.
pub fn sections(markdown: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for chunk in markdown.split("\n## ").skip(1) {
        let (heading, body) = chunk.split_once('\n').unwrap_or((chunk, ""));
        if SECTIONS.iter().any(|(_, h)| *h == heading.trim()) {
            out.push(json!({"heading": heading.trim(), "text": body.trim()}));
        }
    }
    out
}
