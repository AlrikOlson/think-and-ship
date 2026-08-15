//! `roadmap_get` driven through the real `tools/call` seam by a real MCP
//! client, because that is where this chunk's acceptance points.
//!
//! # Why the seam and not the engine
//!
//! `ids` and `fields` are the first typed ARRAY parameters in the `roadmap_*`
//! family. An array is exactly where an input schema and a handler can disagree
//! while every direct engine call stays green: `Vec<String>` deserializes from
//! a JSON array by default, and the forgiving `string_or_seq` this seam uses
//! also has to accept a bare string. Neither claim is observable from inside
//! the engine, so an engine-only test would prove nothing about the criterion
//! that names an MCP client.
//!
//! # The table is foreign, and that is deliberate
//!
//! WATERWORKS is a municipal water utility's maintenance plan — not this
//! project, not the telescope backlog the truncation gates use, not the
//! greenhouse or the rail depot. A projection gate driven by production-shaped
//! data can pass on an accident of production data; one driven by a table the
//! code has never seen cannot.
//!
//! Every string this file asserts on appears NOWHERE but the record it was
//! written into — no summary phrase is reused in a title, so an assertion that
//! a projection "contains the summary" cannot be satisfied by the title coming
//! along for the ride.

use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult},
};
use serde_json::{Value, json};
use think_and_ship::mcp::UnifiedService;
use think_and_ship::roadmap::domain::{Chunk, ChunkStatus};
use think_and_ship::roadmap::{RoadmapEngine, RoadmapService};
use think_and_ship::ship::ShipService;
use think_and_ship::ship::engine::ShipEngine;
use think_and_ship::signal::{SignalEngine, SignalService};
use think_and_ship::think::ThinkService;
use think_and_ship::think::config::ThinkConfig;
use think_and_ship::think::engine::core::ReasoningServer;

/// `(id, title, summary, acceptance)`.
///
/// The summaries are the load-bearing part: each is a sentence about municipal
/// waterworks that shares no distinctive wording with its own title, so
/// "the record carried the summary" cannot be satisfied by the title.
const WATERWORKS: &[(&str, &str, &str, &str)] = &[
    (
        "reservoir-drawdown-window",
        "The drawdown window",
        "Silt at the intake cannot be dredged while the upper basin is holding head.",
        "The basin is measured empty before any dredge is scheduled",
    ),
    (
        "chlorine-contact-time",
        "Contact time at low flow",
        "Overnight demand falls far enough that the residual leaves the clearwell early.",
        "Residual is sampled at the far end of the clearwell, not at the dose point",
    ),
    (
        "hydrant-flow-survey",
        "The flow survey",
        "Nobody has opened the eastern hydrants since the main was relined.",
        "Every hydrant east of the relining is flowed and its result written down",
    ),
    (
        "meter-reading-backlog",
        "The reading backlog",
        "Estimated bills have been going out on the strength of a walk nobody has done.",
        "No account is billed on an estimate older than one cycle",
    ),
];

fn seeded_engine() -> RoadmapEngine {
    let mut engine = RoadmapEngine::new("waterworks-seam".into());
    for (i, (id, title, summary, acceptance)) in WATERWORKS.iter().enumerate() {
        engine
            .add_chunk_with_content(
                (*id).to_string(),
                (*title).to_string(),
                String::new(),
                ChunkStatus::Pending,
                (i as u32 + 1) * 10,
                format!("Plain-prose fallback for {id}."),
                vec![(*acceptance).to_string()],
                Vec::new(),
                false,
                Some(
                    serde_json::from_value(json!({ "version": 1, "summary": summary }))
                        .expect("the structured body parses"),
                ),
                None,
            )
            .unwrap_or_else(|e| panic!("seeding {id}: {e}"));
    }
    engine
}

fn build_unified() -> UnifiedService {
    let mut cfg = ThinkConfig::default();
    cfg.display.color_output = false;
    UnifiedService::new(
        ThinkService::new(ReasoningServer::new(cfg)),
        ShipService::new(ShipEngine::new("waterworks-seam".into())),
        RoadmapService::new(seeded_engine()),
        SignalService::new(SignalEngine::new("waterworks-seam".into())),
    )
}

/// Call `roadmap_get` over a real duplex-transported MCP session and return the
/// structured envelope. Every test goes through here, so nothing in this file
/// can accidentally shortcut the seam it exists to exercise.
async fn call_get(arguments: Value) -> Value {
    let (server_tx, client_tx) = tokio::io::duplex(1 << 20);
    tokio::spawn(async move {
        let running = build_unified()
            .serve(server_tx)
            .await
            .expect("server.serve");
        let _ = running.waiting().await;
    });
    let client = ().serve(client_tx).await.expect("client.serve");

    let mut req = CallToolRequestParams::new("roadmap_get");
    req.arguments = Some(
        arguments
            .as_object()
            .expect("arguments are an object")
            .clone(),
    );
    let response: CallToolResult = client
        .peer()
        .call_tool(req)
        .await
        .expect("the call must succeed as a protocol operation");
    assert_eq!(
        response.is_error,
        Some(false),
        "every outcome here degrades through the structured envelope; a protocol \
         error would cancel sibling calls in a parallel batch"
    );
    Value::Object(
        response
            .structured_content
            .and_then(|v| v.as_object().cloned())
            .expect("roadmap_get answers through a structured envelope"),
    )
}

fn records(envelope: &Value) -> &Vec<Value> {
    envelope["records"]
        .as_array()
        .unwrap_or_else(|| panic!("expected records[], got {envelope}"))
}

/// THE CRITERION THAT NAMES AN MCP CLIENT: typed arrays cross the seam, and
/// what comes back is the FULL record — the text `roadmap_status` cannot carry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_typed_arrays_cross_the_seam_and_the_whole_record_comes_back() {
    let (first, second) = (&WATERWORKS[0], &WATERWORKS[2]);
    let envelope = call_get(json!({ "ids": [first.0, second.0] })).await;

    assert_eq!(envelope["returned"], json!(2), "{envelope}");
    assert_eq!(envelope["unknown"], json!([]), "{envelope}");
    assert_eq!(
        envelope["fields"],
        json!([]),
        "no projection was asked for, and the echo must say so"
    );

    let got = records(&envelope);
    assert_eq!(
        got.iter().map(|r| &r["id"]).collect::<Vec<_>>(),
        vec![&json!(first.0), &json!(second.0)],
        "records come back in the order the ids named them"
    );

    // The premise: these strings are NOT in the titles, so finding them can
    // only mean the stored body was returned.
    for (record, row) in got.iter().zip([first, second]) {
        let (_, title, summary, acceptance) = row;
        assert!(
            !title.contains(*summary) && !title.contains(*acceptance),
            "premise broken — the title '{title}' already carries what this test \
             is about to look for, so the assertion below would prove nothing"
        );
        assert_eq!(record["content"]["summary"], json!(summary), "{record}");
        assert_eq!(record["acceptance"], json!([acceptance]), "{record}");
        assert!(
            record["description"]
                .as_str()
                .is_some_and(|d| !d.is_empty()),
            "the description is part of the full record: {record}"
        );
    }
}

/// A single id may arrive as a bare string. This is the forgiving-shape half of
/// `string_or_seq`, and it is only observable at the seam.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_id_may_arrive_as_a_bare_string_rather_than_a_list() {
    let envelope = call_get(json!({ "ids": WATERWORKS[1].0 })).await;
    assert_eq!(envelope["returned"], json!(1), "{envelope}");
    assert_eq!(records(&envelope)[0]["id"], json!(WATERWORKS[1].0));
}

/// `ids` IS A SET, AND THAT IS WHAT KEEPS THE CAP HONEST.
///
/// This gate exists because a live probe against the real 570-chunk store
/// falsified the cap's own derivation. `GET_ID_CAP` was sized against the 20
/// largest DISTINCT records (97,469 B); with duplicates answered twice, naming
/// the single largest record 20 times returned 137,772 B — 41% past the bound
/// the cap was chosen to respect. The worst case was `N × max`, not
/// `sum of the top N`.
///
/// So the response is asserted at BOTH ends of the same call: one record for a
/// thrice-named id, and a payload no larger than asking for it once. The size
/// half is the one that would have caught the original defect; the count half
/// alone could be satisfied by a response that still carried three copies
/// inside `records`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_repeated_id_is_answered_once_so_the_cap_bounds_what_it_claims() {
    let target = WATERWORKS[2].0;

    let once = call_get(json!({ "ids": [target] })).await;
    let thrice = call_get(json!({ "ids": [target, target, target] })).await;

    assert_eq!(
        thrice["returned"],
        json!(1),
        "three names for one chunk is a request for one record: {thrice}"
    );
    assert_eq!(records(&thrice).len(), 1, "{thrice}");
    assert_eq!(
        thrice["unknown"],
        json!([]),
        "a duplicate is not an unknown id: {thrice}"
    );
    assert_eq!(
        serde_json::to_vec(&thrice).expect("serializes").len(),
        serde_json::to_vec(&once).expect("serializes").len(),
        "repeating an id must not grow the payload — that is exactly how the \
         original cap came to guard a bound 41% below the real worst case"
    );
}

/// The cap counts DISTINCT ids, so a request that repeats its way over the
/// limit is answered rather than refused: 3× the cap in names, one chunk in
/// substance, and nothing about it is expensive.
///
/// The premise is asserted first — the raw list really is over the cap — or
/// this passes for the trivial reason that it never approached the line.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn repeating_one_id_past_the_cap_is_still_one_record() {
    let names = RoadmapEngine::GET_ID_CAP * 3;
    assert!(
        names > RoadmapEngine::GET_ID_CAP,
        "premise broken — {names} names is not over the cap of {}",
        RoadmapEngine::GET_ID_CAP
    );
    let ids = vec![WATERWORKS[0].0; names];
    let envelope = call_get(json!({ "ids": ids })).await;

    assert!(
        envelope.get("ok") != Some(&json!(false)),
        "the cap bounds RECORDS, not repetitions: {envelope}"
    );
    assert_eq!(envelope["returned"], json!(1), "{envelope}");
}

/// AN UNKNOWN ID IS NAMED, NOT DROPPED — so "no such chunk" and "that chunk has
/// no summary" stay different answers.
///
/// The known id is asserted to still come back: reporting the unknown one by
/// failing the whole call would be a different bug with the same symptom.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_id_is_reported_by_name_while_the_known_one_still_answers() {
    let known = WATERWORKS[3].0;
    let envelope = call_get(json!({ "ids": [known, "sluice-gate-that-does-not-exist"] })).await;

    assert_eq!(
        envelope["unknown"],
        json!(["sluice-gate-that-does-not-exist"]),
        "the id that matched nothing must be named: {envelope}"
    );
    assert_eq!(envelope["returned"], json!(1), "{envelope}");
    assert_eq!(records(&envelope)[0]["id"], json!(known));
    assert!(
        envelope.get("ok") != Some(&json!(false)),
        "an unknown id is a reported fact, not a failed call: {envelope}"
    );
}

/// OVER THE CAP IS AN ERROR, AND NOTHING COMES BACK.
///
/// The overflow is DERIVED from `GET_ID_CAP` rather than written as a literal,
/// so raising the cap cannot leave this test silently asserting about a request
/// that is no longer over it. The second assertion is the one that matters: a
/// truncated list would satisfy "there was an error" while still being the
/// silent-truncation failure the criterion forbids.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn over_the_cap_is_a_named_error_and_not_a_short_answer() {
    let over = RoadmapEngine::GET_ID_CAP + 1;
    let ids: Vec<String> = (0..over).map(|i| format!("valve-{i}")).collect();
    let envelope = call_get(json!({ "ids": ids })).await;

    assert_eq!(envelope["ok"], json!(false), "{envelope}");
    let message = envelope["message"].as_str().unwrap_or_default();
    assert!(
        message.contains(&over.to_string())
            && message.contains(&RoadmapEngine::GET_ID_CAP.to_string()),
        "the refusal must name how many were asked for AND the cap, or a caller \
         cannot tell how far over they are: {message}"
    );
    assert!(
        envelope.get("records").is_none(),
        "nothing may come back — a short list reads as 'those chunks do not \
         exist': {envelope}"
    );
}

/// A PROJECTION KEEPS ONLY WHAT WAS NAMED, PLUS `id`.
///
/// Asserted as an exact key set rather than as "contains acceptance", because
/// the failure worth catching is a projection that silently returns everything.
/// The premise — that the unprojected record really does carry the keys this
/// one drops — is proven first, in the same call sequence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_projection_keeps_only_the_named_fields_and_always_the_id() {
    let target = WATERWORKS[0].0;

    let whole = call_get(json!({ "ids": [target] })).await;
    let whole_keys = records(&whole)[0]
        .as_object()
        .expect("a record is an object");
    for expected in ["description", "content", "priority", "status"] {
        assert!(
            whole_keys.contains_key(expected),
            "premise broken — the unprojected record has no '{expected}', so \
             dropping it below would prove nothing: {whole}"
        );
    }

    let projected = call_get(json!({ "ids": [target], "fields": ["acceptance"] })).await;
    let record = &records(&projected)[0];
    let mut keys: Vec<&str> = record
        .as_object()
        .expect("a record is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec!["acceptance", "id"],
        "a projection returns exactly what was named plus the id: {record}"
    );
    assert_eq!(
        projected["fields"],
        json!(["acceptance"]),
        "the applied projection is echoed back"
    );
}

/// AN UNRECOGNISED FIELD IS REFUSED, AND THE REFUSAL CARRIES THE VOCABULARY.
///
/// JSON:API permits either rejecting or ignoring an unknown name, so this is a
/// choice this tool makes rather than one it inherits. Ignoring `contents`
/// would hand back records projected to nothing but `id` — indistinguishable
/// from a board where every named chunk is genuinely bare.
///
/// The vocabulary half is asserted because an error that says only "unknown
/// field" moves the guessing instead of ending it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unrecognised_field_is_refused_with_the_whole_vocabulary() {
    let envelope = call_get(json!({
        "ids": [WATERWORKS[0].0],
        "fields": ["acceptance", "contents"],
    }))
    .await;

    assert_eq!(envelope["ok"], json!(false), "{envelope}");
    let message = envelope["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("contents"),
        "the refusal must name the offending field: {message}"
    );
    for real in ["content", "acceptance", "blocked_by"] {
        assert!(
            message.contains(real),
            "the refusal must quote the vocabulary, and '{real}' is in it: {message}"
        );
    }
    assert!(
        envelope.get("records").is_none(),
        "a rejected projection returns no records at all: {envelope}"
    );
}

/// THE VOCABULARY IS NOT A HAND-KEPT LIST THAT CAN GO STALE.
///
/// Driven by a chunk with EVERY optional field populated, so
/// `skip_serializing_if` cannot hide a key from the comparison. A field added
/// to `Chunk` and forgotten in `PROJECTABLE_FIELDS` becomes unfetchable in
/// silence otherwise — the exact failure a hand-maintained list has.
#[test]
fn projectable_fields_match_the_serialized_record() {
    let fully_populated: Chunk = serde_json::from_value(json!({
        "id": "backwash-cycle",
        "title": "Every optional field populated on purpose",
        "name": "Backwash",
        "status": "pending",
        "priority": 10,
        "description": "d",
        "content": { "version": 1, "summary": "s" },
        "notes": "n",
        "group": "Filtration",
        "acceptance": ["a"],
        "deps": ["chlorine-contact-time"],
        "tier": 2,
        "cross_refs": ["check:filter-turbidity-audit"],
        "shared": true,
        "reprioritize": { "suggested_priority": 5, "reason": "r", "proposed_at": "t" },
        "status_proposal": {
            "suggested_status": "done", "reason": "r", "proposed_at": "t", "source": "tracker"
        },
        "title_proposal": {
            "suggested_title": "t", "reason": "r", "proposed_at": "t", "source": "tracker"
        },
        "obsoleted_reason": "o",
        "blocked_by": {
            "kind": "external", "reason": "r", "evidence": null, "blocked_at": "t"
        },
        "project_id": "waterworks-seam",
        "created_at": "t",
        "updated_at": "t",
    }))
    .expect("the fully-populated chunk deserializes");

    let serialized = serde_json::to_value(&fully_populated).expect("a chunk serializes");
    let mut keys: Vec<&str> = serialized
        .as_object()
        .expect("a chunk is an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();

    // The premise: this instance must actually exercise the optional fields,
    // or the comparison degrades to "the required keys are listed".
    for optional in [
        "blocked_by",
        "tier",
        "group",
        "reprioritize",
        "title_proposal",
    ] {
        assert!(
            keys.contains(&optional),
            "premise broken — '{optional}' did not survive serialization, so this \
             test is no longer driving a fully-populated record"
        );
    }

    let mut vocabulary = Chunk::PROJECTABLE_FIELDS.to_vec();
    vocabulary.sort_unstable();
    assert_eq!(
        keys, vocabulary,
        "Chunk::PROJECTABLE_FIELDS has drifted from what a Chunk serializes to. \
         A field missing from the list is silently unfetchable through \
         roadmap_get; a name in the list that no longer exists makes the \
         unknown-field refusal quote a field nobody can ask for."
    );
}
