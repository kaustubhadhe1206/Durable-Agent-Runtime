//! [Phase 5] End-to-end: run the real bench binary (small settings, real Mistral calls) and
//! check it writes JSON + CSV results and a BENCHMARKS.md built from them.
//! Skipped (passes with a notice) when MISTRAL_API_KEY is not available.

use std::path::Path;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_bench");

// [Phase 5]
fn has_key(test: &str) -> bool {
    let _ = dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env"));
    let ok = std::env::var("MISTRAL_API_KEY").is_ok_and(|k| !k.trim().is_empty());
    if !ok {
        eprintln!("SKIPPED {test}: MISTRAL_API_KEY not set");
    }
    ok
}

// [Phase 5]
fn bench(results: &Path, args: &[&str]) {
    let out = Command::new(BIN)
        .arg("--allow-debug") // tests run the debug build
        .arg("--results-dir")
        .arg(results)
        .args(args)
        .output()
        .expect("failed to launch bench");
    assert!(
        out.status.success(),
        "bench {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

// [Phase 5]
fn files_with(results: &Path, prefix: &str, ext: &str) -> Vec<String> {
    std::fs::read_dir(results)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with(prefix) && n.ends_with(ext))
        .collect()
}

// [Phase 5]
#[test]
fn overhead_and_throughput_write_traceable_results() {
    if !has_key("bench_live") {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    bench(dir.path(), &["overhead", "--steps", "3"]);
    bench(
        dir.path(),
        &["throughput", "--concurrency", "1,2", "--steps-per-run", "1"],
    );

    for name in ["overhead_", "throughput_"] {
        assert_eq!(
            files_with(dir.path(), name, ".json").len(),
            1,
            "{name} json"
        );
        assert_eq!(files_with(dir.path(), name, ".csv").len(), 1, "{name} csv");
    }

    // Every number in BENCHMARKS.md must come from a JSON file: check a value round-trips.
    let json_name = &files_with(dir.path(), "overhead_", ".json")[0];
    let report: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join(json_name)).unwrap())
            .unwrap();
    let p99 = report["metrics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "journal_us_p99")
        .expect("journal_us_p99 metric");
    assert!(p99["value"].as_f64().unwrap() > 0.0);

    let md = std::fs::read_to_string(dir.path().join("BENCHMARKS.md")).unwrap();
    assert!(
        md.contains("## overhead") && md.contains("## throughput"),
        "{md}"
    );
    assert!(
        md.contains(json_name.as_str()),
        "BENCHMARKS.md must cite {json_name}"
    );
    assert!(md.contains("## Machine"));
    assert!(dir.path().join("metrics.db").exists(), "raw samples stored");
}

// [Phase 5]
#[test]
fn debug_build_refuses_without_flag() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(BIN)
        .arg("--results-dir")
        .arg(dir.path())
        .arg("all")
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("release mode"));
}
