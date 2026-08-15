//! Container identity is REMEMBERED, and the memory
//! travels both ways through the projector.
//!
//! Driven through `project_all_with_policy` against `FakeTracker`. The
//! adapter-level halves (rename survival at the wire, vocabulary guards, the
//! regroup patch) are in `tracker_linear.rs` against the GraphQL mock.

use think_and_ship::roadmap::domain::{ChunkStatus, ContainerKind};
use think_and_ship::roadmap::engine::RoadmapEngine;
use think_and_ship::tracker::fake::FakeTracker;
use think_and_ship::tracker::ownership::Ownership;
use think_and_ship::tracker::project::project_all_with_policy;

const PROVIDER: &str = "fake";

fn engine_with(chunks: &[(&str, Option<&str>, ChunkStatus)]) -> RoadmapEngine {
    let mut e = RoadmapEngine::new("proj".into());
    for (id, group, status) in chunks {
        e.add_chunk(
            (*id).into(),
            format!("Chunk {id}"),
            *status,
            10,
            format!("why {id}"),
            vec![],
            vec![],
            false,
        )
        .expect("add");
        e.set_tracker_opt_in(id, PROVIDER, true).expect("opt in");
        if let Some(g) = group {
            e.set_group(id, Some((*g).into())).expect("group");
        }
    }
    e
}

async fn push(e: &mut RoadmapEngine, t: &FakeTracker, initiative: Option<&str>) {
    project_all_with_policy(e, t, None, &Ownership::default(), initiative)
        .await
        .expect("project");
}

/// THE promise: the identity recorded on push N is handed to the adapter on
/// push N+1, for the group and the roof alike. This is the whole of rename
/// survival at the projector's level — an adapter holding the uuid never
/// needs the name to resolve.
#[tokio::test]
async fn the_identity_recorded_last_push_is_passed_back_this_push() {
    let mut e = engine_with(&[("a-1", Some("Quayside"), ChunkStatus::Pending)]);
    let t = FakeTracker::new(PROVIDER);

    push(&mut e, &t, Some("roof")).await;
    push(&mut e, &t, Some("roof")).await;

    assert_eq!(
        t.group_calls(),
        vec![
            ("Quayside".to_string(), None),
            ("Quayside".to_string(), Some("grp-Quayside".to_string())),
        ],
        "first push has nothing to remember; the second must carry the id \
         the first recorded"
    );
    assert_eq!(
        t.initiative_calls(),
        vec![
            ("roof".to_string(), None),
            ("roof".to_string(), Some("init-roof".to_string())),
        ],
        "the roof gets the same memory"
    );
}

/// `created_by_us` is the fact the empty-container decision turns on, and it
/// is STICKY: a later push that merely resolved the container must not erase
/// the record that we minted it.
#[tokio::test]
async fn a_minted_container_is_remembered_as_ours_forever() {
    let mut e = engine_with(&[("a-1", Some("Quayside"), ChunkStatus::Pending)]);
    let t = FakeTracker::new(PROVIDER);

    push(&mut e, &t, Some("roof")).await;
    let after_first = e
        .container_link(ContainerKind::Group, "Quayside", PROVIDER)
        .expect("recorded")
        .clone();
    assert!(after_first.created_by_us, "the first push minted it");
    assert_eq!(after_first.external_id, "grp-Quayside");

    // The second push resolves rather than creates (the fake reports
    // created: false) — the mint memory must survive it.
    push(&mut e, &t, Some("roof")).await;
    assert!(
        e.container_link(ContainerKind::Group, "Quayside", PROVIDER)
            .expect("still recorded")
            .created_by_us,
        "created_by_us is sticky — a resolve is not an un-mint"
    );
    assert!(
        e.container_link(ContainerKind::Initiative, "roof", PROVIDER)
            .expect("roof recorded")
            .created_by_us
    );
}

/// The group-move criterion at the projector: regrouping a chunk re-sends the
/// item carrying the NEW group, and the old group is not re-ensured — the
/// container it leaves behind is left standing (never deleted; the decided
/// empty-container rule).
#[tokio::test]
async fn moving_a_chunk_between_groups_moves_its_item_and_touches_nothing_else() {
    let mut e = engine_with(&[("mover", Some("Quayside"), ChunkStatus::Pending)]);
    let t = FakeTracker::new(PROVIDER);

    push(&mut e, &t, None).await;
    assert_eq!(
        t.items()[0].group.as_deref(),
        Some("Quayside"),
        "precondition"
    );

    e.set_group("mover", Some("Paperwork".into()))
        .expect("regroup");
    push(&mut e, &t, None).await;

    assert_eq!(
        t.items()[0].group.as_deref(),
        Some("Paperwork"),
        "the item must carry its NEW container after the move"
    );
    let groups = t.groups();
    assert!(
        groups.iter().any(|(n, _)| n == "Quayside"),
        "the emptied container still stands — moving out is not deleting"
    );
    assert!(
        groups.iter().any(|(n, _)| n == "Paperwork"),
        "and the new one exists"
    );
    assert_eq!(
        t.group_calls()
            .iter()
            .filter(|(n, _)| n == "Quayside")
            .count(),
        1,
        "the second push does not re-ensure the abandoned group at all"
    );
}
