//! "Reopen after simulated crash": a child process writes to the journal and then calls
//! `std::process::abort()`, the same hard kill demo-agent uses for `CRASH_AT_STEP`. No
//! destructors run and SQLite gets no chance to clean up. The parent then reopens the file.

use std::path::Path;
use std::process::Command;

use durable_core::{Journal, NewStep, StepKind, StepStatus};

// [Phase 1]
const CHILD_ENV: &str = "DURABLE_CORE_CRASH_CHILD_DB";

// [Phase 1]
fn step(seq: u64, name: &str) -> NewStep<'_> {
    NewStep {
        seq,
        step_name: name,
        step_key: "k",
        kind: StepKind::Llm,
        input_json: "{}",
        idempotency_key: None,
    }
}

// [Phase 1]
/// Only does anything when spawned by `journal_survives_process_abort`.
#[test]
fn crash_child() {
    let Ok(db) = std::env::var(CHILD_ENV) else {
        return;
    };
    let j = Journal::open(Path::new(&db)).unwrap();
    j.append_pending(&step(1, "plan")).unwrap();
    j.complete_step(1, r#""plan output""#, 10, 20).unwrap();
    j.append_pending(&step(2, "search")).unwrap();
    // Crash mid-step 2: the PENDING row must survive, step 1 must stay COMPLETED.
    std::process::abort();
}

// [Phase 1]
#[test]
fn journal_survives_process_abort() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("run.db");

    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "crash_child", "--test-threads=1", "--nocapture"])
        .env(CHILD_ENV, &db)
        .status()
        .unwrap();
    assert!(!status.success(), "child should have aborted");

    let j = Journal::open(&db).unwrap();
    let steps = j.steps().unwrap();
    assert_eq!(steps.len(), 2);

    assert_eq!(steps[0].status, StepStatus::Completed);
    assert_eq!(steps[0].output_json.as_deref(), Some(r#""plan output""#));
    assert_eq!((steps[0].tokens_in, steps[0].tokens_out), (10, 20));

    assert_eq!(steps[1].status, StepStatus::Pending);
    assert_eq!(steps[1].step_name, "search");

    // Recovery path: retry the crashed step.
    j.restart_step(2).unwrap();
    j.complete_step(2, r#""results""#, 1, 1).unwrap();
    assert_eq!(
        j.get_step(2).unwrap().unwrap().status,
        StepStatus::Completed
    );
}
