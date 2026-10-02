# Testing Guide

How to verify each phase of the Durable Agent Runtime yourself, and what each command
actually proves. Commands are written for **PowerShell** (the VS Code default on Windows).

Status legend: ✅ available now · 🔜 arrives in that phase (this file is updated as phases land)

---

## 0. One-time setup

Open a **new** terminal in VS Code (Terminal → New Terminal) so it picks up the Rust install:

```powershell
cargo --version
```

If it says "`cargo` is not recognized", VS Code was opened before Rust was installed and still
has the old PATH. Close **all** VS Code windows and reopen. As a temporary fix for the current
terminal only:

```powershell
$env:Path += ";$env:USERPROFILE\.cargo\bin"
```

The first build takes ~1 minute because it
compiles SQLite from C source; later builds are incremental and fast.

The real LLM client (Phase 3+) reads `MISTRAL_API_KEY` from the `.env` file in the repo root.
`.env` is git-ignored. Never commit it. `.env.example` shows the expected format.

---

## 1. The three checks to run after every phase ✅

Run these from the repo root. CI runs the same three on every push to GitHub (**Actions** tab;
a green check ✓ means they all passed).

### `cargo test --workspace`

Compiles every crate and runs **all tests**: unit tests (inside `src/` files, under
`#[cfg(test)]`) and integration tests (files in each crate's `tests/` folder).

- **What it tests:** that the code behaves correctly: the journal stores and reads steps, replay
  skips completed work, crashes are recovered from, and so on.
- **What to look for:** one `test result:` line per test binary. Every one must say
  `ok. N passed; 0 failed`. Binaries with `0 passed` are crates with no tests yet, which is fine.

### `cargo clippy --all-targets -- -D warnings`

Clippy is Rust's linter. `--all-targets` includes tests and examples; `-D warnings` turns every
warning into an error.

- **What it tests:** code quality, not behavior: unused variables, needless clones, suspicious
  patterns, non-idiomatic code. It catches bugs the compiler allows.
- **What to look for:** ends with `Finished` and prints no `warning:` or `error:` lines.

### `cargo fmt --all -- --check`

Checks that all code matches standard Rust formatting, without changing any files.

- **What it tests:** consistent style only.
- **What to look for:** **no output** means OK. If it prints a diff, run `cargo fmt --all` to fix.

### Useful variations

```powershell
cargo test -p durable-core                               # one crate only
cargo test -p durable-core journal                       # only tests whose name contains "journal"
cargo test -p durable-core -- --nocapture --test-threads=1   # show println! output, run one at a time
```

---

## 2. Phase-by-phase verification

### Phase 1: The Journal ✅

```powershell
cargo test -p durable-core
```

Expected: 12 `journal::tests::...` unit tests + `journal_survives_process_abort`, all `ok`.

What each test proves (file: `crates/durable-core/src/journal.rs`):

| Test | What it proves |
| --- | --- |
| `append_writes_pending_row` | A step is recorded as `PENDING` with a start timestamp **before** it runs |
| `complete_records_output_and_tokens` | Finishing a step stores its output, token counts, and completion time |
| `read_by_seq_and_missing_seq` | Steps can be read back by sequence number; missing steps return "none", not an error |
| `empty_journal_has_no_last_seq` | A fresh run starts with an empty journal |
| `duplicate_seq_is_rejected` | The same step number can't be written twice (no silent overwrite) |
| `duplicate_idempotency_key_is_rejected` | A side-effect key can only be used once, the basis of exactly-once side effects |
| `completed_step_cannot_change` | A `COMPLETED` step is immutable: it can't be completed again, failed, or retried |
| `missing_step_transition_reports_not_found` | Updating a step that doesn't exist gives a clear error |
| `failed_step_can_be_restarted_and_completed` | A `FAILED` step can be retried and then succeed |
| `reopen_after_drop_preserves_rows` | Data is on disk: closing and reopening the file loses nothing |
| `pending_row_left_behind_can_be_retried` | A step interrupted mid-run stays `PENDING` and can be re-executed on restart |
| `normal_sync_mode_opens_and_works` | The faster `synchronous=NORMAL` mode works too (benchmarked in Phase 5) |

The **real crash test** (file: `crates/durable-core/tests/journal_crash.rs`):

| Test | What it proves |
| --- | --- |
| `journal_survives_process_abort` | The test re-launches itself as a child process. The child completes step 1, starts step 2, then calls `std::process::abort()`: a hard kill with no cleanup, like `kill -9`. The parent reopens the journal and checks that step 1 is still `COMPLETED` with its output and step 2 is `PENDING` and can be retried. **This is the core durability guarantee.** |
| `crash_child` | Helper used by the test above. It does nothing when run normally. |

### Phase 2: Step API + Replay Engine 🔜

```powershell
cargo test -p durable-core
```

Planned integration tests:

| Test | What it will prove |
| --- | --- |
| fresh run executes all steps | On a new `run_id`, every step closure runs once |
| rerun executes zero closures | Re-running the same `run_id` returns cached outputs; **no LLM calls are repeated** |
| changed input → `NonDeterminismError` | If code or inputs change so step N differs from the journal, it fails loudly instead of replaying wrong data |
| PENDING step re-executes | A step that crashed mid-run is executed again on restart |

### Phase 3: LLM Layer + Mock Server 🔜

```powershell
cargo test -p durable-llm
```

- **Mock LLM tests:** same prompt always returns the same text (deterministic), latency and
  token counts are configurable, and cost is computed from token counts using `pricing.toml`.
- **Real Mistral call:** a small example that sends one request to Mistral and prints the reply,
  token counts, and cost. The exact command is added here when Phase 3 lands. It needs `.env`.

### Phase 4: Demo Agent + Crash Injection 🔜

Normal durable run:

```powershell
cargo run -p demo-agent -- --mode durable
```

Expect 10 steps, each tagged `[EXECUTED]`, then a summary of calls, tokens, and cost.

Crash at step 6, then recover:

```powershell
$env:CRASH_AT_STEP=6; cargo run -p demo-agent -- --mode durable
Remove-Item Env:CRASH_AT_STEP; cargo run -p demo-agent -- --mode durable
```

- First run: dies right after step 6 starts.
- Second run: steps 1–5 show `[REPLAYED]` (served from the journal, **no LLM call, no tokens**),
  steps 6–10 show `[EXECUTED]`.
- The "save report" side effect happens **exactly once** across both runs.

Compare with naive mode (no journal), which starts over and re-pays for every step:

```powershell
$env:CRASH_AT_STEP=6; cargo run -p demo-agent -- --mode naive
Remove-Item Env:CRASH_AT_STEP; cargo run -p demo-agent -- --mode naive
```

> PowerShell sets env vars with `$env:NAME=value`. The `NAME=value cmd` form in CLAUDE.md is
> bash syntax. `Remove-Item Env:NAME` clears the variable again.

**Inspect a journal:** install the VS Code extension **"SQLite Viewer"**, then click a run's
`.db` file to see the `steps` table (status, outputs, tokens, timestamps).

### Phase 5: Benchmarks 🔜

```powershell
cargo run -p bench --release -- all
```

Writes `results/<benchmark>_<timestamp>.json` / `.csv` and regenerates `results/BENCHMARKS.md`.
Measures: duplicate LLM calls after a crash, tokens and cost saved vs naive, recovery time,
journal overhead per step (p50/p95/p99), `FULL` vs `NORMAL` sync cost, and throughput.
Always use `--release`, because debug builds give misleading numbers.

### Phase 6: Chaos Testing 🔜

```powershell
cargo run -p bench --release -- chaos --runs 100
```

Kills the agent at random moments, restarts it until it finishes, and checks: output identical
to a clean run, zero duplicate LLM calls, side effect executed exactly once. Target: **100/100**.

### Phase 7: Concurrency 🔜

```powershell
cargo run -p bench --release -- throughput
```

Runs 1 / 100 / 1,000 agent runs at the same time. All must complete with no errors.

### Phase 8: TypeScript Bindings 🔜

```powershell
cd bindings/ts
npm install
npm test
```

Proves the TypeScript example agent resumes after a crash, using the same Rust engine.
