//! A minimal MCP client over Streamable HTTP: initialize, then JSON-RPC
//! requests whose answers come back as JSON or as a server-sent event stream.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::io::Read;
use std::time::Duration;

const PROTOCOL: &str = "2025-06-18";
const RESPONSE_LIMIT: u64 = 16 * 1024 * 1024;

/// The server refused the access token; the caller refreshes and retries once.
#[derive(Debug)]
pub struct Unauthorized;

impl std::fmt::Display for Unauthorized {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the MCP server refused the access token")
    }
}

impl std::error::Error for Unauthorized {}

pub struct Client {
    url: String,
    token: String,
    agent: ureq::Agent,
    session: Option<String>,
    protocol: String,
    next_id: u64,
}

impl Client {
    pub fn connect(url: &str, token: &str) -> Result<Self> {
        let mut client = Self {
            url: url.to_owned(),
            token: token.to_owned(),
            agent: ureq::AgentBuilder::new()
                .timeout(Duration::from_secs(60))
                .redirects(0)
                .build(),
            session: None,
            protocol: PROTOCOL.to_owned(),
            next_id: 1,
        };
        let init = client.request(
            "initialize",
            json!({"protocolVersion": PROTOCOL, "capabilities": {},
                   "clientInfo": {"name": "harness-signals", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        if let Some(version) = init.get("protocolVersion").and_then(Value::as_str) {
            client.protocol = version.to_owned();
        }
        client.send(
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            None,
        )?;
        Ok(client)
    }

    pub fn tools(&mut self) -> Result<Vec<Value>> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = cursor
                .as_ref()
                .map_or_else(|| json!({}), |c| json!({"cursor": c}));
            let page = self.request("tools/list", params)?;
            tools.extend(
                page.get("tools")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    /// A tool's result: its structured content when present, else its text
    /// content parsed as JSON when it is JSON, else the text itself.
    pub fn call(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let result = self.request("tools/call", json!({"name": name, "arguments": arguments}))?;
        if result.get("isError").and_then(Value::as_bool) == Some(true) {
            let text = text_of(&result);
            bail!(
                "tool {name} failed: {}",
                text.chars().take(200).collect::<String>()
            );
        }
        if let Some(structured) = result.get("structuredContent") {
            return Ok(structured.clone());
        }
        let text = text_of(&result);
        Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let answer = self
            .send(
                &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
                Some(id),
            )?
            .with_context(|| format!("no answer to {method}"))?;
        if let Some(error) = answer.get("error") {
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("error");
            bail!(
                "{method}: {}",
                message.chars().take(200).collect::<String>()
            );
        }
        Ok(answer.get("result").cloned().unwrap_or(Value::Null))
    }

    fn send(&mut self, body: &Value, id: Option<u64>) -> Result<Option<Value>> {
        let mut request = self
            .agent
            .post(&self.url)
            .set("Authorization", &format!("Bearer {}", self.token))
            .set("Accept", "application/json, text/event-stream")
            .set("MCP-Protocol-Version", &self.protocol);
        if let Some(session) = &self.session {
            request = request.set("Mcp-Session-Id", session);
        }
        let response = match request.send_json(body.clone()) {
            Ok(response) => response,
            Err(ureq::Error::Status(401, _)) => return Err(Unauthorized.into()),
            Err(ureq::Error::Status(code, _)) => bail!("the MCP server answered HTTP {code}"),
            Err(ureq::Error::Transport(error)) => {
                bail!("could not reach the MCP server ({})", error.kind())
            }
        };
        if let Some(session) = response.header("Mcp-Session-Id") {
            self.session = Some(session.to_owned());
        }
        let Some(id) = id else {
            return Ok(None);
        };
        let streamed = response
            .header("Content-Type")
            .is_some_and(|t| t.starts_with("text/event-stream"));
        let mut raw = String::new();
        response
            .into_reader()
            .take(RESPONSE_LIMIT)
            .read_to_string(&mut raw)
            .context("reading the MCP answer")?;
        if !streamed {
            return Ok(Some(
                serde_json::from_str(&raw).context("the MCP answer is not JSON")?,
            ));
        }
        Ok(sse_answer(&raw, id))
    }
}

/// The JSON-RPC message with `id` among the events of a server-sent stream.
pub fn sse_answer(raw: &str, id: u64) -> Option<Value> {
    raw.replace("\r\n", "\n")
        .split("\n\n")
        .filter_map(|event| {
            let data: Vec<&str> = event
                .lines()
                .filter_map(|line| line.strip_prefix("data:"))
                .map(|d| d.strip_prefix(' ').unwrap_or(d))
                .collect();
            (!data.is_empty()).then(|| data.join("\n"))
        })
        .filter_map(|data| serde_json::from_str::<Value>(&data).ok())
        .find(|message| message.get("id").and_then(Value::as_u64) == Some(id))
}

fn text_of(result: &Value) -> String {
    result
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_answers_are_matched_by_id() {
        let raw = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\nevent: message\r\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\r\ndata: \"result\":{\"ok\":true}}\r\n\r\n";
        assert_eq!(sse_answer(raw, 2).unwrap()["result"]["ok"], true);
        assert!(sse_answer(raw, 3).is_none());
        let result = json!({"content": [{"type": "text", "text": "{\"a\":1}"}]});
        assert_eq!(text_of(&result), "{\"a\":1}");
    }
}
