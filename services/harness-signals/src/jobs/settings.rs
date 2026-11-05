//! What the runner may use: limits from `signals.json` `issue_agents`, the
//! agent and helper executables, and the home that holds agent transcripts.
//! Environment overrides exist only with `HARNESS_ISSUE_AGENTS_TEST_MODE=1`,
//! so production configuration can never swap the read-only launch profile.

use super::AgentKind;
use crate::config::{home_dir, Config, IssueAgents};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const TEST_MODE: &str = "HARNESS_ISSUE_AGENTS_TEST_MODE";

#[derive(Debug, Clone)]
pub struct Settings {
    pub global_cap: u64,
    pub daily_cap: u64,
    pub timeout: Duration,
    /// The configured agent; `None` picks claude when installed, else codex.
    pub agent: Option<AgentKind>,
    pub herdr_workspace: Option<String>,
    pub ssh: Option<String>,
    pub claude: Option<Vec<String>>,
    pub codex: Option<Vec<String>>,
    pub herdr: Option<Vec<String>>,
    pub difit: Option<Vec<String>>,
    /// Where `.claude/projects` and `.codex/sessions` live.
    pub home: PathBuf,
    pub heartbeat: Duration,
    pub poll: Duration,
    pub grace: Duration,
    /// Take over waits this long for a background agent's step boundary.
    pub takeover_wait: Duration,
    /// A transcript quiet for this long, with no tool call open, is at a step boundary.
    pub step_quiet: Duration,
    /// A job's session quiet this long after its last reply, with its agent idle
    /// or no agent process left, has ended.
    pub idle_end: Duration,
    pub test_mode: bool,
}

pub fn test_mode() -> bool {
    std::env::var(TEST_MODE).as_deref() == Ok("1")
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn env_argv(name: &str) -> Option<Vec<String>> {
    let argv: Vec<String> = serde_json::from_str(&env(name)?).ok()?;
    (!argv.is_empty() && !argv[0].is_empty()).then_some(argv)
}

fn env_u64(name: &str) -> Option<u64> {
    env(name)?.parse().ok()
}

impl Settings {
    pub fn from_config(config: Option<&Config>) -> Self {
        let defaults = IssueAgents::default();
        let agents = config.map_or(&defaults, |c| &c.issue_agents);
        let mut settings = Self {
            global_cap: agents.global_cap(),
            daily_cap: agents.daily_cap(),
            timeout: Duration::from_secs(agents.timeout_seconds()),
            agent: agents.agent.as_deref().and_then(parse_agent),
            herdr_workspace: agents.herdr_workspace().map(str::to_owned),
            ssh: agents.ssh.clone(),
            claude: resolve("claude").map(|p| vec![p.display().to_string()]),
            codex: resolve("codex").map(|p| vec![p.display().to_string()]),
            herdr: resolve("herdr").map(|p| vec![p.display().to_string()]),
            difit: resolve("npx").map(|p| vec![p.display().to_string(), "difit".to_owned()]),
            home: home_dir(),
            heartbeat: Duration::from_secs(10),
            poll: Duration::from_millis(500),
            grace: Duration::from_secs(5),
            takeover_wait: Duration::from_secs(agents.takeover_wait_seconds()),
            step_quiet: Duration::from_secs(2),
            idle_end: Duration::from_secs(10 * 60),
            test_mode: false,
        };
        if test_mode() {
            settings.apply_test_overrides();
        }
        settings
    }

    /// Test mode never falls back to an installed binary: what is not given is absent.
    fn apply_test_overrides(&mut self) {
        self.test_mode = true;
        self.claude = env_argv("HARNESS_ISSUE_AGENTS_CLAUDE_ARGV");
        self.codex = env_argv("HARNESS_ISSUE_AGENTS_CODEX_ARGV");
        self.herdr = env_argv("HARNESS_ISSUE_AGENTS_HERDR_ARGV");
        self.difit = env_argv("HARNESS_ISSUE_AGENTS_DIFIT_ARGV");
        if let Some(home) = env("HARNESS_ISSUE_AGENTS_HOME") {
            self.home = PathBuf::from(home);
        }
        if let Some(cap) = env_u64("HARNESS_ISSUE_AGENTS_GLOBAL_CAP") {
            self.global_cap = cap.clamp(1, 4);
        }
        if let Some(cap) = env_u64("HARNESS_ISSUE_AGENTS_DAILY_CAP") {
            self.daily_cap = cap.clamp(1, 20);
        }
        if let Some(seconds) = env_u64("HARNESS_ISSUE_AGENTS_TIMEOUT_SECONDS") {
            self.timeout = Duration::from_secs(seconds.clamp(1, 7200));
        }
        if let Some(ms) = env_u64("HARNESS_ISSUE_AGENTS_HEARTBEAT_MS") {
            self.heartbeat = Duration::from_millis(ms.max(50));
        }
        if let Some(ms) = env_u64("HARNESS_ISSUE_AGENTS_GRACE_MS") {
            self.grace = Duration::from_millis(ms.max(50));
        }
        if let Some(ms) = env_u64("HARNESS_ISSUE_AGENTS_TAKEOVER_WAIT_MS") {
            self.takeover_wait = Duration::from_millis(ms.max(50));
        }
        if let Some(ms) = env_u64("HARNESS_ISSUE_AGENTS_STEP_QUIET_MS") {
            self.step_quiet = Duration::from_millis(ms.max(10));
        }
        if let Some(ms) = env_u64("HARNESS_ISSUE_AGENTS_IDLE_END_MS") {
            self.idle_end = Duration::from_millis(ms.max(10));
        }
        if let Some(agent) = env("HARNESS_ISSUE_AGENTS_AGENT")
            .as_deref()
            .and_then(parse_agent)
        {
            self.agent = Some(agent);
        }
        if let Some(workspace) = env("HARNESS_ISSUE_AGENTS_HERDR_WORKSPACE") {
            self.herdr_workspace = Some(workspace);
        }
    }

    /// Settings for tests: no executables at all until a test names its fakes.
    #[cfg(test)]
    pub fn for_tests(home: &Path) -> Self {
        Self {
            global_cap: 2,
            daily_cap: 5,
            timeout: Duration::from_secs(30),
            agent: None,
            herdr_workspace: None,
            ssh: None,
            claude: None,
            codex: None,
            herdr: None,
            difit: None,
            home: home.to_owned(),
            heartbeat: Duration::from_millis(100),
            poll: Duration::from_millis(50),
            grace: Duration::from_millis(500),
            takeover_wait: Duration::from_secs(5),
            step_quiet: Duration::from_millis(300),
            idle_end: Duration::from_secs(60),
            test_mode: true,
        }
    }

    pub fn exe(&self, kind: AgentKind) -> Option<&Vec<String>> {
        match kind {
            AgentKind::Claude => self.claude.as_ref(),
            AgentKind::Codex => self.codex.as_ref(),
        }
    }

    /// The agent a new job uses: the configured one, else claude when installed, else codex.
    pub fn pick_agent(&self) -> AgentKind {
        self.agent
            .unwrap_or(if self.claude.is_some() || self.codex.is_none() {
                AgentKind::Claude
            } else {
                AgentKind::Codex
            })
    }
}

pub fn parse_agent(value: &str) -> Option<AgentKind> {
    match value {
        "claude" => Some(AgentKind::Claude),
        "codex" => Some(AgentKind::Codex),
        _ => None,
    }
}

/// Directories searched besides PATH: a service manager starts with a minimal PATH.
fn extra_dirs() -> Vec<PathBuf> {
    let home = home_dir();
    vec![
        home.join(".local/bin"),
        home.join(".npm-global/bin"),
        home.join(".cargo/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ]
}

/// The PATH handed to child processes: theirs plus the usual install directories.
pub fn child_path(first: Option<&Path>) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = first.into_iter().map(Path::to_owned).collect();
    dirs.extend(
        std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
            .unwrap_or_default(),
    );
    dirs.extend(extra_dirs());
    std::env::join_paths(dirs).unwrap_or_default()
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// An installed executable by name, as an absolute path.
pub fn resolve(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();
    path.into_iter()
        .chain(extra_dirs())
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| executable(candidate))
}

pub fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    let filled = std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buffer))
        .is_ok();
    if !filled {
        let seed = format!(
            "{:?}{}{:?}",
            std::time::SystemTime::now(),
            std::process::id(),
            std::thread::current().id()
        );
        let digest = <sha2::Sha256 as sha2::Digest>::digest(seed.as_bytes());
        for (i, b) in buffer.iter_mut().enumerate() {
            *b = digest[i % digest.len()];
        }
    }
    hex::encode(buffer)
}

/// A random version 4 UUID, for Claude's preassigned session id.
pub fn uuid_v4() -> String {
    let mut bytes = hex::decode(random_hex(16)).unwrap_or_else(|_| vec![0; 16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let h = hex::encode(bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// Session, tab and pane ids from agents and Herdr: safe in a path, an argv and a command line.
pub fn safe_token(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

/// This machine's name, for display only.
pub fn machine() -> String {
    let mut buffer = [0u8; 256];
    // SAFETY: the buffer is valid for its whole length and gethostname NUL-terminates within it.
    let ok = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } == 0;
    let name = if ok {
        let end = buffer.iter().position(|b| *b == 0).unwrap_or(buffer.len());
        String::from_utf8_lossy(&buffer[..end]).into_owned()
    } else {
        String::new()
    };
    let name: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .take(128)
        .collect();
    if name.is_empty() {
        "localhost".to_owned()
    } else {
        name
    }
}
