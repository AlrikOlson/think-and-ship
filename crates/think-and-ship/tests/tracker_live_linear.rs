//! The live smoke — the only test in this repo that talks to a real API.
//!
//! Every other tracker test drives a mock, and a mock returns what the test
//! author imagined. A large body of adapter, credential and projector code
//! shipped against imagined responses before anything here ran, and the first
//! real call found a defect on a DEFAULT Linear team. That is the argument for
//! this file.
//!
//! # Running it
//!
//! Ignored by default, so `cargo test` stays offline and deterministic:
//!
//! ```text
//! LINEAR_API_KEY=lin_api_... LINEAR_TEAM=ACME \
//!   cargo test --test tracker_live_linear -- --ignored --nocapture
//! ```
//!
//! Without both variables every test here skips loudly rather than failing —
//! a missing key is a missing key, not a broken adapter.
//!
//! # What it is allowed to do
//!
//! Create issues in the named team and delete them again — and, in exactly one
//! test below, CREATE A TEAM. That widening is
//! deliberate and carries a warning worth reading twice: a created team is not
//! as cleanable as a created issue. The team test uses a throwaway generated
//! key, attempts `teamDelete` afterwards, and PRINTS the leftover loudly when
//! deletion is not permitted — but a key without admin rights can end the run
//! with a real team in the workspace that a human must delete by hand. Point
//! this file at a throwaway workspace, never at one carrying real work.

use think_and_ship::tracker::credential::{AuthScheme, Credential, Secret};
use think_and_ship::tracker::domain::{WorkItem, WorkItemState};
use think_and_ship::tracker::linear::LinearTracker;
use think_and_ship::tracker::port::TrackerPort;

/// The key and team, or `None` when this run is meant to stay offline.
fn live() -> Option<(String, String)> {
    let key = std::env::var("LINEAR_API_KEY").ok()?;
    let team = std::env::var("LINEAR_TEAM").ok()?;
    if key.trim().is_empty() || team.trim().is_empty() {
        return None;
    }
    Some((key, team))
}

fn tracker(key: &str, team: &str) -> LinearTracker {
    // A PERSONAL key goes in a bare Authorization header. Linear rejects the
    // Bearer prefix explicitly, which is why the scheme travels with the secret
    // rather than being guessed from the token's shape.
    let credential = Credential::new(Secret::new(key.to_string()), AuthScheme::Raw);
    LinearTracker::new(team)
        .expect("valid team key")
        .with_credential(&credential)
}

/// The whole point: our own adapter, our own credential type, a real workspace.
/// Create an issue, read it back through `fetch_since`, and compare the content
/// hash of what we sent against the hash of what came back.
///
/// That comparison is not incidental — it IS the assumption the echo fence rests
/// on. If these hashes differ, every inbound event looks like a remote change and
/// the sync loops. `tracker-roundtrip-fidelity` exists because nothing had ever
/// checked it against a real provider.
#[tokio::test]
#[ignore = "talks to the real Linear API; needs LINEAR_API_KEY + LINEAR_TEAM"]
async fn a_real_round_trip_preserves_the_content_hash() {
    let Some((key, team)) = live() else {
        eprintln!("SKIPPED: set LINEAR_API_KEY and LINEAR_TEAM to run the live smoke");
        return;
    };
    let tracker = tracker(&key, &team);

    let mut sent = WorkItem::new("live smoke — round trip fidelity");
    sent.body = "Created by the think-and-ship live smoke test. Safe to delete.".into();
    sent.state = WorkItemState::InProgress;

    let created = tracker
        .upsert_item(&sent)
        .await
        .expect("create must succeed");
    eprintln!(
        "created {} (version {:?})",
        created.external_id, created.version
    );
    assert!(created.created, "a fresh item must report created");

    // Read it back the way the inbound path would.
    let fetched = tracker
        .fetch_since("2020-01-01T00:00:00Z")
        .await
        .expect("fetch_since must succeed");
    let back = fetched
        .iter()
        .find(|i| i.external_id.as_deref() == Some(created.external_id.as_str()))
        .unwrap_or_else(|| panic!("just-created {} not in fetch_since", created.external_id));

    eprintln!("sent  hash: {}", sent.content_hash());
    eprintln!("back  hash: {}", back.content_hash());
    eprintln!(
        "sent  state: {:?}   back state: {:?}",
        sent.state, back.state
    );
    eprintln!("sent  title: {:?}", sent.title);
    eprintln!("back  title: {:?}", back.title);
    eprintln!(
        "sent  body len {}   back body len {}",
        sent.body.len(),
        back.body.len()
    );
    eprintln!(
        "sent  labels {:?}   back labels {:?}",
        sent.labels, back.labels
    );
    eprintln!(
        "sent  assignee {:?}   back assignee {:?}",
        sent.assignee, back.assignee
    );

    assert_eq!(
        sent.content_hash(),
        back.content_hash(),
        "ROUND-TRIP LOSS: the adapter's inbound parse does not invert its outbound \
         build. Every echo would read as a remote change. Fix at the adapter \
         boundary, never by weakening content_hash"
    );
}

/// The defect the first real call exposed, pinned so it cannot come back.
///
/// `team_schema` builds a type→state map with "first of each type wins",
/// justified by a comment claiming Linear returns states in workflow order. It
/// does not. A default team has TWO `started` states — In Progress (position 2)
/// and In Review (position 1002) — and the API returned In Review first, so an
/// in-progress chunk was filed under In Review.
///
/// This asserts the state we actually land in is the EARLIEST started state by
/// position, which is the only stable reading of "the in-progress column".
#[tokio::test]
#[ignore = "talks to the real Linear API; needs LINEAR_API_KEY + LINEAR_TEAM"]
async fn in_progress_lands_in_the_earliest_started_state_not_whichever_came_first() {
    let Some((key, team)) = live() else {
        eprintln!("SKIPPED: set LINEAR_API_KEY and LINEAR_TEAM to run the live smoke");
        return;
    };
    let tracker = tracker(&key, &team);

    let mut item = WorkItem::new("live smoke — state mapping");
    item.body = "Created by the think-and-ship live smoke test. Safe to delete.".into();
    item.state = WorkItemState::InProgress;

    let created = tracker.upsert_item(&item).await.expect("create");

    // Ask Linear what state it actually landed in, by NAME — the adapter only
    // ever sees the type, which is exactly how the collapse hides.
    let name = state_name_of(&key, &created.external_id).await;
    eprintln!("{} landed in state: {name}", created.external_id);

    assert_eq!(
        name, "In Progress",
        "an in-progress chunk landed in {name:?}. `first of each type wins` picked \
         the wrong one of the team's two `started` states: the API does not return \
         states in workflow order, so the map must sort by position first"
    );
}

/// Query the issue's state NAME directly, bypassing our adapter — the adapter's
/// own view is type-only and would agree with itself either way.
async fn state_name_of(key: &str, identifier: &str) -> String {
    let body = serde_json::json!({
        "query": "query S($id: String!) { issue(id: $id) { state { name type position } } }",
        "variables": { "id": identifier },
    });
    let resp = reqwest::Client::new()
        .post("https://api.linear.app/graphql")
        .header("Authorization", key)
        .json(&body)
        .send()
        .await
        .expect("probe request");
    let json: serde_json::Value = resp.json().await.expect("probe json");
    eprintln!("probe: {json}");
    json.pointer("/data/issue/state/name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<none>")
        .to_string()
}

/// The risk that ranks FIRST here, and the one a mock structurally cannot
/// catch: `relate_items` sends the relation type as the bare string "blocks",
/// inferred from user-facing docs and never from the GraphQL schema. A mock
/// accepts any string, so if the literal is wrong, dependencies silently fail to
/// link and nothing anywhere reports it.
///
/// It also pins the DIRECTION. Linear stores one relation from the blocker's
/// side, so `relate_items(from, blocked_by)` has to invert its arguments. An
/// inverted-argument bug produces a relation that exists and points backwards,
/// which reads as success everywhere except a human's eyes.
#[tokio::test]
#[ignore = "talks to the real Linear API; needs LINEAR_API_KEY + LINEAR_TEAM"]
async fn a_blocking_relation_is_accepted_and_points_the_right_way() {
    let Some((key, team)) = live() else {
        eprintln!("SKIPPED: set LINEAR_API_KEY and LINEAR_TEAM to run the live smoke");
        return;
    };
    let tracker = tracker(&key, &team);

    let mut base = WorkItem::new("live smoke — the blocker");
    base.body = "Created by the think-and-ship live smoke test. Safe to delete.".into();
    let blocker = tracker.upsert_item(&base).await.expect("create blocker");

    let mut dep = WorkItem::new("live smoke — the blocked");
    dep.body = "Created by the think-and-ship live smoke test. Safe to delete.".into();
    let blocked = tracker.upsert_item(&dep).await.expect("create blocked");

    // `blocked` is blocked by `blocker`.
    tracker
        .relate_items(
            &blocked.external_id,
            std::slice::from_ref(&blocker.external_id),
        )
        .await
        .expect("the relation literal must be one Linear actually accepts");

    // Ask Linear, from the BLOCKED issue's side, what blocks it.
    let body = serde_json::json!({
        "query": "query R($id: String!) { issue(id: $id) { \
                    inverseRelations { nodes { type issue { identifier } } } \
                    relations { nodes { type relatedIssue { identifier } } } } }",
        "variables": { "id": blocked.external_id },
    });
    let json: serde_json::Value = reqwest::Client::new()
        .post("https://api.linear.app/graphql")
        .header("Authorization", &key)
        .json(&body)
        .send()
        .await
        .expect("probe")
        .json()
        .await
        .expect("probe json");
    eprintln!("relations on {}: {json}", blocked.external_id);

    // The blocker must appear as something blocking US, not as something we block.
    let inverse = json
        .pointer("/data/issue/inverseRelations/nodes")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let found = inverse.iter().any(|r| {
        r.get("type").and_then(serde_json::Value::as_str) == Some("blocks")
            && r.pointer("/issue/identifier")
                .and_then(serde_json::Value::as_str)
                == Some(blocker.external_id.as_str())
    });
    assert!(
        found,
        "expected {} to be recorded as BLOCKING {}. If the relation exists on the \
         other side instead, relate_items has its arguments inverted",
        blocker.external_id, blocked.external_id
    );
}

/// The seeding race, observed instead of suspected (tracker-teamcreate-live-verify).
///
/// `create_target` runs teamCreate and then immediately re-probes, and the
/// re-probe needs the team's workflow STATES — which Linear seeds server-side.
/// The WOW-AI run could not settle whether that seeding outruns the immediate
/// re-probe, because it failed earlier at the sanitized-key comparison. This
/// creates a team with a VALID generated key and observes the answer directly:
/// `Ok` means the fresh team resolved with usable states on the first try;
/// an error names the lag and fails the test so the bounded-retry fix becomes
/// mandatory rather than speculative.
///
/// CLEANUP RUNS BEFORE THE ASSERT, whatever happened: the team is looked up by
/// key and `teamDelete` is attempted; either outcome is printed. When deletion
/// is refused (it needs admin), the leftover team is named in capitals so
/// nobody has to discover it in the sidebar.
#[tokio::test]
#[ignore = "talks to the real Linear API and CREATES A TEAM; needs LINEAR_API_KEY or a connected linear credential"]
async fn a_fresh_team_resolves_on_the_immediate_reprobe_or_the_seeding_lag_is_real() {
    // Env first, like every sibling; else the SEALED STORE the binary itself
    // authenticates with (`tracker connect`) — this one test may run on a
    // machine already connected without pasting the key into a shell line.
    let key = match live() {
        Some((key, _)) => key,
        None => {
            let data_dir = std::env::var("XDG_DATA_HOME")
                .map(std::path::PathBuf::from)
                .or_else(|_| {
                    std::env::var("HOME")
                        .map(|h| std::path::PathBuf::from(h).join(".local").join("share"))
                })
                .map(|p| p.join("think-and-ship"))
                .ok();
            let stored = data_dir.and_then(|d| {
                use think_and_ship::tracker::credential::CredentialStore;
                think_and_ship::tracker::credential::FileCredentialStore::new(&d)
                    .load("linear")
                    .ok()
                    .flatten()
            });
            match stored {
                Some(c) => c.access.expose().to_string(),
                None => {
                    eprintln!(
                        "SKIPPED: set LINEAR_API_KEY or connect a linear credential \
                         (`think-and-ship tracker connect --provider linear`)"
                    );
                    return;
                }
            }
        }
    };

    // A throwaway, valid (alphanumeric, uppercase) key, unlikely to collide:
    // TS + three digits keeps it inside Linear's short-key conventions.
    let unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let fresh_key = format!("TS{:03}", unix % 1000);
    let display = format!("think-and-ship live probe {fresh_key} — safe to delete");
    eprintln!("creating throwaway team '{fresh_key}' ({display})");

    let probe = tracker(&key, &fresh_key);
    let created = probe.create_target(&display).await;
    eprintln!("create_target returned: {created:?}");

    // ── Cleanup FIRST, so a failing assert cannot skip it. ────────────────
    let lookup = serde_json::json!({
        "query": "query T($key: String!) { teams(filter: { key: { eq: $key } }) { nodes { id key name } } }",
        "variables": { "key": fresh_key },
    });
    let found: serde_json::Value = reqwest::Client::new()
        .post("https://api.linear.app/graphql")
        .header("Authorization", &key)
        .json(&lookup)
        .send()
        .await
        .expect("team lookup")
        .json()
        .await
        .expect("team lookup json");
    let team_id = found
        .pointer("/data/teams/nodes/0/id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);
    match &team_id {
        Some(id) => {
            let delete = serde_json::json!({
                "query": "mutation D($id: String!) { teamDelete(id: $id) { success } }",
                "variables": { "id": id },
            });
            let deleted: serde_json::Value = reqwest::Client::new()
                .post("https://api.linear.app/graphql")
                .header("Authorization", &key)
                .json(&delete)
                .send()
                .await
                .expect("team delete")
                .json()
                .await
                .expect("team delete json");
            if deleted.pointer("/data/teamDelete/success") == Some(&serde_json::Value::Bool(true)) {
                eprintln!("cleanup: team '{fresh_key}' deleted");
            } else {
                eprintln!(
                    "MANUAL CLEANUP REQUIRED: team '{fresh_key}' ({id}) could not be \
                     deleted by this key: {deleted}"
                );
            }
        }
        None => eprintln!(
            "cleanup: no team with key '{fresh_key}' found afterwards \
             (nothing was created, or the lookup failed): {found}"
        ),
    }

    // ── The observation this test exists for. ─────────────────────────────
    let info = created.expect(
        "create_target failed on a team it may just have created — if the error \
         is NotFound, the state-seeding lag is REAL and create_target needs a \
         bounded retry after teamCreate",
    );
    assert_eq!(
        info.key, fresh_key,
        "the resolved team's key must be the one we created"
    );
    eprintln!(
        "the immediate re-probe RESOLVED the fresh team with usable states: {}",
        info.detail
    );
}

/// The definitive round trip: not a hand-written body approximating what the
/// projector emits, but `project_all` itself driving the real adapter against
/// the real API, with a chunk that has acceptance criteria and therefore the
/// full heading + checklist + provenance-footer shape.
///
/// This exists because a hand-written probe MISLED me: I wrote
/// `## Acceptance` followed immediately by list items, Linear inserted a blank
/// line, and it looked like a fidelity defect. The projector already emits the
/// blank line, so the defect was in my approximation. The lesson generalizes —
/// test the artifact the system produces, not your recollection of it.
#[tokio::test]
#[ignore = "talks to the real Linear API; needs LINEAR_API_KEY + LINEAR_TEAM"]
async fn the_projectors_own_body_survives_linears_markdown_normalization() {
    use think_and_ship::roadmap::domain::ChunkStatus;
    use think_and_ship::roadmap::engine::RoadmapEngine;
    use think_and_ship::tracker::project::project_all;

    let Some((key, team)) = live() else {
        eprintln!("SKIPPED: set LINEAR_API_KEY and LINEAR_TEAM to run the live smoke");
        return;
    };
    let tracker = tracker(&key, &team);

    let mut e = RoadmapEngine::new("live-smoke".into());
    e.add_chunk(
        "footer-shape".into(),
        "live smoke — projector body shape".into(),
        ChunkStatus::Pending,
        10,
        "Created by the think-and-ship live smoke test. Safe to delete.".into(),
        vec![
            "the acceptance checklist renders".into(),
            "the provenance footer survives".into(),
        ],
        vec![],
        false,
    )
    .expect("add chunk");
    e.set_tracker_opt_in("footer-shape", "linear", true)
        .expect("opt in");

    project_all(&mut e, &tracker, None).await.expect("project");

    let link = e
        .tracker_link("footer-shape", "linear")
        .expect("the projection must have recorded a link")
        .clone();
    eprintln!("projected to {}", link.external_id);

    let fetched = tracker
        .fetch_since("2020-01-01T00:00:00Z")
        .await
        .expect("fetch");
    let back = fetched
        .iter()
        .find(|i| i.external_id.as_deref() == Some(link.external_id.as_str()))
        .expect("our issue must come back");

    eprintln!("--- body Linear returned ---\n{}\n---", back.body);

    assert!(
        back.body.contains("<!-- think-and-ship:"),
        "the provenance footer was STRIPPED by Linear — that is a P0 against the \
         whole provenance design, not a formatting nit"
    );
    assert!(
        back.body.contains("- [ ] the acceptance checklist renders"),
        "the acceptance checklist did not survive"
    );
    // The assertion this test existed to flip. It read `assert_ne` while the
    // adapter declared `labels: true` and never wrote one; now the claim is
    // true and the round trip is faithful.
    assert_eq!(
        back.content_hash(),
        link.our_last_write_hash,
        "ROUND-TRIP LOSS: the hash of what came back differs from the hash we \
         recorded when we wrote it, so every inbound event on this issue would \
         be misjudged. sent labels {:?}, got back {:?}",
        "roadmap:<band>",
        back.labels
    );
}
