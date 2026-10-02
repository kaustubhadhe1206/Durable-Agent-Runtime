//! [Phase 5] Metrics for the benchmark harness:
//! - `Recorder`: stores every timing sample and counter in a SQLite `metrics.db`
//! - `summarize`: HDR-histogram percentiles (p50/p95/p99) over samples
//! - `report`: JSON/CSV result files and the generated `results/BENCHMARKS.md`

// [Phase 5]
pub mod recorder;
// [Phase 5]
pub mod report;
// [Phase 5]
pub mod summary;

pub use recorder::Recorder;
pub use report::{
    load_latest_reports, render_benchmarks_md, write_benchmarks_md, BenchReport, MachineInfo,
    Metric,
};
pub use summary::{summarize, Summary};

// [Phase 5]
/// Errors from the metrics crate.
#[derive(Debug, thiserror::Error)]
pub enum MetricsError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("histogram error: {0}")]
    Histogram(String),
}

pub type Result<T> = std::result::Result<T, MetricsError>;
