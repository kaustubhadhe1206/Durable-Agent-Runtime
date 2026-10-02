//! Benchmark result files: `results/<benchmark>_<timestamp>.json` / `.csv`, and the
//! generated `results/BENCHMARKS.md`, which only shows numbers read from those JSON files.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Result;

// [Phase 5]
/// The machine a benchmark ran on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineInfo {
    pub cpu: String,
    pub logical_cores: usize,
    pub ram_gb: f64,
    pub os: String,
}

// [Phase 5]
/// One named result number.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Metric {
    pub name: String,
    pub value: f64,
    pub unit: String,
    pub description: String,
    /// Shown in the "Headline numbers" table at the top of BENCHMARKS.md.
    #[serde(default)]
    pub headline: bool,
}

// [Phase 5]
impl Metric {
    // [Phase 5]
    pub fn new(name: &str, value: f64, unit: &str, description: &str) -> Self {
        Self {
            name: name.to_string(),
            value,
            unit: unit.to_string(),
            description: description.to_string(),
            headline: false,
        }
    }

    // [Phase 5]
    pub fn headline(mut self) -> Self {
        self.headline = true;
        self
    }
}

// [Phase 5]
/// Everything one benchmark run produced.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchReport {
    pub benchmark: String,
    /// UTC, RFC 3339, e.g. `2026-10-02T12:30:00Z`.
    pub timestamp: String,
    pub git_commit: String,
    pub machine: MachineInfo,
    pub llm_model: String,
    pub config: serde_json::Value,
    pub metrics: Vec<Metric>,
    #[serde(default)]
    pub notes: Vec<String>,
}

// [Phase 5]
impl BenchReport {
    // [Phase 5]
    /// `<benchmark>_<timestamp without separators>`, e.g. `overhead_20261002T123000Z`.
    pub fn file_stem(&self) -> String {
        let ts: String = self
            .timestamp
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        format!("{}_{ts}", self.benchmark)
    }

    // [Phase 5]
    /// Writes `<dir>/<file_stem>.json` and `.csv`; returns both paths.
    pub fn write(&self, dir: impl AsRef<Path>) -> Result<(PathBuf, PathBuf)> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let json_path = dir.join(format!("{}.json", self.file_stem()));
        let csv_path = dir.join(format!("{}.csv", self.file_stem()));
        std::fs::write(&json_path, serde_json::to_string_pretty(self)?)?;
        std::fs::write(&csv_path, self.to_csv())?;
        Ok((json_path, csv_path))
    }

    // [Phase 5]
    pub fn to_csv(&self) -> String {
        let mut out = String::from("benchmark,timestamp,metric,value,unit,description\n");
        for m in &self.metrics {
            let _ = writeln!(
                out,
                "{},{},{},{},{},{}",
                csv_field(&self.benchmark),
                csv_field(&self.timestamp),
                csv_field(&m.name),
                m.value,
                csv_field(&m.unit),
                csv_field(&m.description)
            );
        }
        out
    }
}

// [Phase 5]
/// Quotes a CSV field when it contains a comma, quote, or newline.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

// [Phase 5]
/// Loads the newest JSON report of each benchmark in `dir` (newest = latest timestamp in
/// the file name). Returns `(file name, report)` pairs sorted by benchmark name.
pub fn load_latest_reports(dir: impl AsRef<Path>) -> Result<Vec<(String, BenchReport)>> {
    let mut latest: BTreeMap<String, (String, BenchReport)> = BTreeMap::new();
    let entries = match std::fs::read_dir(dir.as_ref()) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(report) = serde_json::from_str::<BenchReport>(&std::fs::read_to_string(&path)?)
        else {
            tracing::warn!(path = %path.display(), "skipping file that is not a bench report");
            continue;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let newer = latest
            .get(&report.benchmark)
            .is_none_or(|(_, cur)| report.file_stem() > cur.file_stem());
        if newer {
            latest.insert(report.benchmark.clone(), (name, report));
        }
    }
    Ok(latest.into_values().collect())
}

// [Phase 5]
/// Renders BENCHMARKS.md from the given reports. Every number shown comes from a report,
/// and each section names its source JSON file.
pub fn render_benchmarks_md(reports: &[(String, BenchReport)]) -> String {
    let mut md = String::new();
    let _ = writeln!(md, "# Benchmarks\n");
    let _ = writeln!(
        md,
        "_Generated by `cargo run -p bench --release`. Do not edit by hand: every number \
         below is read from the JSON file named in its section._\n"
    );
    if reports.is_empty() {
        let _ = writeln!(md, "No benchmark results yet.");
        return md;
    }

    let _ = writeln!(md, "## Headline numbers\n");
    let _ = writeln!(md, "| Metric | Value | Benchmark | Source |");
    let _ = writeln!(md, "| --- | --- | --- | --- |");
    for (file, r) in reports {
        for m in r.metrics.iter().filter(|m| m.headline) {
            let _ = writeln!(
                md,
                "| {} | {} {} | {} | [{file}]({file}) |",
                m.description,
                fmt_value(m.value),
                m.unit,
                r.benchmark
            );
        }
    }

    for (file, r) in reports {
        let _ = writeln!(md, "\n## {}\n", r.benchmark);
        let _ = writeln!(
            md,
            "- Source: [{file}]({file})\n- Run at: {} (commit `{}`)\n- LLM: {}\n- Config: `{}`",
            r.timestamp, r.git_commit, r.llm_model, r.config
        );
        for note in &r.notes {
            let _ = writeln!(md, "- Note: {note}");
        }
        let _ = writeln!(md, "\n| Metric | Value | Unit | Meaning |");
        let _ = writeln!(md, "| --- | --- | --- | --- |");
        for m in &r.metrics {
            let _ = writeln!(
                md,
                "| `{}` | {} | {} | {} |",
                m.name,
                fmt_value(m.value),
                m.unit,
                m.description
            );
        }
    }

    // Machine info from the most recent report.
    if let Some((_, r)) = reports.iter().max_by_key(|(_, r)| r.file_stem()) {
        let m = &r.machine;
        let _ = writeln!(
            md,
            "\n## Machine\n\n- CPU: {} ({} logical cores)\n- RAM: {:.1} GB\n- OS: {}",
            m.cpu, m.logical_cores, m.ram_gb, m.os
        );
    }
    md
}

// [Phase 5]
/// Regenerates `<dir>/BENCHMARKS.md` from the latest report of each benchmark.
pub fn write_benchmarks_md(dir: impl AsRef<Path>) -> Result<PathBuf> {
    let dir = dir.as_ref();
    let md = render_benchmarks_md(&load_latest_reports(dir)?);
    let path = dir.join("BENCHMARKS.md");
    std::fs::write(&path, md)?;
    Ok(path)
}

// [Phase 5]
/// Whole numbers print without decimals; others with up to 4 significant decimals.
fn fmt_value(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{v:.0}")
    } else if v.abs() >= 100.0 {
        format!("{v:.1}")
    } else if v.abs() >= 1.0 {
        format!("{v:.2}")
    } else {
        format!("{v:.6}")
    }
}

// [Phase 5]
#[cfg(test)]
mod tests {
    use super::*;

    fn report(bench: &str, ts: &str, value: f64) -> BenchReport {
        BenchReport {
            benchmark: bench.to_string(),
            timestamp: ts.to_string(),
            git_commit: "abc1234".to_string(),
            machine: MachineInfo {
                cpu: "Test CPU".to_string(),
                logical_cores: 8,
                ram_gb: 16.0,
                os: "TestOS".to_string(),
            },
            llm_model: "ministral-8b-latest".to_string(),
            config: serde_json::json!({ "steps": 3 }),
            metrics: vec![
                Metric::new("journal_us_p99", value, "us", "Journal overhead p99").headline(),
                Metric::new("note, with comma", 1.5, "x", "has \"quotes\""),
            ],
            notes: vec!["real Mistral calls".to_string()],
        }
    }

    #[test]
    fn file_stem_strips_separators() {
        assert_eq!(
            report("overhead", "2026-10-02T12:30:00Z", 1.0).file_stem(),
            "overhead_20261002T123000Z"
        );
    }

    #[test]
    fn write_then_load_latest_per_benchmark() {
        let dir = tempfile::tempdir().unwrap();
        report("overhead", "2026-10-02T10:00:00Z", 100.0)
            .write(dir.path())
            .unwrap();
        report("overhead", "2026-10-02T11:00:00Z", 200.0)
            .write(dir.path())
            .unwrap();
        report("recovery", "2026-10-02T09:00:00Z", 5.0)
            .write(dir.path())
            .unwrap();
        std::fs::write(dir.path().join("junk.json"), "{}").unwrap();

        let latest = load_latest_reports(dir.path()).unwrap();
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[0].0, "overhead_20261002T110000Z.json");
        assert_eq!(latest[0].1.metrics[0].value, 200.0);
        assert_eq!(latest[1].1.benchmark, "recovery");
    }

    #[test]
    fn csv_escapes_commas_and_quotes() {
        let csv = report("b", "t", 1.0).to_csv();
        assert!(csv.starts_with("benchmark,timestamp,metric,value,unit,description\n"));
        assert!(csv.contains("\"note, with comma\",1.5,x,\"has \"\"quotes\"\"\""));
    }

    #[test]
    fn markdown_shows_numbers_with_their_source_file() {
        let dir = tempfile::tempdir().unwrap();
        report("overhead", "2026-10-02T11:00:00Z", 412.0)
            .write(dir.path())
            .unwrap();
        let path = write_benchmarks_md(dir.path()).unwrap();
        let md = std::fs::read_to_string(path).unwrap();
        assert!(md.contains("## Headline numbers"));
        assert!(md.contains("| Journal overhead p99 | 412 us | overhead |"));
        assert!(md.contains("[overhead_20261002T110000Z.json](overhead_20261002T110000Z.json)"));
        assert!(md.contains("Test CPU (8 logical cores)"));
        assert!(md.contains("commit `abc1234`"));
    }

    #[test]
    fn empty_results_dir_renders_placeholder() {
        let dir = tempfile::tempdir().unwrap();
        let md = render_benchmarks_md(&load_latest_reports(dir.path()).unwrap());
        assert!(md.contains("No benchmark results yet."));
    }

    #[test]
    fn value_formatting() {
        assert_eq!(fmt_value(5.0), "5");
        assert_eq!(fmt_value(1234.56), "1234.6");
        assert_eq!(fmt_value(12.345), "12.35");
        assert_eq!(fmt_value(0.000144), "0.000144");
    }
}
