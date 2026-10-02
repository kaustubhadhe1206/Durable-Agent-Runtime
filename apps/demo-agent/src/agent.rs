//! The 10-step research agent. The same logic runs in two modes:
//! - durable: every step goes through `DurableContext`, so a restart replays finished steps
//! - naive:   steps call Mistral directly, so a restart starts over from step 1

use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::Context;
use durable_core::{DurableContext, StepKind, StepSpec, TokenUsage};
use durable_llm::{LlmClient, LlmError, LlmRequest, LlmResponse, MistralClient};

use crate::events::{now_unix_us, Event, EventLog};

// [Phase 4]
pub const TOTAL_STEPS: u32 = 10;
// [Phase 4]
const SUBTOPICS: usize = 6;

// [Phase 4]
pub enum Runner {
    Durable(Box<DurableContext>),
    Naive,
}

// [Phase 4]
/// Counters for this process (one launch of the program).
#[derive(Debug, Default, Clone, Copy)]
pub struct Totals {
    pub executed: u32,
    pub replayed: u32,
    pub llm_calls: u32,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub saved_tokens_in: u64,
    pub saved_tokens_out: u64,
}

// [Phase 4]
pub struct Agent<'a> {
    pub runner: Runner,
    pub client: &'a MistralClient,
    pub crash_at: Option<u32>,
    pub run_id: String,
    pub mode_name: &'static str,
    pub reports_dir: PathBuf,
    pub totals: Totals,
    // [Phase 5]
    /// Optional `--events` log read by the benchmark harness.
    pub events: Option<EventLog>,
}

// [Phase 4]
/// The result of the whole agent.
pub struct Report {
    pub text: String,
    pub path: String,
}

// [Phase 4]
impl Agent<'_> {
    // [Phase 4]
    pub async fn run(&mut self, topic: &str) -> anyhow::Result<Report> {
        // Step 1: plan
        let plan = self
            .llm(
                1,
                "plan",
                "plan the report",
                LlmRequest::new(format!(
                    "You are planning a short research report on: {topic}\n\
                     List exactly {SUBTOPICS} distinct subtopics to research, one per line, \
                     with no numbering and no extra text."
                ))
                .with_max_tokens(200),
            )
            .await?;
        let subtopics = parse_subtopics(&plan, topic);

        // Steps 2-7: research each subtopic
        let mut notes = Vec::with_capacity(SUBTOPICS);
        for (i, sub) in subtopics.iter().enumerate() {
            let n = 2 + i as u32;
            let summary = self
                .llm(
                    n,
                    &format!("research-{}", i + 1),
                    &format!("research: {}", shorten(sub, 38)),
                    LlmRequest::new(format!(
                        "Research report topic: {topic}\n\
                         Write a concise, factual summary (3-4 sentences) of this subtopic: {sub}"
                    ))
                    .with_max_tokens(250),
                )
                .await?;
            notes.push(format!("## {sub}\n{summary}"));
        }
        let notes = notes.join("\n\n");

        // Step 8: outline
        let outline = self
            .llm(
                8,
                "outline",
                "outline the report",
                LlmRequest::new(format!(
                    "Using these research notes, write a short outline (5-7 bullet points) \
                     for a report on: {topic}\n\nNotes:\n{notes}"
                ))
                .with_max_tokens(300),
            )
            .await?;

        // Step 9: write the final report
        let report = self
            .llm(
                9,
                "write-report",
                "write the final report",
                LlmRequest::new(format!(
                    "Write the final report (about 250 words, plain text) on: {topic}\n\
                     Follow this outline and use the notes.\n\nOutline:\n{outline}\n\n\
                     Notes:\n{notes}"
                ))
                .with_max_tokens(600),
            )
            .await?;

        // Step 10: save the report (side effect, must happen exactly once)
        let path = self.save_report(10, &report).await?;
        Ok(Report { text: report, path })
    }

    // [Phase 4]
    /// Runs one LLM step and prints its [EXECUTED]/[REPLAYED] line.
    async fn llm(
        &mut self,
        n: u32,
        name: &str,
        label: &str,
        request: LlmRequest,
    ) -> anyhow::Result<String> {
        let started = Instant::now();
        let started_unix_us = now_unix_us(); // [Phase 5]
                                             // Copy what the closure needs so it doesn't borrow all of `self`.
        let client = self.client;
        let crash_at = self.crash_at;

        let (resp, replayed, journal_us): (LlmResponse, bool, u64) = match &self.runner {
            Runner::Durable(ctx) => {
                let spec = StepSpec {
                    name,
                    kind: StepKind::Llm,
                    input: &request,
                    idempotency_key: None,
                };
                let outcome = ctx
                    .execute(spec, || async {
                        maybe_crash(crash_at, n);
                        let resp = client.complete(&request).await?;
                        let usage = TokenUsage {
                            tokens_in: resp.tokens_in,
                            tokens_out: resp.tokens_out,
                        };
                        Ok::<_, LlmError>((resp, usage))
                    })
                    .await?;
                (outcome.value, outcome.replayed, outcome.journal_us)
            }
            Runner::Naive => {
                maybe_crash(crash_at, n);
                (client.complete(&request).await?, false, 0)
            }
        };
        self.emit_step(
            n,
            "llm",
            replayed,
            (resp.tokens_in, resp.tokens_out),
            started_unix_us,
            started,
            journal_us,
        )?;

        let t = &mut self.totals;
        if replayed {
            t.replayed += 1;
            t.saved_tokens_in += resp.tokens_in;
            t.saved_tokens_out += resp.tokens_out;
        } else {
            t.executed += 1;
            t.llm_calls += 1;
            t.tokens_in += resp.tokens_in;
            t.tokens_out += resp.tokens_out;
        }
        print_step(
            replayed,
            n,
            label,
            &format!(
                "{} tok, {}",
                resp.tokens_in + resp.tokens_out,
                elapsed_ms(started)
            ),
        );
        Ok(resp.text)
    }

    // [Phase 4]
    /// Step 10: write the report file. Guarded by an idempotency key in durable mode.
    async fn save_report(&mut self, n: u32, report: &str) -> anyhow::Result<String> {
        let started = Instant::now();
        let started_unix_us = now_unix_us(); // [Phase 5]
        let crash_at = self.crash_at;
        let key = format!("save-report-{}", self.run_id);
        let file_stem = format!("{}-{}", self.run_id, self.mode_name);
        let dir = self.reports_dir.clone();

        let save = || async {
            maybe_crash(crash_at, n);
            write_report(&dir, &file_stem, &key, report)
        };

        let (path, replayed, journal_us) = match &self.runner {
            Runner::Durable(ctx) => {
                let spec = StepSpec {
                    name: "save-report",
                    kind: StepKind::SideEffect,
                    input: key.as_str(),
                    idempotency_key: Some(key.as_str()),
                };
                let outcome = ctx
                    .execute(spec, || async {
                        save().await.map(|p| (p, TokenUsage::default()))
                    })
                    .await?;
                (outcome.value, outcome.replayed, outcome.journal_us)
            }
            Runner::Naive => (save().await?, false, 0),
        };
        self.emit_step(
            n,
            "side_effect",
            replayed,
            (0, 0),
            started_unix_us,
            started,
            journal_us,
        )?;

        if replayed {
            self.totals.replayed += 1;
        } else {
            self.totals.executed += 1;
        }
        print_step(
            replayed,
            n,
            "save report (side effect)",
            &elapsed_ms(started),
        );
        Ok(path)
    }

    // [Phase 5]
    /// Appends a `step` line to the event log, if one was requested.
    #[allow(clippy::too_many_arguments)]
    fn emit_step(
        &mut self,
        n: u32,
        kind: &str,
        replayed: bool,
        (tokens_in, tokens_out): (u64, u64),
        started_unix_us: u64,
        started: Instant,
        journal_us: u64,
    ) -> anyhow::Result<()> {
        if let Some(log) = &mut self.events {
            log.write(&Event::Step {
                n,
                kind,
                replayed,
                tokens_in,
                tokens_out,
                started_unix_us,
                duration_us: u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX),
                journal_us,
            })?;
        }
        Ok(())
    }
}

// [Phase 4]
/// Writes the report and appends one line to the "saves" ledger, which counts how many times
/// the side effect really ran.
fn write_report(dir: &Path, file_stem: &str, key: &str, report: &str) -> std::io::Result<String> {
    use std::io::Write;
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{file_stem}.md"));
    std::fs::write(&path, report)?;
    let mut ledger = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(ledger_path(dir, file_stem))?;
    writeln!(ledger, "saved key={key} pid={}", std::process::id())?;
    Ok(path.display().to_string())
}

// [Phase 4]
pub fn ledger_path(dir: &Path, file_stem: &str) -> PathBuf {
    dir.join(format!("{file_stem}.saves.log"))
}

// [Phase 4]
/// How many times the report was really saved (lines in the ledger).
pub fn count_saves(dir: &Path, file_stem: &str) -> anyhow::Result<usize> {
    match std::fs::read_to_string(ledger_path(dir, file_stem)) {
        Ok(s) => Ok(s.lines().filter(|l| !l.trim().is_empty()).count()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(e).context("reading save ledger"),
    }
}

// [Phase 4]
/// Simulates a hard crash (like `kill -9`): no cleanup, no destructors.
fn maybe_crash(crash_at: Option<u32>, n: u32) {
    if crash_at == Some(n) {
        eprintln!("\n*** CRASH_AT_STEP={n}: simulating a crash right after step {n} started ***");
        std::process::abort();
    }
}

// [Phase 4]
fn print_step(replayed: bool, n: u32, label: &str, detail: &str) {
    let tag = if replayed { "[REPLAYED]" } else { "[EXECUTED]" };
    println!("{tag} step {n:>2}/{TOTAL_STEPS}  {label:<48} ({detail})");
}

// [Phase 4]
fn elapsed_ms(started: Instant) -> String {
    format!("{} ms", started.elapsed().as_millis())
}

// [Phase 4]
/// Turns the plan's text into exactly `SUBTOPICS` subtopics: strips numbering, bullets and
/// markdown, drops blank lines, and pads with generic subtopics if the model gave too few.
pub fn parse_subtopics(plan: &str, topic: &str) -> Vec<String> {
    let mut subs: Vec<String> = plan
        .lines()
        .map(clean_line)
        .filter(|l| !l.is_empty())
        .take(SUBTOPICS)
        .collect();
    let mut i = subs.len();
    while subs.len() < SUBTOPICS {
        i += 1;
        subs.push(format!("{topic}: aspect {i}"));
    }
    subs
}

// [Phase 4]
fn clean_line(line: &str) -> String {
    let mut s = line.trim();
    // Leading bullets: "-", "*", "•"
    s = s.trim_start_matches(['-', '*', '•']).trim_start();
    // Leading numbering: "1.", "2)", "10 -"
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits > 0 {
        let rest = s[digits..].trim_start();
        if let Some(r) = rest.strip_prefix(['.', ')', ':', '-']) {
            s = r.trim_start();
        }
    }
    s.replace("**", "").trim().to_string()
}

// [Phase 4]
fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max.saturating_sub(3)).collect();
        format!("{cut}...")
    }
}

// [Phase 4]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_lines() {
        let plan = "Chemistry\nCost\nSafety\nRange\nCharging\nManufacturing\nExtra";
        let subs = parse_subtopics(plan, "t");
        assert_eq!(subs.len(), 6);
        assert_eq!(subs[0], "Chemistry");
        assert_eq!(subs[5], "Manufacturing");
    }

    #[test]
    fn strips_numbering_bullets_and_markdown() {
        let plan = "1. **Chemistry**\n2) Cost\n\n- Safety\n* Range\n• Charging\n10 - Recycling";
        assert_eq!(
            parse_subtopics(plan, "t"),
            vec![
                "Chemistry",
                "Cost",
                "Safety",
                "Range",
                "Charging",
                "Recycling"
            ]
        );
    }

    #[test]
    fn pads_when_too_few_subtopics() {
        let subs = parse_subtopics("Only one", "EVs");
        assert_eq!(subs.len(), 6);
        assert_eq!(subs[0], "Only one");
        assert_eq!(subs[1], "EVs: aspect 2");
        assert_eq!(subs[5], "EVs: aspect 6");
    }

    #[test]
    fn shorten_keeps_short_and_cuts_long() {
        assert_eq!(shorten("abc", 10), "abc");
        assert_eq!(shorten("abcdefghijk", 8), "abcde...");
    }

    #[test]
    fn ledger_counts_saves() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(count_saves(dir.path(), "r").unwrap(), 0);
        write_report(dir.path(), "r", "k", "text").unwrap();
        write_report(dir.path(), "r", "k", "text").unwrap();
        assert_eq!(count_saves(dir.path(), "r").unwrap(), 2);
        let saved = std::fs::read_to_string(dir.path().join("r.md")).unwrap();
        assert_eq!(saved, "text");
    }
}
