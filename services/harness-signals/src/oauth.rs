//! One-time sign-in for MCP servers that only accept interactive OAuth
//! (Nightwatch): discover the authorization server, register this service as a
//! public client, have a person approve it in a browser (PKCE), and keep the
//! refresh token in a 0600 file. Afterwards the service refreshes its own
//! access tokens. Token values are never printed, logged or put in the DB.

use crate::config;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

const LOGIN_WAIT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credentials {
    pub mcp_url: String,
    pub token_endpoint: String,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    pub refresh_token: String,
    #[serde(default)]
    pub access_token: Option<String>,
    /// Unix seconds.
    #[serde(default)]
    pub expires_at: Option<i64>,
}

pub fn default_path(provider: &str) -> PathBuf {
    config::config_home().join(format!("signals/{provider}.oauth"))
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout(Duration::from_secs(30))
        .redirects(0)
        .build()
}

/// An error that names the endpoint and status, never a body that might echo a token.
fn call(request: ureq::Request, what: &str, body: Option<RequestBody>) -> Result<Value> {
    let response = match body {
        None => request.call(),
        Some(RequestBody::Json(json)) => request.send_json(json),
        Some(RequestBody::Form(form)) => {
            let pairs: Vec<(&str, &str)> = form.iter().map(|(k, v)| (*k, v.as_str())).collect();
            request.send_form(&pairs)
        }
    };
    match response {
        Ok(response) => response
            .into_json()
            .with_context(|| format!("{what}: invalid JSON")),
        Err(ureq::Error::Status(code, response)) => {
            // OAuth errors carry a short code such as invalid_grant; keep only that.
            let reason = response
                .into_json::<Value>()
                .ok()
                .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_owned))
                .filter(|e| {
                    e.len() <= 64 && e.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                })
                .map(|e| format!(" ({e})"))
                .unwrap_or_default();
            bail!("{what}: HTTP {code}{reason}")
        }
        Err(ureq::Error::Transport(error)) => bail!("{what}: {}", error.kind()),
    }
}

enum RequestBody {
    Json(Value),
    Form(Vec<(&'static str, String)>),
}

struct Endpoints {
    authorization: String,
    token: String,
    registration: String,
    scope: Option<String>,
}

fn origin(url: &str) -> Result<(String, String)> {
    let (scheme, rest) = url.split_once("://").context("the MCP URL has no scheme")?;
    let (host, path) = rest.split_once('/').map_or((rest, ""), |(h, p)| (h, p));
    Ok((
        format!("{scheme}://{host}"),
        format!("/{path}").trim_end_matches('/').to_owned(),
    ))
}

fn discover(mcp_url: &str) -> Result<Endpoints> {
    let agent = agent();
    let (base, path) = origin(mcp_url)?;
    let resource = [
        format!("{base}/.well-known/oauth-protected-resource{path}"),
        format!("{base}/.well-known/oauth-protected-resource"),
    ]
    .iter()
    .find_map(|url| call(agent.get(url), "protected resource metadata", None).ok())
    .context("the MCP server publishes no OAuth protected resource metadata")?;
    let issuer = resource
        .pointer("/authorization_servers/0")
        .and_then(Value::as_str)
        .unwrap_or(&base)
        .trim_end_matches('/')
        .to_owned();
    if !config::secure_url(&issuer) {
        bail!("the authorization server is not https");
    }
    let metadata = [
        format!("{issuer}/.well-known/oauth-authorization-server"),
        format!("{issuer}/.well-known/openid-configuration"),
    ]
    .iter()
    .find_map(|url| call(agent.get(url), "authorization server metadata", None).ok())
    .context("the authorization server publishes no metadata")?;
    let text = |key: &str| -> Result<String> {
        let value = metadata
            .get(key)
            .and_then(Value::as_str)
            .with_context(|| format!("authorization server metadata has no {key}"))?;
        if !config::secure_url(value) {
            bail!("{key} is not https");
        }
        Ok(value.to_owned())
    };
    let scope = resource
        .get("scopes_supported")
        .or_else(|| metadata.get("scopes_supported"))
        .and_then(Value::as_array)
        .map(|s| {
            s.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|s| !s.is_empty());
    Ok(Endpoints {
        authorization: text("authorization_endpoint")?,
        token: text("token_endpoint")?,
        registration: text("registration_endpoint")?,
        scope,
    })
}

fn random_token(bytes: usize) -> Result<String> {
    let mut buffer = vec![0u8; bytes];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buffer))
        .context("reading /dev/urandom")?;
    Ok(base64url(&buffer))
}

pub fn base64url(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len() * 4 / 3 + 3);
    for chunk in data.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..=chunk.len() {
            out.push(ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
    }
    out
}

pub fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(&value[i + 1..i + 3], 16) {
                    out.push(b);
                    i += 2;
                } else {
                    out.push(b'%');
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(url: &str, key: &str) -> Option<String> {
    url.split_once('?')?
        .1
        .split('#')
        .next()?
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| decode(v))
}

pub fn save(path: &Path, credentials: &Credentials) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = path.parent().context("credentials path has no parent")?;
    std::fs::create_dir_all(dir)?;
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("writing {}", tmp.display()))?;
        std::io::Write::write_all(&mut file, &serde_json::to_vec_pretty(credentials)?)?;
    }
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Credentials> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!(
            "reading {} (sign in first: scripts/signals-service.sh login nightwatch)",
            path.display()
        )
    })?;
    serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
}

fn open_browser(url: &str) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener)
        .arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Interactive: register, have a person approve in a browser, store the refresh token.
pub fn login(mcp_url: &str, path: &Path) -> Result<()> {
    if !config::secure_url(mcp_url) {
        bail!("the MCP URL must be https");
    }
    let endpoints = discover(mcp_url)?;
    let listener = tiny_http::Server::http("127.0.0.1:0").map_err(|e| anyhow::anyhow!("{e}"))?;
    let port = listener
        .server_addr()
        .to_ip()
        .map(|a| a.port())
        .context("no local port")?;
    let redirect = format!("http://127.0.0.1:{port}/callback");
    let mut registration = serde_json::json!({
        "client_name": "harness-signals",
        "redirect_uris": [redirect],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    if let Some(scope) = &endpoints.scope {
        registration["scope"] = Value::String(scope.clone());
    }
    let client = call(
        agent().post(&endpoints.registration),
        "client registration",
        Some(RequestBody::Json(registration)),
    )?;
    let client_id = client
        .get("client_id")
        .and_then(Value::as_str)
        .context("registration returned no client_id")?
        .to_owned();
    let client_secret = client
        .get("client_secret")
        .and_then(Value::as_str)
        .map(str::to_owned);

    let verifier = random_token(48)?;
    let challenge = base64url(&Sha256::digest(verifier.as_bytes()));
    let state = random_token(24)?;
    let mut url = format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&code_challenge={challenge}&code_challenge_method=S256&state={state}&resource={}",
        endpoints.authorization,
        encode(&client_id),
        encode(&redirect),
        encode(mcp_url),
    );
    if let Some(scope) = &endpoints.scope {
        url.push_str(&format!("&scope={}", encode(scope)));
    }
    eprintln!("Approve harness-signals in your browser. If it does not open, visit:\n\n  {url}\n");
    eprintln!("On a machine without a browser, approve it elsewhere, then paste the address the browser was sent to (it starts with {redirect}) here and press Enter.");
    open_browser(&url);

    let (send, receive) = mpsc::channel::<String>();
    {
        let send = send.clone();
        std::thread::spawn(move || {
            for request in listener.incoming_requests() {
                let target = format!("http://127.0.0.1{}", request.url());
                let done = query_param(&target, "code").is_some()
                    || query_param(&target, "error").is_some();
                let page = if done {
                    "harness-signals is signed in. You can close this tab."
                } else {
                    "Waiting for the sign-in."
                };
                let _ = request.respond(
                    tiny_http::Response::from_string(page).with_header(
                        "Content-Type: text/plain; charset=utf-8"
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    ),
                );
                if done {
                    let _ = send.send(target);
                    break;
                }
            }
        });
    }
    std::thread::spawn(move || {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_ok() && line.contains("code=") {
            let _ = send.send(line.trim().to_owned());
        }
    });
    let answer = receive
        .recv_timeout(LOGIN_WAIT)
        .context("no sign-in within 10 minutes")?;
    if let Some(error) = query_param(&answer, "error") {
        bail!(
            "the sign-in was not approved ({})",
            error.chars().take(64).collect::<String>()
        );
    }
    if query_param(&answer, "state").as_deref() != Some(state.as_str()) {
        bail!("the sign-in answer does not match this request (state mismatch)");
    }
    let code = query_param(&answer, "code").context("the sign-in answer has no code")?;
    let mut form = vec![
        ("grant_type", "authorization_code".to_owned()),
        ("code", code),
        ("redirect_uri", redirect),
        ("client_id", client_id.clone()),
        ("code_verifier", verifier),
        ("resource", mcp_url.to_owned()),
    ];
    if let Some(secret) = &client_secret {
        form.push(("client_secret", secret.clone()));
    }
    let tokens = call(
        agent().post(&endpoints.token),
        "token exchange",
        Some(RequestBody::Form(form)),
    )?;
    let credentials = credentials_from(
        &tokens,
        Credentials {
            mcp_url: mcp_url.to_owned(),
            token_endpoint: endpoints.token,
            client_id,
            client_secret,
            refresh_token: String::new(),
            access_token: None,
            expires_at: None,
        },
    )?;
    save(path, &credentials)?;
    eprintln!(
        "Signed in. Credentials saved to {} (owner-only).",
        path.display()
    );
    Ok(())
}

fn credentials_from(tokens: &Value, mut credentials: Credentials) -> Result<Credentials> {
    let access = tokens
        .get("access_token")
        .and_then(Value::as_str)
        .context("the token response has no access_token")?;
    credentials.access_token = Some(access.to_owned());
    if let Some(refresh) = tokens.get("refresh_token").and_then(Value::as_str) {
        credentials.refresh_token = refresh.to_owned();
    }
    if credentials.refresh_token.is_empty() {
        bail!("the server issued no refresh token, so the service could not stay signed in");
    }
    credentials.expires_at = tokens
        .get("expires_in")
        .and_then(Value::as_i64)
        .map(|seconds| chrono::Utc::now().timestamp() + seconds);
    Ok(credentials)
}

/// A usable access token, refreshing (and saving the rotated refresh token) when needed.
pub fn access_token(path: &Path, force_refresh: bool) -> Result<(String, String)> {
    let credentials = load(path)?;
    let fresh = credentials.access_token.is_some()
        && credentials
            .expires_at
            .is_none_or(|at| at - 120 > chrono::Utc::now().timestamp());
    if fresh && !force_refresh {
        return Ok((
            credentials.access_token.unwrap_or_default(),
            credentials.mcp_url,
        ));
    }
    let mut form = vec![
        ("grant_type", "refresh_token".to_owned()),
        ("refresh_token", credentials.refresh_token.clone()),
        ("client_id", credentials.client_id.clone()),
        ("resource", credentials.mcp_url.clone()),
    ];
    if let Some(secret) = &credentials.client_secret {
        form.push(("client_secret", secret.clone()));
    }
    let tokens = call(
        agent().post(&credentials.token_endpoint),
        "token refresh (sign in again with: scripts/signals-service.sh login nightwatch)",
        Some(RequestBody::Form(form)),
    )?;
    let refreshed = credentials_from(&tokens, credentials)?;
    save(path, &refreshed)?;
    Ok((
        refreshed.access_token.clone().unwrap_or_default(),
        refreshed.mcp_url,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_encoding_and_callback_parsing() {
        // RFC 7636 appendix B.
        let challenge = base64url(&Sha256::digest(
            b"dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk",
        ));
        assert_eq!(challenge, "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(
            encode("http://127.0.0.1:1/cb"),
            "http%3A%2F%2F127.0.0.1%3A1%2Fcb"
        );
        let answer = "http://127.0.0.1:5/callback?code=a%2Bb&state=xyz";
        assert_eq!(query_param(answer, "code").as_deref(), Some("a+b"));
        assert_eq!(query_param(answer, "state").as_deref(), Some("xyz"));
        assert_eq!(query_param(answer, "nope"), None);
        assert_eq!(
            origin("https://nightwatch.laravel.com/mcp").unwrap(),
            (
                "https://nightwatch.laravel.com".to_owned(),
                "/mcp".to_owned()
            )
        );
    }

    #[test]
    fn credentials_are_owner_only_and_need_a_refresh_token() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("signals/nightwatch.oauth");
        let base = Credentials {
            mcp_url: "https://x/mcp".into(),
            token_endpoint: "https://x/token".into(),
            client_id: "c".into(),
            client_secret: None,
            refresh_token: String::new(),
            access_token: None,
            expires_at: None,
        };
        assert!(credentials_from(&serde_json::json!({"access_token": "a"}), base.clone()).is_err());
        let saved = credentials_from(
            &serde_json::json!({"access_token": "a", "refresh_token": "r", "expires_in": 3600}),
            base,
        )
        .unwrap();
        save(&path, &saved).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let (token, url) = access_token(&path, false).unwrap();
        assert_eq!((token.as_str(), url.as_str()), ("a", "https://x/mcp"));
    }
}
