//! Client for the Mistral chat completions API (https://docs.mistral.ai/api/).

use std::time::{Duration, Instant};

use reqwest::StatusCode;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::client::{LlmClient, LlmRequest, LlmResponse};
use crate::LlmError;

// [Phase 3]
/// Open-weight Ministral 8B: available on the free tier with a generous rate limit.
pub const DEFAULT_MODEL: &str = "ministral-8b-latest";
// [Phase 3]
pub const DEFAULT_BASE_URL: &str = "https://api.mistral.ai";
// [Phase 3]
const DEFAULT_MAX_RETRIES: u32 = 5;
// [Phase 3]
const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
// [Phase 3]
const MAX_BACKOFF: Duration = Duration::from_secs(20);

// [Phase 3]
pub struct MistralClient {
    http: reqwest::Client,
    api_key: String,
    model: String,
    base_url: String,
    max_retries: u32,
}

// Manual Debug so the API key never ends up in logs or panic messages.
// [Phase 3]
impl std::fmt::Debug for MistralClient {
    // [Phase 3]
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MistralClient")
            .field("api_key", &"<redacted>")
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

// [Phase 3]
impl MistralClient {
    // [Phase 3]
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Result<Self, LlmError> {
        let api_key = api_key.into();
        if api_key.trim().is_empty() {
            return Err(LlmError::MissingApiKey);
        }
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()?;
        Ok(Self {
            http,
            api_key,
            model: model.into(),
            base_url: DEFAULT_BASE_URL.to_string(),
            max_retries: DEFAULT_MAX_RETRIES,
        })
    }

    // [Phase 3]
    /// Reads `MISTRAL_API_KEY` (required) and `MISTRAL_MODEL` (optional) from the environment.
    /// Apps load `.env` into the environment before calling this.
    pub fn from_env() -> Result<Self, LlmError> {
        let key = std::env::var("MISTRAL_API_KEY").map_err(|_| LlmError::MissingApiKey)?;
        let model = std::env::var("MISTRAL_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
        Self::new(key, model)
    }

    // [Phase 3]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    // [Phase 3]
    /// How many times to retry after a rate limit (429) or server error (5xx).
    pub fn with_max_retries(mut self, max_retries: u32) -> Self {
        self.max_retries = max_retries;
        self
    }

    // [Phase 3]
    fn request_body(&self, req: &LlmRequest) -> Value {
        let mut messages = Vec::new();
        if let Some(system) = &req.system {
            messages.push(json!({ "role": "system", "content": system }));
        }
        messages.push(json!({ "role": "user", "content": req.prompt }));

        let mut body = json!({
            "model": self.model,
            "messages": messages,
            "max_tokens": req.max_tokens,
            "temperature": req.temperature,
        });
        if let Some(seed) = req.random_seed {
            body["random_seed"] = json!(seed);
        }
        body
    }

    // [Phase 3]
    async fn complete_inner(&self, req: &LlmRequest) -> Result<LlmResponse, LlmError> {
        let url = format!(
            "{}/v1/chat/completions",
            self.base_url.trim_end_matches('/')
        );
        let body = self.request_body(req);
        let mut attempt: u32 = 0;

        loop {
            let started = Instant::now();
            let sent = self
                .http
                .post(&url)
                .bearer_auth(&self.api_key)
                .json(&body)
                .send()
                .await;

            let retry_after = match sent {
                Ok(resp) if resp.status().is_success() => {
                    let text = resp.text().await?;
                    let latency_ms =
                        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
                    return parse_response(&text, latency_ms);
                }
                Ok(resp) if is_retryable(resp.status()) => {
                    let wait = retry_after_header(&resp);
                    let status = resp.status();
                    tracing::warn!(%status, attempt, "mistral request will be retried");
                    if attempt >= self.max_retries {
                        return Err(LlmError::Api {
                            status: status.as_u16(),
                            body: resp.text().await.unwrap_or_default(),
                        });
                    }
                    wait
                }
                Ok(resp) => {
                    return Err(LlmError::Api {
                        status: resp.status().as_u16(),
                        body: resp.text().await.unwrap_or_default(),
                    });
                }
                // Network-level failures (timeout, connection reset) are worth retrying too.
                Err(e) if (e.is_timeout() || e.is_connect()) && attempt < self.max_retries => {
                    tracing::warn!(error = %e, attempt, "mistral network error will be retried");
                    None
                }
                Err(e) => return Err(e.into()),
            };

            tokio::time::sleep(retry_after.unwrap_or_else(|| backoff(attempt))).await;
            attempt += 1;
        }
    }
}

// [Phase 3]
impl LlmClient for MistralClient {
    // [Phase 3]
    fn model(&self) -> &str {
        &self.model
    }

    // [Phase 3]
    fn complete(
        &self,
        request: &LlmRequest,
    ) -> impl std::future::Future<Output = Result<LlmResponse, LlmError>> + Send {
        self.complete_inner(request)
    }
}

// [Phase 3]
fn is_retryable(status: StatusCode) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
}

// [Phase 3]
/// Exponential backoff: 0.5s, 1s, 2s, 4s, ... capped at `MAX_BACKOFF`.
fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500)
        .saturating_mul(2u32.saturating_pow(attempt))
        .min(MAX_BACKOFF)
}

// [Phase 3]
fn retry_after_header(resp: &reqwest::Response) -> Option<Duration> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|secs| Duration::from_secs(secs).min(MAX_BACKOFF))
}

// [Phase 3]
#[derive(Deserialize)]
struct ApiResponse {
    model: String,
    choices: Vec<ApiChoice>,
    usage: ApiUsage,
}

// [Phase 3]
#[derive(Deserialize)]
struct ApiChoice {
    message: ApiMessage,
}

// [Phase 3]
#[derive(Deserialize)]
struct ApiMessage {
    /// Usually a string; reasoning models may return a list of content chunks instead.
    content: Value,
}

// [Phase 3]
#[derive(Deserialize)]
struct ApiUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

// [Phase 3]
fn parse_response(body: &str, latency_ms: u64) -> Result<LlmResponse, LlmError> {
    let api: ApiResponse = serde_json::from_str(body)
        .map_err(|e| LlmError::InvalidResponse(format!("{e}: {}", truncate(body, 200))))?;
    let choice = api
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| LlmError::InvalidResponse("response has no choices".to_string()))?;
    Ok(LlmResponse {
        text: content_text(&choice.message.content)?,
        model: api.model,
        tokens_in: api.usage.prompt_tokens,
        tokens_out: api.usage.completion_tokens,
        latency_ms,
    })
}

// [Phase 3]
fn content_text(content: &Value) -> Result<String, LlmError> {
    match content {
        Value::String(s) => Ok(s.clone()),
        Value::Array(chunks) => Ok(chunks
            .iter()
            .filter(|c| c["type"] == "text")
            .filter_map(|c| c["text"].as_str())
            .collect()),
        other => Err(LlmError::InvalidResponse(format!(
            "unexpected content: {}",
            truncate(&other.to_string(), 200)
        ))),
    }
}

// [Phase 3]
fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

// [Phase 3]
#[cfg(test)]
mod tests {
    use super::*;

    /// A real response body captured from the Mistral API (only the id shortened).
    const CAPTURED: &str = r#"{"id":"ef08","created":1790959115,"model":"ministral-8b-2512",
        "usage":{"prompt_tokens":8,"total_tokens":10,"completion_tokens":2},
        "object":"chat.completion","choices":[{"index":0,"finish_reason":"stop",
        "message":{"role":"assistant","tool_calls":null,"content":"ok"}}]}"#;

    #[test]
    fn parses_mistral_response() {
        let r = parse_response(CAPTURED, 42).unwrap();
        assert_eq!(r.text, "ok");
        assert_eq!(r.model, "ministral-8b-2512");
        assert_eq!((r.tokens_in, r.tokens_out, r.latency_ms), (8, 2, 42));
    }

    #[test]
    fn parses_chunked_content() {
        let content = json!([{"type": "thinking", "thinking": []}, {"type": "text", "text": "hi"}]);
        assert_eq!(content_text(&content).unwrap(), "hi");
    }

    #[test]
    fn rejects_malformed_response() {
        assert!(matches!(
            parse_response("not json", 0),
            Err(LlmError::InvalidResponse(_))
        ));
        assert!(matches!(
            parse_response(
                r#"{"model":"m","choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#,
                0
            ),
            Err(LlmError::InvalidResponse(_))
        ));
    }

    #[test]
    fn request_body_has_expected_shape() {
        let c = MistralClient::new("k", "ministral-8b-latest").unwrap();
        let req = LlmRequest::new("hello")
            .with_system("be brief")
            .with_max_tokens(10)
            .with_seed(7);
        let body = c.request_body(&req);
        assert_eq!(body["model"], "ministral-8b-latest");
        assert_eq!(body["max_tokens"], 10);
        assert_eq!(body["random_seed"], 7);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["content"], "hello");
        assert!(c
            .request_body(&LlmRequest::new("x"))
            .get("random_seed")
            .is_none());
    }

    #[test]
    fn empty_api_key_is_rejected_and_key_is_never_printed() {
        assert!(matches!(
            MistralClient::new("  ", "m"),
            Err(LlmError::MissingApiKey)
        ));
        let c = MistralClient::new("secret-key-123", "m").unwrap();
        assert!(!format!("{c:?}").contains("secret-key-123"));
    }

    #[test]
    fn backoff_grows_and_is_capped() {
        assert_eq!(backoff(0), Duration::from_millis(500));
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(3), Duration::from_secs(4));
        assert_eq!(backoff(30), MAX_BACKOFF);
    }
}
