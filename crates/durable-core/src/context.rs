//! `DurableContext`: the developer-facing step API.
//!
//! ```ignore
//! let ctx = DurableContext::open("run-42").await?;
//! let plan = ctx.step("plan", &input, || async { do_work(&input).await }).await?;
//! let text = ctx.llm_step("draft", &plan, || async { llm_call(&plan).await }).await?;
//! ctx.side_effect("send_email", "email-run-42", || async { send().await }).await?;
//! ```
//!
//! Each call takes the next sequence number, so steps must be awaited one after another
//! (not concurrently via `join!`); otherwise the seq order would differ between runs.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::journal::{Journal, JournalError, StepKind, SyncMode};
use crate::replay::{Decision, NonDeterminismError, ReplayError, Replayer, StepRequest};

// [Phase 2]
/// Error type user closures may return: anything convertible to a boxed error
/// (`std::io::Error`, `String`, `anyhow::Error`, ...). `Send + Sync` lets it cross threads.
pub type BoxError = Box<dyn std::error::Error + Send + Sync>;

// [Phase 2]
/// Directory for run journals when none is given. Overridable via `DURABLE_RUNS_DIR`.
pub const DEFAULT_RUNS_DIR: &str = "runs";

// [Phase 2]
/// Errors returned by `DurableContext`.
#[derive(Debug, thiserror::Error)]
pub enum DurableError {
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    NonDeterminism(#[from] NonDeterminismError),
    #[error("step {seq} ({name}) failed: {source}")]
    StepFailed {
        seq: u64,
        name: String,
        source: BoxError,
    },
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid run id {0:?}: use only letters, digits, '-' and '_'")]
    InvalidRunId(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("journal for step {seq} is COMPLETED but has no output")]
    MissingOutput { seq: u64 },
    #[error("context lock poisoned (a previous step panicked)")]
    LockPoisoned,
}

// [Phase 2]
impl From<ReplayError> for DurableError {
    // [Phase 2]
    fn from(e: ReplayError) -> Self {
        match e {
            ReplayError::Journal(j) => DurableError::Journal(j),
            ReplayError::NonDeterminism(nd) => DurableError::NonDeterminism(nd),
            ReplayError::MissingOutput { seq } => DurableError::MissingOutput { seq },
        }
    }
}

// [Phase 2]
pub type Result<T> = std::result::Result<T, DurableError>;

// [Phase 2]
/// Token counts reported by an LLM step.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub tokens_in: u64,
    pub tokens_out: u64,
}

// [Phase 2]
/// Describes one step for `DurableContext::execute`.
#[derive(Debug, Clone, Copy)]
pub struct StepSpec<'a, I: ?Sized> {
    pub name: &'a str,
    pub kind: StepKind,
    pub input: &'a I,
    pub idempotency_key: Option<&'a str>,
}

// [Phase 2]
/// The result of a step plus how it was obtained.
#[derive(Debug, Clone, PartialEq)]
pub struct StepOutcome<T> {
    pub seq: u64,
    pub value: T,
    /// `true` if served from the journal without running the closure.
    pub replayed: bool,
    pub usage: TokenUsage,
    // [Phase 5]
    /// Time spent writing/reading the journal for this step, in microseconds
    /// (the durability overhead; excludes the step's own work).
    pub journal_us: u64,
}

// [Phase 2]
/// Counters for one `DurableContext` (one process lifetime of a run).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunStats {
    pub executed: u64,
    pub replayed: u64,
    /// Tokens spent by steps executed in this process.
    pub tokens_executed: TokenUsage,
    /// Tokens of replayed steps: what a naive restart would have paid again.
    pub tokens_replayed: TokenUsage,
}

// [Phase 2]
struct Inner {
    replayer: Replayer,
    next_seq: u64,
    stats: RunStats,
}

// [Phase 2]
/// A durable run. Steps executed through it are journaled and replayed after a restart.
pub struct DurableContext {
    run_id: String,
    // std Mutex (not tokio's): it is only held for short synchronous journal calls and is
    // never held across an `.await`, so it can't block the async runtime or deadlock.
    inner: Mutex<Inner>,
}

// [Phase 2]
impl DurableContext {
    // [Phase 2]
    /// Opens run `run_id` in `$DURABLE_RUNS_DIR` (default `./runs`), creating it if needed.
    pub async fn open(run_id: &str) -> Result<Self> {
        let dir = std::env::var_os("DURABLE_RUNS_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_RUNS_DIR));
        Self::open_in(dir, run_id).await
    }

    // [Phase 2]
    /// Opens run `run_id` in `dir` with `SyncMode::Full`.
    pub async fn open_in(dir: impl AsRef<Path>, run_id: &str) -> Result<Self> {
        Self::open_with(dir, run_id, SyncMode::Full).await
    }

    // [Phase 2]
    /// Opens run `run_id` in `dir` with an explicit sync mode. The journal file is
    /// `<dir>/<run_id>.db`.
    pub async fn open_with(dir: impl AsRef<Path>, run_id: &str, sync: SyncMode) -> Result<Self> {
        validate_run_id(run_id)?;
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let journal = Journal::open_with(dir.join(format!("{run_id}.db")), sync)?;
        Ok(Self {
            run_id: run_id.to_string(),
            inner: Mutex::new(Inner {
                replayer: Replayer::new(journal),
                next_seq: 1,
                stats: RunStats::default(),
            }),
        })
    }

    // [Phase 2]
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    // [Phase 2]
    /// Counters for steps run through this context so far.
    pub fn stats(&self) -> Result<RunStats> {
        Ok(self.lock()?.stats)
    }

    // [Phase 2]
    /// A general step (kind `tool`). Returns the cached value on replay.
    pub async fn step<I, T, F, Fut, E>(&self, name: &str, input: &I, f: F) -> Result<T>
    where
        I: Serialize + ?Sized,
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = std::result::Result<T, E>>,
        E: Into<BoxError>,
    {
        let spec = StepSpec {
            name,
            kind: StepKind::Tool,
            input,
            idempotency_key: None,
        };
        let outcome = self
            .execute(spec, move || async move {
                f().await.map(|v| (v, TokenUsage::default()))
            })
            .await?;
        Ok(outcome.value)
    }

    // [Phase 2]
    /// An LLM step: the closure also reports token usage, which is journaled.
    pub async fn llm_step<I, T, F, Fut, E>(&self, name: &str, input: &I, f: F) -> Result<T>
    where
        I: Serialize + ?Sized,
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = std::result::Result<(T, TokenUsage), E>>,
        E: Into<BoxError>,
    {
        let spec = StepSpec {
            name,
            kind: StepKind::Llm,
            input,
            idempotency_key: None,
        };
        Ok(self.execute(spec, f).await?.value)
    }

    // [Phase 2]
    /// A side effect guarded by `idempotency_key`. Once it completes it never runs again for
    /// this run. If the process dies *during* the effect, it re-runs on restart, so pass the
    /// same key to the external system so it can deduplicate.
    pub async fn side_effect<T, F, Fut, E>(
        &self,
        name: &str,
        idempotency_key: &str,
        f: F,
    ) -> Result<T>
    where
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = std::result::Result<T, E>>,
        E: Into<BoxError>,
    {
        let spec = StepSpec {
            name,
            kind: StepKind::SideEffect,
            input: idempotency_key,
            idempotency_key: Some(idempotency_key),
        };
        let outcome = self
            .execute(spec, move || async move {
                f().await.map(|v| (v, TokenUsage::default()))
            })
            .await?;
        Ok(outcome.value)
    }

    // [Phase 2]
    /// The general form behind `step`, `llm_step` and `side_effect`. Returns whether the
    /// value was replayed, which the demo uses for its [EXECUTED]/[REPLAYED] tags.
    pub async fn execute<I, T, F, Fut, E>(
        &self,
        spec: StepSpec<'_, I>,
        f: F,
    ) -> Result<StepOutcome<T>>
    where
        I: Serialize + ?Sized,
        T: Serialize + DeserializeOwned,
        F: FnOnce() -> Fut,
        Fut: Future<Output = std::result::Result<(T, TokenUsage), E>>,
        E: Into<BoxError>,
    {
        let input_json = serde_json::to_string(spec.input)?;

        // Stage 1 (locked): claim a seq and ask the replayer what to do.
        let begin_started = Instant::now(); // [Phase 5] journal timing
        let (seq, decision) = {
            let mut inner = self.lock()?;
            let seq = inner.next_seq;
            let decision = inner.replayer.begin(&StepRequest {
                seq,
                name: spec.name,
                kind: spec.kind,
                input_json: &input_json,
                idempotency_key: spec.idempotency_key,
            })?;
            inner.next_seq += 1;
            if let Decision::Replay {
                tokens_in,
                tokens_out,
                ..
            } = &decision
            {
                inner.stats.replayed += 1;
                inner.stats.tokens_replayed.tokens_in += tokens_in;
                inner.stats.tokens_replayed.tokens_out += tokens_out;
            }
            (seq, decision)
        }; // guard dropped here, before any `.await`
        let begin_us = micros_since(begin_started);

        if let Decision::Replay {
            output_json,
            tokens_in,
            tokens_out,
        } = decision
        {
            return Ok(StepOutcome {
                seq,
                value: serde_json::from_str(&output_json)?,
                replayed: true,
                usage: TokenUsage {
                    tokens_in,
                    tokens_out,
                },
                journal_us: begin_us,
            });
        }

        // Stage 2 (unlocked): run the user's closure. The step is already durably PENDING.
        let result = f().await;

        // Stage 3 (locked): record the result.
        let record_started = Instant::now(); // [Phase 5] journal timing
        let mut inner = self.lock()?;
        match result {
            Ok((value, usage)) => {
                let output_json = serde_json::to_string(&value)?;
                inner.replayer.record_success(
                    seq,
                    &output_json,
                    usage.tokens_in,
                    usage.tokens_out,
                )?;
                inner.stats.executed += 1;
                inner.stats.tokens_executed.tokens_in += usage.tokens_in;
                inner.stats.tokens_executed.tokens_out += usage.tokens_out;
                Ok(StepOutcome {
                    seq,
                    value,
                    replayed: false,
                    usage,
                    journal_us: begin_us + micros_since(record_started),
                })
            }
            Err(e) => {
                let source: BoxError = e.into();
                inner.replayer.record_failure(seq, &source.to_string())?;
                Err(DurableError::StepFailed {
                    seq,
                    name: spec.name.to_string(),
                    source,
                })
            }
        }
    }

    // [Phase 2]
    fn lock(&self) -> Result<MutexGuard<'_, Inner>> {
        self.inner.lock().map_err(|_| DurableError::LockPoisoned)
    }
}

// [Phase 5]
fn micros_since(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_micros()).unwrap_or(u64::MAX)
}

// [Phase 2]
/// Run ids become file names, so restrict them to a safe character set (no path traversal).
fn validate_run_id(run_id: &str) -> Result<()> {
    let ok = !run_id.is_empty()
        && run_id.len() <= 128
        && run_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err(DurableError::InvalidRunId(run_id.to_string()))
    }
}

// [Phase 2]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_validation() {
        assert!(validate_run_id("run-42_a").is_ok());
        for bad in ["", "../x", "a/b", "a b", "a.db"] {
            assert!(validate_run_id(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
