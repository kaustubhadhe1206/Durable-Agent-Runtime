# Durable Agent Runtime: Project Context for Claude

## What this project is
A Rust runtime that makes LLM agents crash-proof. Every agent step (LLM call, tool call,
side effect) is recorded in a per-run SQLite journal *before and after* it executes. If the
process dies, the agent restarts and replays from the journal: completed steps return their
cached results, so there are no duplicate LLM calls (no wasted tokens) and no repeated side effects.

This is a portfolio project targeting Rivet (rivet.dev): Rust, durable execution, stateful actors,
SQLite-per-actor. Measured performance numbers are a first-class deliverable.

## Workspace layout
```
durable-agent/
  Cargo.toml                 # workspace
  crates/
    durable-core/            # journal, step API, replay engine (library)
    durable-llm/             # LLM client wrapper + mock LLM server
    durable-metrics/         # metrics recorder + report export
  apps/
    demo-agent/              # 10-step research agent (durable + naive modes)
    bench/                   # benchmark + chaos harness, writes reports
  bindings/
    ts/                      # napi-rs TypeScript bindings (Phase 8)
  results/                   # benchmark outputs (JSON/CSV/markdown), committed
  PLAN.md                    # phased execution plan
```

## Core concepts
- **Run**: one execution of an agent, identified by `run_id`. Has its own SQLite file.
- **Step**: a unit of work with a sequence number (`seq`), a name, and a `step_key`
  (hash of name + inputs).
- **Journal**: append-only table of steps with status `PENDING` / `COMPLETED` / `FAILED`.
- **Replay**: on restart, steps already `COMPLETED` return cached output without executing.
- **Non-determinism check**: if the step at `seq` N has a different `step_key` than the journal
  recorded, fail loudly with `NonDeterminismError`.
- **Idempotency key**: side-effect steps carry a key so external actions are never repeated.

## Rules for Claude
1. Work only on the phase I name from PLAN.md. Stop at that phase's checkpoint and summarize
   what was done, what is left, and how to verify it.
2. **Hand-written zone**: `crates/durable-core/src/journal.rs` and
   `crates/durable-core/src/replay.rs` are written by me. Do not write their implementation.
   You may explain concepts, review my code, point out bugs, and write tests for them.
3. Every feature ships with tests. Before saying a task is done, run:
   `cargo fmt --all`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`.
4. No `unwrap()`/`expect()` in library crates. Use `thiserror` in libraries, `anyhow` in apps.
5. Never invent or estimate benchmark numbers. Numbers only come from `apps/bench` output
   in `results/`.
6. Prefer simple, readable code over clever abstractions. Explain any non-obvious Rust
   (lifetimes, Pin, Send/Sync bounds) in a short comment.
7. Small commits with conventional commit messages (`feat:`, `fix:`, `test:`, `bench:`).

## Commands
- Build: `cargo build --workspace`
- Test: `cargo test --workspace`
- Demo (durable): `cargo run -p demo-agent -- --mode durable`
- Demo with crash: `CRASH_AT_STEP=6 cargo run -p demo-agent -- --mode durable`
- Benchmarks: `cargo run -p bench --release -- all`
