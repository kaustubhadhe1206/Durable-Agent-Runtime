//! LLM layer: an `LlmClient` trait, a Mistral implementation, and token-based pricing.

pub mod client;
pub mod mistral;
pub mod pricing;

pub use client::{LlmClient, LlmRequest, LlmResponse};
pub use mistral::{MistralClient, DEFAULT_MODEL};
pub use pricing::{ModelPrice, Pricing, PricingError};

/// Errors from LLM clients.
#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("MISTRAL_API_KEY is not set (add it to .env)")]
    MissingApiKey,
    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("LLM API returned status {status}: {body}")]
    Api { status: u16, body: String },
    #[error("invalid LLM response: {0}")]
    InvalidResponse(String),
}
