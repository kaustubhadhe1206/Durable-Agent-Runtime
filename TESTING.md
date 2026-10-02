# Testing Guide

How to verify each phase of the Durable Agent Runtime yourself, and what each command
actually proves. Commands are written for **PowerShell** (the VS Code default on Windows).

Status legend: ✅ available now · 🔜 arrives in that phase (this file is updated as phases land)

**Finding code by phase:** every function and struct has a comment like `// [Phase 3]` above
it, naming the phase it was written in. In VS Code press `Ctrl+Shift+F` and search for
`[Phase 3]` to see everything from that phase.

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

### Phase 2: Step API + Replay Engine ✅

```powershell
cargo test -p durable-core
```

Expected: 19 unit tests (journal + replay + context) and 10 integration tests across
`journal_crash.rs` and `replay.rs`, all `ok`. Only the replay tests are run with:

```powershell
cargo test -p durable-core --test replay
```

The **integration tests** use a small 3-step agent (plan → search → save) and count how many
step closures actually run. File: `crates/durable-core/tests/replay.rs`.

| Test | What it proves |
| --- | --- |
| `fresh_run_executes_all_steps` | **(a)** A new `run_id` runs every step exactly once and records all 3 as `COMPLETED`, with the right kinds (llm / tool / side_effect) |
| `rerun_with_same_run_id_executes_zero_closures` | **(b)** Running the same `run_id` again returns identical output with **zero** closures executed, so no LLM calls are repeated. Stats report the replayed tokens (what a naive restart would have paid again) |
| `changed_input_raises_non_determinism_error` | **(c)** If the input to step 1 changes between runs, the run stops with `NonDeterminismError` instead of replaying stale results, and the mismatched step does not run |
| `pending_step_re_executes_after_crash` | **(d)** A child process completes step 1, then calls `abort()` *inside* step 2. On rerun, step 1 is replayed and **only** step 2 runs again |
| `failed_step_is_recorded_and_retried_on_next_run` | A step that returns an error is stored as `FAILED` and retried on the next run |
| `execute_reports_replayed_flag` | The lower-level API reports whether a value was replayed (used by the demo's `[REPLAYED]` tags) |
| `invalid_run_id_is_rejected` | Run ids like `../escape` are rejected, since they become file names |
| `crash_inside_step_child` | Helper for test (d). It does nothing when run normally |

The **unit tests** in `crates/durable-core/src/replay.rs`:

| Test | What it proves |
| --- | --- |
| `step_key_is_deterministic_and_input_sensitive` | The same name + input always hash the same; any change gives a different key |
| `new_step_executes_and_is_recorded_pending` | A new step is written as `PENDING` *before* the caller runs it |
| `completed_step_replays_cached_output` | A `COMPLETED` step returns its stored output and token counts |
| `changed_input_is_non_deterministic` | A key mismatch produces `NonDeterminismError` with both keys for debugging |
| `non_determinism_is_checked_for_pending_steps_too` | Mismatches are caught even for a step that crashed mid-run |
| `pending_and_failed_steps_re_execute` | `PENDING` and `FAILED` steps are reset and run again |

### Phase 3: LLM Layer (real Mistral) ✅

This phase connects the project to the **real Mistral AI**. There is no fake AI: the live tests
send real questions to Mistral using the key in `.env`. The default model is
`ministral-8b-latest` (open-weight, available on the free plan).

#### Try it yourself: ask Mistral one question

```powershell
cargo run -p durable-llm --example mistral_ping
cargo run -p durable-llm --example mistral_ping -- "What is the capital of Japan?"
```

You'll see Mistral's reply, how many tokens (word pieces) went in and out, how long it took,
and what it *would* cost on a paid plan (the free plan charges $0).

#### Run the tests

```powershell
cargo test -p durable-llm
```

Expected: 10 unit tests + 3 live tests, all `ok`. To watch the live replies:

```powershell
cargo test -p durable-llm --test mistral_live -- --nocapture
```

Live tests (file: `crates/durable-llm/tests/mistral_live.rs`). These really call Mistral:

| Test | In simple words |
| --- | --- |
| `real_call_returns_text_tokens_latency_and_cost` | Ask Mistral a real question. Check we get an answer, token counts, a time taken, and that the cost is worked out from the token counts |
| `bad_api_key_is_rejected_without_retrying` | A wrong password (API key) is refused straight away, and we don't keep retrying |
| `durable_step_with_real_llm_is_not_repeated_on_rerun` | **Phases 2 + 3 together:** ask Mistral inside a durable step, then run it again. Mistral is called only **once**, and the second time the exact same answer comes from the notebook |

Unit tests. These check our own code, with no internet needed:

| Test | In simple words |
| --- | --- |
| `cost_is_computed_from_token_counts` | The cost maths is right (tokens × price per million) |
| `unknown_model_is_an_error` | Asking the price of a model we don't know gives a clear error, not a made-up price |
| `invalid_toml_is_an_error` | A broken price file is reported, not silently ignored |
| `bundled_table_covers_default_model_and_dated_names` | The price file has every model we use |
| `parses_mistral_response` | We correctly read a real Mistral reply (saved from an actual call) |
| `parses_chunked_content` | We can read replies that come in several pieces |
| `rejects_malformed_response` | A garbled reply gives a clear error instead of crashing |
| `request_body_has_expected_shape` | The question we send is in the format Mistral expects |
| `empty_api_key_is_rejected_and_key_is_never_printed` | A missing key is caught, and the key never shows up in logs |
| `backoff_grows_and_is_capped` | When Mistral says "slow down", we wait a bit longer each time, up to a limit |

> **No key?** The live tests print `SKIPPED` and pass. That's what happens on GitHub unless you
> add the key there: repo **Settings → Secrets and variables → Actions → New repository
> secret**, name `MISTRAL_API_KEY`.

Prices come from `crates/durable-llm/pricing.toml`, copied from
<https://mistral.ai/pricing/api/>. Free-plan speed limits for your key: Ministral 8B allows 188
requests/minute, 3B allows 750, 14B allows 30.

### Phase 4: Demo Agent + Crash Injection ✅

A 10-step research agent that really uses Mistral. It researches a topic and writes a report:

| Step | What it does |
| --- | --- |
| 1 | Plan: ask Mistral for 6 subtopics |
| 2–7 | Research: one Mistral summary per subtopic |
| 8 | Outline the report |
| 9 | Write the final report |
| 10 | Save the report to a file (a **do-once action**, protected by an idempotency key) |

Each step prints a tag:

- `[EXECUTED]`: the step really ran (a real Mistral call)
- `[REPLAYED]`: the answer came from the notebook (journal): instant, free, no Mistral call

#### Demo 1: crash at step 6, then restart (durable mode)

```powershell
$env:CRASH_AT_STEP=6; cargo run -p demo-agent -- --mode durable --fresh
Remove-Item Env:CRASH_AT_STEP; cargo run -p demo-agent -- --mode durable
```

- First command: steps 1–5 show `[EXECUTED]`, then the program crashes when step 6 starts.
- Second command: steps 1–5 show `[REPLAYED]` (0 ms each) and steps 6–10 show `[EXECUTED]`.
  The summary says **LLM calls made: 4** and **report saves: 1**.
- Run the second command once more: all 10 steps show `[REPLAYED]`, LLM calls made: 0,
  and report saves is still 1.

`--fresh` deletes the old notebook so you start a brand-new run. Without it, the agent resumes
whatever run is already saved under the same `--run-id` (default `demo`).

#### Demo 2: the same crash without a notebook (naive mode)

```powershell
$env:CRASH_AT_STEP=6; cargo run -p demo-agent -- --mode naive --fresh
Remove-Item Env:CRASH_AT_STEP; cargo run -p demo-agent -- --mode naive
```

The second command shows **all 10 steps `[EXECUTED]`**: it starts over, asks Mistral all over
again, and pays for steps 1–5 twice. Run it again and "report saves" becomes 2: the do-once
action happened twice.

#### Demo 3: the non-determinism check

```powershell
cargo run -p demo-agent -- --mode durable --topic "Something else"
```

After Demo 1, this fails with "Non-determinism detected": the notebook was written for a
different topic, so reusing its answers would be wrong. Add `--fresh` or a new `--run-id` to fix.

> PowerShell sets env vars with `$env:NAME=value` (the `NAME=value cmd` form in CLAUDE.md is
> bash syntax). `Remove-Item Env:NAME` clears it. If you forget, the restart crashes at step 6
> again, because step 6 is the first step that really runs.

**Where things are saved:** notebooks in `runs/` (`runs/demo.db`), reports in `reports/`
(`reports/demo-durable.md`), and `reports/demo-durable.saves.log` gets one line each time the
report is *really* saved. To see inside a notebook, install the VS Code extension
**"SQLite Viewer"** and click `runs/demo.db`.

Other options: `--topic "..."`, `--run-id NAME`. Full list: `cargo run -p demo-agent -- --help`.

#### Automatic tests (Phase 4)

```powershell
cargo test -p demo-agent
```

Expected: 7 unit tests + 2 end-to-end tests, all `ok`. The end-to-end tests take about 45
seconds because they make ~30 real Mistral calls. File: `apps/demo-agent/tests/crash_recovery.rs`.

| Test | In simple words |
| --- | --- |
| `durable_crash_at_6_then_restart_replays_1_to_5` | Does Demo 1 automatically: crash at step 6, restart, check steps 1–5 are `[REPLAYED]`, 6–10 are `[EXECUTED]`, only 4 Mistral calls, report saved **once**. Then a third run: everything replayed, 0 calls, still one save |
| `naive_restart_redoes_everything_and_repeats_the_side_effect` | Does Demo 2: after a crash, naive mode redoes all 10 steps (9 Mistral calls), and running again saves the report a **second** time |
| unit tests (`agent.rs`, `main.rs`) | Reading the 6 subtopics out of Mistral's plan (numbers, bullets, too few lines), the `CRASH_AT_STEP` value, and counting saves |

### Phase 5: Benchmarks ✅

Benchmarks **measure** how well the project works and turn it into numbers (for your resume).
Every benchmark uses **real Mistral calls**.

#### Run all benchmarks (takes a few minutes)

```powershell
cargo run -p bench --release -- all
```

`--release` builds an optimized program. Benchmarks refuse to run without it, because a debug
build gives slow, misleading numbers. When it finishes, open **`results/BENCHMARKS.md`**.

#### What each benchmark measures, in simple words

| Command | What it measures |
| --- | --- |
| `recovery` | Crashes the real demo agent at step 6 and restarts it, in both modes, 3 times. Counts **duplicate Mistral calls** (steps redone after the crash: durable should be 0, naive 5), **tokens and cost saved**, and **recovery time** (restart until the first new step begins) |
| `overhead` | Runs 50 real steps and measures how long the **notebook writes** take per step, and what share of the total step time that is |
| `sync-modes` | The notebook has two safety levels. `FULL` survives even a power cut; `NORMAL` survives a program crash but is faster. This measures how much slower `FULL` is |
| `throughput` | Runs 1, then 10 agents **at the same time**, and measures steps per second. With real Mistral this is limited by Mistral's speed and free-plan limits |

You can run one at a time, with smaller or larger settings:

```powershell
cargo run -p bench --release -- recovery --reps 3 --crash-at 6
cargo run -p bench --release -- overhead --steps 50
cargo run -p bench --release -- sync-modes --steps 30
cargo run -p bench --release -- throughput --concurrency 1,10 --steps-per-run 5
```

#### Where the numbers go

- `results/<benchmark>_<time>.json` and `.csv`: the results of each run (committed to git).
- `results/BENCHMARKS.md`: a summary generated from the newest JSON of each benchmark. Every
  number in it names the JSON file it came from. Never edit it by hand.
- `results/metrics.db`: every single raw measurement (not committed; it can be large).

> Words you'll see: **p50** = the typical value (half the steps were faster). **p99** = the
> slow end (only 1 in 100 steps was slower). **µs** = microsecond, one millionth of a second.

#### Automatic tests (Phase 5)

```powershell
cargo test -p bench
cargo test -p durable-metrics
```

| Test | In simple words |
| --- | --- |
| `overhead_and_throughput_write_traceable_results` | Runs two small benchmarks for real (real Mistral) and checks the JSON, CSV and BENCHMARKS.md files appear, and that BENCHMARKS.md names the JSON file its numbers came from |
| `debug_build_refuses_without_flag` | Benchmarks refuse to run in a slow debug build |
| `durable_restart_has_no_duplicates` / `naive_restart_counts_redone_steps_as_duplicates` | The duplicate-call and recovery-time maths is right, checked on sample event lines |
| `percentiles_of_1_to_100` (and others in `durable-metrics`) | p50/p95/p99 maths, storing measurements in `metrics.db`, writing CSV/JSON, building BENCHMARKS.md |

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
