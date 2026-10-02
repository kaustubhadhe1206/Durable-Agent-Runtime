//! Durable execution core: the per-run SQLite journal, the step API, and the replay engine.

// [Phase 2]
pub mod context;
// [Phase 1]
pub mod journal;
// [Phase 2]
pub mod replay;

// [Phase 2]
pub use context::{
    BoxError, DurableContext, DurableError, RunStats, StepOutcome, StepSpec, TokenUsage,
};
// [Phase 1]
pub use journal::{Journal, JournalError, NewStep, StepKind, StepRecord, StepStatus, SyncMode};
// [Phase 2]
pub use replay::{step_key, Decision, NonDeterminismError, ReplayError, Replayer, StepRequest};
