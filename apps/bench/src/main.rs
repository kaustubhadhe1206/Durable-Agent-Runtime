//! [Phase 5] Benchmark harness. Every benchmark uses real Mistral calls.
//!
//! cargo run -p bench --release -- all
//! cargo run -p bench --release -- recovery --reps 3

// [Phase 5]
mod env;
// [Phase 5]
mod journal_cost;
// [Phase 5]
mod recovery;
// [Phase 5]
mod throughput;

use std::path::PathBuf;

use anyhow::bail;
use clap::{Parser, Subcommand};

use env::BenchEnv;

// [Phase 5]
#[derive(Parser, Debug)]
#[command(about = "Benchmarks for the durable agent runtime (real Mistral calls)")]
struct Args {
    #[command(subcommand)]
    command: Cmd,
    /// Where JSON/CSV results and BENCHMARKS.md are written (default: <repo>/results).
    #[arg(long, global = true)]
    results_dir: Option<PathBuf>,
    /// Allow running a debug build (numbers will not be representative).
    #[arg(long, global = true)]
    allow_debug: bool,
}

// [Phase 5]
#[derive(Subcommand, Debug)]
enum Cmd {
    /// Crash + restart the demo agent: duplicate calls, tokens/cost saved, recovery time.
    Recovery {
        #[arg(long, default_value_t = 3)]
        reps: u32,
        #[arg(long, default_value_t = 6)]
        crash_at: u32,
        /// Use this demo-agent binary instead of building the release one.
        #[arg(long)]
        demo_agent: Option<PathBuf>,
    },
    /// Journal overhead per step (p50/p95/p99) and as a share of step time.
    Overhead {
        #[arg(long, default_value_t = 50)]
        steps: u32,
    },
    /// Journal cost with synchronous=FULL vs NORMAL.
    SyncModes {
        #[arg(long, default_value_t = 30)]
        steps: u32,
    },
    /// Steps per second with several concurrent runs.
    Throughput {
        /// Comma-separated numbers of concurrent runs.
        #[arg(long, value_delimiter = ',', default_value = "1,10")]
        concurrency: Vec<usize>,
        #[arg(long, default_value_t = 5)]
        steps_per_run: u32,
    },
    /// Run every benchmark with default settings.
    All,
}

// [Phase 5]
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();
    let args = Args::parse();

    if cfg!(debug_assertions) && !args.allow_debug {
        bail!("benchmarks must run in release mode: cargo run -p bench --release -- <command>");
    }

    // The workspace root is two levels above this crate's manifest (apps/bench).
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let _ = dotenvy::from_path(root.join(".env")); // MISTRAL_API_KEY
    let results_dir = args.results_dir.unwrap_or_else(|| root.join("results"));
    let env = BenchEnv::new(root, results_dir)?;
    println!(
        "bench session {} | model {} | commit {}",
        env.timestamp,
        env.model(),
        env.git_commit
    );

    match args.command {
        Cmd::Recovery {
            reps,
            crash_at,
            demo_agent,
        } => recovery::run(&env, reps, crash_at, demo_agent)?,
        Cmd::Overhead { steps } => journal_cost::overhead(&env, steps).await?,
        Cmd::SyncModes { steps } => journal_cost::sync_modes(&env, steps).await?,
        Cmd::Throughput {
            concurrency,
            steps_per_run,
        } => throughput::run(&env, &concurrency, steps_per_run).await?,
        Cmd::All => {
            recovery::run(&env, 3, 6, None)?;
            journal_cost::overhead(&env, 50).await?;
            journal_cost::sync_modes(&env, 30).await?;
            throughput::run(&env, &[1, 10], 5).await?;
        }
    }
    Ok(())
}
