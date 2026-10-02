# Execution Plan: Durable Agent Runtime

How to use this with Claude in VS Code:
- Put `CLAUDE.md` and `PLAN.md` in the repo root. Claude reads `CLAUDE.md` automatically.
- Start each phase in a fresh chat and paste that phase's prompt.
- Don't move on until every "Done when" box is checked. Commit at the end of each phase.
- Phases 1 and 2 are where you write the core logic yourself. That's what you'll be asked
  about in interviews, and it mirrors Rivet's policy of limited AI for core engine code.

Estimated time: about 4 weeks part-time.

---

## Phase 0: Setup (Day 1)

Prompt:
> Read CLAUDE.md. Create the Cargo workspace exactly as described in "Workspace layout"
> with empty crates that compile. Add dependencies: tokio (full), rusqlite (bundled),
> serde, serde_json, thiserror, anyhow, sha2, tracing, tracing-subscriber, clap (derive).
> Add a GitHub Actions workflow running fmt, clippy, and test. Stop when `cargo build`
> and `cargo test` pass.

Done when:
- [ ] `cargo build --workspace` and `cargo test --workspace` pass
- [ ] CI is green on GitHub

---

## Phase 1: The Journal (Days 2–5), YOU write this

Goal: a SQLite-backed, append-only log of steps that survives `kill -9`.

Schema (suggested starting point):
```sql
CREATE TABLE steps (
  seq           INTEGER PRIMARY KEY,
  step_name     TEXT NOT NULL,
  step_key      TEXT NOT NULL,     -- sha256(name + serialized inputs)
  kind          TEXT NOT NULL,     -- 'llm' | 'tool' | 'side_effect'
  status        TEXT NOT NULL,     -- 'PENDING' | 'COMPLETED' | 'FAILED'
  input_json    TEXT NOT NULL,
  output_json   TEXT,
  idempotency_key TEXT,
  tokens_in     INTEGER DEFAULT 0,
  tokens_out    INTEGER DEFAULT 0,
  started_at_us INTEGER NOT NULL,
  completed_at_us INTEGER
);
```

Key decisions to make yourself (and be ready to explain):
- WAL mode on; `synchronous=FULL` vs `NORMAL` (you'll benchmark this in Phase 5).
- Write `PENDING` before executing a step, update to `COMPLETED` after.
- What happens to a step found as `PENDING` on restart (crashed mid-step)?
  Answer: re-execute it (at-least-once), and rely on idempotency keys for side effects.

How to use Claude here (allowed prompts):
> Explain how SQLite WAL mode and `synchronous=FULL` affect durability after a crash.

> Review my journal.rs for correctness and durability bugs. Don't rewrite it; list issues.

> Write unit tests for journal.rs covering: append, mark completed, read by seq,
> reopen file after drop, and a PENDING row left behind.

Done when:
- [ ] You wrote `journal.rs` yourself
- [ ] Tests pass, including "reopen after simulated crash"

---

## Phase 2: Step API + Replay Engine (Days 6–10), YOU write `replay.rs`

Goal: the developer-facing API.
```rust
let ctx = DurableContext::open(run_id).await?;
let plan = ctx.step("plan", &input, || async { llm.call(prompt).await }).await?;
ctx.side_effect("send_email", idem_key, || async { send(..).await }).await?;
```

Replay logic you implement:
1. Each `ctx.step` call gets the next `seq`.
2. If the journal has `COMPLETED` at this `seq` with a matching `step_key`, return cached output.
3. If the `step_key` doesn't match, return `NonDeterminismError`.
4. Otherwise write `PENDING`, execute, write `COMPLETED` with output and token counts.

Claude prompt (scaffolding only):
> Create `DurableContext` in crates/durable-core/src/context.rs with the API in PLAN.md
> Phase 2, delegating to the `Journal` and `Replayer` types I wrote. Don't implement
> replay.rs. Then write integration tests: (a) fresh run executes all steps, (b) rerun
> with the same run_id executes zero closures, (c) changed input raises
> NonDeterminismError, (d) a PENDING step re-executes on restart.

Done when:
- [ ] All 4 integration tests pass
- [ ] You can explain step 3 (non-determinism) in your own words

---

## Phase 3: LLM Layer + Mock Server (Days 11–13)

Goal: real LLM calls for the demo, a deterministic mock for benchmarks.

Prompt:
> In crates/durable-llm, build an `LlmClient` trait with two implementations:
> (1) a real client for [Anthropic or OpenAI] reading the API key from env,
> (2) a `MockLlm` with configurable latency (default 800ms) and fixed token counts
> per call, returning deterministic text from a hash of the prompt.
> Every response returns tokens_in, tokens_out, and latency. Add a `pricing.toml`
> with per-model $/1M tokens and a function computing cost from token counts. Tests included.

Done when:
- [ ] Mock and real clients both work behind the same trait
- [ ] Cost is computed from tokens, never hardcoded per call

---

## Phase 4: Demo Agent + Crash Injection (Days 14–16)

Goal: the 10-step research agent, in two modes.

Prompt:
> Build apps/demo-agent: a 10-step research agent (plan → 6 search/summarize steps →
> outline → draft → final answer → side-effect step "save report" with idempotency key).
> Two modes via `--mode`: `durable` (uses DurableContext) and `naive` (same logic, no journal,
> starts over on restart). Support `CRASH_AT_STEP=N` which calls `std::process::abort()`
> right after step N starts. Add `--llm mock|real`. Print each step with a
> [EXECUTED] or [REPLAYED] tag and a final summary of calls, tokens, and cost.

Done when:
- [ ] `CRASH_AT_STEP=6` then rerun shows steps 1–5 as [REPLAYED], 6–10 [EXECUTED]
- [ ] The "save report" side effect happens exactly once across crash + restart

---

## Phase 5: Metrics Recorder + Benchmark Harness (Days 17–21)

This is where your resume numbers come from.

Metrics to record (each one maps to a resume claim):
1. **Duplicate LLM calls after crash.** Target: 0. Compare to naive mode.
2. **Tokens and cost saved on recovery** vs naive restart (absolute and %).
3. **Recovery time**: process start → first new step executing (ms).
4. **Journal overhead per step**: p50/p95/p99 in microseconds, and as % of a step's
   total time (with 800ms mock LLM latency).
5. **Durability tradeoff**: overhead with `synchronous=FULL` vs `NORMAL`.
6. **Throughput**: steps/sec with 1, 100, and 1,000 concurrent agent runs.
7. **Chaos pass rate**: runs that recover correctly out of N random kills (Phase 6).

Prompt:
> Build crates/durable-metrics: a recorder that stores timing samples and counters in a
> separate `metrics.db`, plus HDR-histogram percentiles (use the `hdrhistogram` crate).
> Then build apps/bench with subcommands: `recovery`, `overhead`, `sync-modes`,
> `throughput`, `all`. Use MockLlm for all benchmarks. Each run writes
> results/<benchmark>_<timestamp>.json and .csv, plus regenerates results/BENCHMARKS.md
> with a summary of the metrics listed in PLAN.md Phase 5, machine specs (CPU, RAM, OS),
> and the git commit hash. Run benchmarks in --release only.

Done when:
- [ ] `cargo run -p bench --release -- all` produces results/BENCHMARKS.md
- [ ] Every number in BENCHMARKS.md is traceable to a JSON file in results/

---

## Phase 6: Chaos Testing (Days 22–24)

Goal: prove it actually works under real crashes, not just a scripted one.

Prompt:
> Add `bench chaos --runs 100`: for each run, spawn demo-agent as a child process in durable
> mode with MockLlm, send SIGKILL at a random time (uniform over the expected run duration),
> restart it until it completes, then verify: (a) final output identical to a clean
> reference run, (b) zero duplicate COMPLETED LLM calls per seq, (c) the side effect
> executed exactly once. Record pass/fail, number of kills, and total recovery time per run.
> Add the chaos summary to BENCHMARKS.md.

Done when:
- [ ] 100/100 runs pass (if not, fix the bug; that story is great interview material)

---

## Phase 7: Concurrency (Days 25–26)

Prompt:
> Add a `Runtime` that runs many durable agent runs concurrently on tokio, each with its own
> SQLite file, using `spawn_blocking` for SQLite I/O. Add a bounded semaphore for
> max concurrent LLM calls. Update the throughput benchmark to cover 1/100/1,000 runs.

Done when:
- [ ] 1,000 concurrent runs complete with no errors; throughput recorded

---

## Phase 8: TypeScript Bindings (Days 27–29)

AI-assisted is fine here (matches Rivet's "reviewed AI for SDKs" policy).

Prompt:
> Create bindings/ts with napi-rs exposing `DurableContext.open(runId)`,
> `ctx.step(name, input, asyncFn)`, and `ctx.sideEffect(name, key, asyncFn)`. Add a
> TypeScript example agent in under 30 lines plus a test that crashes and resumes it.

Done when:
- [ ] `npm test` passes and the TS example resumes after a crash

---

## Phase 9: Ship It (Days 30–32)

- [ ] README: the problem (1 paragraph), a 30-second GIF of crash → resume, architecture
      diagram, the BENCHMARKS.md headline numbers, and an "AI usage" section stating which
      parts you hand-wrote (journal, replay) vs AI-assisted (bindings, harness, CI)
- [ ] 2-minute demo video: durable vs naive side by side, both killed at step 6
- [ ] Short blog post or LinkedIn write-up: "What I learned building durable execution in Rust"
- [ ] Fill in the resume bullets below with your real numbers

---

## Resume Bullet Templates

Only use numbers from your own results/BENCHMARKS.md.

- Built a durable execution runtime in Rust that makes LLM agents crash-proof via a
  per-run SQLite write-ahead journal and deterministic replay, eliminating duplicate LLM
  calls after failure ([X]% token/cost savings vs naive restart).
- Validated correctness with a chaos harness that SIGKILLed agents at random points across
  [N] runs, achieving [N/N] correct recoveries with exactly-once side effects.
- Kept journaling overhead to [X] µs p99 per step ([Y]% of step latency) and benchmarked
  SQLite durability modes, showing [tradeoff found].
- Scaled to [N] concurrent agent runs at [X] steps/sec on tokio; shipped TypeScript
  bindings via napi-rs.
