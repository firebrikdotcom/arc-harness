//! End to end: a fake Sentry over HTTP, a signed Nightwatch delivery to the
//! real receiver, and the files each leaves in the harness DB.

use crate::providers::{nightwatch, nightwatch_poll, sentry::fixtures};
use crate::{config, once, server};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use std::path::Path;
use std::sync::{Arc, Mutex, RwLock};

const TOKEN: &str = "sntrys_test_token_value";

/// `(path, Authorization header)` of every request the fake Sentry received.
type Requests = Arc<Mutex<Vec<(String, String)>>>;

/// Serves the issue list and latest event; records each request's path and auth header.
fn fake_sentry() -> (String, Requests) {
    let http = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", http.server_addr());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        for request in http.incoming_requests() {
            let auth = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.as_str().to_owned())
                .unwrap_or_default();
            let url = request.url().to_owned();
            log.lock().unwrap().push((url.clone(), auth.clone()));
            let body = if auth != format!("Bearer {TOKEN}") {
                None
            } else if url.starts_with("/api/0/projects/acme/api/issues/") {
                Some(fixtures::ISSUES)
            } else if url == "/api/0/organizations/acme/issues/101/events/latest/" {
                Some(fixtures::EVENT)
            } else {
                Some("{}")
            };
            let response = match body {
                Some(body) => tiny_http::Response::from_string(body).with_status_code(200),
                None => tiny_http::Response::from_string("denied").with_status_code(401),
            };
            let _ = request.respond(response);
        }
    });
    (base, seen)
}

fn write_config(dir: &Path, body: Value) -> std::path::PathBuf {
    let path = dir.join("signals.json");
    std::fs::write(&path, body.to_string()).unwrap();
    path
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn sentry_issues_become_card_heat_without_storing_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let (db, project) = crate::store::testing::db_with_target(dir.path());
    let (base, seen) = fake_sentry();
    let token_file = dir.path().join("sentry.token");
    std::fs::write(&token_file, format!("{TOKEN}\n")).unwrap();
    let config_path = write_config(
        dir.path(),
        serde_json::json!({"projects": [{
            "name": "api", "target": "demo-1", "environment": "production",
            "path_map": [{"from": "/var/www/html/", "to": ""}],
            "providers": [{"type": "sentry", "base_url": base, "org": "acme", "project": "api",
                           "token_file": token_file}]
        }]}),
    );

    let summaries = once(&config_path, &db).unwrap();
    assert_eq!(summaries[0]["totals"]["open"], 2, "{summaries}");

    let signals = db.join("targets/demo-1/signals");
    let heat = read(&signals.join("heat.json"));
    // Issue 101 was raised in src/models/User.php, called from src/app/.
    assert_eq!(heat["cards"]["models"]["heat"], 1.0);
    assert_eq!(heat["cards"]["models"]["top"][0]["share"], "raised");
    assert_eq!(heat["cards"]["app"]["top"][0]["share"], "caller");
    // Issue 102 has no frames: it is listed as unmapped, not dropped.
    assert_eq!(heat["unmapped"][0]["title"], "Deprecated call");
    assert_eq!(heat["sources"]["sentry:acme/api"]["ok"], true);

    let requests = seen.lock().unwrap().clone();
    let list = &requests[0].0;
    assert!(
        list.contains("statsPeriod=24h") && list.contains("environment=production"),
        "{list}"
    );
    assert!(requests
        .iter()
        .all(|(_, auth)| auth == &format!("Bearer {TOKEN}")));
    let fetched = requests.len();

    // A second poll reuses the stored frames instead of fetching the event again.
    once(&config_path, &db).unwrap();
    assert_eq!(seen.lock().unwrap().len(), fetched + 1);

    for entry in std::fs::read_dir(&signals).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains(TOKEN), "the token reached the DB");
        assert!(
            !text.contains("secret details"),
            "an exception message beyond the title was stored"
        );
    }
    assert!(!project.join("signals").exists());

    // A rejected token is recorded as a failed poll; earlier issues stay.
    std::fs::write(&token_file, "wrong").unwrap();
    once(&config_path, &db).unwrap();
    let heat = read(&signals.join("heat.json"));
    assert_eq!(heat["sources"]["sentry:acme/api"]["ok"], false);
    assert!(heat["sources"]["sentry:acme/api"]["error"]
        .as_str()
        .unwrap()
        .contains("HTTP 401"));
    assert_eq!(heat["totals"]["open"], 2);
}

#[test]
fn signed_nightwatch_deliveries_update_the_heat_and_forgeries_are_refused() {
    let dir = tempfile::tempdir().unwrap();
    let (db, _) = crate::store::testing::db_with_target(dir.path());
    let secret_file = dir.path().join("nightwatch.secret");
    std::fs::write(&secret_file, "whsec").unwrap();
    let config_path = write_config(
        dir.path(),
        serde_json::json!({"projects": [{
            "name": "api", "target": "demo-1", "environment": "production",
            "path_map": [{"from": "/var/www/html/", "to": ""}],
            "providers": [{"type": "nightwatch", "secret_file": secret_file}]
        }]}),
    );
    let loaded = config::load(&config_path).unwrap();
    let http = server::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", http.server_addr());
    let shared = Arc::new(RwLock::new(loaded));
    {
        let db = db.clone();
        let (poll_now, _requested) = std::sync::mpsc::channel();
        std::thread::spawn(move || server::run(http, db, shared, poll_now));
    }

    let body = nightwatch::fixtures::delivery("issue.opened", "production");
    let mut mac = Hmac::<Sha256>::new_from_slice(b"whsec").unwrap();
    mac.update(body.as_bytes());
    let signature = hex::encode(mac.finalize().into_bytes());
    let post = |path: &str, signature: &str| match ureq::post(&format!("{base}{path}"))
        .set("Nightwatch-Signature", signature)
        .send_string(&body)
    {
        Ok(response) => response.status(),
        Err(ureq::Error::Status(code, _)) => code,
        Err(error) => panic!("{error}"),
    };

    assert_eq!(post("/hooks/nightwatch/api", "deadbeef"), 401);
    assert!(!db.join("targets/demo-1/signals/issues.json").exists());
    assert_eq!(post("/hooks/nightwatch/other", &signature), 404);
    assert_eq!(post("/hooks/nightwatch/api", &signature), 204);

    let heat = read(&db.join("targets/demo-1/signals/heat.json"));
    assert_eq!(heat["cards"]["app"]["heat"], 1.0);
    assert_eq!(
        heat["cards"]["app"]["top"][0]["title"],
        "ErrorException: Undefined index"
    );
    assert_eq!(heat["sources"]["nightwatch"]["issues"], 1);
    assert_eq!(
        ureq::get(&format!("{base}/healthz"))
            .call()
            .unwrap()
            .status(),
        200
    );
}

/// A Nightwatch-like MCP server and token endpoint. Only `new-token` is accepted.
fn fake_nightwatch() -> (String, Arc<Mutex<Vec<String>>>) {
    let http = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}", http.server_addr());
    let calls = Arc::new(Mutex::new(Vec::new()));
    let log = calls.clone();
    std::thread::spawn(move || {
        for mut request in http.incoming_requests() {
            let mut body = String::new();
            std::io::Read::read_to_string(request.as_reader(), &mut body).unwrap();
            let auth = request
                .headers()
                .iter()
                .find(|h| h.field.equiv("Authorization"))
                .map(|h| h.value.as_str().to_owned())
                .unwrap_or_default();
            let reply = |status: u16, text: String, sse: bool| {
                let kind = if sse {
                    "text/event-stream"
                } else {
                    "application/json"
                };
                tiny_http::Response::from_string(text)
                    .with_status_code(status)
                    .with_header(
                        format!("Content-Type: {kind}")
                            .parse::<tiny_http::Header>()
                            .unwrap(),
                    )
                    .with_header("Mcp-Session-Id: s-1".parse::<tiny_http::Header>().unwrap())
            };
            if request.url() == "/oauth/token" {
                log.lock().unwrap().push("refresh".into());
                let ok =
                    body.contains("grant_type=refresh_token") && body.contains("refresh_token=r-1");
                let _ = request.respond(if ok {
                    reply(
                        200,
                        r#"{"access_token":"new-token","refresh_token":"r-2","expires_in":3600}"#
                            .into(),
                        false,
                    )
                } else {
                    reply(400, r#"{"error":"invalid_grant"}"#.into(), false)
                });
                continue;
            }
            if auth != "Bearer new-token" {
                let _ = request.respond(reply(401, "{}".into(), false));
                continue;
            }
            let message: Value = serde_json::from_str(&body).unwrap();
            let id = message["id"].clone();
            let method = message["method"].as_str().unwrap_or("").to_owned();
            if id.is_null() {
                let _ = request.respond(reply(202, String::new(), false));
                continue;
            }
            let result = match method.as_str() {
                "initialize" => {
                    serde_json::json!({"protocolVersion": "2025-06-18", "capabilities": {}})
                }
                "tools/call" => {
                    let name = message["params"]["name"].as_str().unwrap().to_owned();
                    let args = &message["params"]["arguments"];
                    log.lock().unwrap().push(name.clone());
                    let text = match name.as_str() {
                        "list_applications" => nightwatch_poll::fixtures::APPS.to_owned(),
                        "list_environments" => nightwatch_poll::fixtures::ENVS.to_owned(),
                        "list_issues" if args.get("cursor").is_some() => {
                            nightwatch_poll::fixtures::LIST_2.to_owned()
                        }
                        "list_issues" => nightwatch_poll::fixtures::LIST_1.to_owned(),
                        "get_issue" if args["identifier"] == "509" => {
                            nightwatch_poll::fixtures::DETAIL.to_owned()
                        }
                        "get_issue" => {
                            "## Exception Summary\n\n- File: `app/Services/Quiltt/Api.php:1`\n"
                                .to_owned()
                        }
                        _ => "unexpected".to_owned(),
                    };
                    serde_json::json!({"content": [{"type": "text", "text": text}]})
                }
                _ => serde_json::json!({}),
            };
            let answer =
                serde_json::json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string();
            // Answer initialize as an event stream, everything else as JSON.
            let _ = request.respond(if method == "initialize" {
                reply(200, format!("event: message\ndata: {answer}\n\n"), true)
            } else {
                reply(200, answer, false)
            });
        }
    });
    (base, calls)
}

#[test]
fn nightwatch_is_polled_through_mcp_with_token_refresh_and_read_only_tools() {
    let dir = tempfile::tempdir().unwrap();
    let (db, _) = crate::store::testing::db_with_target(dir.path());
    std::fs::write(
        db.join("targets/demo-1/board/board.json"),
        serde_json::json!({"nodes": [
            {"id": "billing", "owns": ["app/Services/Billing/"]},
            {"id": "quiltt", "owns": ["app/Services/Quiltt/"]}
        ]})
        .to_string(),
    )
    .unwrap();
    let (base, calls) = fake_nightwatch();
    let credentials = dir.path().join("nightwatch.oauth");
    crate::oauth::save(
        &credentials,
        &crate::oauth::Credentials {
            mcp_url: format!("{base}/mcp"),
            token_endpoint: format!("{base}/oauth/token"),
            client_id: "c-1".into(),
            client_secret: None,
            refresh_token: "r-1".into(),
            access_token: Some("old-token".into()),
            expires_at: Some(chrono::Utc::now().timestamp() + 3600),
        },
    )
    .unwrap();
    let config_path = write_config(
        dir.path(),
        serde_json::json!({"projects": [{
            "name": "api", "target": "demo-1", "environment": "production",
            "providers": [{"type": "nightwatch", "credentials_file": credentials, "application": "acme",
                           "mcp_url": format!("{base}/mcp")}]
        }]}),
    );

    let summaries = once(&config_path, &db).unwrap();
    assert_eq!(summaries[0]["totals"]["open"], 3, "{summaries}");
    let signals = db.join("targets/demo-1/signals");
    let heat = read(&signals.join("heat.json"));
    assert_eq!(heat["cards"]["billing"]["heat"], 1.0);
    assert_eq!(heat["cards"]["billing"]["top"][0]["count"], 21);
    assert_eq!(heat["cards"]["billing"]["top"][0]["users"], 18);
    assert_eq!(heat["cards"]["quiltt"]["top"][0]["share"], "caller");
    assert_eq!(
        heat["cards"]["billing"]["top"][0]["url"],
        "https://nightwatch.laravel.com/us/environments/e0000000-0000-4000-8000-000000000002/issues/509"
    );
    assert_eq!(heat["sources"]["nightwatch"]["ok"], true);

    // The refused token was refreshed once, and the rotated refresh token stored.
    let stored = std::fs::read_to_string(&credentials).unwrap();
    assert!(stored.contains("new-token") && stored.contains("r-2"));
    let called = calls.lock().unwrap().clone();
    assert_eq!(called.iter().filter(|c| *c == "refresh").count(), 1);
    assert!(called.iter().all(|c| c == "refresh"
        || [
            "list_applications",
            "list_environments",
            "list_issues",
            "get_issue"
        ]
        .contains(&c.as_str())));

    for entry in std::fs::read_dir(&signals).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        for private in [
            "new-token",
            "r-2",
            "secret message",
            "4ab61b31-2fd8",
            "ops@example.com",
            "0003a026",
        ] {
            assert!(!text.contains(private), "{private} reached the DB");
        }
    }

    // Unchanged issues are not looked up again on the next poll.
    let details = called.iter().filter(|c| *c == "get_issue").count();
    once(&config_path, &db).unwrap();
    let again = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| *c == "get_issue")
        .count();
    assert_eq!(again, details);

    // Dropping the provider drops its issues on the next poll.
    let config_path = write_config(
        dir.path(),
        serde_json::json!({"projects": [{"name": "api", "target": "demo-1",
            "providers": [{"type": "command", "argv": ["printf", "[]"]}]}]}),
    );
    once(&config_path, &db).unwrap();
    let heat = read(&signals.join("heat.json"));
    assert_eq!(heat["totals"]["open"], 0);
    assert!(heat["sources"].get("nightwatch").is_none());
}

#[test]
fn a_signed_webhook_triggers_an_immediate_poll_when_polling_is_configured() {
    let dir = tempfile::tempdir().unwrap();
    let (db, _) = crate::store::testing::db_with_target(dir.path());
    let secret_file = dir.path().join("nightwatch.secret");
    std::fs::write(&secret_file, "whsec").unwrap();
    let config_path = write_config(
        dir.path(),
        serde_json::json!({"projects": [{"name": "api", "target": "demo-1",
            "providers": [{"type": "nightwatch", "secret_file": secret_file,
                           "credentials_file": dir.path().join("nightwatch.oauth")}]}]}),
    );
    let http = server::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", http.server_addr());
    let shared = Arc::new(RwLock::new(config::load(&config_path).unwrap()));
    let (poll_now, requested) = std::sync::mpsc::channel();
    std::thread::spawn(move || server::run(http, db, shared, poll_now));
    let body = nightwatch::fixtures::delivery("issue.opened", "production");
    let mut mac = Hmac::<Sha256>::new_from_slice(b"whsec").unwrap();
    mac.update(body.as_bytes());
    let response = ureq::post(&format!("{base}/hooks/nightwatch/api"))
        .set(
            "Nightwatch-Signature",
            &hex::encode(mac.finalize().into_bytes()),
        )
        .send_string(&body)
        .unwrap();
    assert_eq!(response.status(), 202);
    assert_eq!(
        requested
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap(),
        "api"
    );
}
