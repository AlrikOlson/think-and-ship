//! End-to-end smoke test for the UnifiedService: all four families register
//! together and expose the full 53-tool surface (think 11, ship 13, roadmap 17,
//! tracker 2, signal 10 — all canonical).

use std::collections::BTreeSet;

use think_and_ship::mcp::UnifiedService;
use think_and_ship::roadmap::{RoadmapEngine, RoadmapService};
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::signal::{SignalEngine, SignalService};
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::engine::core::ReasoningServer;

fn build_unified() -> UnifiedService {
    let mut cfg = ThinkConfig::default();
    cfg.display.color_output = false;
    let think = ThinkService::new(ReasoningServer::new(cfg));
    let ship = ShipService::new(ShipEngine::new("test-abc123".into()));
    let roadmap = RoadmapService::new(RoadmapEngine::new("test-abc123".into()));
    let signal = SignalService::new(SignalEngine::new("test-abc123".into()));
    UnifiedService::new(think, ship, roadmap, signal)
}

#[test]
fn lists_53_canonical_tools_across_four_families() {
    let svc = build_unified();
    let tools = svc.list_tools_view();
    assert_eq!(
        tools.len(),
        53,
        "expected 11 think_* + 13 ship_* + 17 roadmap_* + 10 signal_*, got {}",
        tools.len()
    );

    let names: BTreeSet<String> = tools.iter().map(|t| t.name.to_string()).collect();
    let count = |prefix: &str| names.iter().filter(|n| n.starts_with(prefix)).count();
    assert_eq!(count("think_"), 11, "11 think_* tools");
    assert_eq!(
        count("ship_"),
        13,
        "13 ship_* tools (incl. the two gate verbs)"
    );
    assert_eq!(count("roadmap_"), 17, "17 roadmap_* tools");
    assert_eq!(count("signal_"), 10, "10 signal_* tools");
}

/// Serialized size of the whole `tools/list` payload, in bytes — what the
/// CLIENT holds after a handshake.
///
/// NOT what the model pays. See [`model_facing_bytes`]; this figure is roughly
/// 3x that one, and conflating them is what
/// [`tools_list_payload_stays_within_budget`] used to do.
fn tools_list_bytes(svc: &UnifiedService) -> usize {
    serde_json::to_vec(&svc.list_tools_view())
        .expect("tool list serializes")
        .len()
}

/// The bytes that actually reach a model: `name` + `description` +
/// `inputSchema`, summed across every served tool.
///
/// # Why exactly these three fields
///
/// An Anthropic Messages API tool definition carries `name`, `description` and
/// `input_schema` — and no `output_schema`. A client bridging MCP to that API
/// therefore drops `outputSchema` before the model ever sees it. The same is
/// true of the data it describes: `structuredContent` is delivered to
/// application code, not into the conversation.
///
/// So `outputSchema` (60% of our wire payload) and `annotations` are costs the
/// CLIENT bears, not the context window. They are worth watching — hence
/// [`tools_list_bytes`] survives — but they are not what makes an agent's
/// context expensive, and a budget that adds them together cannot tell you
/// which lever to pull.
fn model_facing_bytes(svc: &UnifiedService) -> usize {
    svc.list_tools_view()
        .iter()
        .map(|t| {
            t.name.len()
                + t.description.as_deref().unwrap_or_default().len()
                + serde_json::to_vec(&t.input_schema)
                    .expect("input schema serializes")
                    .len()
        })
        .sum()
}

/// Total bytes of prose across every tool's `description`.
fn description_bytes(svc: &UnifiedService) -> usize {
    svc.list_tools_view()
        .iter()
        .map(|t| t.description.as_deref().unwrap_or_default().len())
        .sum()
}

/// THE CEILING. Measured 2026-07-27: 48 tools serialize to 87,897
/// bytes ≈ 20,757 tokens (cl100k proxy). That number is the whole basis of the
/// tool-surface-size decision — option D, keep the surface as it is,
/// because our primary client (Claude Code) defers these schemas behind
/// ToolSearch and never loads them.
///
/// A decision that rests on a number has to fail loudly when the number moves.
/// The budget carries ~14% headroom over the measurement; blowing it means the
/// surface grew enough that D deserves re-deciding, not that this line should
/// be raised. Pair with [`descriptions_stay_substantial`] — the ceiling alone
/// would reward hitting the budget by gutting the prose, which is exactly the
/// option (C) that was rejected.
///
/// # The line moved once, deliberately, on 2026-07-28
///
/// 100,000 -> 140,000, and the reasoning belongs here rather than in a commit
/// message nobody will re-read. An outputSchema audit found 15 of 48
/// tools shipping no `outputSchema` — all ten `signal_*` among them — which
/// left clients with nothing to validate `structuredContent` against for a
/// whole family. Writing those schemas costs 38,325 B, because seven
/// `signal_*` tools return a bare `Signal` and MCP cannot `$ref` a schema
/// across tools, so its 3,576 B is paid seven times.
///
/// The gate did its job: it refused the increment and sent the question to a
/// human. The answer was that structured output is worth the bytes, on two
/// grounds. First, the client this server actually runs against defers schemas
/// behind ToolSearch and never loads them (the option-D reasoning above still
/// holds). Second — and this is the fact that did not exist when the line was
/// first drawn — `FamilySelection` now lets an operator drop `signal_*`
/// entirely, so all-families is no longer the only payload on offer: a
/// think-only deployment stays at ~42 KB.
///
/// The headroom is deliberately thin now (~4 KB over the measured 135,524 B).
/// That is the point. The next thing that wants to grow the surface should
/// have to argue for it too.
///
/// # CORRECTION: the wire is not the model's context
///
/// Everything above is true of the WIRE, and the sentence this docstring used
/// to open with — that the payload is what "a non-deferring client actually
/// pays for on connect" — quietly became a claim about model context. It is
/// not one.
///
/// An Anthropic Messages API tool definition carries `name`, `description` and
/// `input_schema`, with no `output_schema`, so a bridging client drops
/// `outputSchema` before the model sees it; and `structuredContent`, the data
/// that schema describes, is handed to application code rather than into the
/// conversation. `outputSchema` is 60.3% of this number and `annotations`
/// another 4.2% — nearly two thirds of what this test guards is invisible to
/// the model.
///
/// That means the 100,000 -> 140,000 re-decision above was argued against a
/// figure roughly 3x the model-facing cost. The line is NOT being re-touched
/// here: it was moved once, deliberately, by a human, and quietly moving it
/// again on the strength of my own later analysis would destroy the only thing
/// that made the first move trustworthy. What changes instead is that the
/// model-facing cost is now measured separately, by
/// [`model_facing_tool_surface_stays_lean`], so the NEXT decision can be
/// argued against the right number.
///
/// # The line moved a second time, deliberately, on 2026-07-31
///
/// 140,000 -> 150,000, human-approved for `webapp-approval-gates`. The two
/// gate verbs (`ship_gate_open` / `ship_gate_wait`) measured 4,621 B on the
/// wire — 143,967 B total against the 140,000 line. The gate refused the
/// increment and the question went to the human, as designed; the answer was
/// to keep both verbs and move the line, on three grounds. First, approval
/// gates are a genuinely NEW capability (an agent pausing on a human answer
/// from the webapp), not schema backfill for an existing one — the first
/// move's own test. Second, per the CORRECTION above, ~2/3 of these wire
/// bytes are outputSchema/annotations the model never sees; the model-facing
/// gate below is the number that guards context, and it holds. Third,
/// `FamilySelection` still offers lean deployments to anyone who does not
/// want the ship family at all. Two verbs rather than one multiplexed tool
/// was deliberate: open and wait have disjoint inputs and outputs, and a
/// discriminator arg would bury the headless-safety contract that each
/// description carries.
///
/// # The line moved a third time, deliberately, on 2026-08-08
///
/// 150,000 -> 152,500, human-approved for `roadmap-get-bounded-fetch`.
///
/// The chunk that asked for `roadmap_get` wrote its own refusal into its
/// acceptance: *if a tool cannot fit under the human-set ceiling, the fallback
/// is `ids`/`fields` arguments on `roadmap_export` — the agent brings that
/// number to the human and does NOT move the line.* Both figures were
/// re-measured before a line of it was designed (148,407 wire / 52,915
/// model-facing, 1,593 and 585 B free, 50 tools) and put to the human as the
/// opening act of the session. They chose the 51st tool over the fallback and
/// named both new lines. That is the only reason this number changed; an
/// implementing agent may not edit it to make its own chunk fit.
///
/// WHAT IT BOUGHT — the verb between `roadmap_status` (every chunk, one
/// truncated line each, no body) and `roadmap_export` (1,504,641 B on this
/// board, too large to return at all). Reading three records previously meant
/// spilling the export to a file and running `jq` over it. Measured at
/// **+2,321 B wire**, landing at 150,728 — headroom 1,772 B.
///
/// WHY IT IS CHEAP FOR A NEW TOOL — the `outputSchema` was designed against
/// the priced refusal in `roadmap::output_schemas`, not in ignorance of it. A
/// `Vec<ChunkOutput>` would have added an eleventh inlined copy of the chunk
/// schema; `records` is a bare `Value` instead, which schemars emits as `true`.
/// That is not thrift dressed as design — `fields` projects each record, so the
/// shape genuinely varies per call and an enumerated schema would describe a
/// response the tool usually does not return.
///
/// # The line moved a fourth time, on human approval, on 2026-08-08
///
/// 152,500 -> 154,000, human-approved for the focus verbs.
///
/// The rule above stands and was obeyed: the agent implementing this did not
/// edit the number to fit. It measured, tried to fit, FAILED to fit, and
/// brought three costed options to the human, who chose this one.
///
/// WHAT WAS MEASURED, in the order it was measured:
///
/// | State                                   | Wire bytes | vs 152,500 |
/// |-----------------------------------------|-----------:|-----------:|
/// | before this chunk                       |    150,728 |  −1,772 ok |
/// | both focus verbs, first draft           |    154,173 |  +1,673 ✗  |
/// | after trimming duplicated field prose   |    153,630 |  +1,130 ✗  |
/// | after ALSO cutting every Pitfalls line  |    153,047 |    +547 ✗  |
///
/// THE DECIDING NUMBER is the last row. Stripping the prose entirely from both
/// new tools STILL does not fit. So this was never a question of whether the
/// new descriptions were too generous — the surface cannot absorb two tools at
/// any prose level, and "write less" was not an available answer. Landing
/// figure with the prose kept: **153,630 B, headroom 370 B**.
///
/// WHY TWO VERBS AND NOT ONE. A single `roadmap_focus` with a mode
/// discriminator fits with ~1,100 B to spare, and was offered and rejected.
/// `read_only_hint` cannot be both true and false, so multiplexing would leave
/// a client unable to tell a safe call from a mutating one — on the one pair of
/// verbs where that distinction is the entire point.
///
/// THE CHEAPER SURFACE THAT WAS NOT TAKEN, recorded so it is not rediscovered:
/// `outputSchema` is 86,349 B, **56% of the payload**, and much of it is
/// duplicated — five `signal_*` tools each carry a byte-identical 3,457 B
/// schema. MCP has no cross-tool `$ref`, so reclaiming it means reshaping the
/// schemas, which is its own chunk
/// (`chunk-output-schema-is-a-partial-projection`). Doing that work would buy
/// back far more than this move spent.
///
/// # The line moved a fifth time, on 2026-08-15, for structured check results
///
/// 154,000 -> 157,000, for `ship_check`'s `report` argument
/// (structured-test-results, issue #21).
///
/// WHAT WAS MEASURED. Before the chunk: 153,587 B, headroom 413 B — nothing
/// fits under that. First draft landed at 157,279; trimming the description
/// prose and moving every rationale line in the new report types from `///`
/// to `//` (the GetArgs discipline) brought it to **156,363 B, +2,776 over
/// the old line**. The remaining cost splits as ~900 B of input surface
/// (the `report {format, path}` arg + the description clauses an agent must
/// know BEFORE calling — that a report problem never fails the check, and
/// that stdout is never scraped) and ~1,900 B of `ship_check` outputSchema
/// (ReportRecord + TestResults + TestFailure), which the model never pays.
///
/// WHY NOT ZERO. The degradation contract has to be stated where the call is
/// formed: an agent that doesn't know a missing report is safe will not ask
/// for one, and the whole feature exists so a red gate can name the failing
/// test instead of the agent re-running the suite to find out.
///
/// The outputSchema-duplication note above still stands as the real place to
/// reclaim surface; this raise does not spend it.
#[test]
fn tools_list_payload_stays_within_budget() {
    const BUDGET: usize = 157_000;
    let svc = build_unified();
    let actual = tools_list_bytes(&svc);
    assert!(
        actual <= BUDGET,
        "tools/list is {actual} bytes, over the {BUDGET}-byte budget (was 87,897 when \
         the surface-size decision chose option D, 135,524 B after the outputSchema work, \
         143,967 B after the approval-gate verbs, 150,728 B after roadmap_get, 153,630 B \
         after the focus verbs, 156,363 B after ship_check's structured report). Re-decide \
         the surface rather than raising this — the line has moved exactly five times, with \
         every argument written above. Before proposing a sixth, read the outputSchema \
         note: over half of this payload is outputSchema and much of it is duplicated, so \
         there is real surface to reclaim before there is a reason to move the line again."
    );
}

/// THE FLOOR, and the reason the ceiling above is safe to have.
///
/// The Inputs/Returns/Pitfalls prose is why agents recover against this server
/// — `think_record_step`'s own pitfall section has twice caught a live
/// XML-serialization failure. Under client-side schema deferral the description
/// is *precisely* what survives into context, so it matters more, not less.
///
/// Non-vacuity is asserted by exact value, not by a count: a byte floor alone
/// could be satisfied by 48 tools of filler, so specific known tools must still
/// carry their specific known sections.
#[test]
fn descriptions_stay_substantial() {
    const FLOOR: usize = 20_000;
    let svc = build_unified();

    let total = description_bytes(&svc);
    assert!(
        total >= FLOOR,
        "tool descriptions total {total} bytes, under the {FLOOR}-byte floor (was 22,303 when \
         the surface-size decision rejected description-tiering). Relocating this prose is fine; \
         deleting it is not."
    );

    let tools = svc.list_tools_view();
    let describe = |name: &str| -> String {
        tools
            .iter()
            .find(|t| t.name == name)
            .unwrap_or_else(|| panic!("tool {name} should be served"))
            .description
            .as_deref()
            .unwrap_or_default()
            .to_string()
    };

    // Demanded by exact value: the sections, on the tools that earned them.
    let record_step = describe("think_record_step");
    assert!(
        record_step.contains("Pitfalls:"),
        "think_record_step lost its Pitfalls section — the prose that caught a live failure"
    );
    assert!(
        record_step.contains("</thought>"),
        "think_record_step lost the XML-serialization warning naming the closing tag"
    );
    assert!(
        describe("ship_check").contains("PREFER passing `command`"),
        "ship_check lost the guidance that makes a check verified rather than self-reported"
    );
    assert!(
        describe("roadmap_reprioritize").contains("does NOT reorder"),
        "roadmap_reprioritize lost the warning that it only proposes"
    );
}

/// Where the recorded measurement lives. Tracked in git ON PURPOSE — its diff
/// is the whole feature.
const SURFACE_RECORD: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/tool_surface.txt");

/// Render the current measurement in the record's line format.
fn surface_record(svc: &UnifiedService) -> String {
    format!(
        "# Written by tests/unified_service.rs::records_the_measured_tool_surface.\n\
         # A RECORD, NOT A GATE — nothing here is asserted. The ceiling is\n\
         # tools_list_payload_stays_within_budget; this file is the slope.\n\
         tools {}\n\
         wire_bytes {}\n\
         model_facing_bytes {}\n\
         description_bytes {}\n",
        svc.list_tools_view().len(),
        tools_list_bytes(svc),
        model_facing_bytes(svc),
        description_bytes(svc),
    )
}

/// THE SLOPE — the thing a ceiling structurally cannot show you.
///
/// # The failure this exists for, measured rather than asserted
///
/// `f275f10` drew the line at 100,000 B against a measured 87,897, a deliberate
/// ~14% of headroom. Rebuilding each commit since gives the real
/// curve, in `wire / model-facing` bytes at 48 tools throughout:
///
/// ```text
/// f275f10   87,887 / 42,624   the line is drawn, 14% headroom
/// 6dd672b   92,751 / 43,999   +4,864 green
/// 9d8fbd0   93,834 / 44,328   +1,083 green
/// d013648   93,834 / 44,328       ±0 green   <- a real commit that moved nothing
/// 2ca8063   97,199 / 44,988   +3,365 green   headroom now 2.8%
/// befabaa  135,524 / 44,988  +38,325 REFUSED -> a human moved the line to 140,000
/// dd3c7e3  136,366 / 45,409     +842 green
/// 55c1395  137,230 / 45,715     +864 green   headroom now 2,770 B
/// ```
///
/// Four green commits ate 87% of the original headroom, and the fifth took the
/// blame for a wall the other four built. That is not a gate failing; it is a
/// gate doing exactly its job, which is to catch the increment that crosses the
/// line and nothing else.
///
/// # Why a record and not a second gate
///
/// A warn band or a delta-fail would be a second blocking gate on one number,
/// and a second gate gets raised in the same motion as the first — it buys a
/// ritual, not information. What was missing was never enforcement. It was that
/// **nobody was ever shown a number**: all four commits passed code review, and
/// review cannot catch a slope it is not told about.
///
/// So this writes the measurement into a tracked file and asserts nothing. The
/// delta then appears in the one place a human is already looking — the diff —
/// and `git log -p tests/tool_surface.txt` is the curve.
///
/// # The staleness objection, and why it dissolves
///
/// An unenforced record can drift from reality. But it is refreshed by the same
/// `cargo test` run that enforces the ceiling, so it is exactly as fresh as the
/// gate it accompanies: if the suite never runs, the ceiling is dead too and
/// staleness here is the smaller problem. Writing only on change keeps a
/// no-op run from dirtying the tree.
///
/// # Both numbers, because they have different consumers
///
/// The correction above established that `outputSchema` is
/// ~60% of the wire figure and the Messages API drops it before the model sees
/// it. The curve above shows why recording one number would mislead: across
/// that first four-commit ratchet the wire figure grew 9,312 B while the
/// model-facing figure grew 2,364 B, so ~75% of the alarm was cost the model
/// never pays. Over the whole span wire grew 56% and model-facing 7.2%.
#[test]
fn records_the_measured_tool_surface() {
    let svc = build_unified();
    let current = surface_record(&svc);
    let previous = std::fs::read_to_string(SURFACE_RECORD).unwrap_or_default();

    if previous == current {
        println!(
            "tool surface unchanged ({} bytes on the wire)",
            tools_list_bytes(&svc)
        );
        return;
    }

    let field = |text: &str, key: &str| -> Option<i64> {
        text.lines()
            .find_map(|l| l.strip_prefix(key)?.trim().parse().ok())
    };
    for key in ["wire_bytes", "model_facing_bytes", "description_bytes"] {
        match (field(&previous, key), field(&current, key)) {
            (Some(was), Some(now)) if was != now => {
                println!("tool surface MOVED: {key} {was} -> {now} ({:+})", now - was);
            }
            (None, Some(now)) => println!("tool surface recorded for the first time: {key} {now}"),
            _ => {}
        }
    }
    // Deliberately not asserted, and deliberately not a hard error: a read-only
    // checkout should still run the suite.
    if let Err(e) = std::fs::write(SURFACE_RECORD, &current) {
        println!("could not update {SURFACE_RECORD}: {e}");
    }
}

/// `ship_finalize` is the one verb whose name a caller cannot derive from the
/// family prefix — `ship_` + "ship" is the shape a model reaches for, and it is
/// wrong. Answering it with the real name costs one branch; a bare "unknown
/// tool" costs a retry against 51 candidates.
#[test]
fn a_misderived_finalize_name_is_answered_with_the_real_one() {
    assert_eq!(
        UnifiedService::replacement_for_retired("ship_ship").as_deref(),
        Some("ship_finalize"),
    );

    // Canonical and unrelated names are not misderived — they route normally.
    assert!(UnifiedService::replacement_for_retired("ship_finalize").is_none());
    assert!(UnifiedService::replacement_for_retired("think_record_step").is_none());
    assert!(UnifiedService::replacement_for_retired("audit_anything").is_none());
}

/// The `instructions` block is pasted into the model's system prompt by several
/// MCP clients, so a wrong tool count there misinforms every fresh agent. Nothing
/// tied that prose to the registry, and it drifted (roadmap advertised 8 against a
/// real 12). Bind the two: the advertised count per family must equal the number of
/// canonical tools that family actually serves, so neither side can move alone.
#[test]
fn advertised_tool_counts_match_the_real_registry() {
    use rmcp::ServerHandler;

    let svc = build_unified();
    let info = svc.get_info();
    let instructions = info
        .instructions
        .as_deref()
        .expect("the unified server must ship instructions");

    let names: Vec<String> = svc
        .list_tools_view()
        .iter()
        .map(|t| t.name.to_string())
        .collect();

    for family in think_and_ship::mcp::UnifiedFamily::ALL.map(|f| f.prefix()) {
        let prefix = format!("{family}_");
        let served = names.iter().filter(|n| n.starts_with(&prefix)).count();
        let advertised = advertised_count(instructions, family).unwrap_or_else(|| {
            panic!("instructions never advertise a count for {family}_*:\n{instructions}")
        });
        assert_eq!(
            advertised, served,
            "instructions say {family}_* has {advertised} tools; the registry serves {served}"
        );
    }
}

/// Pull `N` out of the `<family>_* (N tools)` line of the instructions block.
fn advertised_count(instructions: &str, family: &str) -> Option<usize> {
    let marker = format!("{family}_*");
    let line = instructions
        .lines()
        .find(|l| l.trim().starts_with(&marker))?;
    let open = line.find('(')?;
    let rest = &line[open + 1..];
    let end = rest.find(" tools")?;
    rest[..end].trim().parse().ok()
}

#[test]
fn route_of_classifies_tools_by_family() {
    use think_and_ship::mcp::UnifiedFamily;
    assert_eq!(
        UnifiedService::route_of("think_record_step"),
        Some(UnifiedFamily::Think)
    );
    assert_eq!(
        UnifiedService::route_of("ship_set_objective"),
        Some(UnifiedFamily::Ship)
    );
    // An unknown prefix routes nowhere rather than falling through to a family.
    assert!(UnifiedService::route_of("audit_anything").is_none());
}

/// MCP 2026 readiness: we DECLARE support for MCP `2026-07-28`.
///
/// This is deliberately separate from the end-to-end negotiation test in
/// `think_and_ship_e2e.rs`, and the separation is the point. rmcp's
/// `negotiate_protocol_version` echoes any version present in the global
/// `ProtocolVersion::KNOWN_VERSIONS` constant without ever consulting
/// `ServerHandler::supported_protocol_versions()` — so a connection can
/// succeed at `2026-07-28` while the server declares no such support, and a
/// server can declare support that no client ever exercises. Only
/// `server/discover` (SEP-2575) reads the declaration, and that is what a
/// stateless client uses to find out what we speak.
///
/// Deliberately breaking the declaration found this: restricting
/// `supported_protocol_versions()` to `2025-11-25` left the end-to-end test
/// green. This test is the one that goes red.
#[test]
fn we_declare_support_for_2026_07_28() {
    use rmcp::ServerHandler;
    use rmcp::model::ProtocolVersion;

    let service = build_unified();
    let declared = service.supported_protocol_versions();
    assert!(
        declared.contains(&ProtocolVersion::V_2026_07_28),
        "server/discover would not advertise 2026-07-28; declared: {declared:?}"
    );
}

// ---------------------------------------------------------------------------
// mcp-family-selection: per-deployment tool-family selection.
//
// These reuse the harness above on purpose. The measurement of what a narrowed
// deployment actually saves has to come off the SAME `tools_list_bytes` that
// sets the ceiling, or the saving and the budget are denominated differently
// and neither can be trusted against the other.
// ---------------------------------------------------------------------------

use think_and_ship::mcp::unified::{Family, FamilySelection, FamilySelectionError};

fn build_with(sel: FamilySelection) -> UnifiedService {
    build_unified().with_families(sel)
}

/// THE DEFAULT IS EVERY FAMILY, and it is byte-identical.
///
/// This is the compatibility contract: an install that sets nothing must not be
/// able to tell that selection exists. Asserting equal *bytes* rather than equal
/// counts is deliberate — a filter that reordered families would pass a count
/// check and still change every existing client's payload.
#[test]
fn default_selection_is_byte_identical_to_no_selection() {
    let plain = build_unified();
    let explicit = build_with(FamilySelection::all());
    assert_eq!(
        tools_list_bytes(&plain),
        tools_list_bytes(&explicit),
        "an all-families selection must serialize identically to no selection at all"
    );
    assert_eq!(plain.list_tools_view().len(), 53);
    assert_eq!(explicit.list_tools_view().len(), 53);
}

/// A narrowed deployment serves exactly its families — nothing leaks through.
#[test]
fn a_narrowed_deployment_lists_only_its_families() {
    let sel = FamilySelection::parse("think,roadmap").expect("valid selection");
    let svc = build_with(sel);
    let names: BTreeSet<String> = svc
        .list_tools_view()
        .iter()
        .map(|t| t.name.to_string())
        .collect();

    assert!(names.iter().all(|n| n.starts_with("think_")
        || n.starts_with("roadmap_")
        || n.starts_with("tracker_")));
    assert_eq!(names.iter().filter(|n| n.starts_with("ship_")).count(), 0);
    assert_eq!(names.iter().filter(|n| n.starts_with("signal_")).count(), 0);
    // think 11 + the Roadmap family's 19 (17 roadmap_* AND the 2 tracker_* that
    // ride it — 11+13+17+10 is 51, and the 53-tool total is those two).
    assert_eq!(names.len(), 30);
}

/// THE SAVING, measured through the ceiling's own instrument.
///
/// Recorded as an exact-ish assertion rather than prose so it fails when the
/// surface moves: a think-only deployment is the smallest useful one, and if it
/// ever stops being dramatically cheaper than the full surface, family selection
/// has stopped earning its complexity.
#[test]
fn narrowing_measurably_shrinks_the_payload() {
    let full = tools_list_bytes(&build_unified());
    let think_only = tools_list_bytes(&build_with(FamilySelection::parse("think").expect("valid")));

    assert!(
        think_only < full / 2,
        "a think-only deployment should cost well under half the full surface; \
         got {think_only} vs {full}"
    );
    println!(
        "full: {full} B  think-only: {think_only} B  saved: {} B",
        full - think_only
    );
}

/// TRACKER IS STRUCTURAL, NOT A FIFTH FAMILY.
///
/// The `tracker_*` tools hang off `RoadmapService` and `route_of` sends them to
/// `Family::Roadmap`. So "tracker_* needs roadmap_*" cannot be violated by a
/// selection — there is no way to express it — and an operator who writes
/// `tracker` gets the family that actually carries those tools. This test is
/// what keeps that from silently becoming untrue if tracker ever moves.
#[test]
fn tracker_rides_the_roadmap_family_and_cannot_be_orphaned() {
    assert_eq!(Family::parse("tracker"), Some(Family::Roadmap));

    let svc = build_with(FamilySelection::parse("tracker").expect("valid"));
    let names: BTreeSet<String> = svc
        .list_tools_view()
        .iter()
        .map(|t| t.name.to_string())
        .collect();
    assert!(
        names.iter().any(|n| n.starts_with("tracker_")),
        "selecting tracker must actually expose the tracker_* tools"
    );
    assert!(
        names.iter().any(|n| n.starts_with("roadmap_")),
        "tracker_* is meaningless without the roadmap it mirrors, so it arrives with it"
    );
    let roadmap_only = build_with(FamilySelection::parse("roadmap").expect("valid"));
    assert_eq!(
        svc.list_tools_view().len(),
        roadmap_only.list_tools_view().len(),
        "'tracker' and 'roadmap' name the same family"
    );
}

/// A MISSPELLING IS AN ERROR, NOT A NARROWER SERVER.
///
/// This is the acceptance criterion that matters most: a typo that silently
/// dropped a family would be indistinguishable, from the client's side, from
/// the tools never having existed.
#[test]
fn an_unknown_family_is_refused_by_name() {
    let err = FamilySelection::parse("think,roadmpa").expect_err("typo must not be tolerated");
    assert_eq!(
        err,
        FamilySelectionError::Unknown {
            token: "roadmpa".into()
        }
    );
    let msg = err.to_string();
    assert!(
        msg.contains("roadmpa"),
        "the message must name the offending token: {msg}"
    );
    assert!(msg.contains("think"), "and list the known families: {msg}");
}

/// An empty selection would be a server that can do nothing. Refused.
#[test]
fn an_empty_selection_is_refused() {
    assert_eq!(
        FamilySelection::parse(",  ,").unwrap_err(),
        FamilySelectionError::Empty
    );
}

/// Operator-friendly parsing: case, padding and a trailing comma are fine.
/// (Tolerating these is what keeps the strict unknown-token rule reasonable.)
#[test]
fn selection_parsing_is_forgiving_about_shape_but_not_about_names() {
    let sel = FamilySelection::parse(" Think , SHIP, ").expect("shape noise is tolerated");
    assert!(sel.contains(Family::Think) && sel.contains(Family::Ship));
    assert!(!sel.contains(Family::Roadmap) && !sel.contains(Family::Signal));
    assert_eq!(sel.summary(), "think_*, ship_*");
}

/// LISTING AND DISPATCH MUST AGREE.
///
/// Every tool this deployment lists must route to a family this deployment
/// exposes, and every exposed family must actually contribute tools. The
/// `tracker_*` bug this server already hit — advertised by the family registry
/// but unroutable by prefix — was exactly this agreement failing in the other
/// direction, so it is asserted rather than assumed.
///
/// Checked across a spread of selections, not just one, because the failure
/// mode is a single family wired wrong.
#[test]
fn every_listed_tool_routes_to_an_exposed_family() {
    for spec in [
        "think",
        "ship",
        "signal",
        "roadmap",
        "think,signal",
        "think,ship,roadmap,signal",
    ] {
        let sel = FamilySelection::parse(spec).expect("valid selection");
        let svc = build_with(sel);
        let tools = svc.list_tools_view();
        assert!(!tools.is_empty(), "selection '{spec}' listed nothing");

        for tool in &tools {
            let route = UnifiedService::route_of(&tool.name)
                .unwrap_or_else(|| panic!("listed tool '{}' routes nowhere", tool.name));
            assert!(
                sel.contains(route),
                "selection '{spec}' lists '{}', which dispatches to the unexposed {}_* family",
                tool.name,
                route.prefix()
            );
        }

        for family in sel.selected() {
            assert!(
                tools
                    .iter()
                    .any(|t| UnifiedService::route_of(&t.name) == Some(family)),
                "selection '{spec}' exposes {}_* but lists none of its tools",
                family.prefix()
            );
        }
    }
}

/// The selection is fixed at construction, so two views taken from the same
/// service — as two connections would take them — are identical. This is the
/// stateless-core requirement (`tools/list` may not vary per connection) held
/// as a property rather than a comment.
#[test]
fn the_surface_does_not_vary_between_views() {
    let svc = build_with(FamilySelection::parse("think,signal").expect("valid"));
    assert_eq!(tools_list_bytes(&svc), tools_list_bytes(&svc));
    assert_eq!(
        svc.families(),
        FamilySelection::parse("signal,think").expect("valid")
    );
}

// ---------------------------------------------------------------------------
// outputSchema disposition: absence of an outputSchema must never again
// be ambiguous between "deliberate" and "nobody noticed".
// ---------------------------------------------------------------------------

use think_and_ship::mcp::unified::{SCHEMA_EXEMPT, SCHEMA_PENDING_BUDGET};

/// EVERY TOOL IS DISPOSITIONED — schema, declared exemption, or known gap.
///
/// This is the actual defect the audit found. Fourteen tools shipped with no
/// `outputSchema` for no recorded reason, alongside one that has a real reason,
/// and nothing in the codebase could tell them apart. A new tool that arrives
/// with no schema and no entry in either list now fails here rather than
/// quietly joining the pile.
#[test]
fn every_tool_is_dispositioned_for_output_schema() {
    let svc = build_unified();
    let mut undeclared = Vec::new();

    for tool in svc.list_tools_view() {
        let name = tool.name.to_string();
        let has_schema = tool.output_schema.is_some();
        let exempt = SCHEMA_EXEMPT.iter().any(|(n, _)| *n == name);
        let pending = SCHEMA_PENDING_BUDGET.contains(&name.as_str());

        assert!(
            !(exempt && pending),
            "'{name}' is listed as BOTH exempt and a known gap; it is one or the other"
        );
        if has_schema {
            assert!(
                !exempt && !pending,
                "'{name}' carries an outputSchema but is still listed as exempt/pending — \
                 remove it from that list"
            );
            continue;
        }
        if !exempt && !pending {
            undeclared.push(name);
        }
    }

    assert!(
        undeclared.is_empty(),
        "these tools have no outputSchema and no declared reason: {undeclared:?}. \
         Add a schema, or declare it in SCHEMA_EXEMPT (with a reason) / \
         SCHEMA_PENDING_BUDGET (a gap that must shrink, never grow)."
    );
}

/// The lists describe the CURRENT server, not a stale wish.
///
/// A name that lingers after its tool is renamed or its schema is added would
/// silently license a real gap elsewhere, so both lists are checked against the
/// live tool surface in the other direction too.
#[test]
fn schema_disposition_lists_have_no_stale_entries() {
    let svc = build_unified();
    let served: BTreeSet<String> = svc
        .list_tools_view()
        .iter()
        .map(|t| t.name.to_string())
        .collect();

    for (name, reason) in SCHEMA_EXEMPT {
        assert!(
            served.contains(*name),
            "SCHEMA_EXEMPT names '{name}', which this server does not serve"
        );
        assert!(
            !reason.trim().is_empty(),
            "the exemption for '{name}' must carry a reason"
        );
    }
    for name in SCHEMA_PENDING_BUDGET {
        assert!(
            served.contains(*name),
            "SCHEMA_PENDING_BUDGET names '{name}', which this server does not serve"
        );
    }
}

/// THE GAP IS CLOSED AND MUST STAY CLOSED.
///
/// Pinned as an emptiness rather than a count: 47 of 48 tools carry a schema
/// and the 48th is a declared exemption. A new tool that ships without either
/// fails `every_tool_is_dispositioned_for_output_schema`; this test is the
/// second lock, so that "just add it to the pending list" stops being an
/// available move.
#[test]
fn the_known_schema_gap_stays_closed() {
    assert!(
        SCHEMA_PENDING_BUDGET.is_empty(),
        "the outputSchema gap reopened with {:?} — a new tool must ship with a schema, \
         or a declared SCHEMA_EXEMPT entry explaining why it cannot have one",
        SCHEMA_PENDING_BUDGET
    );
}

// ---------------------------------------------------------------------------
// mcp-model-facing-budget: two budgets, two consumers, two numbers.
// ---------------------------------------------------------------------------

/// THE BUDGET THAT MATTERS — what an agent's context window actually pays.
///
/// Measured 2026-07-28: 46,047 B ≈ 11,512 tokens across 48 tools. For scale,
/// Atlassian's engineering guidance treats "10,000–17,000+ tokens of context
/// per request just for tool definitions" as the alarm band for a single large
/// server. On the WIRE figure we look 2x through the top of that band; on the
/// figure the model actually sees we sit at its low end.
///
/// The ceiling here is deliberately TIGHTER in proportion than the wire budget
/// (~13% headroom vs the wire's ~3%), because this is the number with a real
/// consumer. Growth here is growth in every agent's context on every session;
/// growth in the wire figure is growth in a client's memory.
///
/// Paired with [`descriptions_stay_substantial`] in the same way the wire
/// ceiling is: a cap alone would reward deleting the prose that makes the tools
/// usable, and that prose is precisely what survives into context under
/// client-side schema deferral.
///
/// # RAISED 2026-08-07: 52,000 → 53,500, for `blocked-by-set-and-cleared`
///
/// This ceiling says "argue for it before raising it", so here is the argument
/// and the arithmetic behind it. A human made the call; an implementing agent
/// may not edit this number to make its own chunk fit.
///
/// WHAT WAS ADDED — `roadmap_update_chunk` gained the write path for a chunk's
/// blocker: a `blocked_by` object (kind, reason, optional evidence) and a
/// `clear_blocked_by` flag. Measured at **+1,070 B**: 745 B of input schema,
/// 325 B of tool prose. Roughly 268 tokens per session.
///
/// WHY IT IS WORTH THAT — `blocked_by` is the readiness vocabulary three
/// queued chunks all consume (the scheduler skip, the status counts, the
/// tracker projection). Until a blocker can be RETRACTED as easily as it is
/// recorded, the field goes unused and the reason a chunk is stuck goes on
/// living in its title, which is the failure the whole line of work undoes.
///
/// WHAT WAS RECLAIMED FIRST, so the raise is the residue and not the opening
/// move. Both were measured, not estimated:
///
///   * **−625 B model-facing.** A `///` on the new `BlockedByArgs` explaining
///     an internal serde decision was being serialized by schemars into the
///     input schema — maintainer rationale charged to every agent on every
///     session. Rewritten as a `//`. The general lesson outlived the chunk:
///     on an argument struct, only what steers a CALLER earns a `///`.
///   * **−9,070 B wire.** Describing `blocked_by` in `ChunkOutput` cost that
///     much, because `ChunkOutput` is inlined into ~10 roadmap tool schemas
///     and MCP cannot `$ref` across tools. Dropped rather than paid, keeping
///     `tools_list_payload_stays_within_budget` under its own ceiling; the
///     omission is documented where it is felt, in `roadmap::output_schemas`.
///
/// WHAT WAS CONSIDERED AND NOT DONE — the same `///`-leaks-into-schema defect
/// exists elsewhere in production (`ThinkStep::tools_used` ships a
/// forensic finding about a call-count undercount into `think_record_step`'s
/// schema; trimming it was measured at −352 B). Reclaiming ~4 of those would
/// have fit this feature under the old line. It was rejected as the WRONG
/// reason to do it: trimming other families' documentation to pay for this
/// chunk is how a budget quietly becomes an argument for worse docs. The
/// sweep is filed on its own merits as `doc-rationale-leaks-into-tool-schemas`.
///
/// Headroom after the raise: 618 B. Still thin, still on purpose.
///
/// # RAISED 2026-08-08: 53,500 → 55,000, for `roadmap-get-bounded-fetch`
///
/// Same rule, same procedure: a human made the call after being shown the
/// measurement, and this ceiling was the BINDING one — 585 B free against a
/// tool that needed roughly twice that, which is what made "a 51st tool does
/// not fit" a fact rather than an opinion. The chunk's own acceptance said to
/// bring the number and take the `ids`/`fields`-on-`roadmap_export` fallback
/// unless a human said otherwise; they said otherwise and named this line.
///
/// WHAT WAS ADDED — `roadmap_get(ids, fields)`, measured at **+1,370 B**:
/// 1,023 B of tool prose and 347 B of name plus input schema. Roughly 343
/// tokens per session, and it is prose rather than schema because three of this
/// tool's four contracts are refusals a caller has to know about BEFORE forming
/// a call — over the cap is an error, an unmatched id is reported, an unknown
/// field is refused with the vocabulary. A schema cannot say any of that.
///
/// 54 B of that total were spent AFTER the first measurement, on the clause
/// saying `ids` is a set. A live probe had just shown that answering a repeated
/// id twice let one call return 137,772 B through a cap derived against 97,469,
/// so the behaviour changed; a changed contract nobody is told about is the
/// cheaper half of that fix and the wrong half.
///
/// WHY NOT TRIM IT UNDER THE OLD LINE — it could have been done, and it was
/// rejected for the reason [`descriptions_stay_substantial`] exists: the
/// description is precisely what survives into context under client-side
/// schema deferral, so cutting the three refusal clauses to hit a number would
/// buy 585 B by making the tool quietly easier to misuse. The `///`-on-an-arg-
/// struct leak that funded the LAST raise was checked for and is not present
/// here: `GetArgs` carries its rationale in `//` from the first commit, and
/// only the two caller-facing lines are `///`.
///
/// Headroom after the raise: 715 B. Still thin, still on purpose.
///
/// # The companion line, moved on 2026-08-08 with the wire ceiling
///
/// 55,000 -> 57,000, for the two focus verbs.
///
/// The human was shown the WIRE measurement and chose to keep both verbs with
/// their prose (see `tools_list_payload_stays_within_budget`). This is the same
/// decision measured on the other axis — the precedent set by `roadmap_get` is
/// that a tool decision names BOTH lines, so it is recorded here rather than
/// left to fail silently on the next run.
///
/// WHAT WAS ADDED — **+2,500 B**: `roadmap_focus_get` at 849 B of prose plus
/// 325 B of name and input schema, `roadmap_focus_set` at 827 B plus 499 B.
/// ~625 tokens per session, landing at 56,819 with 181 B of headroom.
///
/// WHY THE PROSE IS THE EXPENSIVE HALF, AND STAYS — both descriptions are
/// mostly refusal contracts a caller must know BEFORE forming a call: that an
/// unknown or ambiguous workstream writes nothing and returns candidates, that
/// a blank lane is refused rather than defaulted, that no synonym is accepted
/// for a mode. Those are the clauses that stop a mutating verb from being
/// misused, and a schema cannot express one of them.
///
/// WHY IT IS NOT WORSE — this pair is the only place the focus contract is
/// stated, because both tools were deliberately shipped WITHOUT an
/// `outputSchema` (measured refusal in `roadmap::output_schemas`). Had they
/// carried one, the wire figure would have grown another 1,096 B while this
/// figure stayed put — outputSchema never reaches the model. The two budgets
/// disagreeing here is the split doing exactly what it was built for.
///
/// # The companion line, moved on 2026-08-15 with the wire ceiling
///
/// 57,000 -> 58,500, for `ship_check`'s `report` argument
/// (structured-test-results, issue #21). Same decision as the wire raise
/// above, measured on this axis: headroom before the chunk was 179 B, the
/// trimmed landing figure is **57,915 B, +1,094**. What the model pays for is
/// the `report {format, path}` input schema plus the description clauses it
/// must know before forming the call — that the exit code stays the source of
/// truth, that a report problem degrades instead of failing the check, and
/// that stdout is never scraped. Those are refusal-contract prose of exactly
/// the kind the focus-verbs raise defended; a schema cannot carry them.
#[test]
fn model_facing_tool_surface_stays_lean() {
    const BUDGET: usize = 58_500;
    let svc = build_unified();
    let actual = model_facing_bytes(&svc);
    assert!(
        actual <= BUDGET,
        "model-facing tool surface is {actual} bytes (~{} tokens), over the {BUDGET}-byte \
         budget (was 46,047 when the budget was split from the wire figure, 54,285 before \
         the focus verbs, 56,821 before ship_check's structured report). This is the number \
         an agent pays on every session — argue for it before raising it.",
        actual / 4
    );
}

/// THE DIVERGENCE PROOF, and the reason the split budget is not just a second
/// name for the same arithmetic.
///
/// Two deliberate mutations, applied to a real served tool, with opposite
/// signatures:
///
///   * padding a `description` must move BOTH figures — it is prose the model
///     reads and bytes the client holds;
///   * attaching an `outputSchema` must move ONLY the wire figure — the client
///     stores it, the model never receives it.
///
/// If one probe could not tell those apart, the two metrics would be measuring
/// the same thing under different names and the split would be decoration.
/// Asserted by construction rather than by trusting the arithmetic above.
#[test]
fn the_two_budgets_measure_different_things() {
    let svc = build_unified();
    let base_wire = tools_list_bytes(&svc);
    let base_model = model_facing_bytes(&svc);

    // Mutation A — pad a description. Both figures must move, by ~the same amount.
    let mut padded = svc.list_tools_view();
    const PAD: usize = 5_000;
    let filler = "x".repeat(PAD);
    padded[0].description = Some(
        format!(
            "{}{filler}",
            padded[0].description.as_deref().unwrap_or_default()
        )
        .into(),
    );
    let padded_wire = serde_json::to_vec(&padded).expect("serializes").len();
    let padded_model: usize = padded
        .iter()
        .map(|t| {
            t.name.len()
                + t.description.as_deref().unwrap_or_default().len()
                + serde_json::to_vec(&t.input_schema)
                    .expect("serializes")
                    .len()
        })
        .sum();
    assert!(
        padded_wire >= base_wire + PAD,
        "a padded description must grow the wire figure ({base_wire} -> {padded_wire})"
    );
    assert!(
        padded_model >= base_model + PAD,
        "a padded description must ALSO grow the model-facing figure \
         ({base_model} -> {padded_model}) — it is prose the model reads"
    );

    // Mutation B — attach an outputSchema. ONLY the wire figure may move.
    let mut schema_added = svc.list_tools_view();
    let victim = schema_added
        .iter_mut()
        .find(|t| t.output_schema.is_none())
        .expect("think_export_trace is the declared exemption and carries no output schema");
    let mut blob = serde_json::Map::new();
    blob.insert("type".into(), serde_json::Value::String("object".into()));
    blob.insert(
        "description".into(),
        serde_json::Value::String("y".repeat(PAD)),
    );
    victim.output_schema = Some(std::sync::Arc::new(blob));

    let schema_wire = serde_json::to_vec(&schema_added).expect("serializes").len();
    let schema_model: usize = schema_added
        .iter()
        .map(|t| {
            t.name.len()
                + t.description.as_deref().unwrap_or_default().len()
                + serde_json::to_vec(&t.input_schema)
                    .expect("serializes")
                    .len()
        })
        .sum();
    assert!(
        schema_wire >= base_wire + PAD,
        "an added outputSchema must grow the wire figure ({base_wire} -> {schema_wire})"
    );
    assert_eq!(
        schema_model, base_model,
        "an added outputSchema must NOT move the model-facing figure — if it does, \
         model_facing_bytes is summing a field the model never receives"
    );
}
