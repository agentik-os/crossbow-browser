//! Provider discovery, explicitly requested local-server diagnostics and bounded
//! prompt-only conversations, plus the legacy Crossbow cloud client. Discovery
//! never activates a provider. Chat sends only the caller's explicit text turns;
//! neither a local endpoint nor an installed CLI proves offline inference.

pub mod chat;
pub mod local;
pub mod providers;
mod streaming;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum AiError {
    #[error("network: {0}")]
    Network(String),
    #[error("{code}: {message}")]
    Api { status: u16, code: String, message: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: String,
}

impl Message {
    pub fn system(s: impl Into<String>) -> Message {
        Message { role: Role::System, content: s.into() }
    }
    pub fn user(s: impl Into<String>) -> Message {
        Message { role: Role::User, content: s.into() }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CompleteRequest {
    pub messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct AiHealth {
    pub ok: bool,
    pub models: Vec<String>,
    #[serde(default)]
    pub gateway: Option<String>,
}

/// What a page-aware feature (summaries, "ask this page") would send.
#[derive(Clone, Debug, Default, Serialize)]
pub struct PageContext {
    pub url: String,
    pub title: String,
    pub selection: Option<String>,
    /// Visible text, truncated by the caller.
    pub text: Option<String>,
}

impl PageContext {
    pub fn to_message(&self) -> Message {
        let mut s = format!("Page: {}\nURL: {}\n", self.title, self.url);
        if let Some(sel) = &self.selection {
            s.push_str(&format!("Selected text:\n{sel}\n"));
        }
        if let Some(t) = &self.text {
            s.push_str(&format!("Page text:\n{t}\n"));
        }
        Message::user(s)
    }
}

pub struct AiClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl AiClient {
    pub fn new(base_url: &str, session_token: &str) -> AiClient {
        AiClient {
            base_url: base_url.trim_end_matches('/').to_string(),
            token: session_token.to_string(),
            http: reqwest::Client::builder().user_agent(concat!("Crossbow/", env!("CARGO_PKG_VERSION"))).build().expect("http client"),
        }
    }

    pub async fn health(&self) -> Result<AiHealth, AiError> {
        let res = self.http.get(format!("{}/v1/ai/health", self.base_url)).bearer_auth(&self.token).send().await.map_err(|e| AiError::Network(e.to_string()))?;
        let res = check(res).await?;
        res.json().await.map_err(|e| AiError::Network(e.to_string()))
    }

    /// Stream a completion; `on_text` receives each text chunk as it arrives.
    /// Returns the whole text.
    pub async fn complete(&self, req: &CompleteRequest, mut on_text: impl FnMut(&str)) -> Result<String, AiError> {
        let res = self
            .http
            .post(format!("{}/v1/ai/complete", self.base_url))
            .bearer_auth(&self.token)
            .json(req)
            .send()
            .await
            .map_err(|e| AiError::Network(e.to_string()))?;
        let res = check(res).await?;
        let mut out = String::new();
        let mut pending = Vec::new();
        let mut stream = res.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| AiError::Network(e.to_string()))?;
            pending.extend_from_slice(&chunk);
            // Only emit complete UTF-8 sequences.
            let valid = match std::str::from_utf8(&pending) {
                Ok(_) => pending.len(),
                Err(e) => e.valid_up_to(),
            };
            if valid > 0 {
                let text = std::str::from_utf8(&pending[..valid]).unwrap_or_default().to_string();
                on_text(&text);
                out.push_str(&text);
                pending.drain(..valid);
            }
        }
        Ok(out)
    }
}

async fn check(res: reqwest::Response) -> Result<reqwest::Response, AiError> {
    if res.status().is_success() {
        return Ok(res);
    }
    let status = res.status().as_u16();
    let body: serde_json::Value = res.json().await.unwrap_or_default();
    Err(AiError::Api {
        status,
        code: body["error"]["code"].as_str().unwrap_or("http").to_string(),
        message: body["error"]["message"].as_str().unwrap_or("request failed").to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_shape_matches_worker() {
        let r = CompleteRequest { messages: vec![Message::system("be brief"), Message::user("hi")], model: None };
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"messages":[{"role":"system","content":"be brief"},{"role":"user","content":"hi"}]}"#);
    }

    #[test]
    fn page_context_message() {
        let m = PageContext { url: "https://example.com/".into(), title: "Example".into(), selection: Some("x".into()), text: None }.to_message();
        assert_eq!(m.role, Role::User);
        assert!(m.content.contains("URL: https://example.com/"));
    }
}
