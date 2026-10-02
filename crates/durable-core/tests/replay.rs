//! Phase 2 integration tests for `DurableContext` + the replay engine.

use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

use durable_core::{
    DurableContext, DurableError, Journal, StepKind, StepSpec, StepStatus, TokenUsage,
};

// [Phase 2]
/// Runs a fixed 3-step agent: plan (llm) → search (tool) → save (side effect).
/// `calls` counts how many closures actually executed.
async fn three_step_agent(
    ctx: &DurableContext,
    topic: &str,
    calls: &AtomicUsize,
) -> Result<String, DurableError> {
    let plan: String = ctx
        .llm_step("plan", topic, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::io::Error>((
                format!("plan for {topic}"),
                TokenUsage {
                    tokens_in: 10,
                    tokens_out: 20,
                },
            ))
        })
        .await?;

    let results: Vec<String> = ctx
        .step("search", &plan, || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::io::Error>(vec![format!("result of {plan}")])
        })
        .await?;

    ctx.side_effect("save", "save-report-1", || async {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok::<_, std::io::Error>(())
    })
    .await?;

    Ok(results.join(","))
}

// [Phase 2]
/// (a) A new run_id executes every step closure exactly once.
#[tokio::test]
async fn fresh_run_executes_all_steps() {
    let dir = tempfile::tempdir().unwrap();
    let calls = AtomicUsize::new(0);
    let ctx = DurableContext::open_in(dir.path(), "run1").await.unwrap();

    let out = three_step_agent(&ctx, "rust", &calls).await.unwrap();

    assert_eq!(out, "result of plan for rust");
    assert_eq!(calls.load(Ordering::SeqCst), 3);
    let stats = ctx.stats().unwrap();
    assert_eq!((stats.executed, stats.replayed), (3, 0));
    assert_eq!(stats.tokens_executed.tokens_out, 20);

    let journal = Journal::open(dir.path().join("run1.db")).unwrap();
    let steps = journal.steps().unwrap();
    assert_eq!(steps.len(), 3);
    assert!(steps.iter().all(|s| s.status == StepStatus::Completed));
    assert_eq!(
        steps.iter().map(|s| s.kind).collect::<Vec<_>>(),
        vec![StepKind::Llm, StepKind::Tool, StepKind::SideEffect]
    );
    assert_eq!(steps[2].idempotency_key.as_deref(), Some("save-report-1"));
}

// [Phase 2]
/// (b) Rerunning a completed run_id replays everything and runs zero closures.
#[tokio::test]
async fn rerun_with_same_run_id_executes_zero_closures() {
    let dir = tempfile::tempdir().unwrap();
    let calls = AtomicUsize::new(0);
    let first = {
        let ctx = DurableContext::open_in(dir.path(), "run1").await.unwrap();
        three_step_agent(&ctx, "rust", &calls).await.unwrap()
    };
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let ctx = DurableContext::open_in(dir.path(), "run1").await.unwrap();
    let second = three_step_agent(&ctx, "rust", &calls).await.unwrap();

    assert_eq!(second, first, "replayed output must equal the original");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "no closure may run on replay"
    );
    let stats = ctx.stats().unwrap();
    assert_eq!((stats.executed, stats.replayed), (0, 3));
    assert_eq!(
        stats.tokens_replayed,
        TokenUsage {
            tokens_in: 10,
            tokens_out: 20
        }
    );
}

// [Phase 2]
/// (c) Changing a step's input between runs fails loudly instead of replaying stale data.
#[tokio::test]
async fn changed_input_raises_non_determinism_error() {
    let dir = tempfile::tempdir().unwrap();
    let calls = AtomicUsize::new(0);
    {
        let ctx = DurableContext::open_in(dir.path(), "run1").await.unwrap();
        three_step_agent(&ctx, "rust", &calls).await.unwrap();
    }

    let ctx = DurableContext::open_in(dir.path(), "run1").await.unwrap();
    let err = three_step_agent(&ctx, "golang", &calls).await.unwrap_err();

    let DurableError::NonDeterminism(nd) = err else {
        panic!("expected NonDeterminism, got {err:?}");
    };
    assert_eq!(nd.seq, 1);
    assert_eq!(nd.recorded_name, "plan");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        3,
        "mismatched step must not run"
    );
}

// [Phase 2]
#[tokio::test]
async fn failed_step_is_recorded_and_retried_on_next_run() {
    let dir = tempfile::tempdir().unwrap();
    let attempts = AtomicUsize::new(0);

    // [Phase 2]
    async fn flaky(ctx: &DurableContext, attempts: &AtomicUsize) -> Result<usize, DurableError> {
        ctx.step("flaky", "in", || async {
            let n = attempts.fetch_add(1, Ordering::SeqCst);
            if n == 0 {
                Err("transient failure".to_string())
            } else {
                Ok(n)
            }
        })
        .await
    }

    let ctx = DurableContext::open_in(dir.path(), "run1").await.unwrap();
    let err = flaky(&ctx, &attempts).await.unwrap_err();
    assert!(matches!(err, DurableError::StepFailed { seq: 1, .. }));
    drop(ctx);

    {
        let journal = Journal::open(dir.path().join("run1.db")).unwrap();
        assert_eq!(
            journal.get_step(1).unwrap().unwrap().status,
            StepStatus::Failed
        );
    }

    let ctx = DurableContext::open_in(dir.path(), "run1").await.unwrap();
    assert_eq!(flaky(&ctx, &attempts).await.unwrap(), 1);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

// [Phase 2]
#[tokio::test]
async fn execute_reports_replayed_flag() {
    let dir = tempfile::tempdir().unwrap();

    // [Phase 2]
    async fn run(ctx: &DurableContext) -> durable_core::StepOutcome<String> {
        let spec = StepSpec {
            name: "s",
            kind: StepKind::Tool,
            input: &42,
            idempotency_key: None,
        };
        ctx.execute(spec, || async {
            Ok::<_, std::io::Error>(("v".to_string(), TokenUsage::default()))
        })
        .await
        .unwrap()
    }

    let first = run(&DurableContext::open_in(dir.path(), "r").await.unwrap()).await;
    let second = run(&DurableContext::open_in(dir.path(), "r").await.unwrap()).await;
    assert!(!first.replayed);
    assert!(second.replayed);
    // [Phase 5] an executed step wrote PENDING + COMPLETED, which takes measurable time
    assert!(first.journal_us > 0, "journal time should be measured");
    assert_eq!((first.seq, second.seq), (1, 1));
    assert_eq!(second.value, "v");
}

// [Phase 2]
#[tokio::test]
async fn invalid_run_id_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let result = DurableContext::open_in(dir.path(), "../escape").await;
    assert!(matches!(result, Err(DurableError::InvalidRunId(_))));
}

// ---- (d) PENDING step re-executes after a real crash ------------------------------------

// [Phase 2]
const CHILD_ENV: &str = "DURABLE_CORE_REPLAY_CRASH_DIR";

// [Phase 2]
/// Spawned by `pending_step_re_executes_after_crash`: completes step 1, then aborts the
/// process *inside* step 2, leaving it PENDING in the journal. No-op when run normally.
#[tokio::test]
async fn crash_inside_step_child() {
    let Ok(dir) = std::env::var(CHILD_ENV) else {
        return;
    };
    let ctx = DurableContext::open_in(Path::new(&dir), "run1")
        .await
        .unwrap();
    let _: String = ctx
        .step("one", "in", || async {
            Ok::<_, std::io::Error>("1".to_string())
        })
        .await
        .unwrap();
    let _: String = ctx
        .step("two", "in", || async {
            std::process::abort();
            #[allow(unreachable_code)]
            Ok::<_, std::io::Error>(String::new())
        })
        .await
        .unwrap();
}

// [Phase 2]
/// (d) After a hard crash mid-step, the rerun replays step 1 and re-executes only step 2.
#[tokio::test]
async fn pending_step_re_executes_after_crash() {
    let dir = tempfile::tempdir().unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "crash_inside_step_child",
            "--test-threads=1",
            "--nocapture",
        ])
        .env(CHILD_ENV, dir.path())
        .status()
        .unwrap();
    assert!(!status.success(), "child should have aborted");

    {
        let journal = Journal::open(dir.path().join("run1.db")).unwrap();
        let steps = journal.steps().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].status, StepStatus::Completed);
        assert_eq!(steps[1].status, StepStatus::Pending);
    }

    let calls = AtomicUsize::new(0);
    let ctx = DurableContext::open_in(dir.path(), "run1").await.unwrap();
    let one: String = ctx
        .step("one", "in", || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::io::Error>("1".to_string())
        })
        .await
        .unwrap();
    let two: String = ctx
        .step("two", "in", || async {
            calls.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::io::Error>("2".to_string())
        })
        .await
        .unwrap();

    assert_eq!((one.as_str(), two.as_str()), ("1", "2"));
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "only the PENDING step re-runs"
    );
    let stats = ctx.stats().unwrap();
    assert_eq!((stats.executed, stats.replayed), (1, 1));
}
