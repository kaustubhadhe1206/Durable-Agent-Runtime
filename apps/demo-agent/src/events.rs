//! [Phase 5] Optional machine-readable event log (`--events PATH`), one JSON object per line.
//! The benchmark harness reads it to measure recovery time, duplicate calls and tokens.
//!
//! Each line is written straight to the file (no buffering), so lines written before a
//! crash (`abort()`) are still there afterwards.

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

// [Phase 5]
#[derive(Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event<'a> {
    /// Written first thing when the process starts.
    Start {
        unix_us: u64,
        mode: &'a str,
        run_id: &'a str,
    },
    /// Written when a step finishes (executed or replayed).
    Step {
        n: u32,
        kind: &'a str,
        replayed: bool,
        tokens_in: u64,
        tokens_out: u64,
        /// When the step began, as Unix time in microseconds.
        started_unix_us: u64,
        /// Wall time of the whole step, including the LLM call and journal writes.
        duration_us: u64,
        /// Time spent in the journal (0 in naive mode).
        journal_us: u64,
    },
    /// Written when the agent finished all steps.
    Done { unix_us: u64 },
}

// [Phase 5]
pub struct EventLog {
    file: File,
}

// [Phase 5]
impl EventLog {
    // [Phase 5]
    /// Creates (truncates) the event file.
    pub fn create(path: &Path) -> std::io::Result<Self> {
        Ok(Self {
            file: File::create(path)?,
        })
    }

    // [Phase 5]
    pub fn write(&mut self, event: &Event<'_>) -> anyhow::Result<()> {
        let line = serde_json::to_string(event)?;
        writeln!(self.file, "{line}")?;
        Ok(())
    }
}

// [Phase 5]
/// Current Unix time in microseconds.
pub fn now_unix_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

// [Phase 5]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_one_json_object_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut log = EventLog::create(&path).unwrap();
        log.write(&Event::Start {
            unix_us: 1,
            mode: "durable",
            run_id: "r",
        })
        .unwrap();
        log.write(&Event::Step {
            n: 1,
            kind: "llm",
            replayed: false,
            tokens_in: 3,
            tokens_out: 4,
            started_unix_us: 2,
            duration_us: 5,
            journal_us: 6,
        })
        .unwrap();
        drop(log);

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["event"], "start");
        assert_eq!(lines[1]["event"], "step");
        assert_eq!(lines[1]["journal_us"], 6);
    }
}
