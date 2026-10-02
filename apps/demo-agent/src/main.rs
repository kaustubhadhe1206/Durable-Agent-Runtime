//! Demo: a 10-step research agent backed by real Mistral calls.
//!
//! cargo run -p demo-agent -- --mode durable
//! $env:CRASH_AT_STEP=6; cargo run -p demo-agent -- --mode durable   (PowerShell)

mod agent;

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, Context};
use clap::{Parser, ValueEnum};
use durable_core::{DurableContext, DurableError};
use durable_llm::{LlmClient, MistralClient, Pricing};

use agent::{count_saves, Agent, Runner, Totals, TOTAL_STEPS};

const DEFAULT_TOPIC: &str = "How solid-state batteries could change electric cars";

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Mode {
    /// Journal every step; a restart resumes where it stopped.
    Durable,
    /// No journal; a restart starts over from step 1.
    Naive,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Durable => "durable",
            Mode::Naive => "naive",
        }
    }
}

#[derive(Parser, Debug)]
#[command(about = "10-step research agent (durable + naive modes) using Mistral")]
struct Args {
    #[arg(long, value_enum, default_value_t = Mode::Durable)]
    mode: Mode,
    /// What the agent researches.
    #[arg(long, default_value = DEFAULT_TOPIC)]
    topic: String,
    /// Name of this run. Rerunning with the same id resumes it (durable mode).
    #[arg(long, default_value = "demo")]
    run_id: String,
    /// Where journals are stored.
    #[arg(long, default_value = "runs")]
    runs_dir: PathBuf,
    /// Where reports and the save ledger are written.
    #[arg(long, default_value = "reports")]
    reports_dir: PathBuf,
    /// Delete this run's journal and reports first, to start a brand-new run.
    #[arg(long)]
    fresh: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = dotenvy::dotenv(); // loads MISTRAL_API_KEY from .env if present
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let args = Args::parse();
    let crash_at = crash_at_from_env()?;
    let file_stem = format!("{}-{}", args.run_id, args.mode.name());
    if args.fresh {
        reset_run(&args, &file_stem)?;
    }

    let client = MistralClient::from_env()?;
    let pricing = Pricing::bundled()?;

    println!("=== Durable Agent Demo ===");
    println!(
        "mode: {} | run: {} | model: {}",
        args.mode.name(),
        args.run_id,
        client.model()
    );
    println!("topic: {}", args.topic);
    if let Some(n) = crash_at {
        println!("CRASH_AT_STEP={n}: the program will crash when step {n} starts");
    }
    println!();

    let runner = match args.mode {
        Mode::Durable => Runner::Durable(Box::new(
            DurableContext::open_in(&args.runs_dir, &args.run_id).await?,
        )),
        Mode::Naive => Runner::Naive,
    };
    let mut agent = Agent {
        runner,
        client: &client,
        crash_at,
        run_id: args.run_id.clone(),
        mode_name: args.mode.name(),
        reports_dir: args.reports_dir.clone(),
        totals: Totals::default(),
    };

    let started = Instant::now();
    let report = match agent.run(&args.topic).await {
        Ok(r) => r,
        Err(e) => {
            if let Some(DurableError::NonDeterminism(nd)) = e.downcast_ref::<DurableError>() {
                eprintln!("\nNon-determinism detected: {nd}");
                eprintln!(
                    "The journal for run '{}' was recorded with different inputs (for example \
                     a different --topic). Use --fresh or a different --run-id.",
                    args.run_id
                );
            }
            return Err(e);
        }
    };

    println!("\n=== Final report ===\n{}\n", report.text.trim());
    println!("saved to: {}", report.path);

    let saves = count_saves(&args.reports_dir, &file_stem)?;
    print_summary(
        &agent.totals,
        &pricing,
        client.model(),
        started.elapsed().as_secs_f64(),
        saves,
        &args,
    );
    Ok(())
}

fn print_summary(t: &Totals, pricing: &Pricing, model: &str, secs: f64, saves: usize, args: &Args) {
    let cost = |tin, tout| match pricing.cost_usd(model, tin, tout) {
        Ok(c) => format!("${c:.6}"),
        Err(_) => "n/a (model not in pricing.toml)".to_string(),
    };
    println!("\n=== Summary (this launch, {} mode) ===", args.mode.name());
    println!(
        "steps:          {} executed, {} replayed (of {TOTAL_STEPS})",
        t.executed, t.replayed
    );
    println!("LLM calls made: {}", t.llm_calls);
    println!(
        "tokens used:    {} in / {} out   cost {}",
        t.tokens_in,
        t.tokens_out,
        cost(t.tokens_in, t.tokens_out)
    );
    if args.mode == Mode::Durable {
        println!(
            "tokens saved:   {} in / {} out   cost {}  (replayed from the journal)",
            t.saved_tokens_in,
            t.saved_tokens_out,
            cost(t.saved_tokens_in, t.saved_tokens_out)
        );
    }
    println!(
        "report saves:   {saves} (total times the 'save report' side effect ran for run '{}')",
        args.run_id
    );
    println!("time:           {secs:.1} s");
    println!("(costs use paid-tier prices; the Mistral free tier charges $0)");
}

/// Reads `CRASH_AT_STEP` (1-10) from the environment, if set.
fn crash_at_from_env() -> anyhow::Result<Option<u32>> {
    match std::env::var("CRASH_AT_STEP") {
        Ok(v) if v.trim().is_empty() => Ok(None),
        Ok(v) => parse_crash_at(&v).map(Some),
        Err(_) => Ok(None),
    }
}

fn parse_crash_at(v: &str) -> anyhow::Result<u32> {
    let n: u32 = v
        .trim()
        .parse()
        .with_context(|| format!("CRASH_AT_STEP must be a number, got {v:?}"))?;
    if !(1..=TOTAL_STEPS).contains(&n) {
        bail!("CRASH_AT_STEP must be between 1 and {TOTAL_STEPS}, got {n}");
    }
    Ok(n)
}

/// `--fresh`: remove this run's journal (plus SQLite's WAL side files) and reports.
fn reset_run(args: &Args, file_stem: &str) -> anyhow::Result<()> {
    let db = args.runs_dir.join(format!("{}.db", args.run_id));
    let mut files = vec![
        args.reports_dir.join(format!("{file_stem}.md")),
        agent::ledger_path(&args.reports_dir, file_stem),
    ];
    if args.mode == Mode::Durable {
        files.push(with_suffix(&db, "-wal"));
        files.push(with_suffix(&db, "-shm"));
        files.push(db);
    }
    for f in files {
        match std::fs::remove_file(&f) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("removing {}", f.display())),
        }
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crash_at_parsing() {
        assert_eq!(parse_crash_at("6").unwrap(), 6);
        assert_eq!(parse_crash_at(" 10 ").unwrap(), 10);
        assert!(parse_crash_at("0").is_err());
        assert!(parse_crash_at("11").is_err());
        assert!(parse_crash_at("six").is_err());
    }

    #[test]
    fn wal_suffix() {
        assert_eq!(
            with_suffix(Path::new("runs/demo.db"), "-wal"),
            PathBuf::from("runs/demo.db-wal")
        );
    }
}
