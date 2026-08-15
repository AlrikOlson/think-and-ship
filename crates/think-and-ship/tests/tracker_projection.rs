//! Projection: a chunk becomes a GitHub issue, idempotently, and nothing
//! leaves without explicit consent.
//!
//! Every test here drives the REAL adapter — the same request shapes, headers,
//! payloads and status handling that would hit github.com — against an
//! in-process mock server. There is no credential anywhere in this file and no
//! way to supply one, because credential custody is its own concern; what is being
//! proven is the projector and the adapter's wire behaviour, not that a token
//! works.
//!
//! `server.received_requests()` is the load-bearing assertion in several of
//! these: it is the difference between "the result happened to be the same" and
//! "no call was made at all".

use think_and_ship::roadmap::domain::ChunkStatus;
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::budget::{RateBudget, Spend, Transport};
use think_and_ship::tracker::github::GithubTracker;
use think_and_ship::tracker::outbox::TrackerOutbox;
use think_and_ship::tracker::project::{ProjectionOutcome, project_all};
use wiremock::matchers::{body_json_string, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

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
    e.set_tracker_opt_in(id, "github", true).expect("opt in");
}

/// A created-issue response: `number` for paths, `id` for dependencies. They are
/// deliberately different integers here, because in reality they are.
fn issue(number: u64, database_id: u64) -> serde_json::Value {
    serde_json::json!({
        "number": number,
        "id": database_id,
        "state": "open",
        "updated_at": "2026-07-25T09:00:00Z",
    })
}

fn github(server: &MockServer) -> GithubTracker {
    GithubTracker::new("owner/repo")
        .expect("valid target")
        .with_api_base(&server.uri())
}

/// THE promise of the consent gate: upgrading cannot fill anyone's tracker. With no
/// opt-in, the adapter is never asked to do anything at all.
#[tokio::test]
async fn a_default_configuration_emits_nothing() {
    let server = MockServer::start().await;
    let mut e = engine();
    add(&mut e, "c1", vec![]);
    // Deliberately NO opt_in().

    let report = project_all(&mut e, &github(&server), None)
        .await
        .expect("run");

    assert!(report.outcomes.is_empty());
    assert!(
        server
            .received_requests()
            .await
            .expect("recorded")
            .is_empty(),
        "not one request may leave without explicit consent"
    );
}

/// A chunk becomes an issue carrying title, body, acceptance as a checklist and
/// the machine-readable provenance footer.
#[tokio::test]
async fn a_chunk_projects_to_an_issue_with_its_provenance() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue(12, 900_001)))
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    e.link_chunk("c1", "think:7").expect("think ref");
    e.link_chunk("c1", "task:projector").expect("ship ref");
    opt_in(&mut e, "c1");

    let report = project_all(&mut e, &github(&server), None)
        .await
        .expect("run");
    assert_eq!(
        report.outcomes[0].1,
        ProjectionOutcome::Created {
            external_id: "owner/repo#12".into()
        }
    );

    let reqs = server.received_requests().await.expect("recorded");
    assert_eq!(reqs.len(), 1);
    let sent: serde_json::Value = reqs[0].body_json().expect("json body");
    assert_eq!(sent["title"], "Chunk c1");
    let body = sent["body"].as_str().expect("body");
    assert!(body.contains("why c1 exists"));
    assert!(body.contains("- [ ] c1 works"), "acceptance as a checklist");

    // The footer a machine can read back — the thing a field-sync tool cannot
    // emit, because it never saw the reasoning.
    let footer = body
        .split("<!-- think-and-ship:")
        .nth(1)
        .expect("footer present");
    let json_text = footer.trim().trim_end_matches("-->").trim();
    let parsed: serde_json::Value = serde_json::from_str(json_text).expect("footer is JSON");
    assert_eq!(parsed["chunk"], "c1");
    assert_eq!(parsed["think"][0], "think:7");
    assert_eq!(parsed["ship"][0], "task:projector");

    // The binding is recorded, so the next run patches instead of creating.
    assert_eq!(
        e.tracker_link("c1", "github").expect("link").external_id,
        "owner/repo#12"
    );
}

/// Re-running an unchanged projection must make ZERO outbound calls. Asserted on
/// the server's request log, not on the resulting value — "same result" and "no
/// request" are very different properties, and only the second one keeps a
/// restart from looking like activity to everyone watching the repo.
#[tokio::test]
async fn re_running_an_unchanged_chunk_makes_no_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue(12, 900_001)))
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");

    let tracker = github(&server);
    project_all(&mut e, &tracker, None).await.expect("first");
    assert_eq!(server.received_requests().await.expect("recorded").len(), 1);

    let second = project_all(&mut e, &tracker, None).await.expect("second");
    assert!(matches!(
        second.outcomes[0].1,
        ProjectionOutcome::Skipped { .. }
    ));
    assert_eq!(
        server.received_requests().await.expect("recorded").len(),
        1,
        "an unchanged chunk must not reach the provider at all"
    );
}

/// Identity, not title, decides create-vs-patch — so renaming a ticket upstream
/// (or renaming the chunk locally) can never mint a duplicate.
#[tokio::test]
async fn a_changed_chunk_patches_the_same_issue() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue(12, 900_001)))
        .mount(&server)
        .await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/issues/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue(12, 900_001)))
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    let tracker = github(&server);
    project_all(&mut e, &tracker, None).await.expect("first");

    e.update_chunk("c1", Some("Renamed".into()), None, None, None, None, None)
        .expect("rename");
    let report = project_all(&mut e, &tracker, None).await.expect("second");

    assert_eq!(
        report.outcomes[0].1,
        ProjectionOutcome::Patched {
            external_id: "owner/repo#12".into()
        }
    );
    let posts = server
        .received_requests()
        .await
        .expect("recorded")
        .into_iter()
        .filter(|r: &Request| r.method == wiremock::http::Method::POST)
        .count();
    assert_eq!(posts, 1, "a rename must never create a second issue");
}

/// Deps become real blocking links, and only after both issues exist — the
/// ordering constraint the 4th port verb was added for. The POST body carries
/// the DATABASE id, not the issue number: they are different integers, and
/// sending the wrong one is a 422 that looks like a permissions problem.
#[tokio::test]
async fn deps_become_native_blocking_links_with_the_database_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue(1, 900_001)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue(2, 900_002)))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/repos/owner/repo/issues/2/dependencies/blocked_by"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!([])))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues/2/dependencies/blocked_by"))
        // The blocker's DATABASE id (900_001), never its number (1).
        .and(body_json_string(r#"{"issue_id":900001}"#))
        .respond_with(ResponseTemplate::new(201).set_body_json(serde_json::json!({})))
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "base", vec![]);
    add(&mut e, "c1", vec!["base".into()]);
    opt_in(&mut e, "base");
    opt_in(&mut e, "c1");

    let report = project_all(&mut e, &github(&server), None)
        .await
        .expect("run");
    assert_eq!(report.relations_written, vec!["c1".to_string()]);
    assert!(report.relations_degraded.is_empty());
}

/// The outbox contract, first half: a 5xx queues for replay, and the run carries
/// on to the next chunk instead of abandoning it.
#[tokio::test]
async fn a_5xx_queues_for_replay() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(503).set_body_string("unavailable"))
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    let outbox = TrackerOutbox::new(None);

    let report = project_all(&mut e, &github(&server), Some(&outbox))
        .await
        .expect("a queued failure is not a run failure");

    assert!(matches!(
        report.outcomes[0].1,
        ProjectionOutcome::Queued { .. }
    ));
    assert_eq!(outbox.queued_chunks("github"), vec!["c1".to_string()]);
    assert!(
        e.tracker_link("c1", "github").is_none(),
        "a queued write has not landed, so no binding may be recorded"
    );
}

/// The outbox contract, second half: a 4xx is a contract rejection that would
/// fail identically forever, so it is logged loudly and NEVER queued.
#[tokio::test]
async fn a_4xx_is_logged_loudly_and_never_queued() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(422).set_body_string("Validation Failed"))
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    let outbox = TrackerOutbox::new(None);

    let report = project_all(&mut e, &github(&server), Some(&outbox))
        .await
        .expect("a rejection is not a run failure");

    assert!(matches!(
        report.outcomes[0].1,
        ProjectionOutcome::Rejected { .. }
    ));
    assert!(
        outbox.is_empty(),
        "queueing a 4xx builds a backlog that can never drain"
    );
}

/// A queued projection replays against the real adapter once the provider
/// recovers, and leaves the queue empty.
#[tokio::test]
async fn a_queued_projection_replays_when_the_provider_recovers() {
    let down = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&down)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    let outbox = TrackerOutbox::new(None);
    project_all(&mut e, &github(&down), Some(&outbox))
        .await
        .expect("queued");
    assert_eq!(outbox.len(), 1);

    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue(12, 900_001)))
        .mount(&up)
        .await;

    assert_eq!(outbox.flush(&github(&up)).await, 1);
    assert!(outbox.is_empty());
    assert_eq!(up.received_requests().await.expect("recorded").len(), 1);
}

/// GitHub signals a secondary rate limit with 403/429 plus `retry-after`. That
/// must arrive as `RateLimited` — which `retryable()` classes as replayable — so
/// an exhausted budget queues rather than being mistaken for a contract error
/// and dropped.
#[tokio::test]
async fn a_secondary_rate_limit_queues_rather_than_being_dropped() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(
            ResponseTemplate::new(403)
                .insert_header("retry-after", "60")
                .set_body_string("You have exceeded a secondary rate limit"),
        )
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    let outbox = TrackerOutbox::new(None);

    let report = project_all(&mut e, &github(&server), Some(&outbox))
        .await
        .expect("rate limiting is not a run failure");
    assert!(matches!(
        report.outcomes[0].1,
        ProjectionOutcome::Queued { .. }
    ));
    assert_eq!(outbox.len(), 1);
}

/// The budget is keyed per provider-transport. GitHub bills REST and GraphQL
/// from separate buckets (900 pts/min and 2,000 pts/min), so exhausting one must
/// leave the other completely untouched — a shared limiter would throttle work
/// that had budget while hiding that the other bucket was empty.
#[test]
fn rest_and_graphql_budgets_are_not_shared() {
    let mut budget = RateBudget::github();
    for _ in 0..900 {
        assert_eq!(budget.spend("github", Transport::Rest, 1), Spend::Ok);
    }
    assert!(matches!(
        budget.spend("github", Transport::Rest, 1),
        Spend::Exhausted { .. }
    ));

    assert_eq!(budget.remaining("github", Transport::Rest), Some(0));
    assert_eq!(
        budget.remaining("github", Transport::GraphQl),
        Some(2_000),
        "REST spending must not consume the GraphQL bucket"
    );
    assert_eq!(budget.spend("github", Transport::GraphQl, 1), Spend::Ok);
}

/// The cross-machine story, end to end — and the answer to the discovered
/// problem that tracker links were local-only.
///
/// Machine A projects a chunk and records the binding. That binding rides the
/// chunk's cloud envelope as a sidecar. Machine B reconciles, inherits the
/// binding, and therefore PATCHES the existing issue instead of creating a
/// second one. Without this, two machines each mint their own twin and every
/// later write is ambiguous about which one it meant.
#[tokio::test]
async fn a_second_machine_inherits_the_binding_instead_of_minting_a_twin() {
    let github_a = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue(12, 900_001)))
        .mount(&github_a)
        .await;

    // --- machine A: project, and bind.
    let mut a = engine();
    add(&mut a, "c1", vec![]);
    opt_in(&mut a, "c1");
    project_all(&mut a, &github(&github_a), None)
        .await
        .expect("machine A projects");
    assert_eq!(
        a.tracker_link("c1", "github").expect("bound").external_id,
        "owner/repo#12"
    );

    // --- the wire: exactly the envelope A's write-through push would send.
    let chunk = a.roadmap().chunks[0].clone();
    let links: Vec<_> = a.roadmap().links.clone();
    let opt_ins: Vec<_> = a.roadmap().tracker_opt_ins.clone();
    let envelope = serde_json::to_value(think_and_ship::cloud::build::from_chunk(
        "proj", &chunk, &links, &opt_ins,
    ))
    .expect("envelope serializes");

    let cloud = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/records"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(serde_json::json!({ "records": [envelope] })),
        )
        .mount(&cloud)
        .await;

    // --- machine B: knows nothing yet.
    let mut b = engine();
    assert!(b.tracker_link("c1", "github").is_none());

    let client = think_and_ship::cloud::client::CloudClient::new(cloud.uri(), "tok");
    think_and_ship::cloud::pull::reconcile_roadmap(&client, &mut b)
        .await
        .expect("machine B reconciles");

    assert_eq!(
        b.tracker_link("c1", "github")
            .expect("inherited")
            .external_id,
        "owner/repo#12",
        "the binding must cross machines, or B mints a twin"
    );
    assert!(
        b.tracker_opt_in("c1", "github").is_some_and(|o| o.enabled),
        "the opt-in must cross too, or B does not know the item is in scope"
    );

    // --- B re-projecting the UNCHANGED chunk writes nothing at all: the content
    // hash crossed too, so B knows A already wrote exactly this. Two machines
    // running a push must not produce two writes of the same content.
    let quiet = MockServer::start().await;
    let report = project_all(&mut b, &github(&quiet), None)
        .await
        .expect("machine B projects");
    assert!(matches!(
        report.outcomes[0].1,
        ProjectionOutcome::Skipped { .. }
    ));
    assert!(
        quiet
            .received_requests()
            .await
            .expect("recorded")
            .is_empty(),
        "the content fence must cross machines too, or every peer re-writes every item"
    );

    // --- and when B genuinely changes the chunk, it PATCHES A's issue rather
    // than creating a second one. No POST mock is mounted, so a create 404s.
    b.update_chunk(
        "c1",
        Some("Renamed on B".into()),
        None,
        None,
        None,
        None,
        None,
    )
    .expect("rename");
    let github_b = MockServer::start().await;
    Mock::given(method("PATCH"))
        .and(path("/repos/owner/repo/issues/12"))
        .respond_with(ResponseTemplate::new(200).set_body_json(issue(12, 900_001)))
        .mount(&github_b)
        .await;
    let report = project_all(&mut b, &github(&github_b), None)
        .await
        .expect("machine B projects");
    assert_eq!(
        report.outcomes[0].1,
        ProjectionOutcome::Patched {
            external_id: "owner/repo#12".into()
        }
    );
}

/// The queue must survive the process, or "queued for replay" is a promise only
/// kept until someone closes their laptop.
#[tokio::test]
async fn a_queued_projection_survives_a_restart() {
    let down = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(503))
        .mount(&down)
        .await;

    let dir = tempfile::TempDir::new().expect("tempdir");
    let queue_path = TrackerOutbox::path_for(dir.path(), "proj");

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");
    {
        let outbox = TrackerOutbox::new(Some(queue_path.clone()));
        project_all(&mut e, &github(&down), Some(&outbox))
            .await
            .expect("queued");
        assert_eq!(outbox.len(), 1);
    } // dropped — as a process exit would drop it.

    // A fresh handle over the same path is what the next run gets.
    let reopened = TrackerOutbox::new(Some(queue_path));
    assert_eq!(
        reopened.queued_chunks("github"),
        vec!["c1".to_string()],
        "the queue must outlive the process that filled it"
    );

    let up = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue(12, 900_001)))
        .mount(&up)
        .await;
    assert_eq!(reopened.flush(&github(&up)).await, 1);
    assert!(reopened.is_empty());
}

/// The second defect the live smoke found, and it was hidden behind the first.
///
/// `provenance_footer` ends with a newline, so every projected body did too.
/// Linear strips trailing whitespace from a description on save, so we wrote
/// one byte we could never read back — and `content_hash` differed on every
/// projected chunk, which is enough to make the echo fence misjudge every
/// inbound event. It only became visible after the label loss was fixed,
/// because that defect was masking it.
///
/// Normalized at the authoring boundary rather than in each adapter: a
/// canonical body has no trailing whitespace, which every provider agrees with.
#[tokio::test]
async fn a_projected_body_never_ends_in_whitespace() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/repos/owner/repo/issues"))
        .respond_with(ResponseTemplate::new(201).set_body_json(issue(1, 1001)))
        .mount(&server)
        .await;

    let mut e = engine();
    add(&mut e, "c1", vec![]);
    opt_in(&mut e, "c1");

    project_all(&mut e, &github(&server), None)
        .await
        .expect("run");

    let sent = server.received_requests().await.expect("recorded");
    let body: serde_json::Value =
        serde_json::from_slice(&sent[0].body).expect("the create body is json");
    let projected = body["body"].as_str().expect("a body was sent");

    assert_eq!(
        projected,
        projected.trim_end(),
        "the projected body ends in whitespace a provider will strip, so its \
         hash can never match its own readback"
    );
    // The footer must still be the LAST thing, so the inbound parse can find it.
    assert!(
        projected.ends_with("-->"),
        "the provenance footer must close the body, got tail: {:?}",
        &projected[projected.len().saturating_sub(30)..]
    );
}

/// The projector's OWN body, rendered as ADF and read back.
///
/// Deliberately not a hand-written body. The Linear live smoke already
/// recorded this lesson the expensive way — a hand-written probe approximated
/// what the projector emits, differed by a blank line, and looked like a
/// fidelity defect that was never there. So this drives `to_work_item` and
/// feeds its real output to the renderer.
///
/// What it proves is the round-trip criterion on the whole authored
/// shape: the description, the acceptance checklist, and — the load-bearing
/// one — the provenance footer arriving back byte for byte. ADF was chosen
/// over wiki markup precisely because those bytes have to survive,
/// so a footer that came back reflowed would invalidate the decision itself.
#[test]
fn the_projectors_own_body_round_trips_through_adf() {
    use think_and_ship::tracker::adf::{plain_text, render_body};
    use think_and_ship::tracker::domain::TrackerCapabilities;
    use think_and_ship::tracker::project::to_work_item;

    let mut e = engine();
    add(&mut e, "c1", vec!["dep-a".into()]);

    // Jira has no native blocking links in our lane yet, so deps render as
    // prose — the harder shape, with a second heading and an italic line.
    let caps = TrackerCapabilities {
        blocking_links: false,
        labels: true,
        assignee: true,
        max_body_len: None,
        required_fields: Vec::new(),
    };
    let chunk = e
        .roadmap()
        .chunks
        .iter()
        .find(|c| c.id == "c1")
        .expect("chunk c1")
        .clone();
    let item = to_work_item(&e, &chunk, "jira", &caps);

    let doc = render_body(&item.body);
    assert_eq!(doc["version"], 1, "ADF documents are version 1");
    assert_eq!(doc["type"], "doc");

    let text = plain_text(&doc);
    assert!(
        text.contains("why c1 exists"),
        "the description was lost: {text}"
    );
    assert!(text.contains("Acceptance"), "the heading was lost: {text}");
    assert!(
        text.contains("[ ] c1 works"),
        "the acceptance checklist was lost: {text}"
    );
    assert!(
        text.contains("Blocked by"),
        "the prose-deps fallback was lost: {text}"
    );

    // The footer, byte for byte. Anything less and the inbound reconcile
    // cannot tell our own writing from a human's.
    let footer_start = item
        .body
        .find("<!-- think-and-ship:")
        .expect("the projector emits a footer");
    let footer = item.body[footer_start..].trim();
    assert!(
        text.contains(footer),
        "the provenance footer did not survive the ADF round trip.\nsent: {footer}\nback: {text}"
    );

    // And it survives BECAUSE it is a codeBlock, not because it happened to
    // fall out of a paragraph unchanged.
    let blocks = doc["content"].as_array().expect("content is an array");
    let footer_node = blocks
        .iter()
        .find(|n| n["type"] == "codeBlock")
        .expect("the footer is carried in a codeBlock, whose text is verbatim");
    assert_eq!(footer_node["content"][0]["text"], footer);
}
