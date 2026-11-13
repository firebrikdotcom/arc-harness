use serde::{Deserialize, Serialize};
use std::{env, fs, io, path::PathBuf, process::Command, sync::Mutex};

static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CollectionSettings {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    pub jev: bool,
    pub workflow: bool,
    #[serde(default)]
    pub workflow_prompts: bool,
    #[serde(default)]
    pub workflow_context: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HookSettings {
    pub session_startup: bool,
    pub workflow_audit: bool,
    pub workflow_gate: bool,
    pub command_observer: bool,
    pub failure_tracking: bool,
    pub worktree_cleanup: bool,
    pub session_log_cleanup: bool,
    pub retrieval_reminder: bool,
}

impl Default for HookSettings {
    fn default() -> Self {
        Self {
            session_startup: true,
            workflow_audit: true,
            workflow_gate: true,
            command_observer: true,
            failure_tracking: true,
            worktree_cleanup: true,
            session_log_cleanup: true,
            retrieval_reminder: true,
        }
    }
}

fn default_enabled() -> bool {
    true
}

impl Default for CollectionSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            jev: true,
            workflow: true,
            workflow_prompts: false,
            workflow_context: false,
        }
    }
}

pub fn path() -> PathBuf {
    env::var_os("HARNESS_CONFIG")
        .or_else(|| env::var_os("HARNESS_AUDIT_SETTINGS"))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let home = env::var_os("HARNESS_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    env::var_os("XDG_CONFIG_HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| {
                            PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".config")
                        })
                        .join("harness")
                });
            home.join("config.json")
        })
}

pub fn initialize() -> io::Result<()> {
    if path().exists() {
        load()?;
        return Ok(());
    }
    let writer = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/harness_config.py");
    let output = Command::new("python3")
        .arg(writer)
        .arg("init")
        .arg("--file")
        .arg(path())
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    Ok(())
}

pub fn load() -> io::Result<CollectionSettings> {
    load_from(&path())
}

fn load_from(path: &std::path::Path) -> io::Result<CollectionSettings> {
    match fs::read(path) {
        Ok(bytes) => {
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            if value.get("version").is_none() {
                return serde_json::from_value(value).map_err(io::Error::other);
            }
            if value["version"] != 1 {
                return Err(io::Error::other("configuration version must be 1"));
            }
            if !matches!(
                value["typesafe"]["mode"].as_str(),
                Some("off" | "shadow" | "active")
            ) || value["typesafe"]["checkpoints"].as_bool().is_none()
                || value["dispatch"]["enabled"].as_bool().is_none()
                || !value["typesafe"]["model"]
                    .as_str()
                    .is_some_and(|m| !m.trim().is_empty() && !m.ends_with("-latest"))
            {
                return Err(io::Error::other(
                    "invalid TypeSafe or dispatch configuration",
                ));
            }
            let telemetry = &value["telemetry"];
            let flag = |key: &str| {
                telemetry[key]
                    .as_bool()
                    .ok_or_else(|| io::Error::other(format!("telemetry.{key} must be boolean")))
            };
            let enabled = flag("enabled")?;
            Ok(CollectionSettings {
                enabled,
                jev: flag("jev")?,
                workflow: flag("workflow")?,
                workflow_prompts: flag("workflow_prompts")?,
                workflow_context: telemetry["workflow_context"].as_bool().unwrap_or(false),
            })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(CollectionSettings::default()),
        Err(error) => Err(error),
    }
}

pub fn save(settings: CollectionSettings) -> io::Result<()> {
    save_to(&path(), settings)
}

pub fn load_hooks() -> io::Result<HookSettings> {
    load_hooks_from(&path())
}

fn load_hooks_from(path: &std::path::Path) -> io::Result<HookSettings> {
    match fs::read(path) {
        Ok(bytes) => {
            let value: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            match value.get("hooks").cloned() {
                Some(mut settings) => {
                    let Some(fields) = settings.as_object_mut() else {
                        return Err(io::Error::other("hooks settings must be an object"));
                    };
                    fields.remove("_doc");
                    let mut merged =
                        serde_json::to_value(HookSettings::default()).map_err(io::Error::other)?;
                    merged.as_object_mut().unwrap().extend(fields.clone());
                    serde_json::from_value(merged).map_err(io::Error::other)
                }
                None => Ok(HookSettings::default()),
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(HookSettings::default()),
        Err(error) => Err(error),
    }
}

pub fn save_hooks_and_apply(settings: HookSettings) -> io::Result<()> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| io::Error::other("settings lock unavailable"))?;
    let writer = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/harness_config.py");
    let values = serde_json::to_string(&settings).map_err(io::Error::other)?;
    let output = Command::new("python3")
        .arg(writer)
        .arg("update")
        .arg("--file")
        .arg(path())
        .arg("--section")
        .arg("hooks")
        .arg("--values")
        .arg(values)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }

    let installer =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/install-hooks.sh");
    let output = Command::new("sh").arg(installer).output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "settings saved but hooks could not be applied: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn save_to(path: &std::path::Path, settings: CollectionSettings) -> io::Result<()> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| io::Error::other("settings lock unavailable"))?;
    // The shared writer locks across processes and merges only the requested
    // section, preserving routing switches, docs and concurrent CLI changes.
    let writer = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../scripts/harness_config.py");
    // Dashboard saves edit category switches only. Never overwrite a master
    // switch changed through config.json after a browser tab was opened.
    let mut changes = serde_json::to_value(settings).map_err(io::Error::other)?;
    changes.as_object_mut().unwrap().remove("enabled");
    let values = serde_json::to_string(&changes).map_err(io::Error::other)?;
    let output = Command::new("python3")
        .arg(writer)
        .arg("update")
        .arg("--file")
        .arg(path)
        .arg("--section")
        .arg("telemetry")
        .arg("--values")
        .arg(values)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    Ok(())
}

impl CollectionSettings {
    pub fn permits(&self, event_type: &str) -> bool {
        if !self.enabled {
            return false;
        }
        if event_type == "workflow.prompt_recorded" {
            self.workflow && self.workflow_prompts
        } else if event_type == "workflow.context_recorded" {
            self.workflow && self.workflow_context
        } else if event_type.starts_with("workflow.") {
            self.workflow
        } else {
            self.jev
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn switches_persist_independently_and_invalid_settings_fail_closed() {
        let dir = env::temp_dir().join(format!("audit-settings-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.json");
        assert_eq!(load_from(&path).unwrap(), CollectionSettings::default());
        let config = CollectionSettings {
            enabled: true,
            jev: false,
            workflow: true,
            workflow_prompts: false,
            workflow_context: false,
        };
        save_to(&path, config).unwrap();
        assert_eq!(load_from(&path).unwrap(), config);
        assert!(!config.permits("jev.checkpoint"));
        assert!(!config.permits("agent.completed"));
        assert!(config.permits("workflow.task_started"));
        fs::write(&path, r#"{"jev":true,"workflow":true}"#).unwrap();
        let legacy = load_from(&path).unwrap();
        assert!(!legacy.workflow_prompts);
        assert!(!legacy.permits("workflow.prompt_recorded"));
        assert!(!legacy.workflow_context);
        assert!(!legacy.permits("workflow.context_recorded"));
        let context = CollectionSettings {
            workflow_context: true,
            ..legacy
        };
        assert!(context.permits("workflow.context_recorded"));
        assert!(!context.permits("workflow.prompt_recorded"));
        assert!(!CollectionSettings {
            workflow: false,
            ..context
        }
        .permits("workflow.context_recorded"));
        assert!(!CollectionSettings {
            enabled: false,
            ..context
        }
        .permits("workflow.context_recorded"));
        let prompts = CollectionSettings {
            workflow_prompts: true,
            ..legacy
        };
        save_to(&path, prompts).unwrap();
        assert!(load_from(&path)
            .unwrap()
            .permits("workflow.prompt_recorded"));
        assert!(!CollectionSettings {
            workflow: false,
            ..prompts
        }
        .permits("workflow.prompt_recorded"));
        // A stale page captured enabled=true before a CLI master opt-out.
        let mut document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        document["telemetry"]["enabled"] = serde_json::json!(false);
        document["typesafe"]["mode"] = serde_json::json!("off");
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        save_to(&path, prompts).unwrap();
        let saved = load_from(&path).unwrap();
        assert!(!saved.enabled);
        assert!(!saved.permits("workflow.prompt_recorded"));
        let document: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(document["typesafe"]["mode"], "off");
        assert!(document["telemetry"]["_doc"].is_string());
        fs::write(&path, "{}").unwrap();
        assert!(load_from(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hook_settings_default_on_for_existing_config_and_read_saved_selections() {
        let dir = env::temp_dir().join(format!("audit-hooks-{}", uuid::Uuid::new_v4()));
        let path = dir.join("settings.json");
        fs::create_dir_all(&dir).unwrap();
        fs::write(&path, r#"{"version":1}"#).unwrap();
        assert_eq!(load_hooks_from(&path).unwrap(), HookSettings::default());
        fs::write(
            &path,
            r#"{"version":1,"hooks":{"_doc":"selection help","session_startup":false,"workflow_audit":true,"workflow_gate":false,"command_observer":false,"failure_tracking":false,"worktree_cleanup":false,"session_log_cleanup":false,"retrieval_reminder":false}}"#,
        )
        .unwrap();
        let saved = load_hooks_from(&path).unwrap();
        assert!(!saved.session_startup);
        assert!(saved.workflow_audit);
        assert!(!saved.workflow_gate);
        fs::remove_dir_all(dir).unwrap();
    }
}
