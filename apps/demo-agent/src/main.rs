//! Demo research agent. Durable/naive modes and crash injection arrive in Phase 4.

use clap::Parser;

#[derive(Parser, Debug)]
#[command(about = "10-step research agent (durable + naive modes)")]
struct Args {}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();
    let _args = Args::parse();
    tracing::info!("demo-agent scaffold: implementation lands in Phase 4");
    Ok(())
}
