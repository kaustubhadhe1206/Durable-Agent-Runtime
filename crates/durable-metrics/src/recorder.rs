//! Stores raw benchmark samples and counters in a separate `metrics.db`, so every summary
//! number can be recomputed from the underlying measurements.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

use crate::Result;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS samples (
    id             INTEGER PRIMARY KEY,
    session        TEXT NOT NULL,
    benchmark      TEXT NOT NULL,
    metric         TEXT NOT NULL,
    value          REAL NOT NULL,
    unit           TEXT NOT NULL,
    recorded_at_us INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS samples_lookup ON samples (session, benchmark, metric);
CREATE TABLE IF NOT EXISTS counters (
    session   TEXT NOT NULL,
    benchmark TEXT NOT NULL,
    name      TEXT NOT NULL,
    value     INTEGER NOT NULL,
    PRIMARY KEY (session, benchmark, name)
);
";

// [Phase 5]
/// Records samples for one benchmark session (one launch of the bench program).
pub struct Recorder {
    conn: Connection,
    session: String,
}

// [Phase 5]
impl Recorder {
    // [Phase 5]
    /// Opens (or creates) `metrics.db` at `path`; samples are tagged with `session`.
    pub fn open(path: impl AsRef<Path>, session: impl Into<String>) -> Result<Self> {
        let conn = Connection::open(path)?;
        // Metrics are not the durability-critical path, so NORMAL sync is enough here.
        let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn,
            session: session.into(),
        })
    }

    // [Phase 5]
    pub fn session(&self) -> &str {
        &self.session
    }

    // [Phase 5]
    /// Stores one measurement, e.g. `("overhead", "journal_us", 412.0, "us")`.
    pub fn record(&self, benchmark: &str, metric: &str, value: f64, unit: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO samples (session, benchmark, metric, value, unit, recorded_at_us)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![self.session, benchmark, metric, value, unit, now_us()],
        )?;
        Ok(())
    }

    // [Phase 5]
    /// Adds `delta` to a named counter.
    pub fn add(&self, benchmark: &str, name: &str, delta: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO counters (session, benchmark, name, value) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (session, benchmark, name) DO UPDATE SET value = value + excluded.value",
            params![self.session, benchmark, name, delta],
        )?;
        Ok(())
    }

    // [Phase 5]
    /// All samples of `metric` for `benchmark` in this session, in recording order.
    pub fn samples(&self, benchmark: &str, metric: &str) -> Result<Vec<f64>> {
        let mut stmt = self.conn.prepare(
            "SELECT value FROM samples WHERE session = ?1 AND benchmark = ?2 AND metric = ?3
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![self.session, benchmark, metric], |r| r.get(0))?;
        let mut out = Vec::new();
        for v in rows {
            out.push(v?);
        }
        Ok(out)
    }

    // [Phase 5]
    /// Current value of a counter (0 if never incremented).
    pub fn counter(&self, benchmark: &str, name: &str) -> Result<i64> {
        let v: Option<i64> = self
            .conn
            .query_row(
                "SELECT value FROM counters WHERE session = ?1 AND benchmark = ?2 AND name = ?3",
                params![self.session, benchmark, name],
                |r| r.get(0),
            )
            .or_else(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => Ok(None),
                other => Err(other),
            })?;
        Ok(v.unwrap_or(0))
    }
}

// [Phase 5]
fn now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

// [Phase 5]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_round_trip_per_session() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metrics.db");
        let a = Recorder::open(&path, "s1").unwrap();
        a.record("overhead", "journal_us", 10.0, "us").unwrap();
        a.record("overhead", "journal_us", 20.0, "us").unwrap();
        a.record("overhead", "other", 1.0, "us").unwrap();
        assert_eq!(
            a.samples("overhead", "journal_us").unwrap(),
            vec![10.0, 20.0]
        );

        // A different session doesn't see s1's samples, but the file keeps both.
        let b = Recorder::open(&path, "s2").unwrap();
        assert!(b.samples("overhead", "journal_us").unwrap().is_empty());
        drop(a);
        let a2 = Recorder::open(&path, "s1").unwrap();
        assert_eq!(a2.samples("overhead", "journal_us").unwrap().len(), 2);
    }

    #[test]
    fn counters_accumulate() {
        let dir = tempfile::tempdir().unwrap();
        let r = Recorder::open(dir.path().join("m.db"), "s").unwrap();
        assert_eq!(r.counter("b", "errors").unwrap(), 0);
        r.add("b", "errors", 2).unwrap();
        r.add("b", "errors", 3).unwrap();
        assert_eq!(r.counter("b", "errors").unwrap(), 5);
    }
}
