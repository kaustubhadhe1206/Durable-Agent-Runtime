//! [Phase 5] Shared benchmark environment: results folder, metrics recorder, Mistral client,
//! machine specs, git commit, and saving reports.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use durable_llm::{LlmClient, MistralClient, Pricing};
use durable_metrics::{write_benchmarks_md, BenchReport, MachineInfo, Metric, Recorder};

// [Phase 5]
pub struct BenchEnv {
    pub workspace_root: PathBuf,
    pub results_dir: PathBuf,
    pub recorder: Recorder,
    pub client: Arc<MistralClient>,
    pub pricing: Pricing,
    /// UTC start time of this bench session (RFC 3339).
    pub timestamp: String,
    pub git_commit: String,
    pub machine: MachineInfo,
}

// [Phase 5]
impl BenchEnv {
    // [Phase 5]
    pub fn new(workspace_root: PathBuf, results_dir: PathBuf) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&results_dir)?;
        let timestamp = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let recorder = Recorder::open(results_dir.join("metrics.db"), timestamp.clone())?;
        Ok(Self {
            git_commit: git_commit(&workspace_root),
            machine: machine_info(),
            client: Arc::new(MistralClient::from_env()?),
            pricing: Pricing::bundled()?,
            recorder,
            timestamp,
            workspace_root,
            results_dir,
        })
    }

    // [Phase 5]
    pub fn model(&self) -> &str {
        self.client.model()
    }

    // [Phase 5]
    /// Cost in USD of the given tokens at the configured model's price.
    pub fn cost_usd(&self, tokens_in: u64, tokens_out: u64) -> anyhow::Result<f64> {
        Ok(self.pricing.cost_usd(self.model(), tokens_in, tokens_out)?)
    }

    // [Phase 5]
    /// Writes the JSON + CSV for one benchmark and regenerates BENCHMARKS.md.
    pub fn save(
        &self,
        benchmark: &str,
        config: serde_json::Value,
        metrics: Vec<Metric>,
        notes: Vec<String>,
    ) -> anyhow::Result<()> {
        let report = BenchReport {
            benchmark: benchmark.to_string(),
            timestamp: self.timestamp.clone(),
            git_commit: self.git_commit.clone(),
            machine: self.machine.clone(),
            llm_model: self.model().to_string(),
            config,
            metrics,
            notes,
        };
        let (json, csv) = report.write(&self.results_dir)?;
        let md = write_benchmarks_md(&self.results_dir)?;
        println!("\n[{benchmark}] results:");
        for m in &report.metrics {
            println!("  {:<34} {:>14.3} {}", m.name, m.value, m.unit);
        }
        println!(
            "  wrote {}\n        {}\n        {}",
            json.display(),
            csv.display(),
            md.display()
        );
        Ok(())
    }
}

// [Phase 5]
/// Short commit hash, with `-dirty` if tracked files have uncommitted changes.
fn git_commit(root: &Path) -> String {
    let run = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let Some(hash) = run(&["rev-parse", "--short", "HEAD"]) else {
        return "unknown".to_string();
    };
    let dirty =
        run(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    if dirty {
        format!("{hash}-dirty")
    } else {
        hash
    }
}

// [Phase 5]
fn machine_info() -> MachineInfo {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();
    let cpu = sys
        .cpus()
        .first()
        .map(|c| c.brand().trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    MachineInfo {
        cpu,
        logical_cores: sys.cpus().len(),
        ram_gb: sys.total_memory() as f64 / 1024.0 / 1024.0 / 1024.0,
        os: sysinfo::System::long_os_version().unwrap_or_else(|| "unknown".to_string()),
    }
}
