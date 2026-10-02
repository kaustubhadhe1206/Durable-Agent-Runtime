//! End-to-end tests: run the real demo-agent binary (with real Mistral calls), crash it with
//! CRASH_AT_STEP, restart it, and check what was replayed vs executed.
//!
//! Skipped (pass with a notice) when MISTRAL_API_KEY is not available.

use std::path::Path;
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_demo-agent");

/// Returns the API key from the environment or the repo's `.env`, or `None` to skip.
fn api_key_or_skip(test: &str) -> Option<String> {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    match std::env::var("MISTRAL_API_KEY") {
        Ok(k) if !k.trim().is_empty() => Some(k),
        _ => {
            eprintln!("SKIPPED {test}: MISTRAL_API_KEY not set");
            None
        }
    }
}

fn run_agent(dir: &Path, key: &str, mode: &str, crash_at: Option<u32>) -> Output {
    let mut cmd = Command::new(BIN);
    cmd.args(["--mode", mode, "--run-id", "t"])
        .arg("--runs-dir")
        .arg(dir.join("runs"))
        .arg("--reports-dir")
        .arg(dir.join("reports"))
        .current_dir(dir) // so the child can't pick up the repo's .env by accident
        .env("MISTRAL_API_KEY", key)
        .env_remove("CRASH_AT_STEP");
    if let Some(n) = crash_at {
        cmd.env("CRASH_AT_STEP", n.to_string());
    }
    cmd.output().expect("failed to launch demo-agent")
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

/// The tag ("[EXECUTED]" / "[REPLAYED]") printed for step `n`, if that step finished.
fn tag_of(out: &str, n: u32) -> Option<&str> {
    let marker = format!("step {n:>2}/10");
    out.lines().find(|l| l.contains(&marker)).map(|l| &l[..10])
}

fn saves(dir: &Path, mode: &str) -> usize {
    std::fs::read_to_string(dir.join(format!("reports/t-{mode}.saves.log")))
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

#[test]
fn durable_crash_at_6_then_restart_replays_1_to_5() {
    let Some(key) = api_key_or_skip("durable_crash_at_6") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();

    // Run 1: crash when step 6 starts.
    let run1 = run_agent(dir.path(), &key, "durable", Some(6));
    let out1 = stdout(&run1);
    assert!(!run1.status.success(), "run 1 should crash\n{out1}");
    for n in 1..=5 {
        assert_eq!(
            tag_of(&out1, n),
            Some("[EXECUTED]"),
            "run 1 step {n}\n{out1}"
        );
    }
    assert_eq!(
        tag_of(&out1, 6),
        None,
        "step 6 must not finish in run 1\n{out1}"
    );

    // Run 2: restart. Steps 1-5 come from the journal; 6-10 really run.
    let run2 = run_agent(dir.path(), &key, "durable", None);
    let out2 = stdout(&run2);
    assert!(
        run2.status.success(),
        "run 2 failed\n{out2}\n{}",
        String::from_utf8_lossy(&run2.stderr)
    );
    for n in 1..=5 {
        assert_eq!(
            tag_of(&out2, n),
            Some("[REPLAYED]"),
            "run 2 step {n}\n{out2}"
        );
    }
    for n in 6..=10 {
        assert_eq!(
            tag_of(&out2, n),
            Some("[EXECUTED]"),
            "run 2 step {n}\n{out2}"
        );
    }
    assert!(out2.contains("LLM calls made: 4"), "{out2}");
    assert_eq!(saves(dir.path(), "durable"), 1, "report saved exactly once");

    // Run 3: everything is already done, so nothing runs and nothing is saved again.
    let run3 = run_agent(dir.path(), &key, "durable", None);
    let out3 = stdout(&run3);
    assert!(run3.status.success(), "{out3}");
    for n in 1..=10 {
        assert_eq!(
            tag_of(&out3, n),
            Some("[REPLAYED]"),
            "run 3 step {n}\n{out3}"
        );
    }
    assert!(out3.contains("LLM calls made: 0"), "{out3}");
    assert_eq!(saves(dir.path(), "durable"), 1, "still exactly one save");
}

#[test]
fn naive_restart_redoes_everything_and_repeats_the_side_effect() {
    let Some(key) = api_key_or_skip("naive_restart") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();

    // Crash at step 2, then restart: naive mode has no journal, so it starts from step 1.
    let run1 = run_agent(dir.path(), &key, "naive", Some(2));
    assert!(!run1.status.success());
    assert_eq!(tag_of(&stdout(&run1), 1), Some("[EXECUTED]"));

    let run2 = run_agent(dir.path(), &key, "naive", None);
    let out2 = stdout(&run2);
    assert!(run2.status.success(), "{out2}");
    assert!(
        !out2.contains("[REPLAYED]"),
        "naive mode never replays\n{out2}"
    );
    for n in 1..=10 {
        assert_eq!(tag_of(&out2, n), Some("[EXECUTED]"), "step {n}\n{out2}");
    }
    assert!(out2.contains("LLM calls made: 9"), "{out2}");
    assert_eq!(saves(dir.path(), "naive"), 1);

    // Running the finished task again repeats every call AND the side effect.
    let run3 = run_agent(dir.path(), &key, "naive", None);
    assert!(run3.status.success());
    assert_eq!(
        saves(dir.path(), "naive"),
        2,
        "naive mode saves the report again"
    );
}
