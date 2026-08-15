//! Read-only census of the workspace's roadmap chunk envelopes.
//!
//! Counts, per authoring project, how many chunk records in the cloud carry
//! the `name` and `group` fields against how many predate them. This is the
//! before/after measurement for re-stamping the cloud — it never writes, so
//! the numbers it prints are evidence about the store rather than a push
//! command's report about itself.
//!
//! Run: `cargo run --example cloud_census`
//! Needs a connected workspace (stored connection or THINK_AND_SHIP_CLOUD_URL
//! + THINK_AND_SHIP_CLOUD_TOKEN).

use std::collections::BTreeMap;

#[derive(Default)]
struct Tally {
    total: usize,
    with_name: usize,
    with_group: usize,
}

fn main() -> anyhow::Result<()> {
    let Some(client) = think_and_ship::cloud::config::client_from_env() else {
        anyhow::bail!(
            "no cloud connection: connect first or set THINK_AND_SHIP_CLOUD_URL + THINK_AND_SHIP_CLOUD_TOKEN"
        );
    };

    let envelopes = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(client.list(Some("roadmap"), None))
        .map_err(|e| anyhow::anyhow!("list roadmap records: {e}"))?;

    let mut per_project: BTreeMap<String, Tally> = BTreeMap::new();
    let mut non_chunk = 0usize;
    for env in &envelopes {
        if env.get("kind").and_then(|v| v.as_str()) != Some("chunk") {
            non_chunk += 1;
            continue;
        }
        let record = env.get("record");
        let project = record
            .and_then(|r| r.get("project_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("(unstamped)");
        let has = |field: &str| {
            record
                .and_then(|r| r.get(field))
                .and_then(|v| v.as_str())
                .is_some_and(|s| !s.is_empty())
        };
        let t = per_project.entry(project.to_string()).or_default();
        t.total += 1;
        if has("name") {
            t.with_name += 1;
        }
        if has("group") {
            t.with_group += 1;
        }
    }

    let mut grand = Tally::default();
    println!(
        "{:<40} {:>6} {:>9} {:>10} {:>7}",
        "project", "chunks", "with name", "with group", "stale"
    );
    for (project, t) in &per_project {
        println!(
            "{:<40} {:>6} {:>9} {:>10} {:>7}",
            project,
            t.total,
            t.with_name,
            t.with_group,
            t.total - t.with_name,
        );
        grand.total += t.total;
        grand.with_name += t.with_name;
        grand.with_group += t.with_group;
    }
    println!(
        "{:<40} {:>6} {:>9} {:>10} {:>7}",
        "TOTAL",
        grand.total,
        grand.with_name,
        grand.with_group,
        grand.total - grand.with_name,
    );
    println!("({} roadmap envelope(s) of other kinds ignored)", non_chunk);
    Ok(())
}
