//! Signal pull/reconcile: the cloud's signals merge into the local
//! engine. NEWEST-wins per signal via lifecycle progress (reconcile-recency-guard):
//! the forward-only status rank + append-only enrichment count decide, exactly as
//! in `merge_signal_stores` — a stale cloud copy must never roll back a newer
//! local transition. Mirror of the roadmap pull proof.

use serde_json::{Value, json};
use think_and_ship::cloud::build::from_signal;
use think_and_ship::cloud::client::CloudClient;
use think_and_ship::cloud::pull::reconcile_signals;
use think_and_ship::signal::SignalEngine;
use think_and_ship::signal::domain::{Signal, SignalKind, SignalStatus};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn signal(id: &str) -> Signal {
    signal_at(id, SignalStatus::New)
}

/// A signal that PROVABLY belongs to `project`. Ownership must be stated in the
/// fixture: the push no longer fills it in for an unstamped signal, so letting
/// it default would mean asserting the laundering rather than the guard.
fn owned_signal(id: &str, project: &str) -> Signal {
    Signal {
        project_id: Some(project.into()),
        ..signal(id)
    }
}

fn signal_at(id: &str, status: SignalStatus) -> Signal {
    Signal {
        id: id.into(),
        kind: SignalKind::Bug,
        from: "dana".into(),
        body: format!("body {id}"),
        content: None,
        created: "2026-06-08T00:00:00Z".into(),
        status,
        enrichment: vec![],
        cross_refs: vec![],
        surfaced_at: None,
        snooze_until: None,
        project_id: None,
    }
}

#[test]
fn upsert_signal_replaces_by_id_or_inserts() {
    let mut engine = SignalEngine::new("t".into());
    engine.upsert_signal(signal("a"));
    engine.upsert_signal(signal("b"));
    assert_eq!(engine.signals().signals.len(), 2);

    // A PROGRESSED cloud copy replaces 'a' — no duplicate.
    engine.upsert_signal(signal_at("a", SignalStatus::Triaged));
    assert_eq!(engine.signals().signals.len(), 2);
    let a = engine
        .signals()
        .signals
        .iter()
        .find(|s| s.id == "a")
        .unwrap();
    assert_eq!(a.status, SignalStatus::Triaged);
}

/// The roadmap race's signal twin: a stale cloud copy (still New) must not
/// roll back a local signal that already advanced to Triaged.
#[test]
fn upsert_signal_keeps_newer_local_lifecycle_progress() {
    let mut engine = SignalEngine::new("t".into());
    engine.upsert_signal(signal_at("a", SignalStatus::Triaged));

    // Reconcile delivers the cloud copy from before the transition's push landed.
    engine.upsert_signal(signal_at("a", SignalStatus::New));

    let a = engine
        .signals()
        .signals
        .iter()
        .find(|s| s.id == "a")
        .unwrap();
    assert_eq!(
        a.status,
        SignalStatus::Triaged,
        "a stale cloud copy rolled back a local lifecycle transition"
    );
}

/// sync-project-scope, the signal family's copy: with an org token every
/// project in the org shares one cloud tenant, so `apply_signal_records` filters
/// on the record's project stamp exactly as roadmap and think do.
///
/// This gate is here because it was NOT: neutralising `record_is_local` reddened
/// the roadmap and think files and left this one entirely green, so the one line
/// standing between the signal store and every other project's signals could
/// have been deleted to applause. The rule is per-FAMILY, and so is its proof.
#[test]
fn foreign_and_unstamped_signals_never_merge() {
    use think_and_ship::cloud::pull::apply_signal_records;

    let mut engine = SignalEngine::new("ours".into());

    let foreign = serde_json::to_value(from_signal(
        "other-project",
        &owned_signal("theirs", "other-project"),
    ))
    .expect("foreign envelope");
    // Unstamped now needs no surgery: a signal with no origin reaches the wire
    // explicitly unprovable, which is exactly the legacy record this models.
    let unstamped =
        serde_json::to_value(from_signal("ours", &signal("legacy"))).expect("legacy envelope");
    let ours = serde_json::to_value(from_signal("ours", &owned_signal("mine", "ours")))
        .expect("our envelope");

    let merged = apply_signal_records(&mut engine, &[foreign, unstamped, ours]);

    // Load-bearing first: which signals actually landed. An earlier count
    // assertion would fail under the same deliberate breakage and hide which
    // half broke.
    let ids: Vec<String> = engine
        .signals()
        .signals
        .iter()
        .map(|s| s.id.clone())
        .collect();
    assert_eq!(
        ids,
        vec!["mine".to_string()],
        "only this project's stamped signal may merge — a foreign or unattributable one is the bleed"
    );
    assert_eq!(merged, 1);
}

#[tokio::test]
async fn reconcile_signals_pulls_cloud_signals_into_the_local_engine() {
    let server = MockServer::start().await;
    let records: Vec<Value> = ["a", "b"]
        .iter()
        .map(|id| serde_json::to_value(from_signal("t", &owned_signal(id, "t"))).unwrap())
        .collect();
    Mock::given(method("GET"))
        .and(path("/v1/records"))
        .and(query_param("family", "signal"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "records": records })))
        .mount(&server)
        .await;

    let client = CloudClient::new(server.uri(), "tok");
    let mut engine = SignalEngine::new("t".into());

    assert_eq!(reconcile_signals(&client, &mut engine).await.unwrap(), 2);
    assert_eq!(engine.signals().signals.len(), 2);

    // Re-pull is idempotent (cloud-wins replace, not duplicate).
    assert_eq!(reconcile_signals(&client, &mut engine).await.unwrap(), 2);
    assert_eq!(engine.signals().signals.len(), 2);
}
