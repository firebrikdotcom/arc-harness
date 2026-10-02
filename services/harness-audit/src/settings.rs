use serde::{Deserialize, Serialize};
use std::{env, fs, io, path::PathBuf, sync::Mutex};

static WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CollectionSettings {
    pub jev: bool,
    pub workflow: bool,
    #[serde(default)]
    pub workflow_prompts: bool,
}

impl Default for CollectionSettings {
    fn default() -> Self {
        Self {
            jev: true,
            workflow: true,
            workflow_prompts: false,
        }
    }
}

pub fn path() -> PathBuf {
    env::var_os("HARNESS_AUDIT_SETTINGS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.harness-db/audit-settings.json")
        })
}

pub fn load() -> io::Result<CollectionSettings> {
    load_from(&path())
}

fn load_from(path: &std::path::Path) -> io::Result<CollectionSettings> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(CollectionSettings::default()),
        Err(error) => Err(error),
    }
}

pub fn save(settings: CollectionSettings) -> io::Result<()> {
    save_to(&path(), settings)
}

fn save_to(path: &std::path::Path, settings: CollectionSettings) -> io::Result<()> {
    let _lock = WRITE_LOCK
        .lock()
        .map_err(|_| io::Error::other("settings lock unavailable"))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(&settings).map_err(io::Error::other)?;
    fs::write(&temp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&temp, fs::Permissions::from_mode(0o600))?;
    }
    fs::rename(temp, path)
}

impl CollectionSettings {
    pub fn permits(&self, event_type: &str) -> bool {
        if event_type == "workflow.prompt_recorded" {
            self.workflow && self.workflow_prompts
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
            jev: false,
            workflow: true,
            workflow_prompts: false,
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
        fs::write(&path, "{}").unwrap();
        assert!(load_from(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
