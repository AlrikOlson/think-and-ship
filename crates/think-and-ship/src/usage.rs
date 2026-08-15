//! Per-tool invocation counts — a LOCAL OPERATIONAL COUNTER, not telemetry.
//!
//! # The gap this closes
//!
//! Nothing in this system recorded that a tool was *invoked*. `corpus` records
//! artifacts (chunks, think steps, signals); the roadmap records plan state;
//! the ship trace records what an agent said it did. None of them answer "which
//! verbs are actually hot?" — a question the tool-surface work needs before it
//! can retire anything.
//!
//! # THE TRAP, labelled here because this is where a future miner will look
//!
//! `tools_used` on a think step **looks** like the answer and is poison. Mining
//! 191 session files (10,588 steps, 864 carrying the field) produces a clean
//! distribution and a flattering headline — "25 of 48 tools never used, 54.6%
//! of model-facing bytes retirable". Then check it against reality:
//! `think_record_step` appears **3** times against **9,999** persisted steps, a
//! ~3,000x undercount on the hottest verb in the system, and `ship_start` reads
//! 0 minutes after being called. The field records what an agent chose to
//! *mention*, and agents omit both the tool they are currently calling and the
//! procedure verbs they run on autopilot — which is exactly the hot path.
//! Cross-checking that "never used" list against artifacts exonerated nearly
//! all of it (`tracker_setup`: 0 mentions, 3 live configs). Acting on the
//! self-report would have retired tools in production use.
//!
//! Counts here are incremented by the dispatcher, not reported by the model.
//! That is the entire difference, and it is why this module exists.
//!
//! # DECIDED: this is not telemetry, so it is on by default
//!
//! `telemetry::consent` records a user decision (2026-06-10): telemetry is
//! **opt-in on every tier**, nothing collected or sent until a human enables
//! it. That posture governs **egress** — the thing being protected is the
//! user's content leaving their machine, which is why `telemetry::scrub` exists
//! to re-verify serialized output and why `should_send` is structurally zero
//! without a configured endpoint.
//!
//! A count of how many times `think_record_step` was called on this machine is
//! a different object:
//!
//! 1. **It is not content.** The key space is closed — the tool names this
//!    binary itself registers — and the value is an integer. There is no
//!    argument, no project text, no path, no identity.
//! 2. **It is already derivable from what we write anyway.** The 9,999 think
//!    steps sitting in the same data dir *are* the evidence of 9,999 calls.
//!    Counting does not create a new disclosure; it creates a cheaper read of
//!    one that already exists — and one that also covers the read-only verbs
//!    that leave no artifact behind, which is the actual new information.
//! 3. **It cannot be transmitted.** Not "we promise not to": no code path
//!    exists. This module is not referenced by `telemetry::shape` or
//!    `telemetry::egress`, and the counts live in their own
//!    [`Domain::Usage`] partition that the telemetry extractor does not read.
//!    A promise is worth less than a missing edge in the call graph.
//!
//! So: **on by default, never transmitted, local only**, in the same class as
//! the `otel` partition [`crate::trace_context`] already writes without asking.
//! Turn it off with `THINK_AND_SHIP_CALL_COUNTS=off`, and note that it is
//! already off wherever `THINK_AND_SHIP_PERSIST` is — a deployment that keeps
//! nothing on disk counts nothing on disk.
//!
//! # DECIDED: what makes a zero trustworthy — the soak threshold
//!
//! A count of 0 has two causes and the number cannot tell them apart:
//!
//! - the verb is genuinely unused, or
//! - **nobody has run the workflow that uses it yet on this build.**
//!
//! The second is not hypothetical — it is the failure that produced this
//! module. A tool-retirement analysis read a self-report, found "25 of 48 tools
//! never used", and would have deleted verbs three live projects depend on. A
//! dispatcher-side counter fixes the *source* of that error and reproduces its
//! *shape* on day one: this counter starts empty at the instant a rebuilt binary
//! first serves, and every one of the 48 tools reads 0. Every one of them is
//! "cold" by a naive read, and none of them are.
//!
//! So a zero is only evidence after an observation window, and the window is
//! defined here rather than left to the judgement of whoever runs the analysis.
//!
//! ## The outside anchor, and why our unit differs
//!
//! Azure API Management ships a built-in policy with the same question in it:
//! an endpoint that has received no traffic for **30 days** is considered
//! unused. That convention assumes an always-on service where traffic is
//! continuous and calendar time is therefore a fair proxy for opportunity.
//!
//! That assumption is false here. This binary only runs while an agent session
//! runs. Thirty calendar days containing two sessions is thirty days of *not
//! being asked*, and a silence nobody had the chance to break is not evidence
//! of anything. The unit that survives the difference is the **active day** — a
//! day on which at least one call landed — which is why [`CallCounts`] records
//! the set of them and not merely the last instant.
//!
//! ## The threshold: both, not either
//!
//! - [`SOAK_MIN_CALLS`] = 500. One real session dispatches on the order of
//!   40–80 calls, so 500 is roughly an order of magnitude above a single
//!   session: no one session's path can dominate the distribution.
//! - [`SOAK_MIN_ACTIVE_DAYS`] = 14. Under this project's actual rhythm, 14
//!   active days spans several calendar weeks, which is where the Azure
//!   30-day convention lands once it is re-expressed in days that count.
//!   Weekly-cadence workflows — signal churn, roadmap refresh, the business
//!   review — cannot be reached by a window of one to three days at all.
//!
//! Both are required because each catches a different lie. Volume alone is
//! satisfied by one hot workflow hammered in an afternoon, which says nothing
//! about the verbs it never touches. Spread alone is satisfied by a trickle too
//! thin to distinguish from noise.
//!
//! ## The family qualifier — the hole the two numbers do not close
//!
//! A soak can be long, broad, and still blind: fourteen active days of nothing
//! but `/roadmap` is no evidence at all about `signal_*`. So a per-verb reading
//! is qualified by its family. If a verb reads 0 and **its whole family reads
//! 0**, the honest verdict is not "cold" but *"that workflow was never run"* —
//! see [`CallCounts::verdict`], which returns that distinction as a value rather
//! than leaving it to prose a reader can skim past.
//!
//! [`Verdict::Cold`] is therefore the only variant that licenses retiring
//! anything, and it is unreachable until the window is met and the family has
//! been exercised.
//!
//! # Bounded keys
//!
//! A name the server does not recognize buckets to [`UNRECOGNIZED`] rather than
//! minting a key. Without that, a client calling random names grows this file
//! without limit — a counter whose key space is attacker-controlled is not a
//! counter, it is a write primitive.
//!
//! # Durability
//!
//! Increments accumulate as an unflushed delta and are folded into the file
//! through [`Persistence::save_merging`], which takes the exclusive advisory
//! lock, re-reads disk, and **sums**. Two servers running against one project
//! therefore both count instead of one clobbering the other — the same failure
//! that seam was built for after the 2026-06-09 incident.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::infra::{Domain, Persistence, PersistenceConfig};

/// Set to `off` / `false` / `0` to stop counting. Anything else (including
/// unset) counts — see the module doc for why the default is on.
pub const CALL_COUNTS_ENV: &str = "THINK_AND_SHIP_CALL_COUNTS";

/// The single bucket every unrecognized tool name collapses into. One key, not
/// one key per name: the key space stays closed.
pub const UNRECOGNIZED: &str = "<unrecognized>";

/// Calls that must be observed before a zero means anything. See the module
/// doc — one session is 40–80 calls, so this is an order of magnitude above
/// "one session's path".
pub const SOAK_MIN_CALLS: u64 = 500;

/// Days on which at least one call landed, before a zero means anything.
/// Active days, not calendar days: a day this binary never ran is a day it was
/// never asked, and silence nobody had the chance to break proves nothing.
pub const SOAK_MIN_ACTIVE_DAYS: usize = 14;

/// Invocation counts for one project, keyed by canonical tool name.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallCounts {
    /// Tool name → times dispatched. Sorted so the file is diffable.
    #[serde(default)]
    pub counts: BTreeMap<String, u64>,
    /// RFC3339 instant of the most recent increment.
    #[serde(default)]
    pub updated_at: String,
    /// `YYYY-MM-DD` for every day on which at least one call landed — the
    /// observation window, without which a zero is unreadable.
    ///
    /// `serde(default)` is load-bearing: files written before the soak work
    /// have no such field, and must read back as "window unknown, therefore
    /// not met" rather than failing to parse.
    #[serde(default)]
    pub active_days: BTreeSet<String>,
}

/// What a per-verb reading is actually allowed to conclude.
///
/// Only [`Verdict::Cold`] licenses retiring anything, and it is unreachable
/// until the soak window is met and the verb's family has been exercised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Observed. The count is the count.
    Used(u64),
    /// Zero, after a met soak, in a family that WAS exercised. The only
    /// reading that supports a retirement argument.
    Cold,
    /// Zero, but the observation window is too short for that to mean
    /// anything yet.
    SoakTooShort(Soak),
    /// Zero, and every other verb in its family is zero too — so the workflow
    /// that would use it was never run. Not evidence about this verb.
    FamilyUnexercised {
        /// The family prefix (`signal`, `roadmap`, …) that read zero.
        family: String,
    },
}

/// Whether the observation window is long enough for a zero to be evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Soak {
    /// Calls observed so far.
    pub total_calls: u64,
    /// Days on which at least one call landed.
    pub active_days: usize,
    /// Both thresholds cleared.
    pub met: bool,
    /// One line per unmet threshold, phrased as what is still missing.
    pub missing: Vec<String>,
}

impl CallCounts {
    /// Total invocations across every tool.
    #[must_use]
    pub fn total(&self) -> u64 {
        self.counts.values().sum()
    }

    /// The family prefix of a tool name — `signal_research` → `signal`.
    /// Unrecognized names have no family.
    #[must_use]
    fn family_of(tool: &str) -> Option<&str> {
        tool.split_once('_').map(|(head, _)| head)
    }

    /// Total calls across every verb sharing `family`'s prefix.
    #[must_use]
    pub fn family_total(&self, family: &str) -> u64 {
        self.counts
            .iter()
            .filter(|(name, _)| Self::family_of(name) == Some(family))
            .map(|(_, n)| *n)
            .sum()
    }

    /// Whether the observation window is long enough for a zero to be
    /// evidence. See the module doc for why both thresholds are required.
    #[must_use]
    pub fn soak(&self) -> Soak {
        let total_calls = self.total();
        let active_days = self.active_days.len();
        let mut missing = Vec::new();
        if total_calls < SOAK_MIN_CALLS {
            missing.push(format!(
                "{} more call(s) — {total_calls} of {SOAK_MIN_CALLS}",
                SOAK_MIN_CALLS - total_calls
            ));
        }
        if active_days < SOAK_MIN_ACTIVE_DAYS {
            missing.push(format!(
                "{} more active day(s) — {active_days} of {SOAK_MIN_ACTIVE_DAYS}",
                SOAK_MIN_ACTIVE_DAYS - active_days
            ));
        }
        Soak {
            total_calls,
            active_days,
            met: missing.is_empty(),
            missing,
        }
    }

    /// What this reading of `tool` is allowed to conclude.
    ///
    /// The order of the checks is the argument: a non-zero count needs no
    /// window at all, and a zero must clear BOTH the global window and its own
    /// family's before it may be called cold.
    #[must_use]
    pub fn verdict(&self, tool: &str) -> Verdict {
        match self.counts.get(tool).copied().unwrap_or(0) {
            n if n > 0 => Verdict::Used(n),
            _ => {
                let soak = self.soak();
                if !soak.met {
                    return Verdict::SoakTooShort(soak);
                }
                match Self::family_of(tool) {
                    Some(family) if self.family_total(family) == 0 => Verdict::FamilyUnexercised {
                        family: family.to_string(),
                    },
                    _ => Verdict::Cold,
                }
            }
        }
    }

    /// Counts as `(name, count)` pairs, hottest first, ties broken by name so
    /// the output is stable across runs.
    #[must_use]
    pub fn ranked(&self) -> Vec<(&str, u64)> {
        let mut rows: Vec<(&str, u64)> = self
            .counts
            .iter()
            .map(|(name, n)| (name.as_str(), *n))
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
        rows
    }
}

/// Fold an unflushed `delta` into the `disk` state by SUMMING each key.
///
/// This is the merge handed to [`Persistence::save_merging`], so it runs under
/// the exclusive lock with a fresh read of disk. Summing — rather than
/// last-writer-wins, which every other family uses — is what makes two live
/// servers on one project both count.
///
/// `active_days` unions for the same reason and one more: the window is the
/// project's, not the process's. A second server that starts today must not
/// shorten a window the first one spent two weeks accumulating.
#[must_use]
pub fn merge_counts(delta: &CallCounts, mut disk: CallCounts) -> CallCounts {
    for (name, n) in &delta.counts {
        *disk.counts.entry(name.clone()).or_insert(0) += n;
    }
    disk.active_days.extend(delta.active_days.iter().cloned());
    disk.updated_at = delta.updated_at.clone();
    disk
}

/// The calendar day of an RFC3339 instant — everything before the `T`.
///
/// Deliberately textual: the stamp is already produced upstream in a fixed
/// format, and re-parsing it into a datetime only to re-format it would add a
/// failure mode to a function whose every failure mode must be silence.
#[must_use]
fn day_of(now: &str) -> Option<String> {
    let day = now.split('T').next().unwrap_or_default();
    (day.len() == 10).then(|| day.to_string())
}

/// Whether a raw `THINK_AND_SHIP_CALL_COUNTS` value means "count".
///
/// Split out from the `std::env` read so the DECISION is reachable by a test
/// and only the environment lookup stays untestable — the same split
/// `push_schedule_interval` and `unattended_propose_enabled` use.
#[must_use]
pub fn counting_enabled(raw: Option<&str>) -> bool {
    match raw {
        None => true,
        Some(v) => {
            let v = v.trim().to_ascii_lowercase();
            !(v == "off" || v == "false" || v == "0" || v == "no")
        }
    }
}

/// The dispatcher's counter. One per server process.
#[derive(Debug)]
pub struct CallCounter {
    store: Persistence,
    project: String,
    /// Every tool name this binary registers, across all families and
    /// regardless of which are selected. A deselected family's tool is still a
    /// real name, and a call that gets refused is still a call.
    known: BTreeSet<String>,
    /// Increments — and the days they landed on — not yet folded into the
    /// file. Retained on write failure so a transient IO error loses nothing,
    /// the active day included: a dropped day silently shortens the window.
    pending: Mutex<CallCounts>,
    enabled: bool,
}

impl CallCounter {
    /// Build against an explicit store — the seam a test uses to point the
    /// counter at a scratch directory without mutating process-global env.
    #[must_use]
    pub fn new(
        store: Persistence,
        project: impl Into<String>,
        known: impl IntoIterator<Item = String>,
        enabled: bool,
    ) -> Self {
        Self {
            store,
            project: project.into(),
            known: known.into_iter().collect(),
            pending: Mutex::new(CallCounts::default()),
            enabled,
        }
    }

    /// Build from the environment, as a real server does.
    #[must_use]
    pub fn from_env(known: impl IntoIterator<Item = String>) -> Self {
        let cfg = PersistenceConfig::from_env();
        let on = cfg.enabled && counting_enabled(std::env::var(CALL_COUNTS_ENV).ok().as_deref());
        let store = Persistence::new(&cfg, Domain::Usage);
        Self::new(store, crate::infra::resolve_project_id(None), known, on)
    }

    /// A counter that never counts — for construction sites that have no
    /// business writing files.
    #[must_use]
    pub fn disabled() -> Self {
        Self::new(
            Persistence::new(&PersistenceConfig::from_env().enabled(false), Domain::Usage),
            String::new(),
            std::iter::empty(),
            false,
        )
    }

    /// Record one dispatch of `tool_name`.
    ///
    /// Total by construction: every failure mode is silence. A counter that
    /// could fail a tool call would be worse than no counter.
    pub fn record(&self, tool_name: &str, now: &str) {
        if !self.enabled {
            return;
        }
        let key = if self.known.contains(tool_name) {
            tool_name
        } else {
            UNRECOGNIZED
        };
        let Ok(mut pending) = self.pending.lock() else {
            return;
        };
        *pending.counts.entry(key.to_string()).or_insert(0) += 1;
        // A malformed stamp costs the day, not the count — the window is
        // allowed to under-report, never to fail a call.
        if let Some(day) = day_of(now) {
            pending.active_days.insert(day);
        }
        pending.updated_at = now.to_string();
        let delta = pending.clone();
        // Held across the write on purpose: the lock is what makes "flush then
        // clear" atomic, so a concurrent caller can neither double-count a
        // flushed increment nor drop an unflushed one.
        if self
            .store
            .save_merging(&self.project, &delta, merge_counts)
            .is_ok()
        {
            *pending = CallCounts::default();
        }
    }

    /// This project's counts as currently persisted, including anything this
    /// process has already flushed.
    #[must_use]
    pub fn snapshot(&self) -> CallCounts {
        load_from(&self.store, &self.project).unwrap_or_default()
    }
}

fn store_handle() -> Persistence {
    Persistence::new(&PersistenceConfig::from_env(), Domain::Usage)
}

/// This project's persisted counts, read with no server running. The question
/// these answer is asked *between* sessions, which is the whole reason the
/// numbers live in a file rather than in a process.
#[must_use]
pub fn load(project: &str) -> Option<CallCounts> {
    load_from(&store_handle(), project)
}

/// [`load`] against an explicit handle. See [`CallCounter::new`].
pub(crate) fn load_from(store: &Persistence, project: &str) -> Option<CallCounts> {
    store.load(project).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PersistenceConfig {
        let dir = std::env::temp_dir().join(format!(
            "tas-usage-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        PersistenceConfig::from_env()
            .enabled(true)
            .with_data_dir(dir)
    }

    fn counter(cfg: &PersistenceConfig, names: &[&str]) -> CallCounter {
        CallCounter::new(
            Persistence::new(cfg, Domain::Usage),
            "proj-usage",
            names.iter().map(|s| (*s).to_string()),
            true,
        )
    }

    #[test]
    fn n_calls_reads_n() {
        let cfg = scratch("exact");
        let c = counter(&cfg, &["think_record_step", "ship_check"]);
        for _ in 0..7 {
            c.record("think_record_step", "2026-07-28T00:00:00Z");
        }
        c.record("ship_check", "2026-07-28T00:00:01Z");
        let got = c.snapshot();
        assert_eq!(got.counts.get("think_record_step"), Some(&7));
        assert_eq!(got.counts.get("ship_check"), Some(&1));
        assert_eq!(got.total(), 8);
    }

    #[test]
    fn unknown_names_share_one_bucket_and_never_mint_keys() {
        let cfg = scratch("bounded");
        let c = counter(&cfg, &["think_record_step"]);
        for i in 0..20 {
            c.record(&format!("attacker_{i}"), "2026-07-28T00:00:00Z");
        }
        let got = c.snapshot();
        assert_eq!(
            got.counts.len(),
            1,
            "20 distinct unknown names must not mint 20 keys"
        );
        assert_eq!(got.counts.get(UNRECOGNIZED), Some(&20));
    }

    /// The claim the CLI rests on: a *later, separate* handle — as a different
    /// process would build — reads what the server wrote.
    #[test]
    fn counts_survive_the_process_boundary() {
        let cfg = scratch("boundary");
        let c = counter(&cfg, &["roadmap_status"]);
        c.record("roadmap_status", "2026-07-28T00:00:00Z");

        let reader = Persistence::new(&cfg, Domain::Usage);
        let got = load_from(&reader, "proj-usage").expect("a later process must see the counts");
        assert_eq!(got.counts.get("roadmap_status"), Some(&1));
        assert!(
            load_from(&reader, "proj-other").is_none(),
            "counts must not bleed across projects"
        );
    }

    /// Two live servers on one project both count. The merge sums; it does not
    /// last-writer-win.
    #[test]
    fn concurrent_servers_both_count() {
        let cfg = scratch("concurrent");
        let a = counter(&cfg, &["signal_capture"]);
        let b = counter(&cfg, &["signal_capture"]);
        a.record("signal_capture", "2026-07-28T00:00:00Z");
        b.record("signal_capture", "2026-07-28T00:00:01Z");
        a.record("signal_capture", "2026-07-28T00:00:02Z");
        assert_eq!(
            b.snapshot().counts.get("signal_capture"),
            Some(&3),
            "a stale writer must not erase the other server's increments"
        );
    }

    #[test]
    fn disabled_counter_writes_nothing() {
        let cfg = scratch("disabled");
        let c = CallCounter::new(
            Persistence::new(&cfg, Domain::Usage),
            "proj-usage",
            ["think_record_step".to_string()],
            false,
        );
        c.record("think_record_step", "2026-07-28T00:00:00Z");
        assert_eq!(c.snapshot(), CallCounts::default());
    }

    #[test]
    fn off_switch_is_read_not_assumed() {
        assert!(counting_enabled(None), "default is on — it never leaves");
        assert!(counting_enabled(Some("on")));
        assert!(counting_enabled(Some("1")));
        for off in ["off", "OFF", "false", "0", "no", " off "] {
            assert!(!counting_enabled(Some(off)), "{off} must disable counting");
        }
    }

    /// Build counts with `days` distinct active days and `n` calls on `tool`.
    fn soaked(tool: &str, n: u64, days: usize) -> CallCounts {
        CallCounts {
            counts: BTreeMap::from([(tool.to_string(), n)]),
            updated_at: "2026-07-28T00:00:00Z".to_string(),
            active_days: (0..days).map(|i| format!("2026-06-{:02}", i + 1)).collect(),
        }
    }

    /// The whole chunk in one assertion: on day one every verb reads zero, and
    /// none of them may be called cold.
    #[test]
    fn day_one_zero_is_never_cold() {
        let counts = soaked("think_record_step", 12, 1);
        match counts.verdict("signal_research") {
            Verdict::SoakTooShort(soak) => {
                assert!(!soak.met);
                assert_eq!(
                    soak.missing.len(),
                    2,
                    "both thresholds are unmet on day one"
                );
            }
            other => panic!("a day-one zero must not be readable as cold: {other:?}"),
        }
    }

    /// Volume alone is not enough — an afternoon of hammering one workflow.
    #[test]
    fn volume_without_spread_does_not_meet_the_soak() {
        let soak = soaked("roadmap_status", SOAK_MIN_CALLS * 4, 2).soak();
        assert!(!soak.met, "4x the call volume over 2 days must not qualify");
        assert_eq!(soak.missing.len(), 1, "only the day threshold is unmet");
        assert!(soak.missing[0].contains("active day"));
    }

    /// Spread alone is not enough either — a trickle indistinguishable from
    /// noise.
    #[test]
    fn spread_without_volume_does_not_meet_the_soak() {
        let soak = soaked("roadmap_status", 30, SOAK_MIN_ACTIVE_DAYS * 3).soak();
        assert!(!soak.met, "30 calls over 6 weeks must not qualify");
        assert_eq!(soak.missing.len(), 1);
        assert!(soak.missing[0].contains("call"));
    }

    /// The hole the two numbers do not close: a met soak made entirely of
    /// `/roadmap` says nothing about `signal_*`.
    #[test]
    fn a_met_soak_still_refuses_a_verb_whose_family_never_ran() {
        let counts = soaked("roadmap_status", SOAK_MIN_CALLS, SOAK_MIN_ACTIVE_DAYS);
        assert!(counts.soak().met, "precondition: the window itself is met");
        assert_eq!(
            counts.verdict("signal_research"),
            Verdict::FamilyUnexercised {
                family: "signal".to_string()
            },
            "an unexercised family is not evidence about its verbs"
        );
    }

    /// And the one reading that DOES license a retirement argument: met soak,
    /// family exercised by a sibling, this verb still zero.
    #[test]
    fn cold_requires_a_met_soak_and_an_exercised_family() {
        let mut counts = soaked("signal_capture", SOAK_MIN_CALLS, SOAK_MIN_ACTIVE_DAYS);
        assert_eq!(counts.verdict("signal_research"), Verdict::Cold);
        assert_eq!(
            counts.verdict("signal_capture"),
            Verdict::Used(SOAK_MIN_CALLS),
            "a non-zero count needs no window at all"
        );

        // Deliberate breakage: drop the family's traffic and Cold must become unreachable.
        counts.counts.insert("signal_capture".to_string(), 0);
        counts
            .counts
            .insert("roadmap_status".to_string(), SOAK_MIN_CALLS);
        assert_ne!(
            counts.verdict("signal_research"),
            Verdict::Cold,
            "Cold must not survive the family going quiet"
        );
    }

    /// The window is the project's, not the process's: a server starting today
    /// must not shorten a window another spent two weeks accumulating.
    #[test]
    fn merge_unions_active_days_rather_than_replacing_them() {
        let disk = soaked("roadmap_status", 5, 10);
        let delta = CallCounts {
            counts: BTreeMap::from([("roadmap_status".to_string(), 1)]),
            updated_at: "2026-07-28T00:00:00Z".to_string(),
            active_days: BTreeSet::from(["2026-07-28".to_string()]),
        };
        let merged = merge_counts(&delta, disk);
        assert_eq!(merged.active_days.len(), 11, "10 old days + 1 new");
        assert!(merged.active_days.contains("2026-06-01"));
        assert!(merged.active_days.contains("2026-07-28"));
    }

    /// The live counter must actually populate the window — the day comes off
    /// the dispatch stamp, and repeated calls on one day count as one day.
    #[test]
    fn recorded_calls_populate_the_observation_window() {
        let cfg = scratch("window");
        let c = counter(&cfg, &["roadmap_status"]);
        c.record("roadmap_status", "2026-07-28T01:00:00Z");
        c.record("roadmap_status", "2026-07-28T09:00:00Z");
        c.record("roadmap_status", "2026-07-29T01:00:00Z");
        let got = c.snapshot();
        assert_eq!(got.total(), 3);
        assert_eq!(
            got.active_days,
            BTreeSet::from(["2026-07-28".to_string(), "2026-07-29".to_string()]),
            "two calendar days, three calls"
        );
    }

    /// A file written before the soak work has no `active_days`. It must read
    /// back as "window unknown, therefore not met" — never fail to parse, and
    /// never silently qualify.
    #[test]
    fn counts_without_a_window_field_load_as_an_unmet_soak() {
        let legacy =
            r#"{"counts":{"think_record_step":90000},"updated_at":"2026-07-01T00:00:00Z"}"#;
        let counts: CallCounts =
            serde_json::from_str(legacy).expect("legacy files must still load");
        assert_eq!(counts.total(), 90_000);
        let soak = counts.soak();
        assert!(
            !soak.met,
            "no recorded window must never pass on call volume alone"
        );
        assert_eq!(soak.active_days, 0);
    }

    #[test]
    fn a_malformed_stamp_costs_the_day_not_the_count() {
        let cfg = scratch("badstamp");
        let c = counter(&cfg, &["roadmap_status"]);
        c.record("roadmap_status", "not-a-timestamp");
        let got = c.snapshot();
        assert_eq!(got.counts.get("roadmap_status"), Some(&1));
        assert!(got.active_days.is_empty());
    }

    #[test]
    fn ranked_is_hottest_first_and_stable() {
        let counts = CallCounts {
            counts: BTreeMap::from([
                ("b_tool".to_string(), 5),
                ("a_tool".to_string(), 5),
                ("hot".to_string(), 9),
            ]),
            updated_at: String::new(),
            active_days: BTreeSet::new(),
        };
        assert_eq!(
            counts.ranked(),
            vec![("hot", 9), ("a_tool", 5), ("b_tool", 5)]
        );
    }
}
