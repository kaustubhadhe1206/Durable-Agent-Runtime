//! [Phase 5] `bench throughput`: steps per second with N durable runs at once, each with its
//! own SQLite journal, each step a real Mistral call. (Phase 7 extends this to 100/1,000.)

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use durable_core::{DurableContext, StepKind, StepSpec, TokenUsage};
use durable_llm::{LlmClient, LlmError, LlmRequest, MistralClient};
use durable_metrics::Metric;

use crate::env::BenchEnv;

const BENCH: &str = "throughput";

// [Phase 5]
/// One durable run of `steps` real LLM steps. Returns how many steps completed.
async fn one_run(
    client: Arc<MistralClient>,
    dir: PathBuf,
    run: usize,
    steps: u32,
) -> anyhow::Result<u32> {
    let ctx = DurableContext::open_in(&dir, &format!("run-{run}")).await?;
    for s in 0..steps {
        let req = LlmRequest::new(format!("Reply with only the number {run}{s}."))
            .with_max_tokens(5)
            .with_temperature(0.0);
        let spec = StepSpec {
            name: "step",
            kind: StepKind::Llm,
            input: &req,
            idempotency_key: None,
        };
        ctx.execute(spec, || async {
            let resp = client.complete(&req).await?;
            let usage = TokenUsage {
                tokens_in: resp.tokens_in,
                tokens_out: resp.tokens_out,
            };
            Ok::<_, LlmError>((resp.text, usage))
        })
        .await?;
    }
    Ok(steps)
}

// [Phase 5]
pub async fn run(env: &BenchEnv, levels: &[usize], steps_per_run: u32) -> anyhow::Result<()> {
    let mut metrics = Vec::new();
    for &concurrency in levels {
        println!("throughput: {concurrency} concurrent runs x {steps_per_run} steps");
        let dir = tempfile::tempdir()?;
        let started = Instant::now();
        let mut handles = Vec::with_capacity(concurrency);
        for run in 0..concurrency {
            handles.push(tokio::spawn(one_run(
                env.client.clone(),
                dir.path().to_path_buf(),
                run,
                steps_per_run,
            )));
        }
        let (mut ok_steps, mut errors) = (0u64, 0u64);
        for h in handles {
            match h.await {
                Ok(Ok(n)) => ok_steps += u64::from(n),
                Ok(Err(e)) => {
                    errors += 1;
                    tracing::warn!(error = %e, "run failed");
                }
                Err(e) => {
                    errors += 1;
                    tracing::warn!(error = %e, "run panicked");
                }
            }
        }
        let secs = started.elapsed().as_secs_f64();
        let sps = ok_steps as f64 / secs;
        env.recorder.record(
            BENCH,
            &format!("steps_per_sec_c{concurrency}"),
            sps,
            "steps/s",
        )?;
        env.recorder
            .add(BENCH, &format!("errors_c{concurrency}"), errors as i64)?;
        println!("  {ok_steps} steps in {secs:.1} s = {sps:.2} steps/s, {errors} failed runs");

        metrics.push(
            Metric::new(
                &format!("steps_per_sec_c{concurrency}"),
                sps,
                "steps/s",
                &format!("Throughput with {concurrency} concurrent runs"),
            )
            .headline(),
        );
        metrics.push(Metric::new(
            &format!("wall_s_c{concurrency}"),
            secs,
            "s",
            &format!("Wall time for {concurrency} runs x {steps_per_run} steps"),
        ));
        metrics.push(Metric::new(
            &format!("failed_runs_c{concurrency}"),
            errors as f64,
            "runs",
            &format!("Runs that failed with {concurrency} concurrent runs"),
        ));
    }
    env.save(
        BENCH,
        serde_json::json!({ "concurrency_levels": levels, "steps_per_run": steps_per_run, "max_tokens": 5 }),
        metrics,
        vec![
            "Every step is a real Mistral call, so throughput is bounded by API latency and the free-tier rate limit, not by the journal.".to_string(),
            "Each run has its own SQLite journal file.".to_string(),
        ],
    )
}
