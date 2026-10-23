//! Machine-local configuration: which projects this machine tracks, and where
//! each one's errors come from. Read from `HARNESS_SIGNALS_CONFIG`, else
//! `HARNESS_CONFIG_HOME/signals.json`, else `~/.config/harness/signals.json`.
//! Secrets are never in this file, only the paths of the files holding them.

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default = "default_poll_seconds")]
    pub poll_seconds: u64,
    /// How fast old errors stop counting: an error's weight halves every this many minutes.
    #[serde(default = "default_half_life_minutes")]
    pub half_life_minutes: f64,
    /// Where the webhook receiver listens. Keep it on loopback; expose it
    /// through a tunnel only if a provider must push to it.
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default)]
    pub projects: Vec<Project>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    /// A short name, also the webhook path segment: `/hooks/nightwatch/<name>`.
    pub name: String,
    /// A registered harness target id, or a path inside one.
    pub target: String,
    /// Only errors from this environment count (e.g. `production`).
    #[serde(default)]
    pub environment: Option<String>,
    /// Rewrites from deployed paths to repository paths, applied in order.
    #[serde(default)]
    pub path_map: Vec<PathMap>,
    pub providers: Vec<Provider>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathMap {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Provider {
    /// Polls Sentry's issue API with a read-only auth token.
    Sentry {
        #[serde(default = "default_sentry_url")]
        base_url: String,
        org: String,
        project: String,
        token_file: String,
        #[serde(default = "default_sentry_limit")]
        limit: u32,
    },
    /// Laravel Nightwatch. With `credentials_file` (written by `login nightwatch`)
    /// the service polls Nightwatch's MCP server; with `secret_file` it also
    /// accepts Nightwatch's signed issue webhooks.
    Nightwatch {
        #[serde(default)]
        credentials_file: Option<String>,
        #[serde(default)]
        secret_file: Option<String>,
        /// The Nightwatch application (name or id) whose issues count.
        #[serde(default)]
        application: Option<String>,
        #[serde(default = "default_nightwatch_mcp")]
        mcp_url: String,
        /// Most open issues read per poll (most recently seen first).
        #[serde(default = "default_sentry_limit")]
        limit: u32,
    },
    /// Runs a command that prints issues as JSON (see docs/observability-heatmap.md).
    Command {
        argv: Vec<String>,
        #[serde(default = "default_command_timeout")]
        timeout_seconds: u64,
    },
}

impl Provider {
    /// A stable identifier for the issues this provider contributes.
    pub fn source(&self, index: usize) -> String {
        match self {
            Self::Sentry { org, project, .. } => format!("sentry:{org}/{project}"),
            Self::Nightwatch { .. } => "nightwatch".to_owned(),
            Self::Command { .. } => format!("command:{index}"),
        }
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Sentry { .. } => "sentry",
            Self::Nightwatch { .. } => "nightwatch",
            Self::Command { .. } => "command",
        }
    }
}

fn default_poll_seconds() -> u64 {
    60
}
fn default_half_life_minutes() -> f64 {
    180.0
}
fn default_listen() -> String {
    "127.0.0.1:18090".to_owned()
}
fn default_sentry_url() -> String {
    "https://sentry.io".to_owned()
}
fn default_sentry_limit() -> u32 {
    50
}
fn default_nightwatch_mcp() -> String {
    "https://nightwatch.laravel.com/mcp".to_owned()
}
fn default_command_timeout() -> u64 {
    60
}

/// The harness configuration directory, by the rule `scripts/harness_config.py` uses.
pub fn config_home() -> PathBuf {
    let set = |name: &str| {
        std::env::var_os(name)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    };
    set("HARNESS_CONFIG_HOME").unwrap_or_else(|| {
        set("XDG_CONFIG_HOME")
            .unwrap_or_else(|| home_dir().join(".config"))
            .join("harness")
    })
}

/// The configuration file this machine reads.
pub fn path() -> PathBuf {
    if let Some(explicit) = std::env::var_os("HARNESS_SIGNALS_CONFIG").filter(|v| !v.is_empty()) {
        return PathBuf::from(explicit);
    }
    config_home().join("signals.json")
}

/// https anywhere; plain http only when the host is exactly a loopback name.
pub fn secure_url(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix("https://") {
        return !rest.is_empty();
    }
    let Some(rest) = url.strip_prefix("http://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    if authority.contains('@') {
        return false;
    }
    let host = match authority.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => authority.rsplit_once(':').map_or(authority, |(h, _)| h),
    };
    matches!(host, "127.0.0.1" | "localhost" | "::1")
}

/// `None` when the file does not exist: the machine tracks nothing.
pub fn load(path: &Path) -> Result<Option<Config>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let config: Config =
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    config.validate()?;
    Ok(Some(config))
}

pub fn slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.poll_seconds < 10 {
            bail!("poll_seconds must be at least 10");
        }
        if !(self.half_life_minutes.is_finite() && self.half_life_minutes >= 1.0) {
            bail!("half_life_minutes must be at least 1");
        }
        let mut names = BTreeSet::new();
        for project in &self.projects {
            let name = &project.name;
            if !slug(name) {
                bail!("project name {name:?} must use only letters, digits, '.', '_' and '-'");
            }
            if !names.insert(name.clone()) {
                bail!("project name {name:?} is used twice");
            }
            if project.providers.is_empty() {
                bail!("project {name:?} has no providers");
            }
            if project
                .providers
                .iter()
                .filter(|p| matches!(p, Provider::Nightwatch { .. }))
                .count()
                > 1
            {
                bail!("project {name:?} has more than one nightwatch provider");
            }
            for provider in &project.providers {
                match provider {
                    Provider::Sentry {
                        base_url,
                        org,
                        project: sentry_project,
                        limit,
                        ..
                    } => {
                        if !secure_url(base_url) {
                            bail!("project {name:?}: sentry base_url must be https (http only for localhost)");
                        }
                        if !slug(org) || !slug(sentry_project) {
                            bail!("project {name:?}: sentry org and project must be slugs");
                        }
                        if !(1..=100).contains(limit) {
                            bail!("project {name:?}: sentry limit must be between 1 and 100");
                        }
                    }
                    Provider::Command {
                        argv,
                        timeout_seconds,
                    } => {
                        if argv.is_empty() || argv[0].is_empty() {
                            bail!("project {name:?}: command argv is empty");
                        }
                        if *timeout_seconds == 0 || *timeout_seconds > 600 {
                            bail!("project {name:?}: command timeout_seconds must be 1 to 600");
                        }
                    }
                    Provider::Nightwatch {
                        credentials_file,
                        secret_file,
                        mcp_url,
                        limit,
                        ..
                    } => {
                        if !(1..=200).contains(limit) {
                            bail!("project {name:?}: nightwatch limit must be between 1 and 200");
                        }
                        if credentials_file.is_none() && secret_file.is_none() {
                            bail!("project {name:?}: nightwatch needs credentials_file (polling) or secret_file (webhooks)");
                        }
                        if !secure_url(mcp_url) {
                            bail!("project {name:?}: nightwatch mcp_url must be https (http only for localhost)");
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// `~/x` and `~` expand to the home directory; anything else is used as is.
pub fn expand(path: &str) -> PathBuf {
    if path == "~" {
        home_dir()
    } else if let Some(rest) = path.strip_prefix("~/") {
        home_dir().join(rest)
    } else {
        PathBuf::from(path)
    }
}

/// A token or secret from its file, trimmed. Errors name the file, never its contents.
pub fn read_secret(path: &str) -> Result<String> {
    let file = expand(path);
    let value = std::fs::read_to_string(&file)
        .with_context(|| format!("reading secret file {}", file.display()))?;
    let value = value.trim().to_owned();
    if value.is_empty() {
        bail!("secret file {} is empty", file.display());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<Config> {
        let config: Config = serde_json::from_str(text)?;
        config.validate()?;
        Ok(config)
    }

    #[test]
    fn a_full_config_parses_with_defaults() {
        let config = parse(
            r#"{"projects":[{"name":"api","target":"demo-1","environment":"production",
              "path_map":[{"from":"/var/www/html/","to":""}],
              "providers":[{"type":"sentry","org":"acme","project":"api","token_file":"~/t"},
                           {"type":"nightwatch","credentials_file":"~/c","secret_file":"~/s"},
                           {"type":"command","argv":["./issues"]}]}]}"#,
        )
        .unwrap();
        assert_eq!(config.poll_seconds, 60);
        assert_eq!(config.listen, "127.0.0.1:18090");
        let providers = &config.projects[0].providers;
        assert_eq!(providers[0].source(0), "sentry:acme/api");
        assert_eq!(providers[1].source(1), "nightwatch");
        assert_eq!(providers[2].source(2), "command:2");
        assert!(
            matches!(&providers[0], Provider::Sentry { base_url, limit: 50, .. } if base_url == "https://sentry.io")
        );
    }

    #[test]
    fn unsafe_or_unknown_settings_are_refused() {
        for bad in [
            r#"{"poll_seconds":1}"#,
            r#"{"projects":[{"name":"a b","target":"x","providers":[{"type":"command","argv":["x"]}]}]}"#,
            r#"{"projects":[{"name":"a","target":"x","providers":[]}]}"#,
            r#"{"projects":[{"name":"a","target":"x","providers":[{"type":"sentry","base_url":"http://evil","org":"o","project":"p","token_file":"t"}]}]}"#,
            r#"{"projects":[{"name":"a","target":"x","providers":[{"type":"sentry","org":"../o","project":"p","token_file":"t"}]}]}"#,
            r#"{"projects":[{"name":"a","target":"x","providers":[{"type":"command","argv":[]}]}]}"#,
            r#"{"projects":[{"name":"a","target":"x","providers":[{"type":"sentry","base_url":"http://localhost.attacker.example","org":"o","project":"p","token_file":"t"}]}]}"#,
            r#"{"projects":[{"name":"a","target":"x","providers":[{"type":"nightwatch"}]}]}"#,
            r#"{"projects":[{"name":"a","target":"x","providers":[{"type":"nightwatch","credentials_file":"c","mcp_url":"http://evil/mcp"}]}]}"#,
            r#"{"projects":[{"name":"a","target":"x","token":"oops","providers":[{"type":"command","argv":["x"]}]}]}"#,
            r#"{"projects":[{"name":"a","target":"x","providers":[{"type":"command","argv":["x"]}]},{"name":"a","target":"y","providers":[{"type":"command","argv":["x"]}]}]}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_https_or_exact_loopback_hosts_are_trusted() {
        for good in [
            "https://sentry.io",
            "http://127.0.0.1:9000/x",
            "http://localhost",
            "http://[::1]:80/",
        ] {
            assert!(secure_url(good), "{good}");
        }
        for bad in [
            "http://localhost.attacker.example",
            "http://127.0.0.1.nip.io",
            "http://user@localhost.evil",
            "http://localhost@evil.example",
            "ftp://localhost",
            "https://",
        ] {
            assert!(!secure_url(bad), "{bad}");
        }
    }

    #[test]
    fn a_missing_file_means_nothing_is_tracked_and_secrets_are_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        assert!(load(&dir.path().join("signals.json")).unwrap().is_none());
        let secret = dir.path().join("token");
        std::fs::write(&secret, "abc123\n").unwrap();
        assert_eq!(read_secret(secret.to_str().unwrap()).unwrap(), "abc123");
        std::fs::write(&secret, " \n").unwrap();
        let error = read_secret(secret.to_str().unwrap())
            .unwrap_err()
            .to_string();
        assert!(error.contains("is empty"));
    }
}
