//! Blocking client for the daemon's `POST /v2/check` endpoint.
//! Runs on a worker thread (see `main.rs`), never on the GTK main loop.

use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

#[derive(Debug, Clone, Deserialize)]
pub struct Replacement {
    pub value: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Rule {
    pub id: String,
    /// `misspelling` for spelling errors, `grammar` otherwise.
    #[serde(default, rename = "issueType")]
    pub issue_type: String,
}

/// One finding. `offset` and `length` are UTF-16 code units, as the daemon reports them.
#[derive(Debug, Clone, Deserialize)]
pub struct Match {
    pub offset: usize,
    pub length: usize,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub replacements: Vec<Replacement>,
    pub rule: Rule,
}

#[derive(Debug, Deserialize)]
struct CheckResponse {
    #[serde(default)]
    matches: Vec<Match>,
}

pub struct Client {
    agent: ureq::Agent,
    url: String,
    token: String,
}

impl Client {
    pub fn new(port: u16, token: String) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(10)))
            .build();
        Self {
            agent: ureq::Agent::new_with_config(config),
            url: format!("http://127.0.0.1:{port}/v2/check"),
            token,
        }
    }

    /// Sends `text` for checking. `language` is `auto`, `fr` or `en`.
    pub fn check(&self, text: &str, language: &str) -> Result<Vec<Match>, String> {
        let body = json!({ "text": text, "language": language }).to_string();
        let mut request = self
            .agent
            .post(&self.url)
            .header("Content-Type", "application/json");
        if !self.token.is_empty() {
            request = request.header("Authorization", &format!("Bearer {}", self.token));
        }
        let mut response = request
            .send(body)
            .map_err(|e| format!("daemon unreachable: {e}"))?;
        let raw = response
            .body_mut()
            .read_to_string()
            .map_err(|e| format!("bad response body: {e}"))?;
        let parsed: CheckResponse =
            serde_json::from_str(&raw).map_err(|e| format!("bad response JSON: {e}"))?;
        Ok(parsed.matches)
    }
}
