//! Trigger-selection and behavioural evaluations for the two core skills.
//!
//! # What a trigger test here can and cannot prove
//!
//! It cannot run a model, so it does not claim to. What decides selection at
//! runtime is the `description` — it is the only part of a skill loaded before
//! activation — so what is checkable, and what these tests check, is that the
//! description actually carries the language a user would use, and that the
//! near-miss requests are explicitly disowned somewhere a model will read.
//!
//! That is a proxy, and stating its limits is part of the test. A skill whose
//! description omits its own trigger phrases cannot be selected reliably no
//! matter how good the model; a skill that never disowns its near-misses will
//! be over-selected. Both failures are visible from the text, and both are what
//! these assertions catch.
//!
//! The behavioural half needs no proxy: it drives the real engine and the real
//! tool seam.

use std::path::{Path, PathBuf};

use think_and_ship::roadmap::RoadmapEngine;
use think_and_ship::roadmap::domain::{ChunkStatus, FocusMode};
use think_and_ship::roadmap::engine::GroupResolution;

fn skills_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills")
}

fn skill_text(name: &str) -> String {
    std::fs::read_to_string(skills_dir().join(name).join("SKILL.md"))
        .unwrap_or_else(|e| panic!("reading {name}/SKILL.md: {e}"))
}

/// The `description` value, flattened to one line.
fn description(name: &str) -> String {
    let t = skill_text(name);
    let fm = t
        .strip_prefix("---\n")
        .and_then(|r| r.split_once("\n---\n").map(|(f, _)| f.to_string()))
        .expect("frontmatter");
    let mut out = String::new();
    let mut in_desc = false;
    for line in fm.lines() {
        if line.starts_with("description:") {
            in_desc = true;
            out.push_str(
                line.trim_start_matches("description:")
                    .trim_end_matches(">-")
                    .trim(),
            );
            continue;
        }
        if in_desc {
            if line.starts_with(' ') || line.trim().is_empty() {
                out.push(' ');
                out.push_str(line.trim());
            } else {
                break;
            }
        }
    }
    out.to_lowercase()
}

/// Content words of an utterance, minus the filler that matches everything.
fn content_words(utterance: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "a", "an", "to", "in", "on", "is", "am", "i", "my", "me", "this", "that", "it",
        "and", "or", "of", "for", "do", "did", "does", "what", "which", "with", "at", "be", "are",
        "was", "were", "run", "give",
    ];
    utterance
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-')
        .filter(|w| w.len() > 1 && !STOP.contains(w))
        .map(str::to_string)
        .collect()
}

// ── Trigger selection ──────────────────────────────────────────────

/// A skill's description must carry the language of the requests it is FOR.
///
/// Asserted per-utterance rather than in aggregate: an average would let one
/// well-covered phrase hide three that are absent.
#[test]
fn positive_triggers_appear_in_the_description_that_decides_selection() {
    let cases: &[(&str, &[&str])] = &[
        (
            "switch-work",
            &[
                "Switch to authentication in build mode.",
                "Focus billing for shaping.",
                "What workstream am I currently focused on?",
            ],
        ),
        (
            "advance-work",
            &[
                "Advance the current work.",
                "Do the next unit in this focus.",
            ],
        ),
    ];

    for (skill, utterances) in cases {
        let desc = description(skill);
        for u in *utterances {
            let words = content_words(u);
            let hits = words.iter().filter(|w| desc.contains(w.as_str())).count();
            // Most of the utterance's meaning-bearing words, not just one.
            assert!(
                hits * 2 >= words.len(),
                "{skill}'s description covers only {hits}/{} content words of {u:?} \
                 — a user saying this would not reliably select it.\nmissing: {:?}",
                words.len(),
                words
                    .iter()
                    .filter(|w| !desc.contains(w.as_str()))
                    .collect::<Vec<_>>()
            );
        }
    }
}

/// Every harness's own invocation form is documented, so a user can actually
/// type the thing the description promises.
#[test]
fn each_harness_invocation_form_is_documented() {
    let matrix = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/HARNESSES.md"),
    )
    .expect("the harness matrix");
    // The four distinct invocation shapes across the twelve harnesses.
    for form in [
        "/<skill",
        "/&lt;skill",
        "$skill-name",
        "@skill-name",
        "/switch-work",
    ] {
        if matrix.contains(form) {
            return;
        }
    }
    panic!("no harness invocation form is documented in docs/HARNESSES.md");
}

/// A near-miss request must be explicitly disowned, or the skill will be
/// selected for work it must not do.
///
/// These are the cases where over-selection is dangerous rather than merely
/// annoying: "run every remaining roadmap chunk" against a skill whose whole
/// contract is one unit, and "fix this bug" against a skill that must not
/// implement anything.
#[test]
fn negative_triggers_are_explicitly_disowned() {
    let cases: &[(&str, &[(&str, &str)])] = &[
        (
            "switch-work",
            &[
                ("fix this authentication bug", "fix this authentication bug"),
                ("show roadmap status", "show roadmap status"),
                ("research payment providers", "research payment providers"),
                ("implement the next chunk", "implement the next chunk"),
            ],
        ),
        (
            "advance-work",
            &[
                ("switch to billing", "switch to billing"),
                (
                    "run every remaining roadmap chunk",
                    "run every remaining roadmap chunk",
                ),
                ("give me a board briefing", "give me a board briefing"),
                ("fix this unrelated typo", "fix this unrelated typo"),
            ],
        ),
    ];

    for (skill, pairs) in cases {
        let body = skill_text(skill).to_lowercase();
        for (utterance, needle) in *pairs {
            assert!(
                body.contains(&needle.to_lowercase()),
                "{skill} never disowns {utterance:?} — a model reading only this skill \
                 has nothing telling it this request belongs elsewhere"
            );
        }
    }
}

/// The negative cases must be disowned in a section that says so, not merely
/// mentioned in passing.
#[test]
fn the_disowning_is_in_a_section_a_reader_will_recognize() {
    for skill in ["switch-work", "advance-work"] {
        let body = skill_text(skill);
        assert!(
            body.contains("Not this skill") || body.contains("Not for"),
            "{skill} has no section marking out what it is NOT for"
        );
    }
}

/// Both skills change consequential state, so both must state that they are
/// manually invoked — the mitigation that has to carry the harnesses which
/// document no manual-only control (Cline, Roo Code, Amp, Goose, Kiro).
#[test]
fn both_core_skills_declare_their_manual_intent_in_the_description() {
    for (skill, phrase) in [
        ("switch-work", "run it when the user types"),
        ("advance-work", "run it when the user types"),
    ] {
        assert!(
            description(skill).contains(phrase),
            "{skill}'s description does not front-load its manual-command intent"
        );
    }
}

// ── Behaviour ──────────────────────────────────────────────────────

fn engine_with_workstreams() -> RoadmapEngine {
    let mut e = RoadmapEngine::new("evals".into());
    for (id, priority, group) in [
        ("auth-1", 10u32, Some("Authentication")),
        ("auth-2", 30, Some("Authentication")),
        ("bill-1", 1, Some("Billing")),
    ] {
        e.add_chunk(
            id.into(),
            format!("Chunk {id}"),
            ChunkStatus::Pending,
            priority,
            String::new(),
            vec![],
            vec![],
            false,
        )
        .expect("chunk added");
        if let Some(g) = group {
            e.set_group(id, Some(g.to_string()))
                .expect("real place name");
        }
    }
    e
}

#[test]
fn an_unknown_group_mutates_no_focus() {
    let mut e = engine_with_workstreams();
    e.focus_set("lane", "Authentication", FocusMode::Build)
        .unwrap();
    assert!(e.focus_set("lane", "Payments", FocusMode::Build).is_err());
    let f = e.focus_get("lane").expect("focus survived");
    assert_eq!(
        (f.group.as_str(), f.mode),
        ("Authentication", FocusMode::Build)
    );
}

#[test]
fn an_ambiguous_group_mutates_no_focus_and_names_the_candidates() {
    let mut e = RoadmapEngine::new("evals".into());
    for (id, group) in [("a1", "Billing core"), ("b1", "Billing reports")] {
        e.add_chunk(
            id.into(),
            "t".into(),
            ChunkStatus::Pending,
            10,
            String::new(),
            vec![],
            vec![],
            false,
        )
        .unwrap();
        e.set_group(id, Some(group.to_string())).unwrap();
    }
    match e.resolve_group("billing") {
        GroupResolution::Ambiguous(names) => {
            assert_eq!(names, vec!["Billing core", "Billing reports"]);
        }
        other => panic!("expected ambiguity, got {other:?}"),
    }
    assert!(e.focus_set("lane", "billing", FocusMode::Build).is_err());
    assert!(e.focus_get("lane").is_none());
}

#[test]
fn two_lanes_keep_independent_focuses() {
    let mut e = engine_with_workstreams();
    e.focus_set("/w/a", "Authentication", FocusMode::Build)
        .unwrap();
    e.focus_set("/w/b", "Billing", FocusMode::Listen).unwrap();
    assert_eq!(e.focus_get("/w/a").unwrap().group, "Authentication");
    assert_eq!(e.focus_get("/w/b").unwrap().group, "Billing");
    assert_eq!(e.focus_get("/w/a").unwrap().mode, FocusMode::Build);
    assert_eq!(e.focus_get("/w/b").unwrap().mode, FocusMode::Listen);
}

#[test]
fn switching_focus_changes_no_chunk_status() {
    let mut e = engine_with_workstreams();
    let before = e.roadmap().chunks.clone();
    e.focus_set("lane", "Authentication", FocusMode::Build)
        .unwrap();
    e.focus_set("lane", "Billing", FocusMode::Shape).unwrap();
    e.focus_clear("lane");
    assert_eq!(e.roadmap().chunks, before);
}

/// The property the whole focused loop rests on: what `advance-work` is handed
/// cannot come from outside its workstream, even when a better candidate exists
/// elsewhere.
#[test]
fn the_focused_frontier_cannot_escape_its_workstream() {
    let e = engine_with_workstreams();
    // bill-1 has the best priority on the whole board.
    assert_eq!(e.next().map(|c| c.id.as_str()), Some("bill-1"));
    // A caller focused on Authentication is never handed it.
    assert_eq!(
        e.next_in_group("Authentication").map(|c| c.id.as_str()),
        Some("auth-1")
    );
    // An empty workstream yields nothing rather than falling through.
    assert!(e.next_in_group("Platform").is_none());
}

#[test]
fn no_ready_work_is_reported_as_a_state_rather_than_an_absence() {
    let mut e = engine_with_workstreams();
    // Block the whole Authentication workstream.
    e.set_status("auth-1", ChunkStatus::InProgress).unwrap();
    e.set_status("auth-2", ChunkStatus::Blocked).unwrap();

    let s = e.group_status("Authentication");
    assert_eq!(s["ready_count"], 0);
    assert_eq!(s["next"], serde_json::Value::Null);
    // The frontier still describes the workstream rather than going silent.
    assert_eq!(s["counts"]["total"], 2);
    assert_eq!(s["blocked_count"], 1);
}

/// A no-focus lane is an actionable stop, not an error and not a default.
#[test]
fn a_lane_with_no_focus_is_answered_rather_than_defaulted() {
    let e = engine_with_workstreams();
    assert!(e.focus_get("never-focused").is_none());
    // And the workstreams are still discoverable, which is what makes the
    // stop actionable rather than a dead end.
    assert_eq!(e.groups(), vec!["Authentication", "Billing"]);
}

/// Mode is closed. An agent cannot invent a fourth boundary by naming one.
#[test]
fn mode_is_closed_at_exactly_three() {
    assert_eq!(FocusMode::ALL.len(), 3);
    for m in FocusMode::ALL {
        assert_eq!(FocusMode::from_wire(m.as_wire()).unwrap(), m);
    }
    for invented in ["implement", "code", "plan", "review", ""] {
        assert!(
            FocusMode::from_wire(invented).is_err(),
            "{invented:?} must not be accepted as a mode"
        );
    }
}

// ── Mode boundaries, as stated in the skill the agent reads ────────

/// Each mode reference states its own boundary. These are the sentences that
/// stop the mode doing the thing it is most tempted to do, so their absence is
/// a defect even though the code cannot enforce them.
#[test]
fn each_mode_reference_states_the_boundary_that_constrains_it() {
    let read = |f: &str| {
        std::fs::read_to_string(skills_dir().join("advance-work").join("references").join(f))
            .unwrap_or_else(|e| panic!("reading {f}: {e}"))
    };

    let shape = read("shape.md");
    assert!(shape.contains("must not modify implementation source"));
    assert!(
        shape.contains("prototype"),
        "shape must close the prototype loophole, not just state the rule"
    );
    assert!(
        shape.to_lowercase().contains("proposal"),
        "shape must say reprioritization is a proposal, not an action"
    );

    let build = read("build.md");
    assert!(build.contains("A red gate means the chunk is not done"));
    assert!(
        build.contains("A skipped check is red"),
        "build must treat an unrun required check as a failure"
    );
    assert!(
        build.contains("Piping hides failures"),
        "build must warn that a piped gate records the filter's exit code"
    );

    let listen = read("listen.md");
    assert!(listen.contains("Never process a second signal"));
    assert!(
        listen.contains("establish with evidence"),
        "listen must require evidence for a relevance claim"
    );

    let speckit = read("speckit.md");
    assert!(speckit.contains("do not initialize Spec Kit"));
    assert!(
        speckit.contains("Find the existing feature before creating one"),
        "the adapter must look for an existing feature first"
    );
}

/// Optional MCP servers are named as optional in both places that matter.
#[test]
fn optional_servers_degrade_rather_than_block() {
    // Whitespace-normalized: these phrases are prose and get re-wrapped by any
    // editor, so matching raw text would fail on a reflow rather than on a
    // missing rule — a gate that fires on formatting teaches people to ignore it.
    let body = skill_text("advance-work")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(body.contains("Optional tools"));
    assert!(
        body.contains("never claim grounding you did not do"),
        "the degraded path must forbid overclaiming"
    );
    assert!(
        body.contains("never refuse work you can still do"),
        "a missing optional server must not become a blocker"
    );
}

// ── Documentation parity ───────────────────────────────────────────

fn doc(name: &str) -> String {
    std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../docs")
            .join(name),
    )
    .unwrap_or_else(|e| panic!("reading docs/{name}: {e}"))
}

/// Per-family tool counts in the docs are CHECKED, not hand-maintained.
///
/// Five stale counts across four files went unnoticed until the surface grew
/// during this initiative — the tests caught those, but `docs/TOOLS.md` had
/// been claiming 12 roadmap tools while the registry served 15 for some time,
/// because nothing compared them. This is that comparison.
#[test]
fn documented_tool_counts_match_the_real_registry() {
    use think_and_ship::mcp::UnifiedService;
    use think_and_ship::roadmap::{RoadmapEngine, RoadmapService};
    use think_and_ship::ship::ShipService;
    use think_and_ship::ship::engine::ShipEngine;
    use think_and_ship::signal::{SignalEngine, SignalService};
    use think_and_ship::think::ThinkService;
    use think_and_ship::think::config::ThinkConfig;
    use think_and_ship::think::engine::core::ReasoningServer;

    let mut cfg = ThinkConfig::default();
    cfg.display.color_output = false;
    let svc = UnifiedService::new(
        ThinkService::new(ReasoningServer::new(cfg)),
        ShipService::new(ShipEngine::new("docs".into())),
        RoadmapService::new(RoadmapEngine::new("docs".into())),
        SignalService::new(SignalEngine::new("docs".into())),
    );
    let names: Vec<String> = svc
        .list_tools_view()
        .iter()
        .map(|t| t.name.to_string())
        .collect();

    let tools = doc("TOOLS.md");
    for family in ["think", "ship", "roadmap", "signal"] {
        let served = names
            .iter()
            .filter(|n| n.starts_with(&format!("{family}_")))
            .count();
        let heading = regex_count(&tools, family);
        assert_eq!(
            heading,
            Some(served),
            "docs/TOOLS.md says the {family}_* family has {heading:?} tools; the registry \
             serves {served}. Update the heading — a count nobody checks is a count that drifts."
        );
    }

    // Every roadmap tool the registry serves has a row in the table.
    for name in names.iter().filter(|n| n.starts_with("roadmap_")) {
        assert!(
            tools.contains(&format!("`{name}`")),
            "docs/TOOLS.md never mentions {name}"
        );
    }
}

/// The `(N tools)` figure from a `## \`<family>_*\` — … (N tools)` heading.
fn regex_count(doc: &str, family: &str) -> Option<usize> {
    let marker = format!("## `{family}_*`");
    let line = doc.lines().find(|l| l.starts_with(&marker))?;
    let open = line.rfind('(')?;
    line[open + 1..]
        .split_whitespace()
        .next()?
        .parse::<usize>()
        .ok()
}

/// The docs describe the surface that exists, not the one that used to.
#[test]
fn the_docs_do_not_advertise_a_retired_destination_as_live() {
    let workflows = doc("WORKFLOWS.md");
    // The stale Codex path may only appear where it is explained as retired.
    for (name, text) in [("WORKFLOWS.md", &workflows)] {
        for line in text.lines().filter(|l| l.contains(".codex/skills")) {
            assert!(
                line.contains("retire")
                    || line.contains("no longer")
                    || line.contains("does not read"),
                "docs/{name} mentions ~/.codex/skills without saying it is retired:\n  {line}"
            );
        }
    }
    // And the current surface is described.
    assert!(
        workflows.contains("switch-work"),
        "WORKFLOWS.md omits switch-work"
    );
    assert!(
        workflows.contains("advance-work"),
        "WORKFLOWS.md omits advance-work"
    );
    assert!(
        workflows.contains("HARNESSES.md"),
        "WORKFLOWS.md must point at the verified harness matrix rather than restating paths"
    );
}

/// Honest limitations are stated, including which harnesses were never run.
#[test]
fn the_docs_state_what_was_not_runtime_verified() {
    // Whitespace-normalized: these are prose sentences that wrap, and a gate
    // that fires on a reflow gets suppressed rather than fixed.
    let migration = doc("MIGRATION.md")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(
        migration.contains("runtime not exercised"),
        "MIGRATION.md must say plainly which harnesses were validated as artifacts only"
    );
    assert!(
        migration.contains("document no manual-only invocation control"),
        "MIGRATION.md must name the harnesses where the manual-only guard is weaker"
    );
}

/// The receipt is mandatory on every outcome, including the ones that did no
/// work — which is where a receipt is most likely to be skipped.
#[test]
fn the_receipt_contract_covers_every_outcome() {
    let body = skill_text("advance-work");
    let receipt =
        std::fs::read_to_string(skills_dir().join("advance-work/references/receipt.md")).unwrap();

    for field in [
        "Focus:",
        "Lane:",
        "Mode:",
        "Unit:",
        "Result:",
        "Evidence:",
        "Native records:",
        "Discoveries:",
        "Next candidate:",
        "Stop reason:",
    ] {
        assert!(body.contains(field), "SKILL.md receipt omits {field}");
        assert!(receipt.contains(field), "receipt.md omits {field}");
    }
    for result in ["completed", "blocked", "no-ready-work", "awaiting-human"] {
        assert!(
            receipt.contains(result),
            "receipt.md has no {result} example"
        );
    }
    assert!(
        receipt.contains("recomputed, **not executed**"),
        "the receipt must say the next candidate is not to be run"
    );
}
