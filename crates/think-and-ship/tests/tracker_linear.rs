//! Linear as the SECOND provider, and the falsification test
//! for the seam.
//!
//! These tests drive the real adapter and the real projector — the same
//! `project_all` that GitHub uses, unmodified — against an in-process GraphQL
//! mock. Nothing here is Linear-specific above the adapter boundary, which is
//! the whole claim: adding a provider adds a file.
//!
//! A GraphQL API has one endpoint, so `wiremock` cannot route by path the way it
//! does for REST. Instead each request is matched on the operation name inside
//! the query body, which is also a stronger assertion: it pins WHICH operation
//! was sent, not merely that something was posted.

use serde_json::{Value, json};
use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::budget::{RateBudget, Spend, Transport};
use think_and_ship::tracker::linear::{AuthScheme, LinearTracker};
use think_and_ship::tracker::ownership::Ownership;
use think_and_ship::tracker::port::TrackerPort;
use think_and_ship::tracker::project::{ProjectionOutcome, project_all, project_all_with_policy};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

/// Route by GraphQL operation name, since every call hits the same path.
///
/// The create counter is per-responder, NOT a global: `cargo test` runs these
/// in one process concurrently, and a shared counter lets one test consume
/// another's issue identifiers — which showed up as a create-order scramble
/// that looked exactly like an inverted relation.
struct ByOperation {
    team_states: Value,
    create: Vec<Value>,
    patch: Value,
    issue_lookup: Value,
    /// What `query IssueLabels` reports the issue currently carries — the read
    /// the patch path performs because Linear rejects removing an absent label.
    issue_labels: Value,
    relate: Value,
    /// The container fixtures. Defaults describe a workspace with no
    /// projects and no initiatives, so tests that never group anything are
    /// untouched.
    team_projects: Value,
    /// One response serves name, state AND initiative membership — the adapter
    /// reads all three in a single `query ProjectState`.
    project_state: Value,
    project_state_set: Value,
    initiatives: Value,
    /// What `query InitiativeById` answers — the remembered-uuid resolution
    /// path.
    initiative_by_id: Value,
    initiative_create: Value,
    initiative_status_set: Value,
    link: Value,
    created: std::sync::atomic::AtomicUsize,
    /// Every `issueLabelCreate` this server served. A per-instance field, NOT a
    /// process-global — integration tests share one process, and a static here
    /// once produced a failure that looked exactly like an inverted relation.
    labels_made: std::sync::atomic::AtomicUsize,
}

impl Respond for ByOperation {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = serde_json::from_slice(&req.body).unwrap_or(Value::Null);
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let data = if query.contains("query TeamSchema") {
            self.team_states.clone()
        // The container operations are matched BEFORE the generic
        // "mutation Create" arm: `CreateProject` and `CreateInitiative`
        // contain that substring, and falling through would serve them an
        // issue payload.
        } else if query.contains("query TeamProjects") {
            self.team_projects.clone()
        } else if query.contains("query ProjectState") {
            self.project_state.clone()
        } else if query.contains("mutation CreateProject") {
            // Dynamic like MakeLabel: the id echoes the requested name, so a
            // test can tell WHICH project a later write attached to.
            let name = body
                .pointer("/variables/input/name")
                .and_then(Value::as_str)
                .unwrap_or("unnamed")
                .to_string();
            json!({ "data": { "projectCreate": {
                "success": true,
                "project": { "id": format!("proj-{name}"), "name": name }
            }}})
        } else if query.contains("mutation SetProjectState") {
            self.project_state_set.clone()
        } else if query.contains("query InitiativeById") {
            self.initiative_by_id.clone()
        } else if query.contains("query Initiatives") {
            self.initiatives.clone()
        } else if query.contains("mutation CreateInitiative") {
            self.initiative_create.clone()
        } else if query.contains("mutation SetInitiativeStatus") {
            self.initiative_status_set.clone()
        } else if query.contains("mutation LinkProjectToInitiative") {
            self.link.clone()
        } else if query.contains("mutation Create") {
            // Serve creates in order so two chunks get distinct identifiers.
            let n = self
                .created
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.create
                .get(n)
                .cloned()
                .unwrap_or_else(|| self.create.last().cloned().unwrap_or(Value::Null))
        } else if query.contains("mutation Patch") {
            self.patch.clone()
        } else if query.contains("query IssueId") {
            self.issue_lookup.clone()
        } else if query.contains("query IssueLabels") {
            self.issue_labels.clone()
        } else if query.contains("mutation Relate") {
            self.relate.clone()
        } else if query.contains("mutation MakeLabel") {
            let n = self
                .labels_made
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let name = body
                .pointer("/variables/input/name")
                .and_then(Value::as_str)
                .unwrap_or("unnamed")
                .to_string();
            json!({ "data": { "issueLabelCreate": {
                "success": true,
                "issueLabel": { "id": format!("lbl-made-{n}"), "name": name }
            }}})
        } else {
            json!({ "errors": [{ "message": format!("unexpected operation: {query}") }] })
        };
        ResponseTemplate::new(200).set_body_json(data)
    }
}

/// A team whose workflow columns are named nothing like Linear's defaults —
/// the anti-corruption case. If anything matched on names, this would break.
fn team_states() -> Value {
    json!({ "data": { "teams": { "nodes": [{
        "id": "team-uuid-1",
        "key": "ENG",
        "states": { "nodes": [
            { "id": "st-up-next", "type": "unstarted" },
            { "id": "st-cooking", "type": "started" },
            { "id": "st-shipped", "type": "completed" },
            { "id": "st-binned",  "type": "canceled" }
        ]},
        // One label WE author and one the team owns, so a test can prove we
        // reuse ours without minting a duplicate and never touch theirs.
        "labels": { "nodes": [
            { "id": "lbl-critical", "name": "roadmap:critical" },
            { "id": "lbl-stale",    "name": "roadmap:later" },
            { "id": "lbl-theirs",   "name": "Bug" }
        ]}
    }]}}})
}

fn issue(identifier: &str, uuid: &str) -> Value {
    json!({ "id": uuid, "identifier": identifier, "updatedAt": "2026-07-25T09:00:00.000Z" })
}

fn responder() -> ByOperation {
    ByOperation {
        created: std::sync::atomic::AtomicUsize::new(0),
        labels_made: std::sync::atomic::AtomicUsize::new(0),
        team_states: team_states(),
        create: vec![
            json!({ "data": { "issueCreate": { "success": true, "issue": issue("ENG-1", "uuid-1") } } }),
            json!({ "data": { "issueCreate": { "success": true, "issue": issue("ENG-2", "uuid-2") } } }),
        ],
        patch: json!({ "data": { "issueUpdate": { "success": true, "issue": issue("ENG-1", "uuid-1") } } }),
        issue_lookup: json!({ "data": { "issue": issue("ENG-1", "uuid-1") } }),
        // The issue holds one stale band of ours and one label of theirs, so a
        // patch has something legitimate to remove and something it must not.
        issue_labels: json!({ "data": { "issue": { "labels": { "nodes": [
            { "id": "lbl-stale" }, { "id": "lbl-theirs" }
        ]}}}}),
        relate: json!({ "data": { "issueRelationCreate": { "success": true } } }),
        team_projects: json!({ "data": { "team": { "projects": { "nodes": [] } } } }),
        project_state: json!({ "data": { "project": {
            "name": "tracker", "state": "planned", "initiatives": { "nodes": [] }
        }}}),
        project_state_set: json!({ "data": { "projectUpdate": { "success": true } } }),
        initiatives: json!({ "data": { "initiatives": { "nodes": [] } } }),
        initiative_by_id: json!({ "errors": [{ "message": "no InitiativeById fixture set" }] }),
        initiative_create: json!({ "data": { "initiativeCreate": {
            "success": true, "initiative": { "id": "init-new" }
        }}}),
        initiative_status_set: json!({ "data": { "initiativeUpdate": { "success": true } } }),
        link: json!({ "data": { "initiativeToProjectCreate": { "success": true } } }),
    }
}

async fn linear_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(responder())
        .mount(&server)
        .await;
    server
}

fn linear(server: &MockServer) -> LinearTracker {
    LinearTracker::new("ENG")
        .expect("valid team key")
        .with_api_base(&server.uri())
        .with_token("lin_api_fake", AuthScheme::Raw)
}

fn engine() -> RoadmapEngine {
    RoadmapEngine::new("proj".into())
}

fn add(e: &mut RoadmapEngine, id: &str, deps: Vec<String>) {
    e.add_chunk(
        id.into(),
        format!("Chunk {id}"),
        ChunkStatus::Pending,
        10,
        format!("why {id} exists"),
        vec![format!("{id} works")],
        deps,
        false,
    )
    .expect("add chunk");
}

fn opt_in(e: &mut RoadmapEngine, id: &str) {
    e.set_tracker_opt_in(id, "linear", true).expect("opt in");
}

/// Every GraphQL request the server saw, as parsed bodies.
async fn sent(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .expect("recorded")
        .iter()
        .filter_map(|r| serde_json::from_slice::<Value>(&r.body).ok())
        .collect()
}

fn operation<'a>(sent: &'a [Value], name: &str) -> Option<&'a Value> {
    sent.iter().find(|b| {
        b.get("query")
            .and_then(Value::as_str)
            .is_some_and(|q| q.contains(name))
    })
}

/// How many times an operation was sent — for asserting idempotence (zero) as
/// firmly as presence (exactly one).
fn count(sent: &[Value], name: &str) -> usize {
    sent.iter()
        .filter(|b| {
            b.get("query")
                .and_then(Value::as_str)
                .is_some_and(|q| q.contains(name))
        })
        .count()
}

/// The same promise as every other provider: nothing leaves without consent.
#[tokio::test]
async fn a_default_configuration_emits_nothing_to_linear() {
    let server = linear_server().await;
    let mut e = engine();
    add(&mut e, "c1", vec![]);

    let report = project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");
    assert!(report.outcomes.is_empty());
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty()
    );
}

/// A chunk becomes a Linear issue through the UNMODIFIED projector, carrying
/// the same provenance footer GitHub gets — the payload is the product, and it
/// is provider-independent.
#[tokio::test]
async fn a_chunk_projects_to_a_linear_issue_with_its_provenance() {
    let server = linear_server().await;
    let mut e = engine();
    add(&mut e, "c1", vec![]);
    e.link_chunk("c1", "think:9").expect("think ref");
    opt_in(&mut e, "c1");

    let report = project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");
    assert_eq!(
        report.outcomes[0].1,
        ProjectionOutcome::Created {
            external_id: "ENG-1".into()
        }
    );

    let sent = sent(&server).await;
    let create = operation(&sent, "mutation Create").expect("a create was sent");
    let input = &create["variables"]["input"];
    assert_eq!(input["title"], "Chunk c1");
    // The team id came from DISCOVERY, not from configuration.
    assert_eq!(input["teamId"], "team-uuid-1");
    let body = input["description"].as_str().expect("description");
    assert!(body.contains("- [ ] c1 works"), "acceptance as a checklist");
    assert!(body.contains("think:9"), "provenance reaches Linear too");

    // The binding uses the human-readable identifier, not the UUID.
    assert_eq!(
        e.tracker_link("c1", "linear").expect("link").external_id,
        "ENG-1"
    );
}

/// The anti-corruption property end to end: this team's "todo" column is called
/// "Up next" and the adapter still resolves it, because only the TYPE is read.
#[tokio::test]
async fn the_state_is_resolved_from_a_team_whose_columns_have_custom_names() {
    let server = linear_server().await;
    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");

    project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");

    let sent = sent(&server).await;
    let create = operation(&sent, "mutation Create").expect("create");
    assert_eq!(
        create["variables"]["input"]["stateId"], "st-up-next",
        "a pending chunk maps to the team's unstarted state whatever it is called"
    );
    // And the discovery query asked by key, not by a hardcoded id.
    let discovery = operation(&sent, "query TeamSchema").expect("discovery");
    assert_eq!(discovery["variables"]["key"], "ENG");
}

/// Priority reaches Linear through the band label, reversed. Our band-10 chunk
/// is `critical`, which is Linear's `1` (urgent) — NOT its `0`.
#[tokio::test]
async fn priority_arrives_reversed_onto_linears_urgency_scale() {
    let server = linear_server().await;
    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");

    project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");

    let sent = sent(&server).await;
    let create = operation(&sent, "mutation Create").expect("create");
    assert_eq!(
        create["variables"]["input"]["priority"], 1,
        "critical is Linear's 1; 0 would mean 'no priority', the opposite of urgent"
    );
}

/// THE direction trap. `c1` depends on `base`, so `c1` is BLOCKED and `base` is
/// the BLOCKER. Linear stores one relation from the blocker's side, so the
/// blocker must be `issueId` and the blocked chunk `relatedIssueId` — the
/// opposite of GitHub. Getting this backwards inverts the dependency graph
/// silently, so the test pins which id lands in which field.
#[tokio::test]
async fn a_dependency_is_written_from_the_blockers_side() {
    let server = linear_server().await;
    let mut e = engine();
    add(&mut e, "base", vec![]);
    add(&mut e, "c1", vec!["base".into()]);
    opt_in(&mut e, "base");
    opt_in(&mut e, "c1");

    let report = project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");
    assert_eq!(report.relations_written, vec!["c1".to_string()]);

    // base created first -> ENG-1/uuid-1; c1 second -> ENG-2/uuid-2.
    let sent = sent(&server).await;
    let relate = operation(&sent, "mutation Relate").expect("a relation was sent");
    let input = &relate["variables"]["input"];
    assert_eq!(input["type"], "blocks");
    assert_eq!(
        input["issueId"], "uuid-1",
        "the BLOCKER (base) is the subject of a `blocks` relation"
    );
    assert_eq!(
        input["relatedIssueId"], "uuid-2",
        "the BLOCKED chunk (c1) is the object — reversing these inverts the graph"
    );
}

/// The no-op short-circuit is projector-level, so it must hold for a second
/// provider with no extra work. Zero requests on the second run.
#[tokio::test]
async fn re_running_an_unchanged_chunk_sends_no_graphql() {
    let server = linear_server().await;
    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    let tracker = linear(&server);

    project_all(&mut e, &tracker, None).await.expect("first");
    let after_first = server.received_requests().await.expect("recorded").len();

    let second = project_all(&mut e, &tracker, None).await.expect("second");
    assert!(matches!(
        second.outcomes[0].1,
        ProjectionOutcome::Skipped { .. }
    ));
    assert_eq!(
        server.received_requests().await.expect("recorded").len(),
        after_first,
        "an unchanged chunk must not reach Linear at all"
    );
}

/// Identity decides create-vs-patch here exactly as it does for GitHub.
#[tokio::test]
async fn a_changed_chunk_patches_rather_than_creating_a_second_issue() {
    let server = linear_server().await;
    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    let tracker = linear(&server);
    project_all(&mut e, &tracker, None).await.expect("first");

    e.update_chunk("c1", Some("Renamed".into()), None, None, None, None, None)
        .expect("rename");
    let report = project_all(&mut e, &tracker, None).await.expect("second");

    assert_eq!(
        report.outcomes[0].1,
        ProjectionOutcome::Patched {
            external_id: "ENG-1".into()
        }
    );
    let sent = sent(&server).await;
    let creates = sent
        .iter()
        .filter(|b| {
            b.get("query")
                .and_then(Value::as_str)
                .is_some_and(|q| q.contains("mutation Create"))
        })
        .count();
    assert_eq!(creates, 1, "a rename must never mint a second issue");
}

/// A failed GraphQL mutation arrives with HTTP 200. If the adapter read the
/// status alone it would report success and the projector would record a link
/// for an issue that does not exist. This is the single most important
/// difference between a REST adapter and a GraphQL one.
#[tokio::test]
async fn a_graphql_error_with_http_200_is_a_failure_not_a_success() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "errors": [{
                "message": "Team not found",
                "extensions": { "code": "INVALID_INPUT" }
            }]
        })))
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    let outbox = think_and_ship::tracker::TrackerOutbox::new(None);

    let report = project_all(&mut e, &linear(&server), Some(&outbox))
        .await
        .expect("a rejection is not a run failure");

    assert!(
        matches!(report.outcomes[0].1, ProjectionOutcome::Rejected { .. }),
        "an errors-array response must be a rejection, not a silent success"
    );
    assert!(
        outbox.is_empty(),
        "a contract rejection must never be queued"
    );
    assert!(
        e.tracker_link("c1", "linear").is_none(),
        "no binding may be recorded for a write that did not land"
    );
}

/// Throttling is reported in the GraphQL body, and must queue rather than drop.
#[tokio::test]
async fn a_throttling_error_queues_for_replay() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "errors": [{ "message": "rate limited", "extensions": { "code": "RATELIMITED" } }]
        })))
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    let outbox = think_and_ship::tracker::TrackerOutbox::new(None);

    let report = project_all(&mut e, &linear(&server), Some(&outbox))
        .await
        .expect("throttling is not a run failure");
    assert!(matches!(
        report.outcomes[0].1,
        ProjectionOutcome::Queued { .. }
    ));
    assert_eq!(outbox.queued_chunks("linear"), vec!["c1".to_string()]);
}

/// The per-provider budget separation now has its first REAL consumer. Linear bills
/// complexity points on GraphQL; GitHub bills a different currency on REST.
/// Exhausting one must leave the other untouched — and the two providers must
/// not share either.
#[test]
fn linears_graphql_spend_never_touches_githubs_rest_budget() {
    let mut budget = RateBudget::new();
    budget.configure("linear", Transport::GraphQl, 10, 3_600);
    budget.configure("github", Transport::Rest, 900, 60);

    for _ in 0..10 {
        assert_eq!(budget.spend("linear", Transport::GraphQl, 1), Spend::Ok);
    }
    assert!(matches!(
        budget.spend("linear", Transport::GraphQl, 1),
        Spend::Exhausted { .. }
    ));

    assert_eq!(budget.remaining("linear", Transport::GraphQl), Some(0));
    assert_eq!(
        budget.remaining("github", Transport::Rest),
        Some(900),
        "a provider exhausting its own bucket must not throttle another"
    );
    assert_eq!(budget.spend("github", Transport::Rest, 1), Spend::Ok);
}

/// The two Linear auth schemes are not interchangeable: a personal key is sent
/// raw, an OAuth token needs Bearer. A credential store returning a bare string
/// cannot express that difference — a constraint the credential-custody work
/// has to honour.
#[tokio::test]
async fn the_two_auth_schemes_produce_different_headers() {
    for (scheme, expected) in [
        (AuthScheme::Raw, "lin_api_fake"),
        (AuthScheme::Bearer, "Bearer lin_oauth_fake"),
    ] {
        let server = linear_server().await;
        let token = match scheme {
            AuthScheme::Raw => "lin_api_fake",
            AuthScheme::Bearer => "lin_oauth_fake",
        };
        let tracker = LinearTracker::new("ENG")
            .expect("valid")
            .with_api_base(&server.uri())
            .with_token(token, scheme);

        let mut e = engine();
        add(&mut e, "c1", vec![]);
        opt_in(&mut e, "c1");
        project_all(&mut e, &tracker, None).await.expect("run");

        let auth = server
            .received_requests()
            .await
            .expect("recorded")
            .first()
            .and_then(|r| r.headers.get("authorization").cloned())
            .expect("an Authorization header was sent");
        assert_eq!(auth.to_str().expect("ascii"), expected);
    }
}

/// THE defect the live smoke found, pinned offline so it cannot come back.
///
/// A default Linear team has TWO `started` states — "In Progress" and
/// "In Review" — and the API does NOT return states in workflow order. A real
/// team came back at positions 0, 5, 4, 1002, 3, 2, 1, so the old
/// "first of each type wins" rule filed every in-progress chunk under In Review.
///
/// This fixture reproduces that exact shape: the LATER state arrives FIRST.
#[tokio::test]
async fn the_earliest_started_state_wins_even_when_it_arrives_last() {
    let server = MockServer::start().await;
    let mut r = responder();
    // Positions and ordering copied from a real Linear team's response.
    r.team_states = json!({ "data": { "teams": { "nodes": [{
        "id": "team-uuid-1",
        "key": "ENG",
        "states": { "nodes": [
            { "id": "st-backlog",     "type": "backlog",   "position": 0.0 },
            { "id": "st-in-review",   "type": "started",   "position": 1002.0 },
            { "id": "st-shipped",     "type": "completed", "position": 3.0 },
            { "id": "st-in-progress", "type": "started",   "position": 2.0 },
            { "id": "st-up-next",     "type": "unstarted", "position": 1.0 }
        ]}
    }]}}});
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(r)
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    e.set_status("c1", ChunkStatus::InProgress).expect("start");

    project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");

    let sent = sent(&server).await;
    let create = operation(&sent, "mutation Create").expect("create");
    assert_eq!(
        create["variables"]["input"]["stateId"], "st-in-progress",
        "an in-progress chunk must land in the EARLIEST started state by position; \
         picking whichever the API happened to return first put real work in In Review"
    );
}

/// A state carrying no position must not win by default. An absent position is
/// missing information, not evidence of being the earliest column.
#[tokio::test]
async fn a_state_with_no_position_does_not_outrank_one_that_has_it() {
    let server = MockServer::start().await;
    let mut r = responder();
    r.team_states = json!({ "data": { "teams": { "nodes": [{
        "id": "team-uuid-1",
        "key": "ENG",
        "states": { "nodes": [
            { "id": "st-no-position", "type": "started" },
            { "id": "st-in-progress", "type": "started",   "position": 2.0 },
            { "id": "st-up-next",     "type": "unstarted", "position": 1.0 },
            { "id": "st-shipped",     "type": "completed", "position": 3.0 }
        ]}
    }]}}});
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(r)
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    e.set_status("c1", ChunkStatus::InProgress).expect("start");

    project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");

    let sent = sent(&server).await;
    let create = operation(&sent, "mutation Create").expect("create");
    assert_eq!(
        create["variables"]["input"]["stateId"], "st-in-progress",
        "a positionless state sorts last, so the one with a real position wins"
    );
}

/// THE defect this test exists for. The adapter declared `labels: true`,
/// consumed the `roadmap:<band>` label to derive a priority, and then never
/// sent it — so Linear held no label, `fetch_since` read none back, and the
/// content hash of every projected chunk disagreed with its own readback. The
/// echo fence then saw drift on every projection, and a loop once a version
/// advanced. Measured against the real API before it was fixed.
#[tokio::test]
async fn the_band_label_is_actually_written_not_just_read_for_its_priority() {
    let server = linear_server().await;
    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");

    project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");

    let sent = sent(&server).await;
    let create = operation(&sent, "mutation Create").expect("create");
    assert_eq!(
        create["variables"]["input"]["labelIds"],
        json!(["lbl-critical"]),
        "the band label must reach Linear, not merely be read for its urgency"
    );
    // And it still carries the priority it always did — the label is the
    // carrier, so writing it must not cost the thing it was carrying.
    assert_eq!(create["variables"]["input"]["priority"], 1);
}

/// A label the team already has is REUSED, never duplicated. Getting this wrong
/// fills a workspace with identical labels, one per projection.
#[tokio::test]
async fn an_existing_band_label_is_reused_rather_than_recreated() {
    let server = linear_server().await;
    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");

    project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");

    let sent = sent(&server).await;
    assert!(
        operation(&sent, "mutation MakeLabel").is_none(),
        "roadmap:critical already exists on the team; creating it again is a bug"
    );
}

/// The write-back half of the cache. Two chunks sharing a band must mint the
/// label ONCE — a read-through cache that never writes back is a
/// duplicate-label factory, and the duplicates only appear under load.
#[tokio::test]
async fn a_label_minted_for_one_chunk_is_reused_by_the_next() {
    let server = MockServer::start().await;
    let mut r = responder();
    // A team with NO roadmap labels at all, so the first chunk must create one.
    r.team_states = json!({ "data": { "teams": { "nodes": [{
        "id": "team-uuid-1",
        "key": "ENG",
        "states": { "nodes": [
            { "id": "st-up-next", "type": "unstarted", "position": 1.0 }
        ]},
        "labels": { "nodes": [] }
    }]}}});
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(r)
        .mount(&server)
        .await;

    let mut e = engine();
    for id in ["c1", "c2", "c3"] {
        add(&mut e, id, vec![]);
        opt_in(&mut e, id);
    }

    project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");

    let sent = sent(&server).await;
    let mints = sent
        .iter()
        .filter(|b| {
            b.get("query")
                .and_then(Value::as_str)
                .is_some_and(|q| q.contains("mutation MakeLabel"))
        })
        .count();
    assert_eq!(
        mints, 1,
        "three chunks share one band, so the label is created once; \
         {mints} creations means the cache never wrote back"
    );
}

/// A patch is ADDITIVE and surgical. `labelIds` on update would replace the
/// whole set and delete labels the team added — which the conflict policy says
/// are theirs. We add ours, and remove only stale labels from our OWN
/// namespace that the issue ACTUALLY carries — Linear fails the whole patch on
/// a removal of an absent label, so the patch path reads the issue's labels
/// first.
#[tokio::test]
async fn a_patch_adds_our_label_and_removes_only_our_stale_ones() {
    let server = linear_server().await;
    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    // First pass creates; the link then makes the second pass a patch.
    project_all(&mut e, &linear(&server), None)
        .await
        .expect("create");
    e.update_chunk(
        "c1",
        None,
        None,
        Some("a changed description".into()),
        None,
        None,
        None,
    )
    .expect("dirty the chunk so it patches");
    project_all(&mut e, &linear(&server), None)
        .await
        .expect("patch");

    let sent = sent(&server).await;
    let patch = operation(&sent, "mutation Patch").expect("patch");

    assert_eq!(
        patch["variables"]["input"]["addedLabelIds"],
        json!(["lbl-critical"]),
        "our band label is ADDED, never set as a replacing list"
    );
    let removed = patch["variables"]["input"]["removedLabelIds"]
        .as_array()
        .expect("removedLabelIds must be present")
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(
        removed.contains(&"lbl-stale"),
        "the team's OTHER roadmap:* label is ours to clear, got {removed:?}"
    );
    assert!(
        !removed.contains(&"lbl-theirs"),
        "the team's own 'Bug' label must never be removed by us — got {removed:?}"
    );
    assert!(
        !removed.contains(&"lbl-critical"),
        "the band we are setting must not also be removed"
    );
}

/// The first full-width live push failed on every patch with a 400 "Label not
/// on issue": the schema knew a stale band label, the issue did not carry it,
/// and Linear rejects the removal rather than no-oping it. The case that
/// discriminates is an issue holding NONE of our stale candidates — the wire
/// payload must then remove nothing at all.
#[tokio::test]
async fn a_stale_band_the_issue_does_not_carry_is_not_removed() {
    let server = MockServer::start().await;
    let mut r = responder();
    // The issue carries only the team's own label — no stale band of ours.
    r.issue_labels = json!({ "data": { "issue": { "labels": { "nodes": [
        { "id": "lbl-theirs" }
    ]}}}});
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(r)
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    project_all(&mut e, &linear(&server), None)
        .await
        .expect("create");
    e.update_chunk(
        "c1",
        None,
        None,
        Some("a changed description".into()),
        None,
        None,
        None,
    )
    .expect("dirty the chunk so it patches");
    project_all(&mut e, &linear(&server), None)
        .await
        .expect("patch");

    let sent = sent(&server).await;
    let patch = operation(&sent, "mutation Patch").expect("patch");
    assert_eq!(
        patch["variables"]["input"]["removedLabelIds"],
        json!([]),
        "removing a label the issue does not hold 400s the WHOLE patch at \
         Linear; the candidate set must be intersected with what is there"
    );
}

/// FLIPPED from the pinning test that guarded the old falsehood
/// (tracker-linear-assignee). The adapter used to declare `assignee: true`
/// while never writing one — the band-label shape of defect. The decision went
/// the other way: Linear assigns by user UUID only, the ownership table says
/// assignees are never ours to author, so the claim is now an honest FALSE and
/// the wire must agree with it in both directions.
#[tokio::test]
async fn the_assignee_is_declared_unsupported_and_the_wire_agrees() {
    let server = linear_server().await;
    let caps = linear(&server).capabilities();
    assert!(
        !caps.assignee,
        "Linear cannot write an assignee (UUID-only assignment, no producer); \
         declaring true would revive the false capability claim this test killed"
    );

    // The degradation contract now has teeth: an item carrying an assignee is
    // refused before any API call instead of the field being silently dropped.
    let mut item = think_and_ship::tracker::domain::WorkItem::new("carries an assignee");
    item.assignee = Some("Ada Lovelace".into());
    assert!(
        caps.admits(&item).is_err(),
        "admits() must refuse an assignee-carrying item once the capability is false"
    );

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");

    let sent = sent(&server).await;
    let create = operation(&sent, "mutation Create").expect("create");
    assert!(
        create["variables"]["input"].get("assigneeId").is_none(),
        "no mutation of ours may mention the field — preserve-by-omission is \
         what keeps a human's assignment safe"
    );
}

/// The read side of the same honesty: an assignee the server volunteers is not
/// read back, and the query does not even ask for one.
///
/// Reading a displayName we could never write puts an unconvergeable field
/// into the echo-fence hash — every assignment in Linear would raise a
/// divergence nothing on our side can resolve or represent.
#[tokio::test]
async fn an_assignee_the_server_volunteers_is_not_read_back() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": { "issues": { "nodes": [{
                "id": "uuid-1",
                "identifier": "ENG-1",
                "title": "assigned by a human",
                "description": "",
                "updatedAt": "2026-07-25T09:00:00.000Z",
                "state": { "type": "started" },
                "labels": { "nodes": [] },
                // Volunteered even though the query no longer selects it — the
                // parse must be gone, not merely the field request.
                "assignee": { "displayName": "Ada Lovelace" }
            }]}}
        })))
        .mount(&server)
        .await;

    let items = linear(&server)
        .fetch_since("2026-01-01T00:00:00.000Z")
        .await
        .expect("fetch");
    assert_eq!(items.len(), 1);
    assert!(
        items[0].assignee.is_none(),
        "a volunteered assignee must not be surfaced into the canonical item"
    );

    let sent = sent(&server).await;
    let changed = operation(&sent, "query Changed").expect("changed");
    assert!(
        !changed["query"]
            .as_str()
            .unwrap_or_default()
            .contains("assignee"),
        "the Changed query must not select a field the adapter refuses to hold"
    );
}

// ───────────────────────────── the initiative ─────────────────────────────

/// Shared setup for the roof tests: one grouped chunk, pushed under a named
/// initiative. `mutate` adjusts the responder's workspace before the push.
async fn push_under_roof(mutate: impl FnOnce(&mut ByOperation), status: ChunkStatus) -> Vec<Value> {
    let server = MockServer::start().await;
    let mut r = responder();
    mutate(&mut r);
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(r)
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    e.set_group("c1", Some("tracker".into())).expect("group");
    if status != ChunkStatus::Pending {
        e.set_status("c1", status).expect("status");
    }

    project_all_with_policy(
        &mut e,
        &linear(&server),
        None,
        &Ownership::default(),
        Some("roof"),
    )
    .await
    .expect("run");
    sent(&server).await
}

/// THE MIGRATION CASE — the exact state of the live workspace the day this
/// ships: the initiative and the project both exist, the project's state is
/// unchanged, and the project is NOT yet a member. The join must happen
/// anyway, which is why the membership check sits BEFORE the state-unchanged
/// early return.
#[tokio::test]
async fn an_existing_unlinked_project_is_joined_even_when_its_state_is_unchanged() {
    let sent = push_under_roof(
        |r| {
            r.initiatives = json!({ "data": { "initiatives": { "nodes": [
                { "id": "init-1", "name": "roof", "status": "Planned" }
            ]}}});
            r.team_projects = json!({ "data": { "team": { "projects": { "nodes": [
                { "id": "proj-1", "name": "tracker" }
            ]}}}});
            // Unchanged state, empty membership: the early-return bait.
            r.project_state = json!({ "data": { "project": {
                "state": "planned", "initiatives": { "nodes": [] }
            }}});
        },
        ChunkStatus::Pending,
    )
    .await;

    let link = operation(&sent, "LinkProjectToInitiative").expect(
        "an unlinked project with an unchanged state must STILL be joined \
         to the initiative — the early return must not eat the link",
    );
    assert_eq!(link["variables"]["input"]["initiativeId"], "init-1");
    assert_eq!(link["variables"]["input"]["projectId"], "proj-1");
    assert_eq!(
        count(&sent, "mutation CreateInitiative"),
        0,
        "the existing initiative is reused, never duplicated"
    );
    assert_eq!(
        count(&sent, "mutation SetInitiativeStatus"),
        0,
        "matching status must cost no write"
    );
    assert_eq!(
        count(&sent, "mutation SetProjectState"),
        0,
        "precondition: the state really was unchanged"
    );
}

/// The idempotence half: a member project must cost ZERO join mutations on
/// re-push, or the cadence re-links the workspace forever.
#[tokio::test]
async fn an_already_linked_project_costs_no_join_mutation() {
    let sent = push_under_roof(
        |r| {
            r.initiatives = json!({ "data": { "initiatives": { "nodes": [
                { "id": "init-1", "name": "roof", "status": "Planned" }
            ]}}});
            r.team_projects = json!({ "data": { "team": { "projects": { "nodes": [
                { "id": "proj-1", "name": "tracker" }
            ]}}}});
            r.project_state = json!({ "data": { "project": {
                "state": "planned", "initiatives": { "nodes": [ { "id": "init-1" } ]}
            }}});
        },
        ChunkStatus::Pending,
    )
    .await;

    assert_eq!(
        count(&sent, "LinkProjectToInitiative"),
        0,
        "membership is read before writing; an existing member is a no-op"
    );
}

/// The third vocabulary, on the PATCH path: our Active is Linear's `Active`
/// (capitalized enum member), not the project's lowercase `started`.
#[tokio::test]
async fn an_existing_initiative_is_repatched_only_in_its_own_vocabulary() {
    let sent = push_under_roof(
        |r| {
            r.initiatives = json!({ "data": { "initiatives": { "nodes": [
                { "id": "init-1", "name": "roof", "status": "Planned" }
            ]}}});
        },
        ChunkStatus::InProgress,
    )
    .await;

    assert_eq!(count(&sent, "mutation CreateInitiative"), 0);
    let patch = operation(&sent, "mutation SetInitiativeStatus")
        .expect("a moving roadmap must move a Planned initiative to Active");
    assert_eq!(
        patch["variables"]["input"]["status"], "Active",
        "InitiativeStatus is a closed CAPITALIZED enum — sending the project's \
         'started' here is a 400"
    );
}

/// A workspace with no initiative gets one, in the initiative vocabulary, and
/// a project created in the same push joins it at birth.
#[tokio::test]
async fn a_missing_initiative_is_created_and_a_fresh_project_joins_at_birth() {
    let sent = push_under_roof(|_| {}, ChunkStatus::InProgress).await;

    let create = operation(&sent, "mutation CreateInitiative").expect("created");
    assert_eq!(create["variables"]["input"]["name"], "roof");
    assert_eq!(
        create["variables"]["input"]["status"], "Active",
        "born already moving, in the initiative vocabulary"
    );
    let link = operation(&sent, "LinkProjectToInitiative")
        .expect("the fresh project must join the fresh initiative");
    assert_eq!(link["variables"]["input"]["initiativeId"], "init-new");
    assert_eq!(link["variables"]["input"]["projectId"], "proj-tracker");
    assert_eq!(
        count(&sent, "query ProjectState"),
        0,
        "a project born this push needs no membership read — it belongs to nothing"
    );
}

// ───────────────────── container ownership ─────────────────────

use think_and_ship::roadmap::domain::ContainerKind;

/// THE RENAME CASE. The human renamed our project; resolve-by-name would miss
/// and mint a duplicate under the old name. With the uuid remembered in the
/// engine, the name never enters resolution at all: no TeamProjects listing,
/// no CreateProject, and the issue still files into the ORIGINAL project.
#[tokio::test]
async fn a_renamed_project_still_resolves_and_mints_no_duplicate() {
    let server = MockServer::start().await;
    let mut r = responder();
    // The project answers by uuid with a name a human chose. The workspace
    // listing is EMPTY — if anything resolved by name, it would create.
    r.project_state = json!({ "data": { "project": {
        "name": "Tracker integration", "state": "planned",
        "initiatives": { "nodes": [] }
    }}});
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(r)
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    e.set_group("c1", Some("tracker".into())).expect("group");
    e.record_container_link(ContainerKind::Group, "tracker", "linear", "proj-1", true)
        .expect("remember");

    project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");

    let sent = sent(&server).await;
    assert_eq!(
        count(&sent, "mutation CreateProject"),
        0,
        "a renamed project must NOT be re-minted under the old name"
    );
    assert_eq!(
        count(&sent, "query TeamProjects"),
        0,
        "the remembered uuid resolves without ever listing by name"
    );
    let create = operation(&sent, "IssueCreateInput").expect("issue created");
    assert_eq!(
        create["variables"]["input"]["projectId"], "proj-1",
        "the issue files into the ORIGINAL project, not a duplicate"
    );
    assert_eq!(
        count(&sent, "mutation SetProjectState"),
        0,
        "and the rename itself is left standing — no write of any kind"
    );
}

/// The ownership rule on state: `paused` records a human's intent to stop,
/// which the plan does not know. Re-deriving over it on every push is the
/// silent overwrite this test exists to kill.
#[tokio::test]
async fn a_paused_project_is_not_patched_back_to_started() {
    let server = MockServer::start().await;
    let mut r = responder();
    r.project_state = json!({ "data": { "project": {
        "name": "tracker", "state": "paused", "initiatives": { "nodes": [] }
    }}});
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(r)
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    e.set_group("c1", Some("tracker".into())).expect("group");
    e.set_status("c1", ChunkStatus::InProgress).expect("start");
    e.record_container_link(ContainerKind::Group, "tracker", "linear", "proj-1", true)
        .expect("remember");

    project_all(&mut e, &linear(&server), None)
        .await
        .expect("run");

    let sent = sent(&server).await;
    assert_eq!(
        count(&sent, "mutation SetProjectState"),
        0,
        "an in-progress group derives 'started', but 'paused' is a human's \
         judgement and must be left standing"
    );
    assert!(
        operation(&sent, "IssueCreateInput").is_some(),
        "the guard costs nothing else — the issue still lands"
    );
}

/// The same rule in the initiative's OWN vocabulary: `Canceled` is never
/// authored and never patched over.
#[tokio::test]
async fn a_canceled_initiative_is_left_standing() {
    let server = MockServer::start().await;
    let mut r = responder();
    r.initiatives = json!({ "data": { "initiatives": { "nodes": [
        { "id": "init-1", "name": "roof", "status": "Canceled" }
    ]}}});
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(r)
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    e.set_status("c1", ChunkStatus::InProgress).expect("start");

    project_all_with_policy(
        &mut e,
        &linear(&server),
        None,
        &Ownership::default(),
        Some("roof"),
    )
    .await
    .expect("run");

    let sent = sent(&server).await;
    assert_eq!(
        count(&sent, "mutation SetInitiativeStatus"),
        0,
        "a moving roadmap derives Active, but a human's Canceled stays"
    );
    assert_eq!(count(&sent, "mutation CreateInitiative"), 0);
}

/// A remembered initiative resolves by id — the name query is never sent, so
/// a renamed roof can neither be duplicated nor bound to a stranger that
/// happens to share the old name.
#[tokio::test]
async fn a_remembered_initiative_resolves_by_id_not_name() {
    let server = MockServer::start().await;
    let mut r = responder();
    r.initiative_by_id = json!({ "data": { "initiative": {
        "id": "init-9", "name": "Roadmap 2026", "status": "Active"
    }}});
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(r)
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    e.set_group("c1", Some("tracker".into())).expect("group");
    e.set_status("c1", ChunkStatus::InProgress).expect("start");
    e.record_container_link(ContainerKind::Initiative, "roof", "linear", "init-9", true)
        .expect("remember");

    project_all_with_policy(
        &mut e,
        &linear(&server),
        None,
        &Ownership::default(),
        Some("roof"),
    )
    .await
    .expect("run");

    let sent = sent(&server).await;
    assert_eq!(
        count(&sent, "query Initiatives("),
        0,
        "the by-name lookup must never run once an id is remembered"
    );
    assert_eq!(count(&sent, "mutation CreateInitiative"), 0);
    assert_eq!(
        count(&sent, "mutation SetInitiativeStatus"),
        0,
        "Active == Active — the rename alone is not a reason to write"
    );
    let link = operation(&sent, "LinkProjectToInitiative").expect("project joins the roof");
    assert_eq!(link["variables"]["input"]["initiativeId"], "init-9");
}

/// The group-move criterion at the wire: regrouping a chunk patches its issue
/// with the NEW project's id, and the old project is left standing untouched.
#[tokio::test]
async fn a_regrouped_issue_is_patched_into_the_new_project() {
    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    e.set_group("c1", Some("a".into())).expect("group a");

    // First push on its own server: project 'a' is minted and the issue lands.
    let first = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(responder())
        .mount(&first)
        .await;
    project_all(&mut e, &linear(&first), None)
        .await
        .expect("first push");
    let born = sent(&first).await;
    let create = operation(&born, "IssueCreateInput").expect("created");
    assert_eq!(create["variables"]["input"]["projectId"], "proj-a");

    // The move, observed on a second server so the assertions see only the
    // second push's wire traffic.
    e.set_group("c1", Some("b".into())).expect("group b");
    let second = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/graphql"))
        .respond_with(responder())
        .mount(&second)
        .await;
    project_all(&mut e, &linear(&second), None)
        .await
        .expect("second push");

    let moved = sent(&second).await;
    let patch = operation(&moved, "IssueUpdateInput").expect("patched, not re-created");
    assert_eq!(
        patch["variables"]["input"]["projectId"], "proj-b",
        "a regrouped chunk's issue must MOVE into the new project"
    );
    assert_eq!(
        count(&moved, "mutation CreateProject"),
        1,
        "only 'b' is minted — the abandoned 'a' is not re-ensured"
    );
    assert!(
        !moved.iter().any(|b| b.to_string().contains("proj-a")),
        "the emptied project is left standing: no write touches it"
    );
}
