//! The Projects v2 board transport, against an in-process
//! GraphQL mock.
//!
//! These tests drive the REAL client — its own query text, its own envelope
//! reader, its own budget — rather than a hand-written approximation of them.
//! The repo already records why that matters: a hand-written Linear probe
//! differed from the projector's real output by a blank line and looked for a
//! while like a fidelity defect that never existed.
//!
//! A GraphQL API has one endpoint, so `wiremock` cannot route by path. Each
//! request is matched on the operation name and the root field inside the query
//! body, which is the stronger assertion anyway: it pins WHICH query was sent,
//! not merely that something was posted.
//!
//! # The recorded-reality tests at the bottom
//!
//! `fixtures/projects_v2_fields.json` is not hand-written. It is the byte-exact
//! answer GitHub gave for a real board, captured by
//! `tests/tracker_live_projects_v2.rs`, and the tests that read it exist so that
//! what GitHub ACTUALLY returns gates the discovery layer instead of what the
//! transport's author imagined. Two assumptions died on contact with it; see
//! the tests for which.

use serde_json::{Value, json};
use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::domain::{WorkItem, WorkItemState};
use think_and_ship::tracker::outbox::TrackerOutbox;
use think_and_ship::tracker::port::{TrackerError, TrackerPort};
use think_and_ship::tracker::project::{ProjectionOutcome, project_all};
use think_and_ship::tracker::projects_v2::{
    FieldSchema, IssueCoordinate, Op, OwnerKind, ProjectsV2Client, ProjectsV2Tracker,
    StatusPlacement, summarise_left_unchanged,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Routes on the root field the client chose, so a client that asked
/// `organization()` about a user board gets the answer GitHub would really
/// give — a top-level error, not a null.
struct Board {
    /// Served when the query names `organization(login:`.
    org: Value,
    /// Served when the query names `user(login:`.
    user: Value,
}

impl Respond for Board {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let data = if !query.contains("query BoardId") {
            json!({ "errors": [{ "message": format!("unexpected operation: {query}") }] })
        } else if query.contains("organization(login:") {
            self.org.clone()
        } else if query.contains("user(login:") {
            self.user.clone()
        } else {
            json!({ "errors": [{ "message": "query named no owner root field" }] })
        };
        ResponseTemplate::new(200).set_body_json(data)
    }
}

fn org_board() -> Value {
    json!({ "data": { "organization": { "projectV2": {
        "id": "PVT_kwDOAbcd4A", "title": "Delivery", "number": 12
    }}}})
}

fn user_board() -> Value {
    json!({ "data": { "user": { "projectV2": {
        "id": "PVT_kwHOxyz123", "title": "Personal roadmap", "number": 3
    }}}})
}

/// Stand up a mock GraphQL endpoint at `/graphql` and point a client at it.
async fn serve(responder: Board) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(responder)
        .mount(&server)
        .await;
    server
}

/// An ORGANISATION-owned board resolves to its node id, and the id is what the
/// later sub-chunks will actually need.
#[tokio::test]
async fn an_organisation_board_resolves_to_its_node_id() {
    let server = serve(Board {
        org: org_board(),
        user: json!({ "errors": [{ "type": "NOT_FOUND", "message": "no such user" }] }),
    })
    .await;

    let client = ProjectsV2Client::new("https://github.com/orgs/acme/projects/12")
        .unwrap()
        .with_api_base(&server.uri());

    let board = client.board().await.expect("the org board must resolve");
    assert_eq!(board.id, "PVT_kwDOAbcd4A");
    assert_eq!(board.title, "Delivery");
    assert_eq!(board.number, 12);
    assert_eq!(client.address().owner_kind, OwnerKind::Organization);
}

/// A USER-owned board resolves through the OTHER root field. Same address
/// shape, different GraphQL query — which is the reason the kind is carried in
/// the address rather than guessed.
#[tokio::test]
async fn a_user_board_resolves_through_the_user_root_field() {
    let server = serve(Board {
        org: json!({ "errors": [{ "type": "NOT_FOUND", "message": "no such organization" }] }),
        user: user_board(),
    })
    .await;

    let client = ProjectsV2Client::new("users/alrik/projects/3")
        .unwrap()
        .with_api_base(&server.uri());

    let board = client.board().await.expect("the user board must resolve");
    assert_eq!(board.id, "PVT_kwHOxyz123");
    assert_eq!(board.number, 3);
}

/// THE MECHANISM, asserted separately from the outcome. Both boards above could
/// resolve from a client that sent one hardcoded root field and a mock that
/// ignored it. Here the mock answers ONLY the organisation field, so a user
/// client that wrongly asked `organization()` would succeed — and must not.
#[tokio::test]
async fn a_user_client_does_not_reach_the_organisation_root_field() {
    let server = serve(Board {
        org: org_board(),
        // The user field is served an error, so resolving proves the client
        // asked for `user(login:)` and nothing else.
        user: json!({ "errors": [{ "type": "NOT_FOUND", "message": "user root field reached" }] }),
    })
    .await;

    let client = ProjectsV2Client::new("users/alrik/projects/3")
        .unwrap()
        .with_api_base(&server.uri());

    let err = client.board().await.unwrap_err();
    match err {
        TrackerError::NotFound(msg) => assert!(
            msg.contains("user root field reached"),
            "the client must have queried user(login:), got: {msg}"
        ),
        other => panic!("expected the user root field to have been queried, got {other:?}"),
    }
}

/// A board that does not exist under an owner that DOES arrives as a null with
/// no GraphQL error — a 200 OK that looks like success to anything checking
/// only `errors`.
#[tokio::test]
async fn a_missing_board_under_a_real_owner_is_not_found_not_success() {
    let server = serve(Board {
        org: json!({ "data": { "organization": { "projectV2": null } } }),
        user: json!({ "data": { "user": { "projectV2": null } } }),
    })
    .await;

    let client = ProjectsV2Client::new("orgs/acme/projects/99")
        .unwrap()
        .with_api_base(&server.uri());

    let err = client.board().await.unwrap_err();
    match &err {
        TrackerError::NotFound(msg) => {
            assert!(
                msg.contains("99"),
                "the refusal must name the number: {msg}"
            );
            assert!(
                msg.contains("acme"),
                "the refusal must name the owner: {msg}"
            );
            assert!(
                msg.contains("orgs/acme/projects/99"),
                "the refusal must name what was looked for: {msg}"
            );
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert!(
        !err.retryable(),
        "a wrong address must not be queued for replay forever"
    );
}

/// An owner that does not exist arrives as a 200 OK with a NOT_FOUND error.
#[tokio::test]
async fn an_unknown_owner_is_surfaced_from_a_two_hundred() {
    let server = serve(Board {
        org: json!({ "data": null, "errors": [{
            "type": "NOT_FOUND",
            "message": "Could not resolve to an Organization with the login of 'nope'."
        }]}),
        user: json!({ "errors": [{ "message": "unused" }] }),
    })
    .await;

    let client = ProjectsV2Client::new("orgs/nope/projects/1")
        .unwrap()
        .with_api_base(&server.uri());

    match client.board().await.unwrap_err() {
        // The message must be GITHUB'S, not ours. Asserting only on "nope"
        // would pass even if the envelope's errors were ignored entirely: the
        // null data would then fall through to our own null-check, whose
        // refusal also names the login. Deliberately ignoring the envelope's
        // errors demonstrated exactly that pass-through.
        TrackerError::NotFound(msg) => assert!(
            msg.contains("Could not resolve to an Organization"),
            "the provider's own error text must be surfaced, got: {msg}"
        ),
        other => panic!("expected NotFound from a 200-with-errors, got {other:?}"),
    }
}

/// Resolution is paid ONCE per run. A board looked up per item would spend the
/// point budget on nothing — and the point of caching it here is that field
/// discovery and the write path will each want the id.
#[tokio::test]
async fn the_board_is_resolved_once_and_then_cached() {
    let server = serve(Board {
        org: org_board(),
        user: json!({ "errors": [{ "message": "unused" }] }),
    })
    .await;

    let client = ProjectsV2Client::new("orgs/acme/projects/12")
        .unwrap()
        .with_api_base(&server.uri());

    let before = client.remaining_points().unwrap();
    let first = client.board().await.unwrap();
    let after_first = client.remaining_points().unwrap();
    let second = client.board().await.unwrap();
    let after_second = client.remaining_points().unwrap();

    assert_eq!(first, second);
    assert_eq!(
        before - after_first,
        1,
        "a board lookup is a query and must cost exactly 1 point"
    );
    assert_eq!(
        after_first, after_second,
        "the second board() must not have hit the network"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        1,
        "exactly one request must have reached the server"
    );
}

/// The credential path is the existing one, and the token must actually reach
/// the wire — a client that holds a token and forgets the header fails only
/// against a real GitHub.
#[tokio::test]
async fn the_credential_is_presented_as_a_bearer_token() {
    let server = serve(Board {
        org: org_board(),
        user: json!({ "errors": [{ "message": "unused" }] }),
    })
    .await;

    let client = ProjectsV2Client::new("orgs/acme/projects/12")
        .unwrap()
        .with_api_base(&server.uri())
        .with_token(
            "ghp_secret",
            think_and_ship::tracker::credential::AuthScheme::Bearer,
        );

    client.board().await.unwrap();

    let requests = server.received_requests().await.unwrap();
    let auth = requests[0]
        .headers
        .get("authorization")
        .expect("the token must be sent")
        .to_str()
        .unwrap();
    assert_eq!(auth, "Bearer ghp_secret");
}

/// An exhausted GraphQL bucket must refuse BEFORE the request is sent — a
/// budget that only reports after the fact protects nothing.
#[tokio::test]
async fn an_exhausted_budget_refuses_before_the_request_is_sent() {
    let server = serve(Board {
        org: org_board(),
        user: json!({ "errors": [{ "message": "unused" }] }),
    })
    .await;

    let client = ProjectsV2Client::new("orgs/acme/projects/12")
        .unwrap()
        .with_api_base(&server.uri());

    // 2,000 points/minute, drained 5 at a time by the mutation weight.
    for _ in 0..400 {
        client
            .graphql(Op::Mutation, "mutation Drain { __typename }", json!({}))
            .await
            .ok();
    }
    assert_eq!(client.remaining_points(), Some(0));

    let sent_before = server.received_requests().await.unwrap().len();
    let err = client.board().await.unwrap_err();
    let sent_after = server.received_requests().await.unwrap().len();

    assert!(
        matches!(err, TrackerError::RateLimited { .. }),
        "expected a rate-limit refusal, got {err:?}"
    );
    assert_eq!(
        sent_before, sent_after,
        "a refused call must not have reached the network"
    );
}

// ---------------------------------------------------------------------------
// Recorded reality. Everything below reads a byte-exact capture from a REAL
// GitHub board rather than a fixture anyone invented — see the module doc.
// ---------------------------------------------------------------------------

/// The captured answer for a real, default Projects v2 board.
const REAL_FIELDS: &str = include_str!("fixtures/projects_v2_fields.json");

fn real_field_nodes() -> Vec<Value> {
    let v: Value = serde_json::from_str(REAL_FIELDS).expect("the capture must be valid JSON");
    v.pointer("/data/node/fields/nodes")
        .and_then(Value::as_array)
        .cloned()
        .expect("the capture must carry field nodes")
}

/// THE ASSUMPTION THAT DIED FIRST, and the one field discovery would have been
/// built on.
///
/// A single-select OPTION id is NOT a GitHub node id. Field ids are long,
/// prefixed and base64-ish (`PVTF_…`, `PVTSSF_…`); option ids are bare 8-character
/// hex (`f75ad846`). Anything that validates an option id by looking for a `PVT`
/// prefix — or that stores one in a column sized for a node id, or that tries to
/// resolve one through `node(id:)` — is wrong, and no invented fixture would ever
/// have said so.
#[test]
fn a_single_select_option_id_is_short_hex_not_a_node_id() {
    let nodes = real_field_nodes();
    let status = nodes
        .iter()
        .find(|n| n.get("dataType").and_then(Value::as_str) == Some("SINGLE_SELECT"))
        .expect("a default board ships a SINGLE_SELECT field");

    let options = status
        .get("options")
        .and_then(Value::as_array)
        .expect("a single-select carries its options");
    assert!(!options.is_empty());

    for o in options {
        let id = o.get("id").and_then(Value::as_str).expect("option id");
        assert!(
            !id.starts_with("PVT"),
            "option id {id:?} — option ids are NOT node ids; do not prefix-check them"
        );
        assert_eq!(
            id.len(),
            8,
            "captured option ids are 8 hex characters; {id:?} is {} — if GitHub \
             changed this, 9b's mapping needs re-checking",
            id.len()
        );
        assert!(
            id.chars().all(|c| c.is_ascii_hexdigit()),
            "option id {id:?} is not hex"
        );
    }
}

/// THE ASSUMPTION THAT DIED SECOND, and this one is good news.
///
/// The last handoff claimed Projects v2 has no type on a single-select, so the
/// NAME would be all 9b could match on — which would have made it the one
/// adapter in this repo forced to match on a human-editable string, against the
/// rule linear.rs sets ("Types are stable; names are not").
///
/// It is false. Every field carries a `dataType`, and the single-select field id
/// carries its own `PVTSSF_` prefix as well. 9b matches on `dataType`.
#[test]
fn every_real_field_carries_a_stable_datatype() {
    let nodes = real_field_nodes();
    assert_eq!(nodes.len(), 13, "a default board ships 13 built-in fields");

    for n in &nodes {
        let name = n.get("name").and_then(Value::as_str).unwrap_or("?");
        assert!(
            n.get("dataType").and_then(Value::as_str).is_some(),
            "field {name:?} carries no dataType — 9b would be forced onto names"
        );
    }

    let status = nodes
        .iter()
        .find(|n| n.get("dataType").and_then(Value::as_str) == Some("SINGLE_SELECT"))
        .expect("SINGLE_SELECT must be discoverable by dataType alone");
    assert!(
        status
            .get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| id.starts_with("PVTSSF_")),
        "a single-select field id carries its own prefix, distinct from PVTF_"
    );
}

/// A default board DOES ship a field called Status — so "Status is conventional
/// but not guaranteed" is right about the guarantee and wrong about the odds.
/// 9b must still degrade when it is absent, but the common case is that it is
/// there, with these three options, and that is worth pinning.
#[test]
fn a_default_board_ships_status_with_todo_in_progress_done() {
    let nodes = real_field_nodes();
    let status = nodes
        .iter()
        .find(|n| n.get("name").and_then(Value::as_str) == Some("Status"))
        .expect("a default board ships Status");

    let names: Vec<&str> = status
        .get("options")
        .and_then(Value::as_array)
        .expect("options")
        .iter()
        .filter_map(|o| o.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, vec!["Todo", "In Progress", "Done"]);
}

/// THE CHUNK'S OWN CASE, read off the real capture rather than off a fixture
/// anyone invented: our canonical states resolve to the board's REAL option ids.
///
/// Note what the ids look like. Nothing here could have been guessed, defaulted
/// or derived from an option's name, which is exactly why the live capture had
/// to exist before this mapping was written.
#[test]
fn the_real_captured_board_maps_our_states_to_its_real_option_ids() {
    let v: Value = serde_json::from_str(REAL_FIELDS).unwrap();
    let schema = FieldSchema::from_field_nodes(v.pointer("/data/node/fields/nodes").unwrap());

    let status = schema
        .status_field()
        .expect("the real board's lifecycle single-select must be discoverable");
    assert!(
        status.id.starts_with("PVTSSF_"),
        "the discovered field must be the single-select, got {:?}",
        status.id
    );

    assert_eq!(
        status.option_for_state(WorkItemState::Todo).unwrap().id,
        "f75ad846"
    );
    assert_eq!(
        status
            .option_for_state(WorkItemState::InProgress)
            .unwrap()
            .id,
        "47fc9ee4"
    );
    assert_eq!(
        status.option_for_state(WorkItemState::Done).unwrap().id,
        "98236657"
    );
}

/// THE FINDING THIS CHUNK PRODUCED, and it is a negative one: a DEFAULT GitHub
/// board cannot express two of the four things the roadmap wants to say.
///
/// There is no option meaning *cancelled* — Todo / In Progress / Done is the
/// whole vocabulary — so an obsoleted chunk has nowhere to land, and there is no
/// second single-select at all, so priority has nowhere to land either. Both are
/// refusals or omissions on the most ordinary board there is, which makes the
/// refusal path the COMMON case for `Cancelled` rather than the exotic one the
/// chunk description expected.
#[test]
fn the_real_captured_board_cannot_express_cancelled_or_priority() {
    let v: Value = serde_json::from_str(REAL_FIELDS).unwrap();
    let schema = FieldSchema::from_field_nodes(v.pointer("/data/node/fields/nodes").unwrap());

    let status = schema.status_field().unwrap();
    assert!(
        status.option_for_state(WorkItemState::Cancelled).is_none(),
        "a default board's Todo/In Progress/Done has no cancelled analogue — if \
         GitHub has changed that, this refusal path is no longer the common case"
    );
    assert!(
        schema.band_field().is_none(),
        "a default board has exactly one single-select, and it is the lifecycle; \
         priority must go unrecorded rather than be written into Status"
    );
    assert_eq!(
        schema.single_selects().count(),
        1,
        "the capture shows a default board ships exactly one single-select"
    );
}

/// The capture answers the pagination question for the DEFAULT case only, and
/// says so: 13 fields fit inside GitHub's own `first: 20` example. A board with
/// custom fields can exceed it, which is why 9b still has to decide about
/// pagination rather than inherit an answer from here.
#[test]
fn the_default_field_count_fits_githubs_example_page_size() {
    let v: Value = serde_json::from_str(REAL_FIELDS).unwrap();
    let total = v
        .pointer("/data/node/fields/totalCount")
        .and_then(Value::as_u64)
        .expect("totalCount");
    assert_eq!(total, 13);
    assert!(
        total < 20,
        "the default board fits one page — this does NOT prove a custom board does"
    );
}

// ---------------------------------------------------------------------------
// Field discovery over the wire, and the refusal that precedes every write.
// ---------------------------------------------------------------------------

/// Serves the board lookup and then the fields connection ONE PAGE PER REQUEST,
/// so pagination is exercised end to end rather than asserted about.
struct BoardWithFields {
    /// The `nodes` array for each page, in order.
    pages: Vec<Value>,
}

impl Respond for BoardWithFields {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if query.contains("query BoardId") {
            return ResponseTemplate::new(200).set_body_json(org_board());
        }
        if !query.contains("query BoardFields") {
            return ResponseTemplate::new(200).set_body_json(json!({
                "errors": [{ "message": format!("unexpected operation: {query}") }]
            }));
        }

        // The cursor IS the page index, so a client that forgets to send
        // `after` re-reads page 0 forever and the test that counts fields fails.
        let index = body
            .pointer("/variables/after")
            .and_then(Value::as_str)
            .and_then(|c| c.strip_prefix("cursor-"))
            .and_then(|n| n.parse::<usize>().ok())
            .map_or(0, |n| n + 1);

        let nodes = self.pages.get(index).cloned().unwrap_or_else(|| json!([]));
        let total: usize = self
            .pages
            .iter()
            .filter_map(Value::as_array)
            .map(Vec::len)
            .sum();

        ResponseTemplate::new(200).set_body_json(json!({ "data": { "node": { "fields": {
            "totalCount": total,
            "pageInfo": {
                "hasNextPage": index + 1 < self.pages.len(),
                "endCursor": format!("cursor-{index}"),
            },
            "nodes": nodes,
        }}}}))
    }
}

async fn serve_fields(pages: Vec<Value>) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(BoardWithFields { pages })
        .mount(&server)
        .await;
    server
}

fn client_for(server: &MockServer) -> ProjectsV2Client {
    ProjectsV2Client::new("orgs/acme/projects/12")
        .unwrap()
        .with_api_base(&server.uri())
}

fn single_select(id: &str, name: &str, options: &[(&str, &str)]) -> Value {
    json!({
        "id": id,
        "name": name,
        "dataType": "SINGLE_SELECT",
        "options": options.iter().map(|(oid, on)| json!({ "id": oid, "name": on }))
            .collect::<Vec<_>>(),
    })
}

fn plain_field(id: &str, name: &str, data_type: &str) -> Value {
    json!({ "id": id, "name": name, "dataType": data_type })
}

/// The assertion the refusal demands, and it is stronger than "an Err came back":
/// NOTHING that reached the server carried a mutation.
async fn assert_no_mutation_reached(server: &MockServer) {
    for req in server.received_requests().await.unwrap() {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            !query.contains("mutation"),
            "a mutation reached the server despite the refusal: {query}"
        );
    }
}

/// PAGINATION, DECIDED RATHER THAN INHERITED. The captured board's 13 fields fit
/// GitHub's own `first: 20` example, which proves nothing about a board carrying
/// custom fields — so the client follows `pageInfo` to exhaustion, and the
/// lifecycle field deliberately sits on the SECOND page where a single-page
/// reader would never see it.
#[tokio::test]
async fn a_board_whose_fields_span_two_pages_is_discovered_in_full() {
    let server = serve_fields(vec![
        json!([
            plain_field("PVTF_1", "Title", "TITLE"),
            plain_field("PVTF_2", "Assignees", "ASSIGNEES"),
        ]),
        json!([
            plain_field("PVTF_3", "Iteration", "ITERATION"),
            single_select(
                "PVTSSF_9",
                "Delivery stage",
                &[
                    ("f75ad846", "Todo"),
                    ("47fc9ee4", "In Progress"),
                    ("98236657", "Done"),
                ],
            ),
        ]),
    ])
    .await;

    let client = client_for(&server);
    let schema = client.field_schema().await.expect("discovery must succeed");

    assert_eq!(
        schema.fields().len(),
        4,
        "both pages must be merged — a client that ignores pageInfo sees 2"
    );
    let status = schema
        .status_field()
        .expect("the lifecycle field lives on page two and must still be found");
    assert_eq!(status.name, "Delivery stage");
    assert_eq!(
        client.status_write_for(WorkItemState::Done).await.unwrap(),
        think_and_ship::tracker::projects_v2::FieldWrite {
            field_id: "PVTSSF_9".into(),
            option_id: "98236657".into(),
        }
    );

    // One board lookup + two field pages. A client that stopped at page one
    // would have sent two.
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

/// Discovery is paid ONCE per run, exactly as the board lookup is. A schema
/// re-fetched per item would spend the point budget on an answer that cannot
/// change mid-run.
#[tokio::test]
async fn the_field_schema_is_discovered_once_and_then_cached() {
    let server = serve_fields(vec![json!([single_select(
        "PVTSSF_9",
        "Status",
        &[("a1b2c3d4", "Todo"), ("98236657", "Done")],
    )])])
    .await;

    let client = client_for(&server);
    let before = client.remaining_points().unwrap();
    let first = client.field_schema().await.unwrap();
    let after_first = client.remaining_points().unwrap();
    let second = client.field_schema().await.unwrap();
    let after_second = client.remaining_points().unwrap();

    assert_eq!(first, second);
    assert_eq!(
        before - after_first,
        2,
        "the first discovery is a board lookup plus one page, at 1 point each"
    );
    assert_eq!(
        after_first, after_second,
        "the second field_schema() must cost ZERO points"
    );
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "the second field_schema() must send ZERO requests"
    );

    // And every later resolution reads the cache too, rather than the network.
    client.status_write_for(WorkItemState::Done).await.unwrap();
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

/// THE REFUSAL. A board whose only single-select is a T-shirt size has no
/// lifecycle vocabulary, and the honest answer is to stop — not to create a
/// field, and not to write "Done" into someone's sizing column.
#[tokio::test]
async fn a_board_with_no_lifecycle_analogue_is_refused_before_any_mutation() {
    let server = serve_fields(vec![json!([
        plain_field("PVTF_1", "Title", "TITLE"),
        single_select("PVTSSF_7", "Size", &[("aa", "S"), ("bb", "M"), ("cc", "L")],),
    ])])
    .await;

    let client = client_for(&server);
    let err = client
        .status_write_for(WorkItemState::InProgress)
        .await
        .unwrap_err();

    match &err {
        TrackerError::Unsupported(msg) => {
            assert!(
                msg.contains("orgs/acme/projects/12"),
                "the refusal must name the board: {msg}"
            );
            assert!(
                msg.contains("Size"),
                "the refusal must name what the board actually has: {msg}"
            );
            assert!(
                msg.contains("Nothing was written"),
                "the refusal must say nothing happened: {msg}"
            );
            assert!(
                msg.contains("no field will ever be created"),
                "the refusal must promise not to modify the board: {msg}"
            );
        }
        other => panic!("expected an Unsupported refusal, got {other:?}"),
    }

    // The point of the whole refusal: not merely that an Err came back.
    assert_no_mutation_reached(&server).await;
    assert_eq!(
        server.received_requests().await.unwrap().len(),
        2,
        "only the board lookup and the fields query may have been sent"
    );
}

/// THE ACCEPTANCE CRITERION for a cancelled state with no board column: proven
/// against the REAL captured board rather than a hand-built one.
///
/// On the most ordinary board there is, an obsoleted chunk must NOT raise — it
/// must come back as a deliberate non-write carrying what the end-of-run
/// summary needs. The states the same board CAN express still resolve, so this
/// is a per-state policy and not a board-wide give-up.
#[tokio::test]
async fn the_real_captured_board_leaves_cancelled_unchanged_rather_than_refusing() {
    let server = serve_fields(vec![json!(real_field_nodes())]).await;
    let client = client_for(&server);

    let placement = client
        .status_placement_for(WorkItemState::Cancelled)
        .await
        .expect("a default board's missing Cancelled is policy, not an error");

    let StatusPlacement::LeftUnchanged(left) = placement else {
        panic!("the captured board has no Cancelled option, so this must be a non-write");
    };
    assert_eq!(left.state, WorkItemState::Cancelled);
    assert!(
        left.offered.contains("'Todo'") && left.offered.contains("'Done'"),
        "the non-write must carry what the board DOES offer: {}",
        left.offered
    );

    // Non-vacuity: the same board, the same call, a state it can express — so
    // "LeftUnchanged" is a real distinction and not what this method always says.
    let expressible = client
        .status_placement_for(WorkItemState::InProgress)
        .await
        .expect("the captured board can express in progress");
    assert!(
        matches!(expressible, StatusPlacement::Write(_)),
        "got: {expressible:?}"
    );

    // And the summary a run would actually print names this very board.
    let summary = summarise_left_unchanged(&[left]).expect("one item must be reported");
    assert!(summary.contains("1 item(s)"), "got: {summary}");

    assert_no_mutation_reached(&server).await;
}

/// The policy must not swallow a MISCONFIGURATION. A board with no lifecycle
/// field at all is a hard refusal even for Cancelled — there is a difference
/// between "your board cannot say this one word" and "your board has no
/// vocabulary at all", and only the first is ordinary.
#[tokio::test]
async fn a_board_with_no_lifecycle_field_still_refuses_even_for_cancelled() {
    let server = serve_fields(vec![json!([single_select(
        "PVTSSF_1",
        "Priority",
        &[("p1", "Critical"), ("p2", "Low")],
    )])])
    .await;
    let client = client_for(&server);

    let err = client
        .status_placement_for(WorkItemState::Cancelled)
        .await
        .expect_err("no lifecycle field at all is a misconfiguration, not policy");
    match &err {
        TrackerError::Unsupported(msg) => {
            assert!(msg.contains("nowhere to record the state"), "got: {msg}");
            assert!(
                msg.contains("no field will ever be created"),
                "the refusal must still promise not to invent a field: {msg}"
            );
        }
        other => panic!("expected an Unsupported refusal, got {other:?}"),
    }

    assert_no_mutation_reached(&server).await;
}

/// The narrower refusal via the RAW resolver, which 9b's loud behaviour still
/// depends on: the lifecycle field exists but has no option meaning
/// `cancelled`. `status_write_for` is deliberately untouched by the policy —
/// only `status_placement_for` applies it.
#[tokio::test]
async fn a_state_the_lifecycle_cannot_express_is_refused_naming_the_real_options() {
    let server = serve_fields(vec![json!([single_select(
        "PVTSSF_9",
        "Status",
        &[
            ("f75ad846", "Todo"),
            ("47fc9ee4", "In Progress"),
            ("98236657", "Done"),
        ],
    )])])
    .await;

    let client = client_for(&server);

    // The states it CAN express still resolve, so this is a per-state refusal
    // rather than a board-wide one.
    assert_eq!(
        client
            .status_write_for(WorkItemState::InProgress)
            .await
            .unwrap()
            .option_id,
        "47fc9ee4"
    );

    let err = client
        .status_write_for(WorkItemState::Cancelled)
        .await
        .unwrap_err();
    match &err {
        TrackerError::Unsupported(msg) => {
            assert!(msg.contains("'Status'"), "name the field: {msg}");
            assert!(msg.contains("cancelled"), "name the state: {msg}");
            assert!(
                msg.contains("'Todo'") && msg.contains("'In Progress'") && msg.contains("'Done'"),
                "the refusal must list what the board does offer: {msg}"
            );
            assert!(
                msg.contains("yours to add, not ours to create"),
                "the refusal must be explicit that we will not invent an option: {msg}"
            );
        }
        other => panic!("expected an Unsupported refusal, got {other:?}"),
    }

    assert_no_mutation_reached(&server).await;
}

/// A priority band with no analogue is NOT a refusal — absent metadata costs a
/// reader nothing, where a wrong lifecycle state corrupts the board's meaning.
/// The asymmetry is deliberate and this is where it is pinned.
#[tokio::test]
async fn a_missing_priority_analogue_degrades_to_none_rather_than_refusing() {
    let server = serve_fields(vec![json!([
        single_select(
            "PVTSSF_9",
            "Status",
            &[("f75ad846", "Todo"), ("98236657", "Done")],
        ),
        single_select("PVTSSF_A", "Urgency", &[("11", "Critical"), ("22", "Low")],),
    ])])
    .await;

    let client = client_for(&server);

    assert_eq!(
        client
            .band_write_for("critical")
            .await
            .unwrap()
            .expect("the board has a Critical option")
            .option_id,
        "11"
    );
    assert!(
        client.band_write_for("medium").await.unwrap().is_none(),
        "a band the board cannot express is an omission, not an error"
    );
    assert_no_mutation_reached(&server).await;
}

// ---------------------------------------------------------------------------
// The attach path — one chunk, one twin per provider.
// ---------------------------------------------------------------------------

/// Serves the three operations an attach needs: the board lookup, the issue-node
/// lookup, and the add-item mutation. Routing on the operation name pins WHICH
/// request was sent rather than merely that something was posted.
struct BoardWithIssues {
    /// The `issue` node served for `query IssueNode`. `Null` models the case
    /// that matters most — a missing issue arrives inside a SUCCESSFUL envelope.
    issue: Value,
    /// The id `addProjectV2ItemById` answers with. One value models both the
    /// first add and a duplicate, because GitHub documents that a duplicate add
    /// returns the EXISTING item id rather than failing.
    item_id: &'static str,
}

impl Respond for BoardWithIssues {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if query.contains("query BoardId") {
            return ResponseTemplate::new(200).set_body_json(org_board());
        }
        if query.contains("query IssueNode") {
            return ResponseTemplate::new(200).set_body_json(json!({
                "data": { "repository": { "issue": self.issue.clone() } }
            }));
        }
        if query.contains("mutation AddBoardItem") {
            return ResponseTemplate::new(200).set_body_json(json!({
                "data": { "addProjectV2ItemById": { "item": { "id": self.item_id } } }
            }));
        }
        ResponseTemplate::new(200).set_body_json(json!({
            "errors": [{ "message": format!("unexpected operation: {query}") }]
        }))
    }
}

async fn serve_issues(issue: Value, item_id: &'static str) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(BoardWithIssues { issue, item_id })
        .mount(&server)
        .await;
    server
}

/// The request bodies that actually carried a mutation — the only way to count
/// writes rather than requests.
async fn mutations_reaching(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter_map(|req| serde_json::from_slice::<Value>(&req.body).ok())
        .filter(|body| {
            body.get("query")
                .and_then(Value::as_str)
                .is_some_and(|q| q.contains("mutation"))
        })
        .collect()
}

/// THE FIRST CRITERION of the attach path: the item attaches to the issue the
/// issues lane already linked, by content id, and no draft is ever created.
///
/// The load-bearing assertions are about what REACHED THE BOARD, not about the
/// value that came back — a client that returned the right item after posting
/// a draft mutation would pass a return-value-only test and be exactly wrong.
#[tokio::test]
async fn the_item_attaches_to_the_linked_issue_by_content_id_and_never_as_a_draft() {
    let server = serve_issues(json!({ "id": "I_kwDOIssue42" }), "PVTI_lADOitem1").await;
    let client = client_for(&server);

    let item = client
        .attach_issue("acme/widgets#42")
        .await
        .expect("a real coordinate must attach");

    let mutations = mutations_reaching(&server).await;
    assert_eq!(mutations.len(), 1, "exactly one write reaches the board");
    let query = mutations[0]
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert_eq!(
        mutations[0]
            .pointer("/variables/content")
            .and_then(Value::as_str),
        Some("I_kwDOIssue42"),
        "the item must attach to the issue the issues lane linked, not to a new identity"
    );
    assert!(
        query.contains("addProjectV2ItemById"),
        "the documented mutation for attaching existing content: {query}"
    );
    assert!(
        !query.contains("DraftIssue"),
        "a draft has no issue behind it and would be a second, unreconcilable twin: {query}"
    );
    assert_eq!(
        mutations[0]
            .pointer("/variables/board")
            .and_then(Value::as_str),
        Some("PVT_kwDOAbcd4A"),
        "the resolved board id, not the pasted address"
    );

    assert_eq!(item.id, "PVTI_lADOitem1");
    assert_eq!(item.content_id, "I_kwDOIssue42");
}

/// IDEMPOTENCY, OUR HALF: adding the same chunk twice within a run finds the
/// existing item instead of posting again.
///
/// Asserted on the wire and on the point budget, because "returns the same
/// value" is also true of a client that re-posts every time.
#[tokio::test]
async fn attaching_the_same_chunk_twice_reaches_the_board_once_and_costs_nothing_again() {
    let server = serve_issues(json!({ "id": "I_kwDOIssue42" }), "PVTI_lADOitem1").await;
    let client = client_for(&server);

    let first = client.attach_issue("acme/widgets#42").await.unwrap();
    let after_first = client.remaining_points();
    let second = client.attach_issue("acme/widgets#42").await.unwrap();

    assert_eq!(
        mutations_reaching(&server).await.len(),
        1,
        "the second attach must not post a second mutation"
    );
    assert_eq!(first, second, "and must yield the same item");
    assert_eq!(
        client.remaining_points(),
        after_first,
        "a cached attach must not spend a point"
    );

    // NON-VACUITY: a DIFFERENT chunk on the same client still reaches the board,
    // so the cache is keyed on the issue rather than swallowing everything.
    client.attach_issue("acme/widgets#43").await.unwrap();
    assert_eq!(mutations_reaching(&server).await.len(), 2);
    assert!(
        client.remaining_points() < after_first,
        "a new issue does spend the budget"
    );
}

/// IDEMPOTENCY, GITHUB'S HALF, encoded as an expectation rather than assumed.
///
/// GitHub documents that "if you try to add an item that already exists, the
/// existing item ID is returned instead". Two COLD clients share no cache, so
/// both really post — and the second must be treated as success returning the
/// same item, not as a failure and not as a second twin. Honest limit: this
/// pins how we READ that answer; the claim itself is documented and has not yet
/// been probed against the live endpoint.
#[tokio::test]
async fn a_duplicate_add_that_does_reach_github_yields_the_existing_item_not_a_second_twin() {
    let server = serve_issues(json!({ "id": "I_kwDOIssue42" }), "PVTI_existing").await;

    let first = client_for(&server)
        .attach_issue("acme/widgets#42")
        .await
        .unwrap();
    let second = client_for(&server)
        .attach_issue("acme/widgets#42")
        .await
        .unwrap();

    assert_eq!(
        mutations_reaching(&server).await.len(),
        2,
        "two cold clients really did both post — otherwise this proves nothing"
    );
    assert_eq!(first, second, "the duplicate add yields the SAME item");
    assert_eq!(first.id, "PVTI_existing");
}

/// An external_id that is not an issue coordinate spends NOTHING — no request,
/// no point — and the refusal names both the shape that works and the reason.
#[tokio::test]
async fn an_external_id_that_is_not_an_issue_coordinate_reaches_no_network_at_all() {
    let server = serve_issues(json!({ "id": "I_kwDOIssue42" }), "PVTI_lADOitem1").await;
    let client = client_for(&server);
    let before = client.remaining_points();

    let err = client
        .attach_issue("not-a-coordinate — the board item")
        .await
        .unwrap_err();
    assert!(
        server.received_requests().await.unwrap().is_empty(),
        "an unreadable coordinate must not spend a request"
    );
    match &err {
        TrackerError::NotFound(msg) => {
            assert!(
                msg.contains("owner/repo#number"),
                "name the shape that would work: {msg}"
            );
            assert!(
                msg.contains("draft"),
                "say why a title cannot simply become an item: {msg}"
            );
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert_eq!(client.remaining_points(), before, "nor a point");

    // NON-VACUITY: the SAME client on a real coordinate does reach the board.
    client.attach_issue("acme/widgets#42").await.unwrap();
    assert!(!server.received_requests().await.unwrap().is_empty());
    assert!(client.remaining_points() < before);
}

/// A missing issue arrives as `null` inside a 200 with no `errors` — the trap
/// `board()` already names. It must refuse BEFORE the mutation, or a write goes
/// out with a null content id.
#[tokio::test]
async fn an_issue_that_does_not_exist_refuses_before_any_mutation() {
    let server = serve_issues(Value::Null, "PVTI_lADOitem1").await;
    let client = client_for(&server);

    let err = client.attach_issue("acme/widgets#404").await.unwrap_err();
    assert_no_mutation_reached(&server).await;
    match &err {
        TrackerError::NotFound(msg) => {
            assert!(msg.contains("acme/widgets#404"), "name the issue: {msg}");
            assert!(
                msg.contains("nothing was added"),
                "say that the board is untouched: {msg}"
            );
        }
        other => panic!("expected NotFound, got {other:?}"),
    }

    // NON-VACUITY: the lookup itself DID happen and answered 200 without errors,
    // so this is the null-inside-success trap rather than an error path.
    assert!(
        !server.received_requests().await.unwrap().is_empty(),
        "the issue lookup must really have been sent"
    );
}

/// The coordinate the board lane parses is the same string the link record
/// holds — asserted here, at the seam, and not only in the unit tests.
#[test]
fn the_board_lane_reads_the_same_external_id_the_link_record_holds() {
    let coordinate = IssueCoordinate::parse("AlrikOlson/think-and-ship#1501").unwrap();
    assert_eq!(
        coordinate.as_external_id(),
        "AlrikOlson/think-and-ship#1501"
    );
    assert_eq!(coordinate.number, 1501);
}

// ---------------------------------------------------------------------------
// The board reaches the projector.
// ---------------------------------------------------------------------------

/// Serves every operation a projection needs: the board lookup, the fields
/// connection, the issue-node lookup, the attach, and the field write.
///
/// `secondary_limit` makes every MUTATION answer the way GitHub answers a
/// secondary rate limit — 403 with `retry-after`, not 429. That is the case the
/// outbox criterion is really about, and it is the one this client used to get
/// wrong.
struct BoardForProjection {
    fields: Vec<Value>,
    item_id: &'static str,
    secondary_limit: bool,
}

impl Respond for BoardForProjection {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();

        if query.contains("mutation") && self.secondary_limit {
            return ResponseTemplate::new(403)
                .insert_header("retry-after", "60")
                .set_body_string("You have exceeded a secondary rate limit");
        }
        if query.contains("query BoardId") {
            return ResponseTemplate::new(200).set_body_json(org_board());
        }
        if query.contains("query BoardFields") {
            return ResponseTemplate::new(200).set_body_json(
                json!({ "data": { "node": { "fields": {
                    "totalCount": self.fields.len(),
                    "pageInfo": { "hasNextPage": false, "endCursor": "cursor-0" },
                    "nodes": self.fields.clone(),
                }}}}),
            );
        }
        if query.contains("query IssueNode") {
            return ResponseTemplate::new(200).set_body_json(json!({
                "data": { "repository": { "issue": { "id": "I_kwDOIssue42" } } }
            }));
        }
        if query.contains("mutation AddBoardItem") {
            return ResponseTemplate::new(200).set_body_json(json!({
                "data": { "addProjectV2ItemById": { "item": { "id": self.item_id } } }
            }));
        }
        if query.contains("mutation SetBoardField") {
            return ResponseTemplate::new(200).set_body_json(json!({
                "data": { "updateProjectV2ItemFieldValue": { "projectV2Item": { "id": self.item_id } } }
            }));
        }
        ResponseTemplate::new(200).set_body_json(json!({
            "errors": [{ "message": format!("unexpected operation: {query}") }]
        }))
    }
}

/// A lifecycle field with a DECOY: `Todo` is first, so a write that picked the
/// first option rather than the resolved one would still look plausible.
fn lifecycle_field() -> Vec<Value> {
    vec![single_select(
        "PVTSSF_status",
        "Status",
        &[
            ("f75ad846", "Todo"),
            ("47fc9ee4", "In Progress"),
            ("98236657", "Done"),
        ],
    )]
}

async fn serve_projection(fields: Vec<Value>, secondary_limit: bool) -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(BoardForProjection {
            fields,
            item_id: "PVTI_lADOitem1",
            secondary_limit,
        })
        .mount(&server)
        .await;
    server
}

fn board_tracker(server: &MockServer) -> ProjectsV2Tracker {
    ProjectsV2Tracker::new(client_for(server))
}

fn linked_item(external_id: &str, state: WorkItemState) -> WorkItem {
    let mut item = WorkItem::new("Chunk that already reached GitHub Issues");
    item.external_id = Some(external_id.to_string());
    item.state = state;
    item
}

/// The one mutation body that carried a field write, or `None`.
async fn field_write_reaching(server: &MockServer) -> Option<Value> {
    mutations_reaching(server).await.into_iter().find(|body| {
        body.get("query")
            .and_then(Value::as_str)
            .is_some_and(|q| q.contains("SetBoardField"))
    })
}

/// THE FIELD WRITE, asserted on the WIRE. The ids that reach GitHub must be
/// the board's OWN — found by field discovery — rather than anything this
/// adapter composed, and the option must be the RESOLVED one rather than the
/// first.
#[tokio::test]
async fn the_status_write_carries_the_boards_own_field_and_the_resolved_option() {
    let server = serve_projection(lifecycle_field(), false).await;
    let tracker = board_tracker(&server);

    let outcome = tracker
        .upsert_item(&linked_item("acme/widgets#42", WorkItemState::InProgress))
        .await
        .expect("a linked chunk must reach the board");

    let write = field_write_reaching(&server)
        .await
        .expect("a resolvable status must produce a field write");
    // Load-bearing FIRST: the option is the one that MEANS in-progress, not the
    // first the board happens to offer.
    assert_eq!(
        write.pointer("/variables/option").and_then(Value::as_str),
        Some("47fc9ee4"),
        "the resolved option, not the first one on the field: {write}"
    );
    assert_eq!(
        write.pointer("/variables/field").and_then(Value::as_str),
        Some("PVTSSF_status"),
        "the field id must come from discovery: {write}"
    );
    assert_eq!(
        write.pointer("/variables/item").and_then(Value::as_str),
        Some("PVTI_lADOitem1"),
        "the item id must come from the attach, not be invented: {write}"
    );
    assert_eq!(
        write.pointer("/variables/board").and_then(Value::as_str),
        Some("PVT_kwDOAbcd4A"),
        "updateProjectV2ItemFieldValue needs the board as well as the item: {write}"
    );
    let query = write
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        query.contains("singleSelectOptionId"),
        "a single-select is written through its own value variant: {query}"
    );

    // The identity that comes back is the issue's, unchanged — the board never
    // mints one, and the item id is spent within the run and then forgotten.
    assert_eq!(outcome.external_id, "acme/widgets#42");
    assert!(
        !outcome.created,
        "the board attaches an issue that already exists; it never creates one"
    );
    assert!(
        outcome.version.is_none(),
        "a board item carries no concurrency token to remember"
    );
}

/// A chunk that has not reached GitHub Issues yet is REFUSED, and the refusal
/// names the fix rather than failing as a null somewhere downstream.
///
/// Paired with a positive in the same test and against the same server: the
/// very next call, with an id present, really does write. Without that half the
/// refusal could be passing because the mock is broken.
#[tokio::test]
async fn a_chunk_with_no_issue_is_refused_and_nothing_reaches_the_board() {
    let server = serve_projection(lifecycle_field(), false).await;
    let tracker = board_tracker(&server);

    let mut unlinked = WorkItem::new("Chunk that has never been pushed anywhere");
    unlinked.state = WorkItemState::Todo;
    let error = match tracker.upsert_item(&unlinked).await {
        Ok(o) => panic!("a board cannot mint an issue, yet it returned {o:?}"),
        Err(e) => e,
    };

    let message = error.to_string();
    assert!(
        message.contains("--provider github"),
        "the refusal must name the fix, not just the problem: {message}"
    );
    assert!(
        message.contains("orgs/acme/projects/12"),
        "the refusal must name the board nothing was written to: {message}"
    );
    assert!(
        !error.retryable(),
        "retrying cannot conjure an issue, so this must never queue for replay"
    );
    assert_no_mutation_reached(&server).await;

    // THE POSITIVE HALF: the same tracker, the same server, an id present.
    tracker
        .upsert_item(&linked_item("acme/widgets#42", WorkItemState::Todo))
        .await
        .expect("the refusal above must be about the missing id, not the board");
    assert!(
        field_write_reaching(&server).await.is_some(),
        "the positive half must actually write, or the refusal proves nothing"
    );
}

/// The decided policy for a state the board cannot express,
/// end to end: the column is left alone, the attach still happens, and the
/// staleness is announced ONCE for the whole run rather than once per item.
#[tokio::test]
async fn a_cancelled_state_leaves_the_column_alone_and_is_announced_once_per_run() {
    let server = serve_projection(lifecycle_field(), false).await;
    let tracker = board_tracker(&server);

    for external_id in ["acme/widgets#42", "acme/widgets#43"] {
        tracker
            .upsert_item(&linked_item(external_id, WorkItemState::Cancelled))
            .await
            .expect("an unexpressible cancelled is not a failure");
    }

    // Load-bearing FIRST: ONE sentence covering BOTH items.
    let summary = tracker
        .take_left_unchanged_summary()
        .expect("two declined writes must produce a summary");
    assert!(
        summary.contains("2 item(s)"),
        "one sentence folds the whole run, not one per item: {summary}"
    );
    assert!(
        summary.contains("orgs/acme/projects/12"),
        "the summary names the board a human has to go and fix: {summary}"
    );
    assert!(
        tracker.take_left_unchanged_summary().is_none(),
        "the accumulator drains, so nothing can announce the staleness twice"
    );

    // The non-write is about the COLUMN only: both items really did reach the
    // board, which is what makes leaving the status alone a partial success
    // rather than a silent skip.
    assert!(
        field_write_reaching(&server).await.is_none(),
        "the column must be left exactly as the human left it"
    );
    assert_eq!(
        mutations_reaching(&server).await.len(),
        2,
        "both items attach; only their status is declined"
    );
}

/// CRITERION 2, end to end through the real projector: a secondary rate limit
/// on the board queues for replay rather than being lost.
///
/// The status is 403, not 429 — which is how GitHub actually reports a
/// secondary limit, and what this client used to misread as a permanent
/// contract rejection that the outbox is forbidden to hold.
#[tokio::test]
async fn a_secondary_rate_limit_on_the_board_queues_instead_of_being_lost() {
    let server = serve_projection(lifecycle_field(), true).await;

    let mut engine = RoadmapEngine::new("proj".into());
    engine
        .add_chunk(
            "c1".into(),
            "Chunk c1".into(),
            ChunkStatus::Pending,
            10,
            "why c1 exists".into(),
            vec!["c1 works".into()],
            vec![],
            false,
        )
        .expect("add chunk");
    engine
        .set_tracker_opt_in("c1", "github_projects", true)
        .expect("opt in");
    // The board patches an issue the issues lane already linked — so the link is the
    // precondition, exactly as it would be on a second push.
    engine
        .record_tracker_link("c1", "github_projects", "acme/widgets#42", "stale", None)
        .expect("seed the link");

    let outbox = TrackerOutbox::new(None);
    let report = project_all(&mut engine, &board_tracker(&server), Some(&outbox))
        .await
        .expect("rate limiting is not a run failure");

    // Load-bearing FIRST: QUEUED, not Rejected. Rejected is what a 403 read as
    // a contract failure would produce, and it means the write is gone.
    assert!(
        matches!(report.outcomes[0].1, ProjectionOutcome::Queued { .. }),
        "a secondary limit must queue for replay, got {:?}",
        report.outcomes[0].1
    );
    assert_eq!(outbox.len(), 1, "the projection is durable, not dropped");
}
