//! The replay engine: decides, for each step, whether to return a cached result or execute.
//!
//! For the step at sequence number `seq`:
//! 1. No journal row → write `PENDING`, execute.
//! 2. Row exists but its `step_key` (or name) differs → `NonDeterminismError`. The code took a
//!    different path than the recorded run, so replaying cached results would be wrong.
//! 3. Row is `COMPLETED` with a matching key → replay: return the cached output, don't execute.
//! 4. Row is `PENDING` (crashed mid-step) or `FAILED` → reset it to a fresh `PENDING`, execute.
//!    This is at-least-once; side effects rely on their idempotency key to stay exactly-once.

use sha2::{Digest, Sha256};

use crate::journal::{Journal, JournalError, NewStep, StepKind, StepRecord, StepStatus};

/// The code asked for a different step than the journal recorded at this `seq`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "non-determinism at step {seq}: journal recorded {recorded_name:?} (key {recorded_key}), \
     but the code ran {actual_name:?} (key {actual_key})"
)]
pub struct NonDeterminismError {
    pub seq: u64,
    pub recorded_name: String,
    pub recorded_key: String,
    pub actual_name: String,
    pub actual_key: String,
}

/// Errors from the replay engine.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    NonDeterminism(#[from] NonDeterminismError),
    #[error("step {seq} is COMPLETED but has no recorded output")]
    MissingOutput { seq: u64 },
}

/// A step the code is about to run.
#[derive(Debug, Clone)]
pub struct StepRequest<'a> {
    pub seq: u64,
    pub name: &'a str,
    pub kind: StepKind,
    pub input_json: &'a str,
    pub idempotency_key: Option<&'a str>,
}

/// What the caller should do with a step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Return the cached output; do not run the step.
    Replay {
        output_json: String,
        tokens_in: u64,
        tokens_out: u64,
    },
    /// The step is recorded as `PENDING`; run it, then call `record_success`/`record_failure`.
    Execute,
}

/// `sha256(name || 0x00 || input_json)` as lowercase hex. The 0x00 separator keeps
/// `("ab", "c")` and `("a", "bc")` from hashing the same.
pub fn step_key(name: &str, input_json: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(name.as_bytes());
    hasher.update([0u8]);
    hasher.update(input_json.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Wraps a `Journal` and applies the replay rules from the module docs.
pub struct Replayer {
    journal: Journal,
}

impl Replayer {
    pub fn new(journal: Journal) -> Self {
        Self { journal }
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Decides whether to replay or execute `req`. On `Execute`, the step is already
    /// durably recorded as `PENDING` when this returns.
    pub fn begin(&self, req: &StepRequest<'_>) -> Result<Decision, ReplayError> {
        let key = step_key(req.name, req.input_json);

        let Some(existing) = self.journal.get_step(req.seq)? else {
            self.journal.append_pending(&NewStep {
                seq: req.seq,
                step_name: req.name,
                step_key: &key,
                kind: req.kind,
                input_json: req.input_json,
                idempotency_key: req.idempotency_key,
            })?;
            return Ok(Decision::Execute);
        };

        check_determinism(&existing, req.name, &key)?;

        match existing.status {
            StepStatus::Completed => {
                let output_json = existing
                    .output_json
                    .ok_or(ReplayError::MissingOutput { seq: req.seq })?;
                Ok(Decision::Replay {
                    output_json,
                    tokens_in: existing.tokens_in,
                    tokens_out: existing.tokens_out,
                })
            }
            StepStatus::Pending | StepStatus::Failed => {
                self.journal.restart_step(req.seq)?;
                Ok(Decision::Execute)
            }
        }
    }

    /// Marks an executing step `COMPLETED`.
    pub fn record_success(
        &self,
        seq: u64,
        output_json: &str,
        tokens_in: u64,
        tokens_out: u64,
    ) -> Result<(), ReplayError> {
        Ok(self
            .journal
            .complete_step(seq, output_json, tokens_in, tokens_out)?)
    }

    /// Marks an executing step `FAILED`; it will re-execute on the next run.
    pub fn record_failure(&self, seq: u64, error_message: &str) -> Result<(), ReplayError> {
        let error_json = serde_json::Value::String(error_message.to_string()).to_string();
        Ok(self.journal.fail_step(seq, &error_json)?)
    }
}

fn check_determinism(
    existing: &StepRecord,
    name: &str,
    key: &str,
) -> Result<(), NonDeterminismError> {
    if existing.step_key == key && existing.step_name == name {
        return Ok(());
    }
    Err(NonDeterminismError {
        seq: existing.seq,
        recorded_name: existing.step_name.clone(),
        recorded_key: existing.step_key.clone(),
        actual_name: name.to_string(),
        actual_key: key.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replayer() -> (tempfile::TempDir, Replayer) {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path().join("run.db")).unwrap();
        (dir, Replayer::new(journal))
    }

    fn req<'a>(seq: u64, name: &'a str, input: &'a str) -> StepRequest<'a> {
        StepRequest {
            seq,
            name,
            kind: StepKind::Tool,
            input_json: input,
            idempotency_key: None,
        }
    }

    #[test]
    fn step_key_is_deterministic_and_input_sensitive() {
        assert_eq!(step_key("plan", "1"), step_key("plan", "1"));
        assert_ne!(step_key("plan", "1"), step_key("plan", "2"));
        assert_ne!(step_key("plan", "1"), step_key("draft", "1"));
        assert_ne!(step_key("ab", "c"), step_key("a", "bc"));
        assert_eq!(step_key("plan", "1").len(), 64);
    }

    #[test]
    fn new_step_executes_and_is_recorded_pending() {
        let (_d, r) = replayer();
        assert_eq!(r.begin(&req(1, "plan", "1")).unwrap(), Decision::Execute);
        let rec = r.journal().get_step(1).unwrap().unwrap();
        assert_eq!(rec.status, StepStatus::Pending);
        assert_eq!(rec.step_key, step_key("plan", "1"));
    }

    #[test]
    fn completed_step_replays_cached_output() {
        let (_d, r) = replayer();
        r.begin(&req(1, "plan", "1")).unwrap();
        r.record_success(1, r#""out""#, 3, 4).unwrap();

        assert_eq!(
            r.begin(&req(1, "plan", "1")).unwrap(),
            Decision::Replay {
                output_json: r#""out""#.to_string(),
                tokens_in: 3,
                tokens_out: 4,
            }
        );
    }

    #[test]
    fn changed_input_is_non_deterministic() {
        let (_d, r) = replayer();
        r.begin(&req(1, "plan", "1")).unwrap();
        r.record_success(1, "0", 0, 0).unwrap();

        let err = r.begin(&req(1, "plan", "2")).unwrap_err();
        let ReplayError::NonDeterminism(nd) = err else {
            panic!("expected NonDeterminism, got {err:?}");
        };
        assert_eq!(nd.seq, 1);
        assert_eq!(nd.recorded_key, step_key("plan", "1"));
        assert_eq!(nd.actual_key, step_key("plan", "2"));
    }

    #[test]
    fn non_determinism_is_checked_for_pending_steps_too() {
        let (_d, r) = replayer();
        r.begin(&req(1, "plan", "1")).unwrap();
        assert!(matches!(
            r.begin(&req(1, "draft", "1")),
            Err(ReplayError::NonDeterminism(_))
        ));
    }

    #[test]
    fn pending_and_failed_steps_re_execute() {
        let (_d, r) = replayer();
        r.begin(&req(1, "a", "1")).unwrap(); // left PENDING
        assert_eq!(r.begin(&req(1, "a", "1")).unwrap(), Decision::Execute);

        r.record_failure(1, "boom").unwrap();
        let rec = r.journal().get_step(1).unwrap().unwrap();
        assert_eq!(rec.status, StepStatus::Failed);
        assert_eq!(rec.output_json.as_deref(), Some(r#""boom""#));

        assert_eq!(r.begin(&req(1, "a", "1")).unwrap(), Decision::Execute);
        assert_eq!(
            r.journal().get_step(1).unwrap().unwrap().status,
            StepStatus::Pending
        );
    }
}
