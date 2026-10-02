//! The provider-neutral LLM interface.

use std::future::Future;
use std::time::Duration;

use serde::{Deserialize, Serialize};

// [Phase 3]
/// One prompt to send to an LLM. It is `Serialize` so it can be used directly as a durable
/// step's input: the journal hashes it, so changing the prompt is detected on replay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmRequest {
    pub system: Option<String>,
    pub prompt: String,
    pub max_tokens: u32,
    pub temperature: f32,
    /// Asks the provider for reproducible sampling, where supported.
    pub random_seed: Option<u64>,
}

// [Phase 3]
impl LlmRequest {
    // [Phase 3]
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            system: None,
            prompt: prompt.into(),
            max_tokens: 512,
            temperature: 0.3,
            random_seed: None,
        }
    }

    // [Phase 3]
    pub fn with_system(mut self, system: impl Into<String>) -> Self {
        self.system = Some(system.into());
        self
    }

    // [Phase 3]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = max_tokens;
        self
    }

    // [Phase 3]
    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = temperature;
        self
    }

    // [Phase 3]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.random_seed = Some(seed);
        self
    }
}

// [Phase 3]
/// An LLM reply with the usage numbers needed for metrics and cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmResponse {
    pub text: String,
    /// The model name the provider reports (may be a dated version of the requested alias).
    pub model: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Wall-clock time of the successful HTTP request, in milliseconds.
    pub latency_ms: u64,
}

// [Phase 3]
impl LlmResponse {
    // [Phase 3]
    pub fn latency(&self) -> Duration {
        Duration::from_millis(self.latency_ms)
    }
}

// [Phase 3]
/// Anything that can answer an `LlmRequest`.
///
/// `complete` returns `impl Future + Send` (rather than using `async fn`) so callers can
/// rely on the future being `Send`, i.e. safe to move between tokio worker threads.
pub trait LlmClient: Send + Sync {
    // [Phase 3]
    /// The model this client sends requests to.
    fn model(&self) -> &str;

    // [Phase 3]
    fn complete(
        &self,
        request: &LlmRequest,
    ) -> impl Future<Output = Result<LlmResponse, crate::LlmError>> + Send;
}
