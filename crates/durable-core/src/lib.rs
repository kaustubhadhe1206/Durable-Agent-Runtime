//! Durable execution core: the per-run SQLite journal, the step API, and the replay engine.
//!
//! Modules land in later phases:
//! - `journal` (Phase 1, hand-written)
//! - `replay` (Phase 2, hand-written)
//! - `context` (Phase 2, `DurableContext` step API)

#[cfg(test)]
mod tests {
    #[test]
    fn crate_compiles() {}
}
