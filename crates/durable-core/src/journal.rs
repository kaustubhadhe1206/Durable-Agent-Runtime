//! The per-run journal: an append-only SQLite log of agent steps.
//!
//! Durability contract:
//! - A step is recorded as `PENDING` *before* it executes and updated to `COMPLETED`
//!   (or `FAILED`) *after*. Each write is a single autocommitted statement, so it is atomic.
//! - The database runs in WAL mode. With `SyncMode::Full`, every commit is fsynced, so a
//!   committed write survives a process crash *and* an OS crash / power loss. With
//!   `SyncMode::Normal`, commits survive a process crash (`kill -9`) but the last few may be
//!   lost on power loss. The tradeoff is benchmarked in Phase 5.
//! - A step left `PENDING` after a restart means the process died mid-step. The replay engine
//!   re-executes it (at-least-once); side effects rely on idempotency keys to stay exactly-once.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS steps (
    seq             INTEGER PRIMARY KEY,
    step_name       TEXT NOT NULL,
    step_key        TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('llm', 'tool', 'side_effect')),
    status          TEXT NOT NULL CHECK (status IN ('PENDING', 'COMPLETED', 'FAILED')),
    input_json      TEXT NOT NULL,
    output_json     TEXT,
    idempotency_key TEXT,
    tokens_in       INTEGER NOT NULL DEFAULT 0,
    tokens_out      INTEGER NOT NULL DEFAULT 0,
    started_at_us   INTEGER NOT NULL,
    completed_at_us INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS steps_idempotency_key
    ON steps (idempotency_key) WHERE idempotency_key IS NOT NULL;
";

const SELECT_COLUMNS: &str = "seq, step_name, step_key, kind, status, input_json, output_json,
     idempotency_key, tokens_in, tokens_out, started_at_us, completed_at_us";

/// Errors returned by the journal.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("step {0} already exists in the journal")]
    DuplicateSeq(u64),
    #[error("idempotency key {0:?} is already used by another step")]
    DuplicateIdempotencyKey(String),
    #[error("step {0} not found in the journal")]
    StepNotFound(u64),
    #[error("step {seq}: cannot move from {from} to {to}")]
    InvalidTransition {
        seq: u64,
        from: StepStatus,
        to: StepStatus,
    },
    #[error("value {0} does not fit in a SQLite integer")]
    OutOfRange(u64),
    #[error("corrupt journal row: {0}")]
    CorruptRow(String),
    #[error("could not enable WAL mode (journal_mode is {0:?})")]
    WalUnavailable(String),
}

pub type Result<T> = std::result::Result<T, JournalError>;

/// How aggressively SQLite fsyncs. See the module docs for the durability tradeoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncMode {
    /// fsync on every commit: survives process crash and power loss.
    #[default]
    Full,
    /// fsync only at WAL checkpoints: survives process crash, may lose recent commits on power loss.
    Normal,
}

impl SyncMode {
    fn pragma_value(self) -> &'static str {
        match self {
            SyncMode::Full => "FULL",
            SyncMode::Normal => "NORMAL",
        }
    }
}

/// What kind of work a step performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    Llm,
    Tool,
    SideEffect,
}

impl StepKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StepKind::Llm => "llm",
            StepKind::Tool => "tool",
            StepKind::SideEffect => "side_effect",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "llm" => Ok(StepKind::Llm),
            "tool" => Ok(StepKind::Tool),
            "side_effect" => Ok(StepKind::SideEffect),
            other => Err(JournalError::CorruptRow(format!("unknown kind {other:?}"))),
        }
    }
}

/// Lifecycle state of a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    Pending,
    Completed,
    Failed,
}

impl StepStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            StepStatus::Pending => "PENDING",
            StepStatus::Completed => "COMPLETED",
            StepStatus::Failed => "FAILED",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "PENDING" => Ok(StepStatus::Pending),
            "COMPLETED" => Ok(StepStatus::Completed),
            "FAILED" => Ok(StepStatus::Failed),
            other => Err(JournalError::CorruptRow(format!(
                "unknown status {other:?}"
            ))),
        }
    }
}

impl std::fmt::Display for StepStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A step about to start executing; written as `PENDING`.
#[derive(Debug, Clone)]
pub struct NewStep<'a> {
    pub seq: u64,
    pub step_name: &'a str,
    pub step_key: &'a str,
    pub kind: StepKind,
    pub input_json: &'a str,
    pub idempotency_key: Option<&'a str>,
}

/// One row of the journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepRecord {
    pub seq: u64,
    pub step_name: String,
    pub step_key: String,
    pub kind: StepKind,
    pub status: StepStatus,
    pub input_json: String,
    /// The step's output when `COMPLETED`, or the error when `FAILED`.
    pub output_json: Option<String>,
    pub idempotency_key: Option<String>,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub started_at_us: i64,
    pub completed_at_us: Option<i64>,
}

/// Raw column values, converted to `StepRecord` outside the rusqlite row closure so that
/// parse failures can be reported as `JournalError` rather than `rusqlite::Error`.
type RawRow = (
    i64,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    i64,
    i64,
    i64,
    Option<i64>,
);

/// A SQLite-backed journal for a single run.
pub struct Journal {
    conn: Connection,
    path: PathBuf,
}

impl Journal {
    /// Opens (or creates) the journal at `path` with `SyncMode::Full`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(path, SyncMode::Full)
    }

    /// Opens (or creates) the journal at `path` with the given sync mode.
    pub fn open_with(path: impl AsRef<Path>, sync: SyncMode) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let conn = Connection::open(&path)?;

        // `PRAGMA journal_mode` returns the resulting mode as a row, so query it rather than
        // using `execute`, and verify WAL was actually enabled.
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(JournalError::WalUnavailable(mode));
        }
        conn.pragma_update(None, "synchronous", sync.pragma_value())?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.execute_batch(SCHEMA)?;

        Ok(Self { conn, path })
    }

    /// Path of the underlying SQLite file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Records a step as `PENDING`. Must be called before the step executes.
    pub fn append_pending(&self, step: &NewStep<'_>) -> Result<()> {
        let seq = to_sql_int(step.seq)?;
        let result = self.conn.execute(
            "INSERT INTO steps (seq, step_name, step_key, kind, status, input_json,
                                idempotency_key, started_at_us)
             VALUES (?1, ?2, ?3, ?4, 'PENDING', ?5, ?6, ?7)",
            params![
                seq,
                step.step_name,
                step.step_key,
                step.kind.as_str(),
                step.input_json,
                step.idempotency_key,
                now_us(),
            ],
        );
        match result {
            Ok(_) => Ok(()),
            Err(e) if is_unique_violation(&e) => {
                // Two unique constraints can fire: the primary key and the idempotency index.
                if self.get_step(step.seq)?.is_some() {
                    Err(JournalError::DuplicateSeq(step.seq))
                } else {
                    Err(JournalError::DuplicateIdempotencyKey(
                        step.idempotency_key.unwrap_or_default().to_string(),
                    ))
                }
            }
            Err(e) => Err(e.into()),
        }
    }

    /// Moves a `PENDING` step to `COMPLETED` with its output and token usage.
    pub fn complete_step(
        &self,
        seq: u64,
        output_json: &str,
        tokens_in: u64,
        tokens_out: u64,
    ) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE steps
             SET status = 'COMPLETED', output_json = ?2, tokens_in = ?3, tokens_out = ?4,
                 completed_at_us = ?5
             WHERE seq = ?1 AND status = 'PENDING'",
            params![
                to_sql_int(seq)?,
                output_json,
                to_sql_int(tokens_in)?,
                to_sql_int(tokens_out)?,
                now_us(),
            ],
        )?;
        self.check_transition(seq, changed, StepStatus::Completed)
    }

    /// Moves a `PENDING` step to `FAILED`, storing the error description.
    pub fn fail_step(&self, seq: u64, error_json: &str) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE steps SET status = 'FAILED', output_json = ?2, completed_at_us = ?3
             WHERE seq = ?1 AND status = 'PENDING'",
            params![to_sql_int(seq)?, error_json, now_us()],
        )?;
        self.check_transition(seq, changed, StepStatus::Failed)
    }

    /// Resets a `PENDING` (crashed mid-step) or `FAILED` step back to a fresh `PENDING`
    /// attempt before re-executing it. `COMPLETED` steps can never be retried.
    pub fn restart_step(&self, seq: u64) -> Result<()> {
        let changed = self.conn.execute(
            "UPDATE steps
             SET status = 'PENDING', output_json = NULL, tokens_in = 0, tokens_out = 0,
                 started_at_us = ?2, completed_at_us = NULL
             WHERE seq = ?1 AND status IN ('PENDING', 'FAILED')",
            params![to_sql_int(seq)?, now_us()],
        )?;
        self.check_transition(seq, changed, StepStatus::Pending)
    }

    /// Reads the step at `seq`, if any.
    pub fn get_step(&self, seq: u64) -> Result<Option<StepRecord>> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM steps WHERE seq = ?1");
        let raw = self
            .conn
            .query_row(&sql, params![to_sql_int(seq)?], read_raw)
            .optional()?;
        raw.map(into_record).transpose()
    }

    /// Finds the step that used `key` as its idempotency key, if any.
    pub fn find_by_idempotency_key(&self, key: &str) -> Result<Option<StepRecord>> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM steps WHERE idempotency_key = ?1");
        let raw = self
            .conn
            .query_row(&sql, params![key], read_raw)
            .optional()?;
        raw.map(into_record).transpose()
    }

    /// All steps, ordered by `seq`.
    pub fn steps(&self) -> Result<Vec<StepRecord>> {
        let sql = format!("SELECT {SELECT_COLUMNS} FROM steps ORDER BY seq");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], read_raw)?;
        let mut out = Vec::new();
        for raw in rows {
            out.push(into_record(raw?)?);
        }
        Ok(out)
    }

    /// Highest recorded `seq`, or `None` for an empty journal.
    pub fn last_seq(&self) -> Result<Option<u64>> {
        let max: Option<i64> = self
            .conn
            .query_row("SELECT MAX(seq) FROM steps", [], |row| row.get(0))?;
        max.map(from_sql_int).transpose()
    }

    /// Turns a zero-rows-changed UPDATE into a precise error.
    fn check_transition(&self, seq: u64, changed: usize, to: StepStatus) -> Result<()> {
        if changed > 0 {
            return Ok(());
        }
        match self.get_step(seq)? {
            None => Err(JournalError::StepNotFound(seq)),
            Some(rec) => Err(JournalError::InvalidTransition {
                seq,
                from: rec.status,
                to,
            }),
        }
    }
}

fn read_raw(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
    ))
}

fn into_record(raw: RawRow) -> Result<StepRecord> {
    let (
        seq,
        step_name,
        step_key,
        kind,
        status,
        input_json,
        output_json,
        idempotency_key,
        tokens_in,
        tokens_out,
        started_at_us,
        completed_at_us,
    ) = raw;
    Ok(StepRecord {
        seq: from_sql_int(seq)?,
        step_name,
        step_key,
        kind: StepKind::parse(&kind)?,
        status: StepStatus::parse(&status)?,
        input_json,
        output_json,
        idempotency_key,
        tokens_in: from_sql_int(tokens_in)?,
        tokens_out: from_sql_int(tokens_out)?,
        started_at_us,
        completed_at_us,
    })
}

fn to_sql_int(v: u64) -> Result<i64> {
    i64::try_from(v).map_err(|_| JournalError::OutOfRange(v))
}

fn from_sql_int(v: i64) -> Result<u64> {
    u64::try_from(v).map_err(|_| JournalError::CorruptRow(format!("negative integer {v}")))
}

fn is_unique_violation(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(err, _)
            if err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                || err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}

/// Microseconds since the Unix epoch. A clock before 1970 is treated as 0.
fn now_us() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_micros()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new_step<'a>(seq: u64, name: &'a str, idem: Option<&'a str>) -> NewStep<'a> {
        NewStep {
            seq,
            step_name: name,
            step_key: "key",
            kind: StepKind::Llm,
            input_json: r#"{"prompt":"hi"}"#,
            idempotency_key: idem,
        }
    }

    fn temp_journal() -> (tempfile::TempDir, Journal) {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path().join("run.db")).unwrap();
        (dir, journal)
    }

    #[test]
    fn append_writes_pending_row() {
        let (_dir, j) = temp_journal();
        j.append_pending(&new_step(1, "plan", None)).unwrap();

        let rec = j.get_step(1).unwrap().unwrap();
        assert_eq!(rec.status, StepStatus::Pending);
        assert_eq!(rec.step_name, "plan");
        assert_eq!(rec.kind, StepKind::Llm);
        assert_eq!(rec.output_json, None);
        assert_eq!(rec.completed_at_us, None);
        assert!(rec.started_at_us > 0);
    }

    #[test]
    fn complete_records_output_and_tokens() {
        let (_dir, j) = temp_journal();
        j.append_pending(&new_step(1, "plan", None)).unwrap();
        j.complete_step(1, r#""the plan""#, 12, 34).unwrap();

        let rec = j.get_step(1).unwrap().unwrap();
        assert_eq!(rec.status, StepStatus::Completed);
        assert_eq!(rec.output_json.as_deref(), Some(r#""the plan""#));
        assert_eq!((rec.tokens_in, rec.tokens_out), (12, 34));
        assert!(rec.completed_at_us.unwrap() >= rec.started_at_us);
    }

    #[test]
    fn read_by_seq_and_missing_seq() {
        let (_dir, j) = temp_journal();
        for seq in 1..=3 {
            j.append_pending(&new_step(seq, &format!("s{seq}"), None))
                .unwrap();
        }
        assert_eq!(j.get_step(2).unwrap().unwrap().step_name, "s2");
        assert!(j.get_step(99).unwrap().is_none());
        assert_eq!(j.last_seq().unwrap(), Some(3));
        let seqs: Vec<u64> = j.steps().unwrap().iter().map(|s| s.seq).collect();
        assert_eq!(seqs, vec![1, 2, 3]);
    }

    #[test]
    fn empty_journal_has_no_last_seq() {
        let (_dir, j) = temp_journal();
        assert_eq!(j.last_seq().unwrap(), None);
        assert!(j.steps().unwrap().is_empty());
    }

    #[test]
    fn duplicate_seq_is_rejected() {
        let (_dir, j) = temp_journal();
        j.append_pending(&new_step(1, "plan", None)).unwrap();
        let err = j.append_pending(&new_step(1, "plan", None)).unwrap_err();
        assert!(matches!(err, JournalError::DuplicateSeq(1)));
    }

    #[test]
    fn duplicate_idempotency_key_is_rejected() {
        let (_dir, j) = temp_journal();
        j.append_pending(&new_step(1, "save", Some("report-1")))
            .unwrap();
        let err = j
            .append_pending(&new_step(2, "save", Some("report-1")))
            .unwrap_err();
        assert!(matches!(err, JournalError::DuplicateIdempotencyKey(k) if k == "report-1"));
        assert_eq!(
            j.find_by_idempotency_key("report-1").unwrap().unwrap().seq,
            1
        );
    }

    #[test]
    fn completed_step_cannot_change() {
        let (_dir, j) = temp_journal();
        j.append_pending(&new_step(1, "plan", None)).unwrap();
        j.complete_step(1, "1", 0, 0).unwrap();

        let err = j.complete_step(1, "2", 0, 0).unwrap_err();
        assert!(matches!(
            err,
            JournalError::InvalidTransition {
                from: StepStatus::Completed,
                to: StepStatus::Completed,
                ..
            }
        ));
        assert!(j.fail_step(1, "boom").is_err());
        assert!(j.restart_step(1).is_err());
        assert_eq!(
            j.get_step(1).unwrap().unwrap().output_json.as_deref(),
            Some("1")
        );
    }

    #[test]
    fn missing_step_transition_reports_not_found() {
        let (_dir, j) = temp_journal();
        assert!(matches!(
            j.complete_step(7, "x", 0, 0).unwrap_err(),
            JournalError::StepNotFound(7)
        ));
    }

    #[test]
    fn failed_step_can_be_restarted_and_completed() {
        let (_dir, j) = temp_journal();
        j.append_pending(&new_step(1, "search", None)).unwrap();
        j.fail_step(1, r#""timeout""#).unwrap();
        assert_eq!(j.get_step(1).unwrap().unwrap().status, StepStatus::Failed);

        j.restart_step(1).unwrap();
        let rec = j.get_step(1).unwrap().unwrap();
        assert_eq!(rec.status, StepStatus::Pending);
        assert_eq!(rec.output_json, None);

        j.complete_step(1, r#""ok""#, 1, 1).unwrap();
        assert_eq!(
            j.get_step(1).unwrap().unwrap().status,
            StepStatus::Completed
        );
    }

    #[test]
    fn reopen_after_drop_preserves_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.db");
        {
            let j = Journal::open(&path).unwrap();
            j.append_pending(&new_step(1, "plan", None)).unwrap();
            j.complete_step(1, r#""p""#, 5, 6).unwrap();
            j.append_pending(&new_step(2, "search", None)).unwrap();
        }
        let j = Journal::open(&path).unwrap();
        let steps = j.steps().unwrap();
        assert_eq!(steps.len(), 2);
        assert_eq!(steps[0].status, StepStatus::Completed);
        assert_eq!(steps[0].tokens_out, 6);
        assert_eq!(steps[1].status, StepStatus::Pending);
    }

    #[test]
    fn pending_row_left_behind_can_be_retried() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("run.db");
        {
            let j = Journal::open(&path).unwrap();
            j.append_pending(&new_step(1, "plan", None)).unwrap();
            // Dropped without completing: simulates dying mid-step.
        }
        let j = Journal::open(&path).unwrap();
        let rec = j.get_step(1).unwrap().unwrap();
        assert_eq!(rec.status, StepStatus::Pending);

        j.restart_step(1).unwrap();
        j.complete_step(1, r#""p""#, 1, 1).unwrap();
        assert_eq!(
            j.get_step(1).unwrap().unwrap().status,
            StepStatus::Completed
        );
    }

    #[test]
    fn normal_sync_mode_opens_and_works() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::open_with(dir.path().join("run.db"), SyncMode::Normal).unwrap();
        j.append_pending(&new_step(1, "plan", None)).unwrap();
        j.complete_step(1, "1", 0, 0).unwrap();
        assert_eq!(j.steps().unwrap().len(), 1);
    }
}
