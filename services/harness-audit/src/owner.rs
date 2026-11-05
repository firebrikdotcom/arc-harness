//! Who may open a session view: its owner only.
//!
//! A session records the owner at start: the OS account that ran it and, when
//! known, the Tailscale login. A request is judged on the TCP peer address
//! alone, never on `X-Forwarded-For`, which any client can set:
//!
//! * loopback peer: the machine owner. When `tailscale serve` put the request
//!   there it adds `Tailscale-User-Login`; that login must then equal the
//!   owner's. Without the header the service's OS account must equal the owner's.
//! * tailnet peer (`100.64.0.0/10`, `fd7a:115c:a1e0::/48`): `tailscale whois`
//!   names the login behind the address; it must equal the owner's.
//! * anything else: denied.

use actix_web::{web, HttpRequest};
use serde_json::Value;
use std::collections::HashMap;
use std::io::Read;
use std::net::IpAddr;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

const WHOIS_TIMEOUT: Duration = Duration::from_secs(2);
const WHOIS_TTL: Duration = Duration::from_secs(60);
const MAC_TAILSCALE: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
const SERVE_HEADER: &str = "Tailscale-User-Login";

/// The owner a session recorded at start.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Owner {
    pub user: String,
    pub login: Option<String>,
}

impl Owner {
    /// Read `{"user": "...", "login": "..."|null}`; a document without a user has no owner.
    pub fn from_json(value: &Value) -> Option<Self> {
        let user = value.get("user")?.as_str()?.trim();
        if user.is_empty() {
            return None;
        }
        let login = value
            .get("login")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|login| !login.is_empty())
            .map(str::to_owned);
        Some(Self {
            user: user.to_owned(),
            login,
        })
    }
}

/// Resolves a tailnet address to the login behind it. Injectable for tests.
pub trait Whois: Send + Sync {
    fn login(&self, ip: IpAddr) -> Option<String>;
}

/// `tailscale whois --json <ip>`, cached per address for a minute.
pub struct TailscaleWhois {
    cache: Mutex<HashMap<IpAddr, (Instant, Option<String>)>>,
}

impl TailscaleWhois {
    pub fn new() -> Self {
        Self {
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn binary() -> Option<String> {
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                let candidate = dir.join("tailscale");
                if candidate.is_file() {
                    return Some(candidate.to_string_lossy().into_owned());
                }
            }
        }
        Path::new(MAC_TAILSCALE)
            .is_file()
            .then(|| MAC_TAILSCALE.to_owned())
    }

    fn lookup(ip: IpAddr) -> Option<String> {
        let binary = Self::binary()?;
        let mut child = Command::new(binary)
            .args(["whois", "--json", &ip.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let mut stdout = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut buffer = Vec::new();
            let _ = stdout.read_to_end(&mut buffer);
            buffer
        });
        let deadline = Instant::now() + WHOIS_TIMEOUT;
        let finished = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
            }
        };
        let output = reader.join().unwrap_or_default();
        if !finished {
            return None;
        }
        login_from_whois(&output)
    }
}

impl Default for TailscaleWhois {
    fn default() -> Self {
        Self::new()
    }
}

impl Whois for TailscaleWhois {
    fn login(&self, ip: IpAddr) -> Option<String> {
        {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some((at, login)) = cache.get(&ip) {
                if at.elapsed() < WHOIS_TTL {
                    return login.clone();
                }
            }
        }
        let login = Self::lookup(ip);
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(ip, (Instant::now(), login.clone()));
        login
    }
}

/// `UserProfile.LoginName` from `tailscale whois --json`.
fn login_from_whois(output: &[u8]) -> Option<String> {
    let document: Value = serde_json::from_slice(output).ok()?;
    document
        .get("UserProfile")?
        .get("LoginName")?
        .as_str()
        .map(str::trim)
        .filter(|login| !login.is_empty())
        .map(str::to_owned)
}

/// Whether the address is on the tailnet.
pub fn is_tailnet(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            let [first, second, ..] = v4.octets();
            first == 100 && (second & 0xC0) == 64
        }
        IpAddr::V6(v6) => {
            let segments = v6.segments();
            segments[..3] == [0xfd7a, 0x115c, 0xa1e0]
        }
    }
}

fn same_login(a: &str, b: &str) -> bool {
    a.eq_ignore_ascii_case(b)
}

/// The owner policy plus what it needs from the machine.
pub struct OwnerCheck {
    service_user: String,
    whois: Arc<dyn Whois>,
}

impl OwnerCheck {
    pub fn new(service_user: impl Into<String>, whois: Arc<dyn Whois>) -> Self {
        Self {
            service_user: service_user.into(),
            whois,
        }
    }

    fn from_env() -> Self {
        let user = ["USER", "LOGNAME"]
            .iter()
            .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
            .unwrap_or_default();
        Self::new(user, Arc::new(TailscaleWhois::new()))
    }

    /// The process-wide instance, so every worker shares one whois cache.
    pub fn shared() -> Arc<Self> {
        static SHARED: OnceLock<Arc<OwnerCheck>> = OnceLock::new();
        SHARED.get_or_init(|| Arc::new(Self::from_env())).clone()
    }

    /// The instance registered as app data, or the shared one.
    pub fn for_request(request: &HttpRequest) -> Arc<Self> {
        request
            .app_data::<web::Data<Self>>()
            .map_or_else(Self::shared, |data| data.clone().into_inner())
    }

    /// The facts a decision needs, taken from the TCP peer and the serve header only.
    pub fn who(request: &HttpRequest) -> Requester {
        Requester {
            peer: request.peer_addr().map(|addr| addr.ip()),
            serve_login: request
                .headers()
                .get(SERVE_HEADER)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        }
    }

    /// The decision. Blocking when the peer is on the tailnet (it runs `tailscale whois`).
    pub fn allows_blocking(&self, requester: &Requester, owner: &Owner) -> bool {
        let Some(peer) = requester.peer else {
            return false;
        };
        let peer = peer.to_canonical();
        if peer.is_loopback() {
            return match requester.serve_login.as_deref() {
                Some(header) => owner
                    .login
                    .as_deref()
                    .is_some_and(|login| same_login(login, header)),
                None => !self.service_user.is_empty() && owner.user == self.service_user,
            };
        }
        if is_tailnet(peer) {
            let Some(owner_login) = owner.login.as_deref() else {
                return false;
            };
            return self
                .whois
                .login(peer)
                .is_some_and(|login| same_login(&login, owner_login));
        }
        false
    }

    /// The decision, off the async executor.
    pub async fn allows(self: Arc<Self>, requester: Requester, owner: Owner) -> bool {
        let needs_whois = requester
            .peer
            .is_some_and(|ip| is_tailnet(ip.to_canonical()));
        if !needs_whois {
            return self.allows_blocking(&requester, &owner);
        }
        web::block(move || self.allows_blocking(&requester, &owner))
            .await
            .unwrap_or(false)
    }
}

#[derive(Debug, Clone)]
pub struct Requester {
    pub peer: Option<IpAddr>,
    pub serve_login: Option<String>,
}

#[cfg(test)]
pub mod tests {
    use super::*;

    pub struct FakeWhois(pub Option<&'static str>);

    impl Whois for FakeWhois {
        fn login(&self, _ip: IpAddr) -> Option<String> {
            self.0.map(str::to_owned)
        }
    }

    pub fn check(user: &str, whois: Option<&'static str>) -> OwnerCheck {
        OwnerCheck::new(user, Arc::new(FakeWhois(whois)))
    }

    fn owner(user: &str, login: Option<&str>) -> Owner {
        Owner {
            user: user.to_owned(),
            login: login.map(str::to_owned),
        }
    }

    fn from(ip: &str, header: Option<&str>) -> Requester {
        Requester {
            peer: Some(ip.parse().unwrap()),
            serve_login: header.map(str::to_owned),
        }
    }

    #[test]
    fn loopback_without_the_serve_header_is_the_machine_owner() {
        let policy = check("savior", None);
        assert!(policy.allows_blocking(&from("127.0.0.1", None), &owner("savior", None)));
        assert!(policy.allows_blocking(&from("::1", None), &owner("savior", Some("a@b.c"))));
        assert!(!policy.allows_blocking(&from("127.0.0.1", None), &owner("other", None)));
        assert!(!check("", None).allows_blocking(&from("127.0.0.1", None), &owner("", None)));
    }

    #[test]
    fn loopback_with_the_serve_header_must_match_the_owner_login() {
        let policy = check("savior", None);
        let session = owner("savior", Some("Savio@Example.com"));
        assert!(policy.allows_blocking(&from("127.0.0.1", Some("savio@example.com")), &session));
        assert!(!policy.allows_blocking(&from("127.0.0.1", Some("eve@example.com")), &session));
        // A header with an owner that recorded no login never matches, even for the same OS user.
        assert!(!policy.allows_blocking(
            &from("127.0.0.1", Some("savio@example.com")),
            &owner("savior", None)
        ));
    }

    #[test]
    fn tailnet_peers_are_matched_with_whois() {
        let session = owner("savior", Some("savio@example.com"));
        let yes = check("savior", Some("savio@example.com"));
        let no = check("savior", Some("eve@example.com"));
        let down = check("savior", None);
        for ip in ["100.64.0.1", "100.127.255.254", "fd7a:115c:a1e0::1"] {
            assert!(yes.allows_blocking(&from(ip, None), &session), "{ip}");
            assert!(!no.allows_blocking(&from(ip, None), &session), "{ip}");
            assert!(!down.allows_blocking(&from(ip, None), &session), "{ip}");
            // The serve header is ignored off loopback.
            assert!(
                !no.allows_blocking(&from(ip, Some("savio@example.com")), &session),
                "{ip}"
            );
        }
        assert!(!yes.allows_blocking(&from("100.64.0.1", None), &owner("savior", None)));
    }

    #[test]
    fn other_addresses_are_denied() {
        let policy = check("savior", Some("savio@example.com"));
        let session = owner("savior", Some("savio@example.com"));
        for ip in [
            "192.168.1.5",
            "10.0.0.2",
            "100.128.0.1",
            "100.63.255.255",
            "8.8.8.8",
            "fd7a:115c:a1e1::1",
            "2001:db8::1",
        ] {
            assert!(
                !policy.allows_blocking(&from(ip, Some("savio@example.com")), &session),
                "{ip}"
            );
        }
        let none = Requester {
            peer: None,
            serve_login: None,
        };
        assert!(!policy.allows_blocking(&none, &session));
    }

    #[test]
    fn whois_output_is_read_for_the_login_only() {
        let ok = br#"{"Node":{"Name":"x"},"UserProfile":{"LoginName":"savio@example.com"}}"#;
        assert_eq!(login_from_whois(ok).as_deref(), Some("savio@example.com"));
        for bad in [
            &b"{}"[..],
            b"not json",
            br#"{"UserProfile":{"LoginName":""}}"#,
            br#"{"UserProfile":{"LoginName":null}}"#,
        ] {
            assert_eq!(login_from_whois(bad), None);
        }
    }
}
