//! [Phase 5] `bench recovery`: crash the real demo-agent (real Mistral calls) at step K,
//! restart it, and compare durable vs naive mode:
//! - duplicate LLM calls after the crash (steps that had finished but ran again)
//! - total LLM calls / tokens / cost across crash + restart
//! - recovery time: restart process spawn → first step that really executes

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context};
use durable_metrics::{summarize, Metric};
use serde::Deserialize;

use crate::env::BenchEnv;

const BENCH: &str = "recovery";

// [Phase 5]
/// One line of demo-agent's `--events` file.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum AgentEvent {
    /// Extra fields in the line (timestamp, mode, run id) are ignored.
    Start {},
    Step {
        n: u32,
        kind: String,
        replayed: bool,
        tokens_in: u64,
        tokens_out: u64,
        started_unix_us: u64,
    },
    Done {},
}

// [Phase 5]
/// What one crash + restart cost.
#[derive(Debug, Clone, PartialEq)]
pub struct CrashOutcome {
    /// LLM steps that had completed before the crash but executed again after it.
    pub duplicate_llm_calls: u32,
    /// LLM calls actually made across both launches.
    pub llm_calls: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    /// Restart spawn → first executed step, in microseconds.
    pub recovery_us: Option<u64>,
}

// [Phase 5]
pub fn parse_events(text: &str) -> anyhow::Result<Vec<AgentEvent>> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).with_context(|| format!("bad event line: {l}")))
        .collect()
}

// [Phase 5]
/// Compares the crashed launch (`run1`) with the restart (`run2`, spawned at `spawn2_us`).
pub fn analyze(run1: &[AgentEvent], run2: &[AgentEvent], spawn2_us: u64) -> CrashOutcome {
    let completed_before_crash: HashSet<u32> = run1
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Step { n, .. } => Some(*n),
            _ => None,
        })
        .collect();

    let mut out = CrashOutcome {
        duplicate_llm_calls: 0,
        llm_calls: 0,
        tokens_in: 0,
        tokens_out: 0,
        recovery_us: None,
    };
    for (launch, events) in [(1, run1), (2, run2)] {
        for e in events {
            let AgentEvent::Step {
                n,
                kind,
                replayed,
                tokens_in,
                tokens_out,
                started_unix_us,
            } = e
            else {
                continue;
            };
            if *replayed {
                continue;
            }
            if launch == 2 && out.recovery_us.is_none() {
                out.recovery_us = Some(started_unix_us.saturating_sub(spawn2_us));
            }
            if kind == "llm" {
                out.llm_calls += 1;
                out.tokens_in += tokens_in;
                out.tokens_out += tokens_out;
                if launch == 2 && completed_before_crash.contains(n) {
                    out.duplicate_llm_calls += 1;
                }
            }
        }
    }
    out
}

// [Phase 5]
/// Builds the release demo-agent binary (unless a path is given) and returns its path.
pub fn demo_agent_binary(env: &BenchEnv, given: Option<PathBuf>) -> anyhow::Result<PathBuf> {
    if let Some(p) = given {
        return Ok(p);
    }
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    println!("building demo-agent (release)...");
    let status = Command::new(cargo)
        .args(["build", "--release", "-p", "demo-agent"])
        .current_dir(&env.workspace_root)
        .status()
        .context("running cargo build for demo-agent")?;
    if !status.success() {
        bail!("cargo build -p demo-agent failed");
    }
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| env.workspace_root.join("target"));
    Ok(target
        .join("release")
        .join(format!("demo-agent{}", std::env::consts::EXE_SUFFIX)))
}

// [Phase 5]
/// Launches demo-agent once; returns (exit success, spawn time in Unix µs, events).
fn launch(
    bin: &Path,
    dir: &Path,
    mode: &str,
    crash_at: Option<u32>,
    events_file: &str,
) -> anyhow::Result<(bool, u64, Vec<AgentEvent>)> {
    let events_path = dir.join(events_file);
    let mut cmd = Command::new(bin);
    cmd.args(["--mode", mode, "--run-id", "bench"])
        .arg("--runs-dir")
        .arg(dir.join("runs"))
        .arg("--reports-dir")
        .arg(dir.join("reports"))
        .arg("--events")
        .arg(&events_path)
        .env_remove("CRASH_AT_STEP");
    if let Some(n) = crash_at {
        cmd.env("CRASH_AT_STEP", n.to_string());
    }
    let spawn_us = now_unix_us();
    let output = cmd.output().context("launching demo-agent")?;
    let text = std::fs::read_to_string(&events_path).unwrap_or_default();
    if crash_at.is_none() && !output.status.success() {
        bail!(
            "demo-agent failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok((output.status.success(), spawn_us, parse_events(&text)?))
}

// [Phase 5]
pub fn run(
    env: &BenchEnv,
    reps: u32,
    crash_at: u32,
    demo_agent: Option<PathBuf>,
) -> anyhow::Result<()> {
    if !(2..=10).contains(&crash_at) {
        bail!("--crash-at must be between 2 and 10");
    }
    let bin = demo_agent_binary(env, demo_agent)?;
    let mut outcomes: Vec<(&str, CrashOutcome)> = Vec::new();

    for rep in 1..=reps {
        for mode in ["durable", "naive"] {
            println!("rep {rep}/{reps} {mode}: crash at step {crash_at}, then restart");
            let dir = tempfile::tempdir()?;
            let (ok1, _, run1) = launch(&bin, dir.path(), mode, Some(crash_at), "run1.jsonl")?;
            if ok1 {
                bail!("demo-agent was expected to crash at step {crash_at}");
            }
            let (_, spawn2, run2) = launch(&bin, dir.path(), mode, None, "run2.jsonl")?;
            if !run2.iter().any(|e| matches!(e, AgentEvent::Done { .. })) {
                bail!("restart did not finish ({mode})");
            }
            let o = analyze(&run1, &run2, spawn2);
            let r = &env.recorder;
            r.record(
                BENCH,
                &format!("duplicate_llm_calls_{mode}"),
                o.duplicate_llm_calls.into(),
                "calls",
            )?;
            r.record(
                BENCH,
                &format!("llm_calls_{mode}"),
                o.llm_calls.into(),
                "calls",
            )?;
            r.record(
                BENCH,
                &format!("tokens_{mode}"),
                (o.tokens_in + o.tokens_out) as f64,
                "tokens",
            )?;
            if let Some(us) = o.recovery_us {
                r.record(BENCH, &format!("recovery_us_{mode}"), us as f64, "us")?;
            }
            println!(
                "  duplicates={} llm_calls={} tokens={} recovery={:?} us",
                o.duplicate_llm_calls,
                o.llm_calls,
                o.tokens_in + o.tokens_out,
                o.recovery_us
            );
            outcomes.push((mode, o));
        }
    }

    let mean = |mode: &str, f: &dyn Fn(&CrashOutcome) -> f64| {
        let v: Vec<f64> = outcomes
            .iter()
            .filter(|(m, _)| *m == mode)
            .map(|(_, o)| f(o))
            .collect();
        v.iter().sum::<f64>() / v.len().max(1) as f64
    };
    let dup = |o: &CrashOutcome| f64::from(o.duplicate_llm_calls);
    let calls = |o: &CrashOutcome| f64::from(o.llm_calls);
    let toks_in = |o: &CrashOutcome| o.tokens_in as f64;
    let toks_out = |o: &CrashOutcome| o.tokens_out as f64;

    let (d_in, d_out) = (mean("durable", &toks_in), mean("durable", &toks_out));
    let (n_in, n_out) = (mean("naive", &toks_in), mean("naive", &toks_out));
    let tokens_d = d_in + d_out;
    let tokens_n = n_in + n_out;
    let cost_d = env.cost_usd(d_in.round() as u64, d_out.round() as u64)?;
    let cost_n = env.cost_usd(n_in.round() as u64, n_out.round() as u64)?;
    let saved_pct = if tokens_n > 0.0 {
        (tokens_n - tokens_d) / tokens_n * 100.0
    } else {
        0.0
    };

    let rec_d = summarize(&env.recorder.samples(BENCH, "recovery_us_durable")?)?;
    let rec_n = summarize(&env.recorder.samples(BENCH, "recovery_us_naive")?)?;

    let mut metrics = vec![
        Metric::new(
            "duplicate_llm_calls_durable",
            mean("durable", &dup),
            "calls",
            "Duplicate LLM calls after a crash, durable mode (mean per crash)",
        )
        .headline(),
        Metric::new(
            "duplicate_llm_calls_naive",
            mean("naive", &dup),
            "calls",
            "Duplicate LLM calls after a crash, naive restart (mean per crash)",
        )
        .headline(),
        Metric::new(
            "llm_calls_durable",
            mean("durable", &calls),
            "calls",
            "LLM calls across crash + restart, durable (mean)",
        ),
        Metric::new(
            "llm_calls_naive",
            mean("naive", &calls),
            "calls",
            "LLM calls across crash + restart, naive (mean)",
        ),
        Metric::new(
            "tokens_durable",
            tokens_d,
            "tokens",
            "Tokens used across crash + restart, durable (mean)",
        ),
        Metric::new(
            "tokens_naive",
            tokens_n,
            "tokens",
            "Tokens used across crash + restart, naive (mean)",
        ),
        Metric::new(
            "tokens_saved",
            tokens_n - tokens_d,
            "tokens",
            "Tokens saved by durable vs naive restart (mean)",
        ),
        Metric::new(
            "tokens_saved_pct",
            saved_pct,
            "%",
            "Tokens saved by durable vs naive restart",
        )
        .headline(),
        Metric::new(
            "cost_durable_usd",
            cost_d,
            "USD",
            "Cost across crash + restart, durable (paid-tier prices)",
        ),
        Metric::new(
            "cost_naive_usd",
            cost_n,
            "USD",
            "Cost across crash + restart, naive (paid-tier prices)",
        ),
        Metric::new(
            "cost_saved_usd",
            cost_n - cost_d,
            "USD",
            "Cost saved by durable vs naive restart (paid-tier prices)",
        ),
    ];
    if let Some(s) = rec_d {
        metrics.push(
            Metric::new(
                "recovery_ms_p50_durable",
                s.p50 / 1000.0,
                "ms",
                "Recovery time, durable: restart spawn to first new step executing (p50)",
            )
            .headline(),
        );
        metrics.push(Metric::new(
            "recovery_ms_max_durable",
            s.max / 1000.0,
            "ms",
            "Recovery time, durable (max)",
        ));
    }
    if let Some(s) = rec_n {
        metrics.push(Metric::new(
            "recovery_ms_p50_naive",
            s.p50 / 1000.0,
            "ms",
            "Naive restart: spawn to first step executing (step 1, all work redone) (p50)",
        ));
    }

    env.save(
        BENCH,
        serde_json::json!({ "reps": reps, "crash_at_step": crash_at, "agent_steps": 10 }),
        metrics,
        vec![
            format!("Each repetition crashes demo-agent (abort) when step {crash_at} starts, then restarts it."),
            "All LLM calls are real Mistral API calls; reply lengths vary run to run, so token and cost figures are means over the repetitions.".to_string(),
        ],
    )
}

// [Phase 5]
fn now_unix_us() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

// [Phase 5]
#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a `step` event line as demo-agent would write it.
    fn step(n: u32, kind: &str, replayed: bool, tokens: u64, started: u64) -> String {
        format!(
            r#"{{"event":"step","n":{n},"kind":"{kind}","replayed":{replayed},"tokens_in":{tokens},"tokens_out":{tokens},"started_unix_us":{started},"duration_us":1,"journal_us":1}}"#
        )
    }

    #[test]
    fn durable_restart_has_no_duplicates() {
        // Launch 1: steps 1-2 complete, crash in step 3.
        let run1 = parse_events(
            &[
                r#"{"event":"start","unix_us":0,"mode":"durable","run_id":"r"}"#.to_string(),
                step(1, "llm", false, 10, 100),
                step(2, "llm", false, 10, 200),
            ]
            .join("\n"),
        )
        .unwrap();
        // Launch 2 (spawned at 1000): 1-2 replayed, 3 executed at 1500, save executed.
        let run2 = parse_events(
            &[
                step(1, "llm", true, 10, 1100),
                step(2, "llm", true, 10, 1200),
                step(3, "llm", false, 10, 1500),
                step(4, "side_effect", false, 0, 1600),
                r#"{"event":"done","unix_us":1700}"#.to_string(),
            ]
            .join("\n"),
        )
        .unwrap();

        let o = analyze(&run1, &run2, 1000);
        assert_eq!(o.duplicate_llm_calls, 0);
        assert_eq!(o.llm_calls, 3);
        assert_eq!((o.tokens_in, o.tokens_out), (30, 30));
        assert_eq!(o.recovery_us, Some(500));
    }

    #[test]
    fn naive_restart_counts_redone_steps_as_duplicates() {
        let run1 = parse_events(
            &[
                step(1, "llm", false, 10, 100),
                step(2, "llm", false, 10, 200),
            ]
            .join("\n"),
        )
        .unwrap();
        let run2 = parse_events(
            &[
                step(1, "llm", false, 10, 1050),
                step(2, "llm", false, 10, 1200),
                step(3, "llm", false, 10, 1300),
            ]
            .join("\n"),
        )
        .unwrap();
        let o = analyze(&run1, &run2, 1000);
        assert_eq!(o.duplicate_llm_calls, 2);
        assert_eq!(o.llm_calls, 5);
        assert_eq!(o.recovery_us, Some(50));
    }

    #[test]
    fn bad_event_line_is_an_error() {
        assert!(parse_events("not json").is_err());
        assert!(parse_events("\n\n").unwrap().is_empty());
    }
}
