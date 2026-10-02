//! [Phase 5] `bench overhead` and `bench sync-modes`: how much time the journal adds to each
//! step, measured on durable steps that make real Mistral calls.

use std::time::Instant;

use durable_core::{DurableContext, StepKind, StepSpec, SyncMode, TokenUsage};
use durable_llm::{LlmClient, LlmError, LlmRequest, LlmResponse};
use durable_metrics::{summarize, Metric, Summary};

use crate::env::BenchEnv;

// [Phase 5]
/// Runs `steps` durable LLM steps and records, per step, the journal time and the total
/// step time under metric names suffixed with `label`.
async fn measure(
    env: &BenchEnv,
    bench: &str,
    label: &str,
    sync: SyncMode,
    steps: u32,
) -> anyhow::Result<()> {
    let dir = tempfile::tempdir()?;
    let ctx = DurableContext::open_with(dir.path(), "journal-cost", sync).await?;
    let client = env.client.clone();

    for i in 0..steps {
        // Distinct prompt per step, so each one is a new journal entry and a real call.
        let req = LlmRequest::new(format!("Reply with only the number {i}."))
            .with_max_tokens(5)
            .with_temperature(0.0);
        let spec = StepSpec {
            name: "step",
            kind: StepKind::Llm,
            input: &req,
            idempotency_key: None,
        };
        let started = Instant::now();
        let outcome = ctx
            .execute(spec, || async {
                let resp: LlmResponse = client.complete(&req).await?;
                let usage = TokenUsage {
                    tokens_in: resp.tokens_in,
                    tokens_out: resp.tokens_out,
                };
                Ok::<_, LlmError>((resp, usage))
            })
            .await?;
        let total_us = started.elapsed().as_micros() as f64;
        let journal_us = outcome.journal_us as f64;

        let r = &env.recorder;
        r.record(bench, &format!("journal_us_{label}"), journal_us, "us")?;
        r.record(bench, &format!("step_us_{label}"), total_us, "us")?;
        r.record(
            bench,
            &format!("llm_ms_{label}"),
            outcome.value.latency_ms as f64,
            "ms",
        )?;
        if (i + 1) % 10 == 0 || i + 1 == steps {
            println!("  {label}: {}/{steps} steps", i + 1);
        }
    }
    Ok(())
}

// [Phase 5]
fn summary(env: &BenchEnv, bench: &str, metric: &str) -> anyhow::Result<Summary> {
    summarize(&env.recorder.samples(bench, metric)?)?
        .ok_or_else(|| anyhow::anyhow!("no samples for {metric}"))
}

// [Phase 5]
/// Journal share of total step time: mean(journal) / mean(step) × 100.
fn overhead_pct(journal: &Summary, step: &Summary) -> f64 {
    if step.mean > 0.0 {
        journal.mean / step.mean * 100.0
    } else {
        0.0
    }
}

// [Phase 5]
pub async fn overhead(env: &BenchEnv, steps: u32) -> anyhow::Result<()> {
    const BENCH: &str = "overhead";
    println!("overhead: {steps} durable steps with real Mistral calls (synchronous=FULL)");
    measure(env, BENCH, "full", SyncMode::Full, steps).await?;

    let j = summary(env, BENCH, "journal_us_full")?;
    let s = summary(env, BENCH, "step_us_full")?;
    let llm = summary(env, BENCH, "llm_ms_full")?;
    let metrics = vec![
        Metric::new(
            "journal_us_p50",
            j.p50,
            "us",
            "Journal overhead per step (p50)",
        ),
        Metric::new(
            "journal_us_p95",
            j.p95,
            "us",
            "Journal overhead per step (p95)",
        ),
        Metric::new(
            "journal_us_p99",
            j.p99,
            "us",
            "Journal overhead per step (p99)",
        )
        .headline(),
        Metric::new(
            "journal_us_mean",
            j.mean,
            "us",
            "Journal overhead per step (mean)",
        ),
        Metric::new(
            "step_ms_p50",
            s.p50 / 1000.0,
            "ms",
            "Total step time incl. Mistral call (p50)",
        ),
        Metric::new(
            "llm_ms_p50",
            llm.p50,
            "ms",
            "Mistral HTTP request time (p50)",
        ),
        Metric::new(
            "journal_overhead_pct",
            overhead_pct(&j, &s),
            "%",
            "Journal time as a share of total step time (mean/mean)",
        )
        .headline(),
    ];
    env.save(
        BENCH,
        serde_json::json!({ "steps": steps, "sync_mode": "FULL", "max_tokens": 5 }),
        metrics,
        vec![
            "Each step is a durable LLM step with a real Mistral call; journal time = PENDING insert + COMPLETED update (incl. fsync).".to_string(),
        ],
    )
}

// [Phase 5]
pub async fn sync_modes(env: &BenchEnv, steps: u32) -> anyhow::Result<()> {
    const BENCH: &str = "sync-modes";
    println!("sync-modes: {steps} steps with synchronous=FULL, then {steps} with NORMAL");
    measure(env, BENCH, "full", SyncMode::Full, steps).await?;
    measure(env, BENCH, "normal", SyncMode::Normal, steps).await?;

    let full = summary(env, BENCH, "journal_us_full")?;
    let normal = summary(env, BENCH, "journal_us_normal")?;
    let full_step = summary(env, BENCH, "step_us_full")?;
    let normal_step = summary(env, BENCH, "step_us_normal")?;
    let ratio = if normal.p50 > 0.0 {
        full.p50 / normal.p50
    } else {
        0.0
    };
    let metrics = vec![
        Metric::new(
            "full_journal_us_p50",
            full.p50,
            "us",
            "synchronous=FULL journal time per step (p50)",
        ),
        Metric::new(
            "full_journal_us_p99",
            full.p99,
            "us",
            "synchronous=FULL journal time per step (p99)",
        ),
        Metric::new(
            "normal_journal_us_p50",
            normal.p50,
            "us",
            "synchronous=NORMAL journal time per step (p50)",
        ),
        Metric::new(
            "normal_journal_us_p99",
            normal.p99,
            "us",
            "synchronous=NORMAL journal time per step (p99)",
        ),
        Metric::new(
            "full_vs_normal_p50_ratio",
            ratio,
            "x",
            "How many times slower FULL journal writes are than NORMAL (p50)",
        )
        .headline(),
        Metric::new(
            "full_overhead_pct",
            overhead_pct(&full, &full_step),
            "%",
            "FULL: journal share of total step time",
        ),
        Metric::new(
            "normal_overhead_pct",
            overhead_pct(&normal, &normal_step),
            "%",
            "NORMAL: journal share of total step time",
        ),
    ];
    env.save(
        BENCH,
        serde_json::json!({ "steps_per_mode": steps, "max_tokens": 5 }),
        metrics,
        vec![
            "FULL fsyncs every commit (survives power loss); NORMAL fsyncs at WAL checkpoints (survives process crash).".to_string(),
            "Steps make real Mistral calls; only the journal portion is compared.".to_string(),
        ],
    )
}
