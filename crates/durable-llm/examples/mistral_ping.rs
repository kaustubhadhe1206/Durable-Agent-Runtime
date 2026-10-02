//! Sends one prompt to Mistral and prints the reply, token counts, latency, and cost.
//!
//! cargo run -p durable-llm --example mistral_ping
//! cargo run -p durable-llm --example mistral_ping -- "Your own question here"

use durable_llm::{LlmClient, LlmRequest, MistralClient, Pricing};

// [Phase 3]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv(); // load MISTRAL_API_KEY from .env if present

    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Explain in one sentence what a database journal is.".to_string());

    let client = MistralClient::from_env()?;
    let pricing = Pricing::bundled()?;

    println!("model:   {}", client.model());
    println!("prompt:  {prompt}\n");

    let resp = client
        .complete(&LlmRequest::new(prompt).with_max_tokens(200))
        .await?;
    let cost = pricing.cost_usd(client.model(), resp.tokens_in, resp.tokens_out)?;

    println!("reply:   {}\n", resp.text.trim());
    println!("tokens:  {} in / {} out", resp.tokens_in, resp.tokens_out);
    println!("latency: {} ms", resp.latency_ms);
    println!("cost:    ${cost:.8} (at paid-tier prices; the free tier charges $0)");
    Ok(())
}
