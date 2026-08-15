//! The Projects v2 live smoke — the second test in this repo that talks to a
//! real API, and the one that exists to stop the field-write layer inventing an
//! opaque id.
//!
//! The board transport shipped against fixtures written by hand. Every request
//! shape in them is real; every response is imagined. `tracker_live_linear.rs`
//! already records what that costs: a large body of adapter, credential and
//! projector code shipped against imagined responses before anything there ran,
//! and the first real call found a defect on a DEFAULT Linear team.
//!
//! The field-write layer is where it would hurt most. A Projects v2 single-select OPTION NODE
//! ID is an opaque string minted per board — it cannot be guessed from the
//! option's name, defaulted, or derived from anything. A mock that returns
//! `opt-1` proves only that the code can read `opt-1`.
//!
//! # Running it
//!
//! Ignored by default, so `cargo test` stays offline and deterministic:
//!
//! ```text
//! PROJECTS_V2_TOKEN=$(gh auth token) \
//! PROJECTS_V2_BOARD=orgs/ACME/projects/12 \
//!   cargo test --test tracker_live_projects_v2 -- --ignored --nocapture
//! ```
//!
//! The `gh` CLI is the easiest source of a token, and it needs one scope the
//! default login does not carry:
//!
//! ```text
//! gh auth refresh -s read:project
//! ```
//!
//! `gh` is a DEVELOPMENT convenience here and nothing more. It appears in this
//! file's documentation and nowhere in `src/` — the adapter never shells out,
//! and the server has no runtime dependency on the CLI. Any token GitHub
//! accepts as a Bearer works just as well.
//!
//! Without both variables every test here skips loudly rather than failing — a
//! missing token is a missing token, not a broken adapter.
//!
//! # What it is allowed to do
//!
//! READ. Nothing in this file creates, edits, archives or deletes anything on a
//! real board. That is a deliberate line: this layer is a transport, and a
//! transport can be proven entirely by reads. The write path is separate, and
//! it belongs behind its own louder warning.

use serde_json::{Value, json};
use think_and_ship::tracker::credential::AuthScheme;
use think_and_ship::tracker::domain::WorkItemState;
use think_and_ship::tracker::projects_v2::{Op, ProjectsV2Client};

/// The token and board address, or `None` when this run is meant to stay
/// offline.
fn live() -> Option<(String, String)> {
    let token = std::env::var("PROJECTS_V2_TOKEN").ok()?;
    let board = std::env::var("PROJECTS_V2_BOARD").ok()?;
    if token.trim().is_empty() || board.trim().is_empty() {
        return None;
    }
    Some((token, board))
}

fn client(token: &str, board: &str) -> ProjectsV2Client {
    ProjectsV2Client::new(board)
        .expect("PROJECTS_V2_BOARD must be orgs/LOGIN/projects/N or users/LOGIN/projects/N")
        .with_token(token, AuthScheme::Bearer)
}

/// The whole point: our own address parser, our own query builder, our own
/// envelope reader, against a real board.
///
/// If the owner-kind reasoning in `projects_v2.rs` is wrong — if GitHub does not
/// in fact answer `organization(login:){projectV2(number:)}` the way the docs
/// show — this is the test that says so, and it says so before 9b builds on top
/// of it.
#[tokio::test]
#[ignore = "talks to the real GitHub GraphQL API; needs PROJECTS_V2_TOKEN + PROJECTS_V2_BOARD"]
async fn a_real_board_resolves_to_a_real_node_id() {
    let Some((token, board)) = live() else {
        eprintln!(
            "SKIPPED: set PROJECTS_V2_TOKEN and PROJECTS_V2_BOARD to run the live smoke.\n\
             \x20 PROJECTS_V2_TOKEN=$(gh auth token)   # needs: gh auth refresh -s read:project\n\
             \x20 PROJECTS_V2_BOARD=orgs/ACME/projects/12"
        );
        return;
    };
    let client = client(&token, &board);

    eprintln!("address: {}", client.address().as_path());
    let resolved = client
        .board()
        .await
        .expect("the configured board must resolve");

    eprintln!("board id:     {}", resolved.id);
    eprintln!("board title:  {}", resolved.title);
    eprintln!("board number: {}", resolved.number);

    // The node id is the ONE thing every later sub-chunk needs, so assert its
    // shape rather than merely that a string came back. GitHub's Projects v2
    // node ids are prefixed; a bare number here would mean we read the wrong
    // field and would break 9c's mutations in a way no mock would catch.
    assert!(
        !resolved.id.is_empty(),
        "a resolved board must carry a node id"
    );
    assert!(
        resolved.id.starts_with("PVT"),
        "expected a ProjectV2 node id (PVT…), got {:?} — if GitHub has changed \
         the prefix, the fixtures in tracker_projects_v2.rs are now lying",
        resolved.id
    );
    assert_eq!(
        resolved.number,
        client.address().number,
        "the board that answered must be the board that was asked for"
    );
}

/// CAPTURE, not assertion. Prints the board's real field schema — including the
/// single-select OPTION NODE IDS — so the field-write layer can be written
/// against what GitHub actually returns instead of against what was imagined.
///
/// This deliberately asserts almost nothing. Its job is to make the real shape
/// visible; the assertions belong in the field-write suite, written from what
/// this prints.
#[tokio::test]
#[ignore = "talks to the real GitHub GraphQL API; needs PROJECTS_V2_TOKEN + PROJECTS_V2_BOARD"]
async fn capture_the_boards_real_field_schema() {
    let Some((token, board)) = live() else {
        eprintln!("SKIPPED: set PROJECTS_V2_TOKEN and PROJECTS_V2_BOARD to capture the schema");
        return;
    };
    let client = client(&token, &board);
    let resolved = client.board().await.expect("board must resolve");

    // GitHub's own documented fields query. `first: 20` is the number their
    // example uses; whether a real board exceeds it is one of the things this
    // capture is here to find out.
    let data = client
        .graphql(
            Op::Query,
            "query BoardFields($id: ID!) { \
               node(id: $id) { ... on ProjectV2 { \
                 fields(first: 20) { \
                   totalCount \
                   nodes { \
                     ... on ProjectV2Field { id name dataType } \
                     ... on ProjectV2IterationField { id name dataType } \
                     ... on ProjectV2SingleSelectField { id name dataType options { id name } } \
                   } } } } }",
            json!({ "id": resolved.id }),
        )
        .await
        .expect("the fields query must succeed");

    eprintln!(
        "=== REAL FIELD SCHEMA for {} ===\n{}",
        client.address().as_path(),
        serde_json::to_string_pretty(&data).unwrap_or_default()
    );

    let nodes = data
        .pointer("/node/fields/nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    eprintln!("field count returned: {}", nodes.len());
    if let Some(total) = data
        .pointer("/node/fields/totalCount")
        .and_then(Value::as_u64)
    {
        eprintln!("field totalCount:     {total}");
        assert!(
            total <= 20,
            "this board has {total} fields but the query asked for 20 — field \
             discovery MUST paginate; recording that here is the point of this \
             capture"
        );
    }

    for n in &nodes {
        let name = n.get("name").and_then(Value::as_str).unwrap_or("?");
        let kind = n.get("dataType").and_then(Value::as_str).unwrap_or("?");
        eprintln!("  field {name:?} ({kind})");
        if let Some(options) = n.get("options").and_then(Value::as_array) {
            for o in options {
                eprintln!(
                    "    option {:?} -> id {:?}",
                    o.get("name").and_then(Value::as_str).unwrap_or("?"),
                    o.get("id").and_then(Value::as_str).unwrap_or("?")
                );
            }
        }
    }

    assert!(
        !nodes.is_empty(),
        "a Projects v2 board always has at least the built-in Title field — an \
         empty list means the query or the node id is wrong"
    );
}

/// The discovery query text, against the real API.
///
/// The offline suite proves the DISCOVERY LOGIC — pagination, dataType
/// selection, the refusal — against a mock that answers whatever we ask. It
/// cannot prove the query GitHub is actually willing to execute, and a
/// paginated query is exactly where that goes wrong: `first: 100` is a
/// different request from the capture's `first: 20`, and `after: $after`
/// introduces a variable the captured query never carried.
///
/// Still READ ONLY. Discovery reads; the refusal happens before any write, and
/// the write itself is 9c's.
#[tokio::test]
#[ignore = "talks to the real GitHub GraphQL API; needs PROJECTS_V2_TOKEN + PROJECTS_V2_BOARD"]
async fn the_real_board_answers_the_paginated_fields_query() {
    let Some((token, board)) = live() else {
        eprintln!("SKIPPED: set PROJECTS_V2_TOKEN and PROJECTS_V2_BOARD to check discovery");
        return;
    };
    let client = client(&token, &board);

    let before = client.remaining_points().expect("the bucket is configured");
    let schema = client
        .field_schema()
        .await
        .expect("the paginated fields query must be one GitHub accepts");
    let after = client.remaining_points().expect("the bucket is configured");

    eprintln!(
        "discovered {} field(s) on {} for {} point(s)",
        schema.fields().len(),
        client.address().as_path(),
        before - after
    );
    for f in schema.fields() {
        eprintln!("  {:?} ({})", f.name, f.data_type);
    }

    assert!(
        !schema.fields().is_empty(),
        "every board has at least the built-in Title field"
    );
    assert_eq!(
        before - after,
        2,
        "discovery is one board lookup plus one page for a board this size"
    );

    match schema.status_field() {
        Some(field) => {
            eprintln!("lifecycle field: {:?} ({})", field.name, field.id);
            for state in [
                WorkItemState::Todo,
                WorkItemState::InProgress,
                WorkItemState::Done,
                WorkItemState::Cancelled,
            ] {
                match field.option_for_state(state) {
                    Some(o) => eprintln!("  {} -> {:?} ({})", state.as_str(), o.name, o.id),
                    None => eprintln!(
                        "  {} -> REFUSED (no analogue on this board)",
                        state.as_str()
                    ),
                }
            }
            assert!(
                field.id.starts_with("PVTSSF_"),
                "the discovered lifecycle field must be a single-select, got {:?}",
                field.id
            );
        }
        None => eprintln!(
            "this board has no lifecycle single-select — every status write would be refused"
        ),
    }

    // Cached, on the real client as much as on the mock.
    let again = client.field_schema().await.unwrap();
    assert_eq!(again.fields().len(), schema.fields().len());
    assert_eq!(
        client.remaining_points(),
        Some(after),
        "the second discovery must cost nothing"
    );
}

/// The budget is real money against a real endpoint, so prove the meter moved
/// the way `projects_v2.rs` claims it does — one point for one query.
#[tokio::test]
#[ignore = "talks to the real GitHub GraphQL API; needs PROJECTS_V2_TOKEN + PROJECTS_V2_BOARD"]
async fn a_live_query_costs_exactly_one_point() {
    let Some((token, board)) = live() else {
        eprintln!("SKIPPED: set PROJECTS_V2_TOKEN and PROJECTS_V2_BOARD to check the meter");
        return;
    };
    let client = client(&token, &board);

    let before = client.remaining_points().expect("the bucket is configured");
    client.board().await.expect("board must resolve");
    let after = client.remaining_points().expect("the bucket is configured");

    eprintln!("graphql points: {before} -> {after}");
    assert_eq!(
        before - after,
        1,
        "a board lookup is one query and GitHub weights a query at 1 point"
    );
}
