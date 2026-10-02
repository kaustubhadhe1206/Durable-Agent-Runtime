//! Live tests against the real Mistral API.
//!
//! They read `MISTRAL_API_KEY` from the environment or the repo's `.env` file. Without a key
//! (e.g. on CI with no secret configured) each test prints a notice and passes.

use std::sync::atomic::{AtomicUsize, Ordering};

use durable_core::{DurableContext, TokenUsage};
use durable_llm::{LlmClient, LlmError, LlmRequest, LlmResponse, MistralClient, Pricing};

// [Phase 3]
/// Loads `.env` from the workspace root and builds a client, or returns `None` to skip.
fn client_or_skip(test: &str) -> Option<MistralClient> {
    let env_path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env");
    let _ = dotenvy::from_path(env_path); // a missing .env is fine
    match MistralClient::from_env() {
        Ok(c) => Some(c),
        Err(LlmError::MissingApiKey) => {
            eprintln!("SKIPPED {test}: MISTRAL_API_KEY not set");
            None
        }
        Err(e) => panic!("could not build client: {e}"),
    }
}

// [Phase 3]
#[tokio::test]
async fn real_call_returns_text_tokens_latency_and_cost() -> anyhow::Result<()> {
    let Some(client) = client_or_skip("real_call") else {
        return Ok(());
    };
    let req = LlmRequest::new("Name one planet in our solar system. Answer in one word.")
        .with_max_tokens(10)
        .with_temperature(0.0);

    let resp = client.complete(&req).await?;
    println!("reply={:?} model={} {:?}", resp.text, resp.model, resp);

    assert!(!resp.text.trim().is_empty(), "reply should not be empty");
    assert!(resp.tokens_in > 0 && resp.tokens_out > 0);
    assert!(resp.latency_ms > 0);
    assert!(
        resp.model.starts_with("ministral"),
        "model was {}",
        resp.model
    );

    // Cost comes from the token counts and pricing.toml, never a hardcoded figure.
    let pricing = Pricing::bundled()?;
    let cost = pricing.cost_usd(client.model(), resp.tokens_in, resp.tokens_out)?;
    let expected = (resp.tokens_in + resp.tokens_out) as f64 * 0.15 / 1_000_000.0;
    assert!((cost - expected).abs() < 1e-15, "cost {cost} != {expected}");
    Ok(())
}

// [Phase 3]
#[tokio::test]
async fn bad_api_key_is_rejected_without_retrying() {
    if client_or_skip("bad_api_key").is_none() {
        return; // no network access expected without a key
    }
    let client = MistralClient::new("definitely-not-a-valid-key", "ministral-8b-latest")
        .expect("non-empty key");
    let err = client
        .complete(&LlmRequest::new("hi").with_max_tokens(3))
        .await
        .unwrap_err();
    assert!(
        matches!(err, LlmError::Api { status: 401, .. }),
        "got {err:?}"
    );
}

// [Phase 3]
/// Phase 2 + Phase 3 together: a durable step backed by a real Mistral call is executed
/// once, and re-running the same run replays it without contacting Mistral again.
#[tokio::test]
async fn durable_step_with_real_llm_is_not_repeated_on_rerun() -> anyhow::Result<()> {
    let Some(client) = client_or_skip("durable_step_with_real_llm") else {
        return Ok(());
    };
    let dir = tempfile::tempdir()?;
    let calls = AtomicUsize::new(0);
    let req = LlmRequest::new("Say hello in French. One word.").with_max_tokens(10);

    // [Phase 3]
    async fn ask(
        ctx: &DurableContext,
        client: &MistralClient,
        req: &LlmRequest,
        calls: &AtomicUsize,
    ) -> anyhow::Result<LlmResponse> {
        Ok(ctx
            .llm_step("greet", req, || async {
                calls.fetch_add(1, Ordering::SeqCst);
                let resp = client.complete(req).await?;
                let usage = TokenUsage {
                    tokens_in: resp.tokens_in,
                    tokens_out: resp.tokens_out,
                };
                Ok::<_, LlmError>((resp, usage))
            })
            .await?)
    }

    let first = {
        let ctx = DurableContext::open_in(dir.path(), "live").await?;
        ask(&ctx, &client, &req, &calls).await?
    };
    let ctx = DurableContext::open_in(dir.path(), "live").await?;
    let second = ask(&ctx, &client, &req, &calls).await?;

    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "Mistral must be called once"
    );
    assert_eq!(first, second, "replay returns the exact original reply");
    let stats = ctx.stats()?;
    assert_eq!(stats.replayed, 1);
    assert_eq!(stats.tokens_replayed.tokens_out, first.tokens_out);
    Ok(())
}
