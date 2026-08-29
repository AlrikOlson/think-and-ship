//! CLI subcommand handlers.

pub mod args;
mod connect;
pub mod otel_stack;
pub mod setup;
mod skills;
pub mod store_health;

pub use connect::{connect, disconnect, print_token};

use std::collections::HashSet;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{Context, Result};
use rmcp::{
    ServiceExt,
    transport::{
        io::stdio,
        streamable_http_server::{
            StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
        },
    },
};
use tokio_util::sync::CancellationToken;
use tracing_subscriber::EnvFilter;

use crate::infra::{
    Broadcaster as EngineBroadcaster, Domain, Persistence as InfraPersistence,
    PersistenceConfig as InfraPersistenceConfig, RepoSink, SyncTarget, discover_repo_root,
    resolve_project_id, shared_from_env,
};
use crate::mcp::UnifiedService;
use crate::roadmap::RoadmapEngine;
use crate::ship::ShipService;
use crate::ship::broadcast::Broadcaster as ShipBroadcaster;
use crate::ship::engine::ShipEngine;
use crate::ship::persistence::{
    Persistence as ShipPersistence, PersistenceConfig as ShipPersistenceConfig,
};
use crate::signal::SignalEngine;
use crate::think::ThinkService;
use crate::think::broadcast::Broadcaster as ThinkBroadcaster;
use crate::think::config::load_config as load_think_config;
use crate::think::engine::core::ReasoningServer;

/// Default bind address when `--http` is passed without a value or with just a
/// port suffix (`:8080`).
const DEFAULT_HTTP_HOST: &str = "127.0.0.1";

pub fn serve(http: Option<String>) -> Result<()> {
    init_tracing();

    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(async move {
            let (unified, project_id) = build_unified()?;
            let pkg_name = env!("CARGO_PKG_NAME");
            let pkg_version = env!("CARGO_PKG_VERSION");
            eprintln!("{pkg_name} {pkg_version} (project: {project_id})");

            match http {
                None => run_stdio(unified).await,
                Some(spec) => {
                    let addr = parse_http_addr(&spec)?;
                    run_http(addr, unified).await
                }
            }
        })
}

/// One-shot back-fill: push the existing local corpus to the cloud
/// (`think-and-ship sync push`). Builds the four engines READ-ONLY
/// (persistence loads the corpus; no cloud/broadcast/repo-sink is wired, so
/// enumeration can never mutate or trigger write-through), collects one envelope
/// per record across all four families, and PUTs each to `/v1/records`. The push
/// is idempotent (the backend dedups) and therefore resumable. `--dry-run`
/// reports the count without contacting the cloud.
/// Why there is no cloud client, phrased as the action that would actually fix it.
///
/// The message this replaces ended "Run `think-and-ship connect` to set the
/// second one up" — advice that was already satisfied for the user who reported
/// it, and that could not possibly have helped, because `connect` wrote nothing
/// a shell could read. A failure message recommending a command incapable of
/// fixing the failure it describes is worse than no message: it spends the
/// reader's time proving the tool wrong before they can start debugging.
///
/// So it diagnoses rather than guesses. The two live states are genuinely
/// different problems with different fixes, and only one of them is "run connect".
/// Bring a machine that connected before the record existed up to date, once,
/// and say so. Called first by every CLI verb that consults the connection.
///
/// THE STATE THIS ANSWERS. `connect` used to write the cloud url and the profile
/// name into an MCP config's `env` block and nowhere else. An MCP host injects
/// that block into the server it spawns, so on those machines the agent syncs
/// perfectly while every shell command reports "not connected" — the two are not
/// out of step, there was simply never a shared thing for them to be in step
/// about. Now there is one, and this puts it there for machines that predate it.
///
/// SAYING SO IS HALF THE FEATURE. Adopting in silence would change what a machine
/// does without ever explaining why `status` suddenly started answering, and the
/// line costs one run: after it, there is a record, and there is nothing left to
/// report. The alternative — telling the user to re-run `connect` — is honest but
/// demands an interactive browser flow on every machine to recover settings
/// already sitting on disk.
///
/// Warnings, never failures. A machine that cannot be adopted is exactly as
/// usable as it was a moment ago, and no cloud verb should die over a migration.
pub(crate) fn adopt_legacy_connection() {
    let data_dir = crate::cloud::connection::data_dir();
    let store = crate::cloud::credential::store_for(&data_dir);
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let Some(adopted) = setup::adopt_legacy_connection_in(
        &data_dir,
        &crate::cloud::connection::project_id(),
        &cwd,
        setup::claude_home_config(),
        store.as_ref(),
        &now,
    ) else {
        return;
    };
    eprintln!(
        "adopted this machine's existing connection to {} (profile {}), read once from {}.\n\
         The CLI and your agent now resolve it from the same record — nothing else to do.",
        adopted.connection.cloud_url, adopted.connection.profile, adopted.from,
    );
}

fn not_connected_advice() -> String {
    let stored = crate::cloud::connection::load();
    let store = crate::cloud::credential::store_for(&crate::cloud::connection::data_dir());
    let env = crate::cloud::config::EnvOverrides::from_env();
    let url = crate::cloud::config::resolve_url(&env, stored.as_ref());
    let token = crate::cloud::config::resolve_token(store.as_ref(), &env, stored.as_ref());

    match (url, token) {
        (None, _) => format!(
            "this project is not connected to a cloud workspace, so there is nowhere to \
             push.\n  Run `think-and-ship connect` — or, for an unattended machine, set \
             {} and {}.",
            crate::cloud::connection::URL_ENV,
            crate::cloud::credential::TOKEN_ENV,
        ),
        (Some((url, _)), None) => format!(
            "this project is recorded as connected to {url}, but no credential answers \
             for it — the token is missing from this machine's credential store.\n  Run \
             `think-and-ship connect` to restore it, or `think-and-ship status` to see \
             exactly what resolved and from where."
        ),
        // Unreachable while this is only called after `client_from_env` returned
        // None, since that resolves precisely these two. Spelled out rather than
        // left to panic, because the entire point of this function is that a user
        // facing a failure is never handed something useless.
        (Some((url, _)), Some(_)) => format!(
            "both halves of the connection to {url} resolved, but the cloud client could \
             not be built. Run `think-and-ship status` and report what it prints."
        ),
    }
}

/// Build the four engines READ-ONLY (persistence loads the corpus; no cloud
/// client / broadcaster / repo-sink is attached, so enumeration can never
/// mutate or trigger write-through) and collect one envelope per record.
/// Shared by `sync push` and `telemetry push`.
fn collect_local_envelopes() -> (String, Vec<crate::cloud::envelope::UnifiedRecordEnvelope>) {
    collect_local_envelopes_for(resolve_project_id(None))
}

/// [`collect_local_envelopes`] for an explicitly named project. Identity is a
/// parameter rather than a resolution because the stores live in the global
/// data dir keyed by project id — no working directory is required to read
/// them, which is what lets `sync push --all-projects` reach a project whose
/// checkout no longer exists anywhere on the machine.
fn collect_local_envelopes_for(
    project_id: String,
) -> (String, Vec<crate::cloud::envelope::UnifiedRecordEnvelope>) {
    let think = ReasoningServer::new_for_project(load_think_config(), project_id.clone());
    let ship = ShipEngine::new(project_id.clone())
        .with_persistence(ShipPersistence::new(&ShipPersistenceConfig::from_env()));
    let roadmap = RoadmapEngine::new(project_id.clone()).with_persistence(InfraPersistence::new(
        &InfraPersistenceConfig::from_env(),
        Domain::Roadmap,
    ));
    let signal = SignalEngine::new(project_id.clone()).with_persistence(InfraPersistence::new(
        &InfraPersistenceConfig::from_env(),
        Domain::Signal,
    ));

    // Mirror the write-through scope exactly: think steps, the full ship cycle
    // (objective + tasks + checks + actions, cycle-scoped ids —
    // sync-ship-full), roadmap chunks, signals.
    let envelopes = crate::cloud::backfill::collect_envelopes(
        &project_id,
        think.all_steps(),
        ship.objective.as_ref().map(|o| (o, ship.tasks.as_slice())),
        roadmap.roadmap(),
        &signal.signals().signals,
    );
    (project_id, envelopes)
}

pub fn sync_push(dry_run: bool, all_projects: bool) -> Result<()> {
    if all_projects {
        return sync_push_all_projects(dry_run);
    }
    let (project_id, envelopes) = collect_local_envelopes();
    let counts = crate::cloud::backfill::BackfillCounts::from_envelopes(&envelopes);

    eprintln!(
        "sync push (project {project_id}): {} record(s) — think {}, ship {}, roadmap {}, signal {}",
        counts.total(),
        counts.think,
        counts.ship,
        counts.roadmap,
        counts.signal,
    );

    if dry_run {
        eprintln!("--dry-run: nothing pushed.");
        return Ok(());
    }

    // After the dry-run exit, because adopting is a WRITE: `--dry-run` promising
    // to touch nothing and then migrating the machine's connection record would
    // be exactly the surprise the flag exists to rule out.
    adopt_legacy_connection();

    let Some(client) = crate::cloud::config::client_from_env() else {
        anyhow::bail!(not_connected_advice());
    };

    let summary = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(crate::cloud::backfill::push_all(&client, &envelopes));

    eprintln!(
        "pushed {} record(s): {} created, {} deduped, {} kept (cloud fresher), {} failed.",
        summary.ok(),
        summary.created,
        summary.deduped,
        summary.kept.len(),
        summary.failed.len(),
    );
    for (label, reason) in &summary.kept {
        eprintln!("  KEPT {label}: {reason}");
    }
    for (label, reason) in &summary.failed {
        eprintln!("  FAILED {label}: {reason}");
    }
    if !summary.failed.is_empty() {
        anyhow::bail!(
            "{} record(s) failed to push; re-run `sync push` to resume (already-pushed records dedup)",
            summary.failed.len()
        );
    }
    Ok(())
}

/// `sync push` over every project with a store on this machine.
///
/// A record's cloud copy is rewritten only on mutation, so a schema field
/// added after a project went quiet never reaches that project's records —
/// however correct the write-through is. The per-project push already repairs
/// this (idempotent, resumable); what was missing was distribution: identity
/// normally comes from the working directory, and most of these projects have
/// no directory to run from anymore. Their stores are still here, keyed by
/// project id, so the repair enumerates the stores and names each project
/// explicitly instead of asking 57 directories to still exist.
fn sync_push_all_projects(dry_run: bool) -> Result<()> {
    let cfg = InfraPersistenceConfig::from_env();
    let dirs: Vec<std::path::PathBuf> =
        [Domain::Think, Domain::Ship, Domain::Roadmap, Domain::Signal]
            .iter()
            .map(|d| InfraPersistence::new(&cfg, *d).sessions_dir().to_path_buf())
            .collect();
    let ids = store_health::project_ids_in(&dirs);
    eprintln!(
        "sync push --all-projects: {} project store(s) on this machine.",
        ids.len()
    );
    if ids.is_empty() {
        eprintln!("nothing to push.");
        return Ok(());
    }

    let client = if dry_run {
        None
    } else {
        // Same ordering argument as the single-project path: adopting the
        // machine's legacy connection is a WRITE, so it must never happen
        // under --dry-run.
        adopt_legacy_connection();
        let Some(client) = crate::cloud::config::client_from_env() else {
            anyhow::bail!(not_connected_advice());
        };
        Some(client)
    };

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let mut total_records = 0usize;
    let mut total_created = 0usize;
    let mut total_deduped = 0usize;
    let mut total_kept = 0usize;
    let mut total_failed = 0usize;
    for id in ids {
        let (project_id, envelopes) = collect_local_envelopes_for(id);
        let counts = crate::cloud::backfill::BackfillCounts::from_envelopes(&envelopes);
        total_records += counts.total();
        eprintln!(
            "  {project_id}: {} record(s) — think {}, ship {}, roadmap {}, signal {}",
            counts.total(),
            counts.think,
            counts.ship,
            counts.roadmap,
            counts.signal,
        );
        if let Some(client) = &client {
            let summary = runtime.block_on(crate::cloud::backfill::push_all(client, &envelopes));
            eprintln!(
                "    pushed {}: {} created, {} deduped, {} kept (cloud fresher), {} failed",
                summary.ok(),
                summary.created,
                summary.deduped,
                summary.kept.len(),
                summary.failed.len(),
            );
            for (label, reason) in &summary.kept {
                eprintln!("    KEPT {label}: {reason}");
            }
            for (label, reason) in &summary.failed {
                eprintln!("    FAILED {label}: {reason}");
            }
            total_created += summary.created;
            total_deduped += summary.deduped;
            total_kept += summary.kept.len();
            total_failed += summary.failed.len();
        }
    }

    if dry_run {
        eprintln!("{total_records} record(s) across all projects. --dry-run: nothing pushed.");
        return Ok(());
    }
    eprintln!(
        "all projects: {total_created} created, {total_deduped} deduped, {total_kept} kept (cloud fresher), {total_failed} failed."
    );
    if total_failed > 0 {
        anyhow::bail!(
            "{total_failed} record(s) failed to push; re-run `sync push --all-projects` to resume (already-pushed records dedup)"
        );
    }
    Ok(())
}

/// Build the unified MCP service from env-driven config. Returns the service
/// alongside the resolved project id so the caller can print a banner.
fn build_unified() -> Result<(UnifiedService, String)> {
    let mut think_config = load_think_config();

    // Spawn the broadcast socket ONCE so both families share a single
    // listener. Clear the path on the think config so ReasoningServer::new
    // doesn't try to bind it a second time — the shared handle is attached
    // below via with_broadcaster instead.
    let shared_broadcast = think_config
        .broadcast
        .path
        .clone()
        .and_then(EngineBroadcaster::spawn);
    if shared_broadcast.is_some()
        && let Some(path) = think_config.broadcast.path.as_ref()
    {
        eprintln!("broadcast: {} (shared by think + ship)", path.display());
    }
    think_config.broadcast.path = None;

    // Git-native trace sink: when THINK_AND_SHIP_SYNC_TARGET=repo-git
    // and we're inside a git repo, mirror traces into `.think-and-ship/`. Both
    // families share one sink so they commit into the same repo tree.
    let repo_sink = resolve_repo_sink();

    // Cloud sync: opt in with THINK_AND_SHIP_SYNC_TARGET=cloud
    // AND THINK_AND_SHIP_CLOUD_URL/_TOKEN. When selected, mutations fire-and-forget
    // push (write-through) to the per-tenant backend system-of-record; one client
    // is shared across all four families (think + ship + roadmap + signal).
    // The explicit SyncTarget::Cloud knob gates it — a token alone is not enough.
    // One project identity for all four families — the think default history
    // is project-scoped (think-trace-durability), so it must persist under
    // the same id the ship/roadmap/signal engines use.
    let project_id = crate::infra::resolve_project_id(None);

    let cloud_client = match crate::infra::repo_sync::SyncTarget::from_env() {
        crate::infra::repo_sync::SyncTarget::Cloud => {
            crate::cloud::config::client_from_env().map(|client| {
                // Durable offline queue (sync-offline-queue): failed pushes
                // persist under the data dir and replay on reconnect/boot.
                let persist = InfraPersistenceConfig::from_env();
                let path = persist.enabled.then(|| {
                    persist
                        .data_dir
                        .join("cloud")
                        .join("outbox")
                        .join(format!("{project_id}.json"))
                });
                client.with_outbox(std::sync::Arc::new(crate::cloud::outbox::Outbox::new(path)))
            })
        }
        _ => None,
    };

    let think_engine = {
        let mut server = ReasoningServer::new_for_project(think_config, project_id.clone());
        if let Some(b) = shared_broadcast.clone() {
            server = server.with_broadcaster(ThinkBroadcaster::from_engine(b));
        }
        if let Some((sink, shared)) = repo_sink.clone() {
            server = server.with_repo_sink(sink, shared);
        }
        if let Some(cloud) = &cloud_client {
            server = server.with_cloud(cloud.clone());
        }
        server
    };
    let think_service = ThinkService::new(think_engine);

    let ship_persist_cfg = ShipPersistenceConfig::from_env();
    let ship_persistence = ShipPersistence::new(&ship_persist_cfg);
    let mut ship_engine = ShipEngine::new(project_id.clone()).with_persistence(ship_persistence);
    if let Some(b) = shared_broadcast.clone() {
        ship_engine = ship_engine.with_broadcaster(ShipBroadcaster::from_engine(b));
    }
    if let Some((sink, shared)) = repo_sink.clone() {
        ship_engine = ship_engine.with_repo_sink(sink, shared);
    }
    if let Some(cloud) = &cloud_client {
        ship_engine = ship_engine.with_cloud(cloud.clone());
    }
    let ship_service = ShipService::new(ship_engine);

    // Roadmap family: native, persisted roadmap state under the
    // Domain::Roadmap partition. Attaching the shared broadcaster +
    // repo_sink so roadmap mutations mirror to git-native Agent Trace and emit
    // family="roadmap" broadcast frames through the one shared socket.
    let roadmap_persistence = crate::infra::Persistence::new(
        &crate::infra::PersistenceConfig::from_env(),
        crate::infra::Domain::Roadmap,
    );
    // Opt-in inheritance (tracker-optin-never-grows): when this project has
    // EXPLICITLY been connected to a tracker, chunks created during this
    // server's life are born in scope. Read once here and re-armed by
    // `setup_local`, so a `tracker_setup` over MCP takes effect immediately
    // rather than at the next restart.
    let mut roadmap_engine = crate::roadmap::RoadmapEngine::new(project_id.clone())
        .with_persistence(roadmap_persistence)
        .with_opt_in_inheritance(inherited_opt_in_provider());
    // Auto-import an existing roadmap (any markdown schema or YAML store) when
    // native state is empty — before the broadcaster/sink attach, so no noise.
    maybe_auto_import_roadmap(&mut roadmap_engine);
    if let Some(b) = shared_broadcast.clone() {
        roadmap_engine =
            roadmap_engine.with_broadcaster(crate::roadmap::broadcast::Broadcaster::from_engine(b));
    }
    if let Some((sink, shared)) = repo_sink {
        roadmap_engine = roadmap_engine.with_repo_sink(sink, shared);
    }
    if let Some(cloud) = &cloud_client {
        roadmap_engine = roadmap_engine.with_cloud(cloud.clone());
    }
    let roadmap_service = crate::roadmap::RoadmapService::new(roadmap_engine);

    // Signal family: native, persisted local signal cache under the
    // Domain::Signal partition. When SyncTarget::Cloud is selected,
    // with_cloud (below) makes signal mutations write through to the cloud
    // system-of-record, turning the local store into a cache rather than a fork.
    let signal_persistence = crate::infra::Persistence::new(
        &crate::infra::PersistenceConfig::from_env(),
        crate::infra::Domain::Signal,
    );
    let mut signal_engine =
        crate::signal::SignalEngine::new(project_id.clone()).with_persistence(signal_persistence);
    if let Some(b) = shared_broadcast {
        signal_engine =
            signal_engine.with_broadcaster(crate::signal::broadcast::Broadcaster::from_engine(b));
    }
    if let Some(cloud) = &cloud_client {
        signal_engine = signal_engine.with_cloud(cloud.clone());
    }
    // Share the roadmap engine handle so signal_promote can create
    // backlog chunks — the coupling lives here at the wire layer (signal →
    // roadmap, acyclic), not in the engine types.
    let signal_service =
        crate::signal::SignalService::new(signal_engine).with_roadmap(roadmap_service.engine());
    // Reciprocal wire-layer handle: roadmap_status folds in a
    // pending-signal count via the SignalEngine handle — composed at the service
    // layer so the RoadmapEngine never depends on the SignalEngine (acyclic).
    let roadmap_service = roadmap_service.with_signal(signal_service.engine());

    // Startup hydrate (sync-think-reconcile): with cloud sync on, one-shot
    // reconcile of think+roadmap+signal so a fresh machine or empty data dir
    // converges to the workspace on boot. Fire-and-forget on the runtime —
    // boot never blocks on the network; counts land on stderr when done.
    if let Some(cloud) = &cloud_client
        && let Ok(handle) = tokio::runtime::Handle::try_current()
    {
        let client = cloud.clone();
        let think = think_service.engine();
        let roadmap = roadmap_service.engine();
        let signal = signal_service.engine();
        handle.spawn(async move {
            // Drain a previous session's queued pushes BEFORE pulling, so our
            // own offline mutations land first and win recency at the store.
            let drained = client.flush_outbox().await;
            if drained > 0 {
                eprintln!(
                    "cloud outbox: flushed {drained} queued push(es) from a previous session"
                );
            }
            let (t, r, s) =
                crate::cloud::pull::reconcile_all(&client, &think, &roadmap, &signal).await;
            if t + r + s > 0 {
                eprintln!("cloud hydrate: merged {t} think / {r} roadmap / {s} signal record(s)");
            }
        });
    }

    // Realtime push-receive: with cloud sync on, subscribe
    // to the backend's /v1/events and refresh the local think/roadmap/signal
    // caches when the tenant's records change remotely (reconnect with
    // backoff; poll fallback while the WS is unavailable). Same gating as
    // write-through.
    if let Some(cloud) = &cloud_client
        && crate::cloud::events::spawn_realtime(
            cloud,
            think_service.engine(),
            roadmap_service.engine(),
            signal_service.engine(),
            // The same subscriber also carries tracker doorbells, so a
            // verified Linear delivery makes the sweep happen now instead of
            // whenever somebody next runs `tracker pull`.
            CliTrackerSweeper,
        )
    {
        eprintln!(
            "cloud realtime: subscribed to {}/v1/events (live refresh for think + roadmap + signal; tracker doorbells)",
            cloud.base_url()
        );
    }

    // The convergence FLOOR (tracker-sweep-schedule). Gated on the TRACKER
    // being configured, NOT on cloud sync — a local-only user with a tracker
    // gets no doorbell (that rides the cloud subscriber above), so they are
    // exactly who this is for.
    {
        let (_, _, tracker_cfg) = tracker_config();
        match sweep_schedule_interval() {
            _ if !crate::tracker::should_project(&tracker_cfg) => {}
            None => eprintln!(
                "tracker sweep: disabled by {SWEEP_INTERVAL_ENV}=0 (run `think-and-ship tracker pull` by hand)"
            ),
            Some(interval) => {
                let provider = tracker_cfg.provider.clone().unwrap_or_default();
                if crate::cloud::events::spawn_sweep_schedule(
                    provider.clone(),
                    interval,
                    CliTrackerSweeper,
                ) {
                    eprintln!(
                        "tracker sweep: checking {provider} every {}s for changes made elsewhere",
                        interval.as_secs()
                    );
                }
            }
        }
    }

    // The outbound twin (tracker-auto-push). Gated on the tracker being
    // configured like the sweep above, and ADDITIONALLY on the operator naming
    // an interval — absent means off, because this one WRITES. See
    // `parse_push_interval` for why the two defaults deliberately differ.
    {
        let (_, _, tracker_cfg) = tracker_config();
        if crate::tracker::should_project(&tracker_cfg)
            && let Some(interval) = push_schedule_interval()
        {
            let provider = tracker_cfg.provider.clone().unwrap_or_default();
            if crate::cloud::events::spawn_push_schedule(
                provider.clone(),
                interval,
                CliTrackerPusher,
            ) {
                eprintln!(
                    "tracker push: mirroring included items to {provider} every {}s",
                    interval.as_secs()
                );
            }
        }
    }

    // The propose switch's announcement (tracker-propose-switch-visibility).
    // Its own block rather than a line inside the sweep-spawn arm, because the
    // DOORBELL proposes too (spawn_realtime carries CliTrackerSweeper), so the
    // switch can be live while the sweep cadence is disabled — a configured
    // and switched-on unattended writer must say so in every such shape.
    // Default-off silence stays silent: no line when the switch is off.
    {
        let (_, _, tracker_cfg) = tracker_config();
        if crate::tracker::should_project(&tracker_cfg) && unattended_propose_enabled() {
            let provider = tracker_cfg.provider.clone().unwrap_or_default();
            eprintln!(
                "tracker propose: writing status/title proposals when unattended sweeps find {provider} changes ({UNATTENDED_PROPOSE_ENV}=on)"
            );
        }
    }

    // Family selection, resolved ONCE here and never per request — the
    // 2026-07-28 core is stateless and tools/list may not vary per connection.
    // A bad value fails the process rather than silently serving fewer tools:
    // a typo that quietly removes a family is indistinguishable, from the
    // client's side, from the tool never having existed.
    let families = crate::mcp::unified::FamilySelection::from_env()
        .with_context(|| format!("invalid {}", crate::mcp::unified::FAMILIES_ENV))?;
    if !families.is_all() {
        eprintln!("tool families: serving {} only", families.summary());
    }

    Ok((
        UnifiedService::new(think_service, ship_service, roadmap_service, signal_service)
            .with_families(families),
        project_id,
    ))
}

/// Overrides the unattended sweep's cadence; `0` turns it off entirely.
pub const SWEEP_INTERVAL_ENV: &str = "THINK_AND_SHIP_TRACKER_SWEEP_SECS";

/// The sweep cadence, or `None` when the operator has switched it off.
///
/// An off switch is not decoration. This is the one task that reaches a
/// third-party API without anybody asking, so somebody who wants that to stop
/// must be able to stop it without also giving up mirroring.
///
/// Split from [`parse_sweep_interval`] so the DECISION is reachable by a test
/// while only the `std::env` read stays untestable. An earlier change shipped
/// a gate inline in a Durable Object that nothing could instantiate, and
/// deliberately breaking it proved the gate was live and uncovered; this is
/// that lesson applied before rather than after.
fn sweep_schedule_interval() -> Option<std::time::Duration> {
    parse_sweep_interval(std::env::var(SWEEP_INTERVAL_ENV).ok().as_deref())
}

/// `None` = absent (use the default), `Some("0")` = off, anything unparseable
/// = the default.
///
/// A typo falls back to the DEFAULT rather than to off, deliberately: going
/// silently quiet is the worse failure for a backstop, because nothing
/// downstream notices a floor that stopped existing.
fn parse_sweep_interval(raw: Option<&str>) -> Option<std::time::Duration> {
    let Some(raw) = raw else {
        return Some(crate::cloud::events::SWEEP_INTERVAL);
    };
    match raw.trim().parse::<u64>() {
        Ok(0) => None,
        Ok(secs) => Some(std::time::Duration::from_secs(secs)),
        Err(_) => Some(crate::cloud::events::SWEEP_INTERVAL),
    }
}

/// Turns the unattended PUSH on, and sets its cadence in seconds. Unset is OFF.
pub const PUSH_INTERVAL_ENV: &str = "THINK_AND_SHIP_TRACKER_PUSH_SECS";

/// The push cadence, or `None` when nobody asked for one.
///
/// Same split as [`sweep_schedule_interval`], so the DECISION is reachable by a
/// test and only the `std::env` read stays untestable.
fn push_schedule_interval() -> Option<std::time::Duration> {
    parse_push_interval(std::env::var(PUSH_INTERVAL_ENV).ok().as_deref())
}

/// `None` = absent, which means OFF — the deliberate asymmetry with
/// [`parse_sweep_interval`], where absent means the default cadence.
///
/// Reading somebody else's tracker on a timer and WRITING to it on a timer are
/// different postures. A user who upgrades and does nothing must see no new
/// network writes at all, so this stays off until a human names an interval.
///
/// The fallback direction inverts for the same reason. An unparseable sweep
/// interval falls back to the default, because a backstop going silently quiet
/// is the worse failure. An unparseable PUSH interval falls back to OFF,
/// because an unattended writer starting up on a typo is the worse failure.
fn parse_push_interval(raw: Option<&str>) -> Option<std::time::Duration> {
    match raw?.trim().parse::<u64>() {
        Ok(0) => None,
        Ok(secs) => Some(std::time::Duration::from_secs(secs)),
        Err(_) => None,
    }
}

/// Lets the unattended sweep (doorbell AND cadence — they are one code path)
/// write STATUS PROPOSALS for genuine remote changes. Unset is OFF.
pub const UNATTENDED_PROPOSE_ENV: &str = "THINK_AND_SHIP_TRACKER_PROPOSE";

/// Whether the unattended sweep may propose. Same split as
/// [`push_schedule_interval`], so the DECISION is reachable by a test and only
/// the `std::env` read stays untestable.
///
/// TWO sources since `mcp-elicitation-consent`: the env var above, and a
/// decision a human made by answering an elicitation prompt (remembered in
/// [`crate::tracker::propose_consent`]). An explicit env value still wins in
/// both directions; the remembered answer only speaks when the env says
/// nothing. Neither speaking is still OFF.
fn unattended_propose_enabled() -> bool {
    let (data_dir, _, _) = tracker_config();
    crate::tracker::propose_consent::resolve(
        std::env::var(UNATTENDED_PROPOSE_ENV).ok().as_deref(),
        &crate::tracker::propose_consent::load(&data_dir),
    )
}

// The parse that used to live here (`parse_propose_switch`) moved to
// `crate::tracker::propose_consent::{env_explicit, resolve}` when the switch
// gained a second source — a human's remembered answer to an elicitation
// prompt. The direction is unchanged and still the WRITER's
// ([`parse_push_interval`], tracker-auto-push): nothing said anywhere is off,
// and a typo is off. What changed is that a typo now ABSTAINS rather than
// asserting "off", so it cannot silently overrule a human who said yes.
//
// A boolean rather than an interval, still deliberately: the cadence already
// exists (the sweep interval and the doorbell). What is consented to here is
// not a schedule but a side effect — may the sweep write down what it saw.

/// Resolve the optional git-native trace sink from the environment.
///
/// Returns `Some((sink, shared))` only when `THINK_AND_SHIP_SYNC_TARGET=repo-git`
/// AND the process is running inside a git repository. Otherwise `None` — the
/// engines fall back to plain XDG persistence (the `Local` default). `shared`
/// comes from `THINK_AND_SHIP_SHARED` (default `false` → gitignored `local/`).
fn resolve_repo_sink() -> Option<(RepoSink, bool)> {
    if SyncTarget::from_env() != SyncTarget::RepoGit {
        return None;
    }
    let cwd = std::env::current_dir().ok()?;
    let root = discover_repo_root(&cwd).or_else(|| {
        eprintln!(
            "think-and-ship: THINK_AND_SHIP_SYNC_TARGET=repo-git but not inside a git \
             repository — falling back to local persistence."
        );
        None
    })?;
    let shared = shared_from_env();
    let partition = if shared {
        "sessions (committed)"
    } else {
        "local (gitignored)"
    };
    eprintln!(
        "git-native traces: {}/.think-and-ship/ → {partition}",
        root.display()
    );
    Some((RepoSink::new(root), shared))
}

async fn run_stdio(unified: UnifiedService) -> Result<()> {
    // A clone kept OUTSIDE the service rmcp takes ownership of, purely so the
    // live OTLP lane can be flushed after the session ends.
    //
    // The `Drop`-based flush is not enough and that was MEASURED, not assumed:
    // a server driven by a finite stdin and exiting immediately POSTed nothing
    // at all — zero requests reached the collector — because rmcp's serve loop
    // does not drop the handler on the path back out. Every span of a short
    // session was lost. This line is the difference.
    let telemetry = unified.clone();
    // `network.transport` per the MCP semconv: stdio is a pipe.
    telemetry.set_transport("pipe");
    let (stdin, stdout) = stdio();
    let running = unified.serve((stdin, stdout)).await?;
    eprintln!("think-and-ship running on stdio");
    let outcome = running.waiting().await;
    // Before the `?`: a session that ended badly is exactly the one whose
    // spans are worth having.
    telemetry.flush_telemetry();
    outcome?;
    Ok(())
}

async fn run_http(addr: SocketAddr, unified: UnifiedService) -> Result<()> {
    // Same reason as `run_stdio`: the service is moved into a factory closure,
    // so the only way the live OTLP lane gets flushed on the way out is a clone
    // held here.
    let telemetry = unified.clone();
    // `network.transport` per the MCP semconv: HTTP rides tcp.
    telemetry.set_transport("tcp");
    let ct = CancellationToken::new();
    let mut config =
        StreamableHttpServerConfig::default().with_cancellation_token(ct.child_token());
    // Both env-driven knobs *replace* rmcp's defaults. Unset → keep defaults
    // (loopback-only host validation, no Origin validation). README documents
    // that public deployments overriding ALLOWED_HOSTS lose the localhost
    // entry unless they include it explicitly.
    if let Some(hosts) = parse_csv_env("THINK_AND_SHIP_HTTP_ALLOWED_HOSTS") {
        eprintln!("http allowed hosts: {hosts:?}");
        config = config.with_allowed_hosts(hosts);
    }
    if let Some(origins) = parse_csv_env("THINK_AND_SHIP_HTTP_ALLOWED_ORIGINS") {
        eprintln!("http allowed origins: {origins:?}");
        config = config.with_allowed_origins(origins);
    }

    let http_service = StreamableHttpService::new(
        move || Ok::<_, std::io::Error>(unified.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );

    let router = axum::Router::new().nest_service("/mcp", http_service);
    // Optional bearer-token auth. Unset env var → no auth layer,
    // so the default --http behaviour is unchanged.
    let bearer = bearer_tokens_from_env();
    if let Some(tokens) = &bearer {
        eprintln!("http bearer auth: {} token(s) required", tokens.len());
    }
    let router = apply_bearer_auth(router, bearer);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding HTTP listener on {addr}"))?;
    let bound = listener
        .local_addr()
        .with_context(|| "reading bound HTTP local_addr")?;
    eprintln!("think-and-ship http on http://{bound}/mcp");

    let served = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            ct.cancel();
        })
        .await;
    telemetry.flush_telemetry();
    served?;
    Ok(())
}

/// Parse a comma-separated env var into a Vec of trimmed, non-empty entries.
/// Returns None when the var is unset, empty, or contains only whitespace and
/// commas (so the caller can leave the rmcp config default in place).
fn parse_csv_env(name: &str) -> Option<Vec<String>> {
    let raw = std::env::var(name).ok()?;
    let entries: Vec<String> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if entries.is_empty() {
        None
    } else {
        Some(entries)
    }
}

/// Bearer-token allowlist from `THINK_AND_SHIP_HTTP_BEARER_TOKENS`
/// (comma-separated). `None` when unset/empty — the caller then mounts no auth
/// layer, preserving the open `--http` default.
fn bearer_tokens_from_env() -> Option<HashSet<String>> {
    Some(
        parse_csv_env("THINK_AND_SHIP_HTTP_BEARER_TOKENS")?
            .into_iter()
            .collect(),
    )
}

/// Decide whether an `Authorization` header value authorizes a request against
/// the bearer allowlist. Accepts exactly `Bearer <token>` (scheme
/// case-insensitive) whose `<token>` is in `allowed`; rejects a missing header,
/// a non-Bearer scheme, an empty token, or an unknown token.
fn is_authorized(auth_header: Option<&str>, allowed: &HashSet<String>) -> bool {
    let Some(header) = auth_header else {
        return false;
    };
    let mut parts = header.splitn(2, ' ');
    let scheme = parts.next().unwrap_or("");
    let token = parts.next().unwrap_or("").trim();
    scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() && allowed.contains(token)
}

/// Wrap `router` with a bearer-token gate when `tokens` is `Some`. Requests
/// without a valid `Authorization: Bearer <token>` get `401` +
/// `WWW-Authenticate: Bearer`. `None` returns the router unchanged.
///
/// `pub` so the HTTP e2e can exercise the real middleware.
pub fn apply_bearer_auth(router: axum::Router, tokens: Option<HashSet<String>>) -> axum::Router {
    use axum::extract::Request;
    use axum::http::{StatusCode, header};
    use axum::middleware::{Next, from_fn};
    use axum::response::IntoResponse;

    let Some(tokens) = tokens else {
        return router;
    };
    let allowed = Arc::new(tokens);
    router.layer(from_fn(move |req: Request, next: Next| {
        let allowed = allowed.clone();
        async move {
            let header = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok());
            if is_authorized(header, &allowed) {
                next.run(req).await
            } else {
                (
                    StatusCode::UNAUTHORIZED,
                    [(header::WWW_AUTHENTICATE, "Bearer")],
                    "unauthorized\n",
                )
                    .into_response()
            }
        }
    }))
}

/// Accept three input shapes:
/// - `:8080`         → `127.0.0.1:8080`
/// - `8080`          → `127.0.0.1:8080`
/// - `host:port`     → parsed as-is
fn parse_http_addr(spec: &str) -> Result<SocketAddr> {
    let spec = spec.trim();
    let normalized = if let Some(port) = spec.strip_prefix(':') {
        format!("{DEFAULT_HTTP_HOST}:{port}")
    } else if spec.parse::<u16>().is_ok() {
        format!("{DEFAULT_HTTP_HOST}:{spec}")
    } else {
        spec.to_string()
    };
    normalized.parse().with_context(|| {
        format!("invalid --http address {spec:?} (expected host:port, :port, or port)")
    })
}

fn init_tracing() {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    // The OTLP log lane is added BESIDE the stderr layer, never instead of it.
    // Stderr is the only channel when no endpoint is configured, so replacing it
    // would mean configuring telemetry silently DELETES the local diagnostic —
    // the operator gains a remote view, loses the one they had, and finds out
    // during the incident. It is also inert unless an endpoint resolves: no
    // endpoint, no thread, no network.
    let otlp = crate::otel_logs::install(&crate::infra::resolve_project_id(None));

    // Best-effort: if a global subscriber is already installed (e.g. by
    // tests), don't fail.
    let _ = tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false),
        )
        .with(otlp)
        .try_init();
}

pub fn init(with_claude_md: bool, full: bool, dry_run: bool, force: bool) -> Result<()> {
    setup::init(with_claude_md, full, dry_run, force)
}

pub fn project_mark(name: Option<&str>, dry_run: bool) -> Result<()> {
    setup::project_mark(name, dry_run)
}

pub fn doctor() -> Result<()> {
    setup::doctor()?;
    report_cross_store_contradictions();
    Ok(())
}

/// The one corruption no single store can see: the same record id held by two
/// projects, each stamping it as its own.
///
/// `doctor` inspects THIS project and would pass a bled store with full marks,
/// because a mis-stamped record is self-consistent — it claims the store it sits
/// in. The contradiction only exists across stores, so this is the only place it
/// can be reported. It names the finding and stops there: a shared id can be an
/// honest collision, and resolving it is `prune --matching`, an operator's call.
fn report_cross_store_contradictions() {
    let cfg = crate::infra::PersistenceConfig::from_env();
    if !cfg.enabled {
        return;
    }
    let persistence = crate::infra::Persistence::new(&cfg, crate::infra::Domain::Roadmap);
    let stores = store_health::load_all_roadmap_stores(persistence.sessions_dir());
    let contested: Vec<_> = store_health::cross_store_duplicates(&stores)
        .into_iter()
        .filter(|d| d.self_claiming().len() > 1)
        .collect();
    if contested.is_empty() {
        return;
    }
    println!(
        "\n⚠ {} chunk id(s) are claimed as their own by MORE THAN ONE project.",
        contested.len()
    );
    println!(
        "  Each copy looks correct inside its own store, so nothing else can see this.\n  \
         At least one stamp is false — a record that bled in and was later claimed."
    );
    for d in contested.iter().take(30) {
        println!("  {}  claimed by: {}", d.id, d.self_claiming().join(", "));
    }
    if contested.len() > 30 {
        println!("  … and {} more.", contested.len() - 30);
    }
    println!(
        "  Decide which project authored them, then remove the other copy with\n  \
         `think-and-ship prune roadmap --matching <id>` from THAT project. Nothing\n  \
         is deleted for you: a shared id can also be an honest collision."
    );
}

/// The rule that used to live only in the docstring of `scripts/purge_sync_bleed.py`,
/// which store-prune-think-signal deleted. It is a BEFORE rule and there is no
/// recovering from ignoring it: a live server holds the store in memory and
/// re-persists it over the prune, so the removed records come straight back.
/// `roadmap prune` already said the mirror-image "restart afterwards", which
/// does not prevent this.
const PRUNE_QUIESCE_WARNING: &str = "\
Before applying: quit every running think-and-ship MCP server for this project
(disconnect the session, or /mcp). A live server holds the store in memory and
will re-persist it over the prune, undoing everything below.";

/// `think-and-ship prune [family]` — remove records that belong to another
/// project, in any family, under one set of rules: dry by default, backup
/// before writing, and never remove a record whose origin can't be proven
/// unless the operator named it. See [`store_health`] for the table.
pub fn prune(
    family: crate::cli::args::PruneFamily,
    matching: &[String],
    apply: bool,
    contested: bool,
) -> Result<()> {
    use crate::cli::args::PruneFamily;

    let persist_cfg = crate::infra::PersistenceConfig::from_env();
    if !persist_cfg.enabled {
        anyhow::bail!(
            "persistence is off, so there is no saved store to prune — \
             set THINK_AND_SHIP_PERSIST=true"
        );
    }
    if apply {
        println!("{PRUNE_QUIESCE_WARNING}\n");
    }

    let mut total_removed = 0usize;
    if matches!(family, PruneFamily::Roadmap | PruneFamily::All) {
        roadmap_prune(matching, apply, contested)?;
    }
    if matches!(family, PruneFamily::Think | PruneFamily::All) {
        total_removed += prune_think(&persist_cfg, matching, apply)?;
    }
    if matches!(family, PruneFamily::Signal | PruneFamily::All) {
        total_removed += prune_signal(&persist_cfg, matching, apply)?;
    }
    if apply && total_removed > 0 {
        println!("\nRestart any running think-and-ship server so it reloads the pruned store.");
    }
    Ok(())
}

/// `think-and-ship adopt [family]` — claim this project's unprovable-origin
/// records as its own, so the ownership table can answer for them from now on.
///
/// The same discipline `prune` pays: dry by default, backup before writing,
/// and one shared decision ([`store_health::adoptable_records`]) rather than a
/// per-family copy. It refuses while anything provably foreign is still in the
/// store, so the natural order is prune, then adopt.
///
/// The think family is deliberately absent: a think step's origin is derived
/// from the `cwd` it was recorded in, so it was never unprovable for a reason a
/// stamp could fix, and there is no field to write. Adopting it would mean
/// inventing an origin column that family does not have.
pub fn adopt(
    family: crate::cli::args::PruneFamily,
    matching: &[String],
    apply: bool,
) -> Result<()> {
    use crate::cli::args::PruneFamily;

    let persist_cfg = crate::infra::PersistenceConfig::from_env();
    if !persist_cfg.enabled {
        anyhow::bail!(
            "persistence is off, so there is no saved store to adopt — \
             set THINK_AND_SHIP_PERSIST=true"
        );
    }
    if apply {
        println!("{PRUNE_QUIESCE_WARNING}\n");
    }

    let mut total = 0usize;
    if matches!(family, PruneFamily::Roadmap | PruneFamily::All) {
        total += adopt_roadmap(&persist_cfg, matching, apply)?;
    }
    if matches!(family, PruneFamily::Signal | PruneFamily::All) {
        total += adopt_signal(&persist_cfg, matching, apply)?;
    }
    if matches!(family, PruneFamily::Think) {
        println!(
            "\nthink: steps carry no origin stamp — their origin is derived from the cwd they \
             were recorded in, which is already provable. Nothing to adopt."
        );
    }
    if apply && total > 0 {
        println!("\nRestart any running think-and-ship server so it reloads the stamped store.");
    }
    Ok(())
}

fn adopt_roadmap(
    persist_cfg: &crate::infra::PersistenceConfig,
    matching: &[String],
    apply: bool,
) -> Result<usize> {
    use crate::roadmap::domain::Roadmap;

    let project_id = resolve_project_id(None);
    let persistence = crate::infra::Persistence::new(persist_cfg, crate::infra::Domain::Roadmap);
    let Some(mut roadmap) = persistence.load::<Roadmap>(&project_id)? else {
        println!("\nroadmap: nothing saved yet for {project_id}.");
        return Ok(0);
    };

    let claimed: std::collections::BTreeSet<String> =
        store_health::adoptable_records(&roadmap.chunks, &project_id, matching)?
            .into_iter()
            .collect();

    println!(
        "\nroadmap for {project_id}: {} chunk(s).",
        roadmap.chunks.len()
    );
    if claimed.is_empty() {
        println!("  Every chunk already states its origin. Nothing to adopt.");
        return Ok(0);
    }
    println!("  {} chunk(s) would be claimed as ours:", claimed.len());
    for id in claimed.iter().take(10) {
        println!("    {id}");
    }
    if claimed.len() > 10 {
        println!("    … and {} more", claimed.len() - 10);
    }
    if !apply {
        println!("  Nothing was changed. Re-run with --apply to stamp them.");
        return Ok(0);
    }

    let store = persistence.path_for(&project_id);
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = store_health::write_backup(&store, &stamp)?;
    println!("  Backed up to {}", backup.display());

    for chunk in &mut roadmap.chunks {
        if claimed.contains(&chunk.id) {
            chunk.project_id = Some(project_id.clone());
        }
    }
    // Plain save, not the merging one: a merge would fold the unstamped copies
    // back in off disk and undo every stamp — the trap roadmap_prune names.
    persistence.save(&project_id, &roadmap)?;
    println!("  Claimed {} chunk(s).", claimed.len());
    Ok(claimed.len())
}

fn adopt_signal(
    persist_cfg: &crate::infra::PersistenceConfig,
    matching: &[String],
    apply: bool,
) -> Result<usize> {
    let project_id = resolve_project_id(None);
    let persistence = crate::infra::Persistence::new(persist_cfg, crate::infra::Domain::Signal);
    let Some(mut signals) = persistence.load::<crate::signal::domain::Signals>(&project_id)? else {
        println!("\nsignal: nothing saved yet for {project_id}.");
        return Ok(0);
    };

    let claimed: std::collections::BTreeSet<String> =
        store_health::adoptable_records(&signals.signals, &project_id, matching)?
            .into_iter()
            .collect();

    println!(
        "\nsignal for {project_id}: {} signal(s).",
        signals.signals.len()
    );
    if claimed.is_empty() {
        println!("  Every signal already states its origin. Nothing to adopt.");
        return Ok(0);
    }
    println!("  {} signal(s) would be claimed as ours:", claimed.len());
    for id in claimed.iter().take(10) {
        println!("    {id}");
    }
    if claimed.len() > 10 {
        println!("    … and {} more", claimed.len() - 10);
    }
    if !apply {
        println!("  Nothing was changed. Re-run with --apply to stamp them.");
        return Ok(0);
    }

    let store = persistence.path_for(&project_id);
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = store_health::write_backup(&store, &stamp)?;
    println!("  Backed up to {}", backup.display());

    for signal in &mut signals.signals {
        if claimed.contains(&signal.id) {
            signal.project_id = Some(project_id.clone());
        }
    }
    persistence.save(&project_id, &signals)?;
    println!("  Claimed {} signal(s).", claimed.len());
    Ok(claimed.len())
}

fn prune_think(
    persist_cfg: &crate::infra::PersistenceConfig,
    matching: &[String],
    apply: bool,
) -> Result<usize> {
    use crate::cli::store_health::{self, ThinkOrigin};

    let project_id = resolve_project_id(None);
    let persistence = crate::think::persistence::Persistence::for_project(
        &crate::infra::PersistenceConfig {
            enabled: persist_cfg.enabled,
            data_dir: persist_cfg.data_dir.clone(),
        },
        &project_id,
    );
    let Some(mut history) = persistence.load_default() else {
        println!("\nthink: nothing saved yet for {project_id}.");
        return Ok(0);
    };

    // The guard: cwd attribution is proof ONLY when our own id
    // is cwd-derived. Under a name override nothing can be proven foreign, and
    // the honest answer is to remove nothing rather than to guess.
    let cwd_is_proof = store_health::cwd_attribution_is_proof(&project_id);
    if !cwd_is_proof {
        println!(
            "\nthink: this project's id is not cwd-derived (a name override is in effect), so no \
             step's origin can be proven. Nothing will be removed — {} step(s) kept.",
            history.steps.len()
        );
        return Ok(0);
    }

    let records: Vec<ThinkOrigin<'_>> = history
        .steps
        .iter()
        .map(|step| ThinkOrigin { step, cwd_is_proof })
        .collect();
    let report = store_health::inspect_records(&records, &project_id);
    let doomed: std::collections::BTreeSet<String> =
        store_health::prunable_records(&records, &project_id, matching)
            .into_iter()
            .collect();

    println!("\nthink for {project_id}: {} step(s).", report.total);
    if report.unstamped > 0 {
        println!(
            "  {} step(s) have no usable cwd. Origin unprovable — kept.",
            report.unstamped
        );
    }
    if doomed.is_empty() {
        println!("  Nothing to remove.");
        return Ok(0);
    }
    println!("  {} step(s) would be removed:", doomed.len());
    for n in doomed.iter().take(10) {
        println!("    step {n}");
    }
    if doomed.len() > 10 {
        println!("    … and {} more", doomed.len() - 10);
    }
    if !apply {
        println!("  Nothing was changed. Re-run with --apply to remove them.");
        return Ok(0);
    }

    let store = persistence.default_store_path();
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = store_health::write_backup(&store, &stamp)?;
    println!("  Backed up to {}", backup.display());

    let before = history.steps.len();
    history
        .steps
        .retain(|s| !doomed.contains(&s.step_number.to_string()));
    let removed = before - history.steps.len();
    // Replacing, not merging: a merge would read the pruned steps straight
    // back off disk and undo the removal — the same trap roadmap_prune names.
    persistence.save_default_replacing(&history);
    println!(
        "  Removed {removed} step(s); {} remain.",
        history.steps.len()
    );
    Ok(removed)
}

fn prune_signal(
    persist_cfg: &crate::infra::PersistenceConfig,
    matching: &[String],
    apply: bool,
) -> Result<usize> {
    use crate::cli::store_health;

    let project_id = resolve_project_id(None);
    let persistence = crate::infra::Persistence::new(persist_cfg, crate::infra::Domain::Signal);
    let Some(mut signals) = persistence.load::<crate::signal::domain::Signals>(&project_id)? else {
        println!("\nsignal: nothing saved yet for {project_id}.");
        return Ok(0);
    };

    let report = store_health::inspect_records(&signals.signals, &project_id);
    let doomed: std::collections::BTreeSet<String> =
        store_health::prunable_records(&signals.signals, &project_id, matching)
            .into_iter()
            .collect();

    println!("\nsignal for {project_id}: {} signal(s).", report.total);
    if report.unstamped > 0 {
        println!(
            "  {} signal(s) predate origin tracking. Origin unprovable — kept unless named with --matching.",
            report.unstamped
        );
        println!(
            "  They will NOT sync until adopted — a push no longer supplies an origin they lack."
        );
    }
    if doomed.is_empty() {
        println!("  Nothing to remove.");
        return Ok(0);
    }
    println!("  {} signal(s) would be removed:", doomed.len());
    for id in doomed.iter().take(10) {
        println!("    {id}");
    }
    if !apply {
        println!("  Nothing was changed. Re-run with --apply to remove them.");
        return Ok(0);
    }

    let store = persistence.path_for(&project_id);
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = store_health::write_backup(&store, &stamp)?;
    println!("  Backed up to {}", backup.display());

    let before = signals.signals.len();
    signals.signals.retain(|s| !doomed.contains(&s.id));
    let removed = before - signals.signals.len();
    persistence.save(&project_id, &signals)?;
    println!(
        "  Removed {removed} signal(s); {} remain.",
        signals.signals.len()
    );
    Ok(removed)
}

/// Install the bundled agent skills into a coding agent's skills directory.
///
/// Twelve harnesses, two scopes, three profiles — every destination recorded
/// in `docs/HARNESSES.md` against the first-party page it came from.
pub fn skills_install(
    client: Option<&str>,
    scope: Option<&str>,
    profile: Option<&str>,
    only: Option<&str>,
    dry_run: bool,
    force: bool,
) -> Result<()> {
    skills::install(client, scope, profile, only, dry_run, force)
}

/// List the bundled skills, their profile, and each harness's install state.
pub fn skills_list(scope: Option<&str>) -> Result<()> {
    skills::list(scope)
}

/// Build an agent's own plugin package for the core skills. Never publishes.
pub fn skills_package(client: &str, out: &std::path::Path, dry_run: bool) -> Result<()> {
    skills::package(client, out, dry_run)
}

/// Retire skill directories this installer no longer writes. Dry-run default.
pub fn skills_migrate(scope: Option<&str>, apply: bool, force: bool) -> Result<()> {
    skills::migrate(scope, apply, force)
}

pub fn status() -> Result<()> {
    // Before the report, not after: `status` is the verb that was LYING on these
    // machines — "Cloud: not connected" on a machine whose agent syncs fine — so
    // it is the one that most has to answer correctly on the very first run.
    adopt_legacy_connection();
    setup::status()
}

/// What an enabled telemetry stream contains — and never contains. Printed at
/// the enable point: with opt-in-everywhere, the enable surface IS the
/// disclosure (decided 2026-06-10).
const TELEMETRY_DISCLOSURE: &str = "\
Anonymized usage data — exactly what would be sent, and what never is:
  • SENT: how many records you have, how they connect to each other, how
    statuses move, which tools you use (as one-way hashes, not names you
    could read back), and rough duration ranges. The shape of your
    workspace, not its contents.
  • NEVER SENT: your writing, your code, titles, descriptions, file paths,
    email addresses, ids, or secrets. We test this by planting fake secrets
    and checking they never make it out; anything that fails that check is
    withheld rather than sent.
  • Off unless you turn it on — on every plan. If you self-host, there is
    nowhere for it to go and it cannot send at all.
  • Turn it off any time: think-and-ship telemetry off";

/// `think-and-ship calls` — per-tool invocation counts, read from the usage
/// partition with no server running.
///
/// The question these answer ("which verbs are actually hot?") is asked
/// *between* sessions, so a read that needs a live server would not answer it.
/// Nothing here sends anything anywhere; see [`crate::usage`] for why a local
/// counter is not the thing the telemetry opt-in governs.
///
/// `tool` asks the one question the raw table cannot answer — *is this verb
/// cold?* — and the answer routes through [`crate::usage::CallCounts::verdict`]
/// so that a zero read before the soak window is met comes back as "not yet
/// evidence" rather than as a number a reader may misinterpret.
pub fn calls(json: bool, tool: Option<&str>) -> Result<()> {
    let project_id = crate::infra::resolve_project_id(None);
    let counts = crate::usage::load(&project_id).unwrap_or_default();
    let soak = counts.soak();

    if json {
        let mut out = serde_json::to_value(&counts)?;
        if let Some(obj) = out.as_object_mut() {
            obj.insert("soak".to_string(), serde_json::to_value(&soak)?);
            if let Some(tool) = tool {
                obj.insert(
                    "verdict".to_string(),
                    serde_json::json!(verdict_line(&counts, tool)),
                );
            }
        }
        println!("{}", serde_json::to_string_pretty(&out)?);
        return Ok(());
    }

    if let Some(tool) = tool {
        println!("{tool}: {}", verdict_line(&counts, tool));
        return Ok(());
    }

    if counts.counts.is_empty() {
        println!("tool calls: nothing counted yet for {project_id}");
        if !crate::infra::PersistenceConfig::from_env().enabled {
            println!(
                "  Counting writes to the same store as everything else, so it is off \
                 while THINK_AND_SHIP_PERSIST is."
            );
        } else if !crate::usage::counting_enabled(
            std::env::var(crate::usage::CALL_COUNTS_ENV).ok().as_deref(),
        ) {
            println!(
                "  Counting is off ({}). Unset it to count again.",
                crate::usage::CALL_COUNTS_ENV
            );
        }
        return Ok(());
    }

    let ranked = counts.ranked();
    let width = ranked.iter().map(|(name, _)| name.len()).max().unwrap_or(4);
    println!("tool calls for {project_id}");
    if !counts.updated_at.is_empty() {
        println!("  last call: {}", counts.updated_at);
    }
    println!();
    for (name, n) in &ranked {
        println!("  {name:<width$}  {n:>8}");
    }
    println!();
    println!(
        "  {:<width$}  {:>8}  ({} distinct)",
        "TOTAL",
        counts.total(),
        ranked.len()
    );
    println!();
    print_soak(&soak);
    Ok(())
}

/// The observation window, printed whether or not it is met — a table without
/// it invites exactly the day-one misreading this whole module exists to stop.
fn print_soak(soak: &crate::usage::Soak) {
    if soak.met {
        println!(
            "  soak MET ({} calls over {} active days) — a zero here is evidence, \
             provided the verb's family was exercised.",
            soak.total_calls, soak.active_days
        );
        return;
    }
    println!(
        "  soak NOT MET ({} calls over {} active days). A zero is NOT yet evidence \
         a verb is unused — it may just be a workflow nobody has run on this build.",
        soak.total_calls, soak.active_days
    );
    for missing in &soak.missing {
        println!("    still needs: {missing}");
    }
}

/// One line of prose for a per-verb verdict. Every branch names *why* the
/// reading is or is not usable, because the number alone is what misleads.
fn verdict_line(counts: &crate::usage::CallCounts, tool: &str) -> String {
    use crate::usage::Verdict;
    match counts.verdict(tool) {
        Verdict::Used(n) => format!("{n} call(s) — used."),
        Verdict::Cold => "0 calls — COLD. The soak window is met and its family was exercised, \
             so this zero is evidence."
            .to_string(),
        Verdict::SoakTooShort(soak) => format!(
            "0 calls — NOT EVIDENCE. The soak window is not met ({} calls over {} \
             active days); still needs {}.",
            soak.total_calls,
            soak.active_days,
            soak.missing.join("; ")
        ),
        Verdict::FamilyUnexercised { family } => format!(
            "0 calls — NOT EVIDENCE. The whole {family}_* family reads 0, so the \
             workflow that would use this verb was never run."
        ),
    }
}

pub fn telemetry_status() -> Result<()> {
    let data_dir = crate::infra::PersistenceConfig::from_env().data_dir;
    let state = crate::telemetry::consent::load(&data_dir);
    let setting = if state.enabled { "ENABLED" } else { "disabled" };
    match &state.decided_at {
        Some(at) => println!("telemetry: {setting} (explicitly, {at})"),
        None => println!("telemetry: {setting} (default — never decided)"),
    }
    println!("\n{TELEMETRY_DISCLOSURE}");
    Ok(())
}

/// Resolve the telemetry ingest endpoint from the environment. Unset, or set
/// empty, means there is nowhere to send: telemetry is structurally zero and
/// consent never gets the chance to matter. There is no built-in endpoint.
fn telemetry_endpoint() -> Option<String> {
    match std::env::var(crate::telemetry::egress::TELEMETRY_URL_VAR) {
        Ok(value) if value.trim().is_empty() => None,
        Ok(value) => Some(value),
        Err(_) => None,
    }
}

/// Explicit one-shot telemetry push: consent-gated, read-only collection,
/// structural shape only. There is no background sender — pushes happen only
/// when a human runs this.
pub fn telemetry_push(dry_run: bool) -> Result<()> {
    let data_dir = crate::infra::PersistenceConfig::from_env().data_dir;
    let consent = crate::telemetry::consent::load(&data_dir);
    let endpoint = telemetry_endpoint();
    if !crate::telemetry::consent::should_send(&consent, endpoint.as_deref()) {
        let why = if endpoint.is_none() {
            "no telemetry endpoint is configured"
        } else {
            "consent is not enabled (run `think-and-ship telemetry on`)"
        };
        println!("telemetry push: nothing sent — {why}.");
        return Ok(());
    }
    let (project_id, envelopes) = collect_local_envelopes();
    let salt = crate::telemetry::egress::load_or_create_salt(&data_dir)?;
    let shape = crate::telemetry::shape::extract(&envelopes, &salt)
        .map_err(|e| anyhow::anyhow!("shape extraction refused: {e}"))?;
    let report = crate::telemetry::egress::build_report(&salt, shape);
    println!(
        "telemetry push (project {project_id}): structural shape of {} record(s), install {}",
        envelopes.len(),
        report.install,
    );
    if dry_run {
        println!("--dry-run: nothing sent. Shape:");
        println!("{}", serde_json::to_string_pretty(&report.shape)?);
        return Ok(());
    }
    let endpoint = endpoint.unwrap_or_default();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(crate::telemetry::egress::send_report(
            &reqwest::Client::new(),
            &endpoint,
            &report,
        ))?;
    println!("sent.");
    Ok(())
}

pub fn telemetry_set(enabled: bool) -> Result<()> {
    if enabled {
        println!("{TELEMETRY_DISCLOSURE}\n");
    }
    let data_dir = crate::infra::PersistenceConfig::from_env().data_dir;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let state = crate::telemetry::consent::set(&data_dir, enabled, &now)?;
    println!(
        "telemetry is now {}.",
        if state.enabled { "ENABLED" } else { "disabled" }
    );
    Ok(())
}

/// Build a persistence-backed roadmap engine for the current project, loading
/// any prior native roadmap off disk. Mirrors `build_unified`'s roadmap
/// construction (minus the broadcaster/repo-sink, which a one-shot CLI call
/// doesn't need).
fn load_roadmap_engine() -> crate::roadmap::RoadmapEngine {
    let project_id = crate::infra::resolve_project_id(None);
    let persistence = crate::infra::Persistence::new(
        &crate::infra::PersistenceConfig::from_env(),
        crate::infra::Domain::Roadmap,
    );
    crate::roadmap::RoadmapEngine::new(project_id)
        .with_persistence(persistence)
        .with_opt_in_inheritance(inherited_opt_in_provider())
}

/// The provider new chunks are born opted in to, read from this project's
/// tracker config. One of the two composition roots calls this; the other is
/// [`build_unified`]. The DECISION itself is
/// [`crate::tracker::config::inherited_opt_in`] — this is only the disk read,
/// kept apart from it so the rule stays reachable by a test that owns no files.
fn inherited_opt_in_provider() -> Option<String> {
    let (_, _, config) = tracker_config();
    crate::tracker::inherited_opt_in(&config).map(str::to_string)
}

/// Render the native roadmap as a markdown (default) or json projection. The
/// output is a generated *view* of native state — `import` is the inverse seed.
pub fn export(format: &str) -> Result<()> {
    let engine = load_roadmap_engine();
    println!("{}", engine.export(format));
    Ok(())
}

/// `think-and-ship roadmap next` — the chunk the agent should pick up: the
/// most urgent pending chunk (smallest priority number) that carries no blocker and whose
/// dependencies are all done. Same selection the `roadmap_next` MCP tool
/// makes, so the CLI and the agent never disagree about what's next — asserted
/// by `tests/roadmap_next_command_seam.rs`, which runs this very binary.
pub fn roadmap_next() -> Result<()> {
    let engine = load_roadmap_engine();
    match engine.next() {
        Some(chunk) => {
            println!("{}  {}", chunk.id, chunk.title);
            if !chunk.description.is_empty() {
                println!("\n{}", chunk.description);
            }
            if !chunk.acceptance.is_empty() {
                println!("\nAcceptance:");
                for a in &chunk.acceptance {
                    println!("  - {a}");
                }
            }
        }
        // An empty answer has two causes now, and saying the wrong one sends a
        // reader looking for a dependency that is not the problem. Count the
        // chunks held back by a blocker rather than guessing.
        None => {
            let held = engine
                .roadmap()
                .chunks
                .iter()
                .filter(|c| {
                    c.status == crate::roadmap::domain::ChunkStatus::Pending
                        && c.blocked_by.is_some()
                })
                .count();
            if held > 0 {
                println!(
                    "Nothing is ready to start. {held} pending chunk(s) carry a blocker and are \
                     skipped — they keep their priority; clear one with `roadmap unblock --id <id>`."
                );
            } else {
                println!(
                    "Nothing is ready to start: no pending chunk has all of its dependencies done."
                );
            }
        }
    }
    Ok(())
}

/// `think-and-ship roadmap block` — record why a chunk cannot be worked, when
/// the answer is not another chunk.
///
/// This and [`roadmap_unblock`] exist so the CLI and `roadmap_update_chunk`
/// never disagree about what a chunk says. They deliberately go through the
/// SAME engine verbs the MCP handler uses — `validate_blocked_by` then
/// `set_blocked_by` — rather than reimplementing the rules for a second
/// surface, so a change to what counts as a legal blocker reaches both at once.
pub fn roadmap_block(id: &str, kind: &str, reason: String, evidence: Option<String>) -> Result<()> {
    let kind = crate::roadmap::domain::BlockerKind::from_wire(kind).map_err(anyhow::Error::msg)?;
    let mut engine = load_roadmap_engine();
    let blocked_by = engine
        .validate_blocked_by(kind, reason, evidence)
        .map_err(anyhow::Error::msg)?;
    let chunk = engine
        .set_blocked_by(id, blocked_by)
        .map_err(anyhow::Error::msg)?;
    let b = chunk.blocked_by.as_ref().expect("just set");
    println!("{} blocked: {} — {}", chunk.id, b.kind.as_wire(), b.reason);
    if let Some(e) = &b.evidence {
        println!("  evidence: {e}");
    }
    Ok(())
}

/// `think-and-ship roadmap unblock` — retract a chunk's blocker.
///
/// Errors when there is no blocker, inheriting that from
/// [`crate::roadmap::RoadmapEngine::clear_blocked_by`]: a clear that prints
/// success has to mean something changed.
pub fn roadmap_unblock(id: &str) -> Result<()> {
    let mut engine = load_roadmap_engine();
    let chunk = engine.clear_blocked_by(id).map_err(anyhow::Error::msg)?;
    println!("{} unblocked", chunk.id);
    Ok(())
}

/// `think-and-ship roadmap prune` — remove chunks that belong to another
/// project. Dry by default: it prints what it would remove and writes nothing
/// unless `--apply`, backs the store up first, and never removes a chunk whose
/// origin can't be proven unless the operator named it. See
/// [`store_health`] for why that last rule is the whole point.
pub fn roadmap_prune(matching: &[String], apply: bool, contested: bool) -> Result<()> {
    use crate::roadmap::domain::Roadmap;

    let persist_cfg = crate::infra::PersistenceConfig::from_env();
    if !persist_cfg.enabled {
        anyhow::bail!(
            "persistence is off, so there is no saved roadmap to prune — \
             set THINK_AND_SHIP_PERSIST=true"
        );
    }
    let project_id = resolve_project_id(None);
    let persistence = crate::infra::Persistence::new(&persist_cfg, crate::infra::Domain::Roadmap);
    let Some(mut roadmap) = persistence.load::<Roadmap>(&project_id)? else {
        println!("No saved roadmap for {project_id} — nothing to prune.");
        return Ok(());
    };

    let report = store_health::inspect(&roadmap, &project_id);
    let mut doomed = store_health::prunable(&roadmap, &project_id, matching);

    // The contested row: records THIS project claims, where another store claims
    // the same id. Off unless asked for, gated on --matching, and gated again on
    // evidence gathered from the other stores — a claimed record is never
    // removable on an operator's say-so alone.
    if contested {
        if matching.is_empty() {
            anyhow::bail!(
                "--contested needs --matching. A contested id proves one of two stamps is \
                 false and never which one, so the ids to remove must be named."
            );
        }
        let stores = store_health::load_all_roadmap_stores(persistence.sessions_dir());
        let evidence: std::collections::BTreeSet<String> =
            store_health::cross_store_duplicates(&stores)
                .into_iter()
                .filter(|d| d.self_claiming().len() > 1)
                .map(|d| d.id)
                .collect();
        let extra =
            store_health::prunable_contested(&roadmap.chunks, &project_id, matching, &evidence);
        if extra.is_empty() {
            println!(
                "  --contested found nothing: no chunk you named is also claimed by another \
                 project's store."
            );
        }
        doomed.extend(extra);
        doomed.sort();
        doomed.dedup();
    }

    println!("Roadmap for {project_id}: {} chunk(s).", report.total);
    if report.unstamped > 0 {
        println!(
            "  {} chunk(s) predate origin tracking. They are kept unless you name them with --matching.",
            report.unstamped
        );
        // The consequence, said out loud. A record with no provable origin is no
        // longer stamped by whoever pushes it — that is what let a cleanup in
        // one project silently claim 22 chunks bled in from another. The cost of
        // refusing to guess is that these records do not round-trip, and an
        // operator who is not told simply watches sync go quiet.
        println!(
            "  They will NOT sync to the cloud until they have an origin: a push no longer\n  \
             stamps them with whoever pushed (that is how one project claimed another's work).\n  \
             Run `think-and-ship adopt` here to claim them as {project_id}'s, once."
        );
    }
    if doomed.is_empty() {
        println!("\nNothing to remove.");
        return Ok(());
    }

    println!("\n{} chunk(s) would be removed:", doomed.len());
    for id in &doomed {
        let owner = roadmap
            .chunks
            .iter()
            .find(|c| &c.id == id)
            .and_then(|c| c.project_id.clone())
            .unwrap_or_else(|| "origin untracked, named by --matching".to_string());
        println!("  {id}  ({owner})");
    }

    if !apply {
        println!("\nNothing was changed. Re-run with --apply to remove them.");
        return Ok(());
    }

    // Back up before writing, so a prune is always undoable by hand.
    let store = persistence.path_for(&project_id);
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = store_health::write_backup(&store, &stamp)?;
    println!("\nBacked up to {}", backup.display());

    let before = roadmap.chunks.len();
    roadmap.chunks.retain(|c| !doomed.contains(&c.id));
    let removed = before - roadmap.chunks.len();
    // Plain save, not the merging one: a merge would fold the removed chunks
    // straight back in from disk.
    persistence.save(&project_id, &roadmap)?;
    println!(
        "Removed {removed} chunk(s); {} remain.",
        roadmap.chunks.len()
    );
    println!(
        "Restart any running think-and-ship server so it reloads the pruned store \
         (a live server still holds the old one in memory)."
    );
    Ok(())
}

/// `think-and-ship roadmap regions` — report the region map, or re-author it.
///
/// Regions are the places the tech-tree canvas is navigated by, and the clauses
/// they are judged against live in [`crate::roadmap::region`]. Read-only with no
/// `--file`; with one, dry by default in the same shape as
/// [`roadmap_prune`] — it says what would change and writes nothing without
/// `--apply`.
///
/// The map is a file rather than a flag because it is authored. There is no
/// derivation from a chunk's own fields that is not the slug the constraint
/// exists to reject, so a region assignment is a decision somebody made, and the
/// file is where that decision is written down and can be re-applied.
pub fn roadmap_regions(file: Option<&str>, apply: bool) -> Result<()> {
    use crate::roadmap::domain::Roadmap;
    use crate::roadmap::region;

    let persist_cfg = crate::infra::PersistenceConfig::from_env();
    if !persist_cfg.enabled {
        anyhow::bail!(
            "persistence is off, so there is no saved roadmap to audit — \
             set THINK_AND_SHIP_PERSIST=true"
        );
    }
    let project_id = resolve_project_id(None);
    let persistence = crate::infra::Persistence::new(&persist_cfg, crate::infra::Domain::Roadmap);
    let Some(mut roadmap) = persistence.load::<Roadmap>(&project_id)? else {
        println!("No saved roadmap for {project_id} — nothing to put on a map.");
        return Ok(());
    };

    if apply && file.is_none() {
        anyhow::bail!("--apply needs --file: there is nothing to apply without a region map");
    }

    let mut assigned = 0usize;
    if let Some(path) = file {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading the region map {path}"))?;
        let map: std::collections::BTreeMap<String, Vec<String>> = serde_json::from_str(&raw)
            .with_context(|| {
                format!("parsing {path} as a JSON object of region name to chunk ids")
            })?;

        let known: std::collections::BTreeSet<&str> =
            roadmap.chunks.iter().map(|c| c.id.as_str()).collect();
        let unknown: Vec<&String> = map
            .values()
            .flatten()
            .filter(|id| !known.contains(id.as_str()))
            .collect();
        if !unknown.is_empty() {
            println!(
                "{} id(s) in {path} name no chunk in this roadmap and are ignored:",
                unknown.len()
            );
            for id in unknown.iter().take(10) {
                println!("  {id}");
            }
        }

        let placement: std::collections::BTreeMap<&str, &str> = map
            .iter()
            .flat_map(|(region, ids)| ids.iter().map(move |id| (id.as_str(), region.as_str())))
            .collect();
        for chunk in &mut roadmap.chunks {
            if let Some(region) = placement.get(chunk.id.as_str()) {
                let region = (*region).to_string();
                if chunk.group.as_deref() != Some(region.as_str()) {
                    chunk.group = Some(region);
                    assigned += 1;
                }
            }
        }
        println!("{assigned} chunk(s) would change region.\n");
    }

    let report = region::audit(
        roadmap
            .chunks
            .iter()
            .map(|c| (c.id.as_str(), c.group.as_deref())),
    );
    println!(
        "Region map for {project_id} — {} chunk(s) across {} region(s), median {}.\n",
        report.total,
        report.regions(),
        report.median
    );
    for (name, pop) in &report.populations {
        println!("  {pop:>4}  {name}");
    }
    if !report.homeless.is_empty() {
        println!("  {:>4}  (no region)", report.homeless.len());
    }

    let failures = report.failures();
    if failures.is_empty() {
        println!("\nThe map satisfies every clause of C7.");
    } else {
        println!("\n{} clause(s) fail:", failures.len());
        for f in &failures {
            println!("  - {f}");
        }
    }

    if file.is_none() {
        return Ok(());
    }
    if !apply {
        println!("\nNothing was changed. Re-run with --apply to write these regions.");
        return Ok(());
    }
    if assigned == 0 {
        println!("\nEvery chunk already sits where the map puts it; nothing written.");
        return Ok(());
    }

    // Back up before writing, so a re-authored map is always undoable by hand.
    let store = persistence.path_for(&project_id);
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let backup = store_health::write_backup(&store, &stamp)?;
    println!("\nBacked up to {}", backup.display());

    // Plain save, not the merging one: a merge folds the old regions back in
    // from disk.
    persistence.save(&project_id, &roadmap)?;
    println!("Wrote {assigned} region assignment(s).");
    println!(
        "Restart any running think-and-ship server so it reloads the store \
         (a live server still holds the old regions in memory)."
    );
    Ok(())
}

/// `think-and-ship roadmap status` — the plan at a glance. Distinct from the
/// top-level `status`, which reports the installation rather than the work.
pub fn roadmap_status() -> Result<()> {
    let engine = load_roadmap_engine();
    let status = engine.status();
    let count = |key: &str| status["counts"][key].as_u64().unwrap_or(0);

    println!(
        "Roadmap — {}\n",
        status["project_id"].as_str().unwrap_or("unknown project")
    );
    println!(
        "  in progress {}   pending {}   blocked {}   backlog {}   done {}   obsoleted {}",
        count("in_progress"),
        count("pending"),
        count("blocked"),
        count("backlog"),
        count("done"),
        count("obsoleted"),
    );
    // The blocker line, printed only when there is one to print. It sits on its
    // own row rather than in the status run above because it is CROSS-CUTTING:
    // every chunk counted here is also counted there, and putting it in the
    // same run would read as a seventh bucket that the others were taken from.
    //
    // Printed at all because a count no human at a terminal can see is not a
    // count they have: `status()` carries the tally, and the agent surface
    // reads it, but the CLI is where a person asks "why is nothing moving".
    let blocked_by = &status["counts"]["blocked_by"];
    if blocked_by["total"].as_u64().unwrap_or(0) > 0 {
        let by_kind: Vec<String> = crate::roadmap::domain::BlockerKind::ALL
            .iter()
            .filter_map(|k| {
                let n = blocked_by[k.as_wire()].as_u64().unwrap_or(0);
                (n > 0).then(|| format!("{} {n}", k.as_wire()))
            })
            .collect();
        println!(
            "\n  blocked by (not a dependency): {}   [{}]",
            blocked_by["total"].as_u64().unwrap_or(0),
            by_kind.join(", ")
        );
    }
    match status["next"].as_str() {
        Some(next) => println!("\n  next: {next}"),
        None => println!("\n  next: nothing ready"),
    }
    if let Some(note) = status["note"].as_str().filter(|n| !n.is_empty()) {
        println!("  {note}");
    }
    Ok(())
}

/// Build the first-party corpus from the local stores, READ-ONLY (mirrors
/// `collect_local_envelopes`: no cloud client / broadcaster / repo-sink is
/// wired, so enumeration can never mutate or trigger write-through).
fn build_local_corpus() -> crate::corpus::Corpus {
    let project_id = resolve_project_id(None);
    let think = ReasoningServer::new_for_project(load_think_config(), project_id.clone());
    let roadmap = RoadmapEngine::new(project_id.clone()).with_persistence(InfraPersistence::new(
        &InfraPersistenceConfig::from_env(),
        Domain::Roadmap,
    ));
    let signal = SignalEngine::new(project_id.clone()).with_persistence(InfraPersistence::new(
        &InfraPersistenceConfig::from_env(),
        Domain::Signal,
    ));
    crate::corpus::build_corpus(
        &project_id,
        &roadmap.roadmap().chunks,
        think.all_steps(),
        &signal.signals().signals,
    )
}

/// `think-and-ship hygiene` (stale-risk-signals): flag stalled in_progress
/// and ready-but-idle pending chunks as signals in the normal triage inbox.
/// Pure detection over the local stores; throttled against existing signals
/// (live or recent ones — including dismissals — suppress re-emission).
pub fn hygiene(dry_run: bool, stall_days: i64, idle_days: i64) -> Result<()> {
    let project_id = resolve_project_id(None);
    let roadmap = RoadmapEngine::new(project_id.clone()).with_persistence(InfraPersistence::new(
        &InfraPersistenceConfig::from_env(),
        Domain::Roadmap,
    ));
    let mut signal = SignalEngine::new(project_id).with_persistence(InfraPersistence::new(
        &InfraPersistenceConfig::from_env(),
        Domain::Signal,
    ));
    let opts = crate::hygiene::HygieneOptions {
        stall_days,
        idle_days,
        ..Default::default()
    };
    let findings = crate::hygiene::detect(
        &roadmap.roadmap().chunks,
        &signal.signals().signals,
        &chrono::Utc::now().to_rfc3339(),
        opts,
    );
    if findings.is_empty() {
        println!("hygiene: nothing stalled or idle (or already signalled) — the roadmap is clean.");
        return Ok(());
    }
    for f in &findings {
        println!(
            "{} chunk:{} — {}",
            if dry_run { "would flag" } else { "flagged" },
            f.chunk_id,
            f.reason
        );
        if !dry_run {
            let id = signal
                .capture(
                    crate::signal::domain::SignalKind::Concern,
                    "hygiene".into(),
                    f.reason.clone(),
                )
                .id
                .clone();
            signal
                .link(&id, &format!("chunk:{}", f.chunk_id))
                .map_err(|e| anyhow::anyhow!(e))?;
        }
    }
    println!(
        "{} finding(s){}",
        findings.len(),
        if dry_run {
            " (dry run — nothing captured)"
        } else {
            " captured as signals"
        }
    );
    Ok(())
}

/// `think-and-ship repair [--dry-run]` (cli-renumber-duplication): drop
/// renumbered clones of the same think step left behind by concurrent
/// writers with divergent numberings. Works directly on the persisted
/// stores (no engine construction — construction itself now auto-repairs;
/// this command exists for the explicit before/after report and dry-run).
pub fn repair(dry_run: bool) -> Result<()> {
    use crate::think::domain::SessionEntry;
    use crate::think::engine::numbering::dedupe_project_clones;
    use crate::think::persistence::Persistence as ThinkPersistence;

    let project_id = resolve_project_id(None);
    let think_cfg = load_think_config();
    let pers = ThinkPersistence::for_project(&think_cfg.persistence, &project_id);
    if !pers.enabled() {
        anyhow::bail!(
            "persistence is disabled — set THINK_AND_SHIP_PERSIST=true so repair can read the stores"
        );
    }
    let Some(mut history) = pers.load_default() else {
        println!("repair: no persisted think trace for project '{project_id}' — nothing to do.");
        return Ok(());
    };
    let mut sessions: std::collections::HashMap<String, SessionEntry> = pers
        .load_sessions()
        .into_iter()
        .map(|(sid, h)| {
            (
                sid,
                SessionEntry {
                    history: h,
                    last_accessed: 0,
                },
            )
        })
        .collect();

    let report = dedupe_project_clones(&mut history, &mut sessions, &project_id, &pers, dry_run);
    println!(
        "repair: {} step(s) before, {} renumbered clone(s) {}, {} step(s) after",
        report.total_steps,
        report.clones_dropped,
        if dry_run {
            "found (dry run — nothing written)"
        } else {
            "dropped"
        },
        report.total_steps - report.clones_dropped
    );
    Ok(())
}

/// `think-and-ship trace export` (otel-genai-export): map the local
/// stores onto OTLP/HTTP JSON with GenAI agent spans. POST the output to any
/// OTLP endpoint (Jaeger :4318, an OTel collector, Datadog) — README has the
/// one-command demo. Read-only store access; deterministic ids.
pub fn trace_export_otel(out: Option<&str>) -> Result<()> {
    let project_id = resolve_project_id(None);
    let think = ReasoningServer::new_for_project(load_think_config(), project_id.clone());
    let ship = ShipEngine::new(project_id.clone())
        .with_persistence(ShipPersistence::new(&ShipPersistenceConfig::from_env()));
    // A caller context adopted by the MCP server in an earlier process. When
    // present the export joins that trace instead of minting its own (SEP-414).
    let inbound = crate::trace_context::load(&project_id);
    let export = crate::otel::build_otel(
        &project_id,
        ship.objective.as_ref().map(|o| (o, ship.tasks.as_slice())),
        think.all_steps(),
        inbound.as_ref(),
    );
    let problems = crate::otel::validate_otlp_with_external_parent(
        &export.body,
        inbound.as_ref().map(|c| c.parent_span_id.as_str()),
    );
    if !problems.is_empty() {
        anyhow::bail!(
            "export failed structural validation: {}",
            problems.join("; ")
        );
    }
    let body = serde_json::to_string_pretty(&export.body)?;
    // Announced on stderr so stdout stays a clean OTLP body for piping.
    if let Some(c) = inbound.as_ref() {
        eprintln!(
            "joined caller trace {} (root parents to span {}, adopted {})",
            c.trace_id, c.parent_span_id, c.adopted_at
        );
    }
    match out {
        Some(path) => {
            std::fs::write(path, &body)?;
            eprintln!(
                "wrote {} spans (project {}) to {path}{}",
                export.spans,
                project_id,
                if export.skipped_steps > 0 {
                    format!(
                        " — skipped {} step(s) without timestamps",
                        export.skipped_steps
                    )
                } else {
                    String::new()
                }
            );
        }
        None => println!("{body}"),
    }
    Ok(())
}

/// `think-and-ship corpus export`: write the versioned,
/// digest-stamped JSONL corpus. The file stays local, and the export reads
/// only this workspace's own first-party stores.
pub fn corpus_export(out: Option<&str>) -> Result<()> {
    let corpus = build_local_corpus();
    let jsonl = crate::corpus::to_jsonl(&corpus);
    match out {
        Some(path) => {
            std::fs::write(path, &jsonl)?;
            eprintln!(
                "wrote corpus v{} ({} events, project {}) to {path}",
                corpus.version,
                corpus.events.len(),
                corpus.project
            );
        }
        None => print!("{jsonl}"),
    }
    Ok(())
}

/// `think-and-ship eval`: replay chunk-completion history and
/// report top-1/top-3 accuracy with raw hit counts. Modes:
/// - default: static baselines on the FULL replay;
/// - `--learned`: the original fixed 70/30 temporal split (kept for
///   continuity);
/// - `--prequential`: the streaming-eval standard — every case past
///   `--warmup` is predicted by a model trained only on its past; static
///   baselines are scored on the SAME post-warmup cases; all learned
///   variants (listwise/pairwise × uniform/decay × ±think-adjacency) are
///   reported side by side. Deterministic end to end.
pub fn eval_run(
    corpus_path: Option<&str>,
    learned: bool,
    prequential: bool,
    warmup: usize,
    weights_out: Option<&str>,
) -> Result<()> {
    use crate::corpus::eval::{Score, baselines, replay_cases, score};
    use crate::corpus::learn::{
        FeatureContext, Loss, TrainOpts, prequential as preq, rank, temporal_split, train,
    };

    let corpus = match corpus_path {
        Some(path) => crate::corpus::parse_jsonl(&std::fs::read_to_string(path)?)
            .map_err(|e| anyhow::anyhow!("{path}: {e}"))?,
        None => build_local_corpus(),
    };
    let cases = replay_cases(&corpus);
    let ctx = FeatureContext::from_corpus(&corpus);
    println!(
        "corpus v{} · project {} · {} events · {} replay cases (next-chunk prediction)",
        corpus.version,
        corpus.project,
        corpus.events.len(),
        cases.len()
    );

    let print_row = |s: &Score| {
        println!(
            "{:<38} {:>4}/{:<3} {:>6.1}% {:>4}/{:<3} {:>6.1}%",
            s.name,
            s.top1,
            s.cases,
            Score::pct(s.top1, s.cases),
            s.top3,
            s.cases,
            Score::pct(s.top3, s.cases),
        );
    };
    let header = || {
        println!(
            "{:<38} {:>8} {:>7} {:>8} {:>7}",
            "predictor", "top-1", "", "top-3", ""
        );
    };
    let digest = crate::corpus::to_jsonl(&corpus)
        .lines()
        .next()
        .and_then(|h| serde_json::from_str::<serde_json::Value>(h).ok())
        .and_then(|v| v.get("digest").and_then(|d| d.as_str()).map(str::to_owned))
        .unwrap_or_default();

    if prequential {
        let warmup = warmup.min(cases.len());
        let tail = &cases[warmup..];
        println!(
            "prequential (test-then-train): warmup {warmup} · {} scored cases · corpus {digest}",
            tail.len()
        );
        header();
        for (name, predictor) in baselines() {
            print_row(&score(name, tail, predictor));
        }
        let variants: [(&str, TrainOpts); 6] = [
            (
                "learned listwise",
                TrainOpts {
                    loss: Loss::Listwise,
                    decay: 1.0,
                    use_adjacency: false,
                },
            ),
            (
                "learned listwise +adjacency",
                TrainOpts {
                    loss: Loss::Listwise,
                    decay: 1.0,
                    use_adjacency: true,
                },
            ),
            (
                "learned listwise +adj +decay.95",
                TrainOpts {
                    loss: Loss::Listwise,
                    decay: 0.95,
                    use_adjacency: true,
                },
            ),
            (
                "learned pairwise",
                TrainOpts {
                    loss: Loss::Pairwise,
                    decay: 1.0,
                    use_adjacency: false,
                },
            ),
            (
                "learned pairwise +adjacency",
                TrainOpts {
                    loss: Loss::Pairwise,
                    decay: 1.0,
                    use_adjacency: true,
                },
            ),
            (
                "learned pairwise +adj +decay.95",
                TrainOpts {
                    loss: Loss::Pairwise,
                    decay: 0.95,
                    use_adjacency: true,
                },
            ),
        ];
        for (name, opts) in variants {
            print_row(&preq(&cases, &ctx, warmup, opts, name));
        }
        return Ok(());
    }

    if !learned {
        header();
        for (name, predictor) in baselines() {
            print_row(&score(name, &cases, predictor));
        }
        return Ok(());
    }

    // --learned: the original fixed temporal 70/30 split.
    let (train_set, holdout) = temporal_split(&cases, 0.7);
    let opts = TrainOpts {
        loss: Loss::Listwise,
        decay: 1.0,
        use_adjacency: false,
    };
    let model = train(train_set, &ctx, opts, &digest);
    println!(
        "split: {} train / {} holdout (time-ordered) · corpus {digest}",
        train_set.len(),
        holdout.len()
    );
    println!(
        "learned weights: {}",
        crate::corpus::learn::FEATURES
            .iter()
            .zip(model.weights.iter())
            .map(|(f, w)| format!("{f}={w:+.3}"))
            .collect::<Vec<_>>()
            .join("  ")
    );
    println!("--- holdout comparison ---");
    header();
    for (name, predictor) in baselines() {
        print_row(&score(name, holdout, predictor));
    }
    print_row(&score("learned (listwise softmax)", holdout, |c| {
        rank(&model, c, &ctx)
    }));

    if let Some(path) = weights_out {
        std::fs::write(path, serde_json::to_string_pretty(&model)?)?;
        eprintln!("wrote weights to {path}");
    }
    Ok(())
}

/// Seed native roadmap chunks from an existing roadmap. With `--file`, parses
/// that one file (markdown or YAML, by extension). Without it, **discovers**
/// every roadmap source in the current project (ROADMAP.md / .yml / .yaml /
/// magistr stores) and merges them, deduping by id. `--dry-run` prints without
/// writing; `shared` marks imported chunks for the committed partition.
pub fn import(file: Option<&str>, shared: bool, dry_run: bool, merge: bool) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let parsed = match file {
        Some(f) => {
            let r = crate::roadmap::import::parse_file_full(std::path::Path::new(f))
                .with_context(|| format!("reading {f}"))?;
            println!("parsed {} chunk(s) from {f}", r.chunks.len());
            r
        }
        None => {
            let sources = crate::roadmap::import::discover_sources(&cwd);
            if sources.is_empty() {
                println!("no roadmap source found under {}", cwd.display());
                return Ok(());
            }
            for s in &sources {
                println!("  source: {}", s.display());
            }
            let r = crate::roadmap::import::import_project_full(&cwd);
            println!(
                "parsed {} chunk(s), {} note section(s) from {} source(s)",
                r.chunks.len(),
                r.notes.len(),
                sources.len()
            );
            r
        }
    };

    if dry_run {
        for c in &parsed.chunks {
            println!(
                "  {} [{:?}] (priority {}) — {}",
                c.id, c.status, c.priority, c.title
            );
        }
        if !parsed.notes.is_empty() {
            println!(
                "  + preserved notes: {}",
                parsed
                    .notes
                    .iter()
                    .map(|n| n.heading.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        println!("dry-run: no native state written.");
        return Ok(());
    }

    // `shared` is accepted for back-compat; seeding writes to the native store.
    let _ = shared;
    let mut engine = load_roadmap_engine();
    let total = parsed.chunks.len();
    if merge {
        let (added, updated) = engine.merge_from_import(parsed);
        println!(
            "merged: {added} new chunk(s), {updated} updated with notes \
             (existing status/priority preserved)."
        );
    } else {
        let added = engine.seed_from_import(parsed);
        println!(
            "imported {added} chunk(s); {} skipped (already present).",
            total - added
        );
    }
    Ok(())
}

/// Best-effort auto-import on server startup: when the native roadmap is empty
/// and the project has a roadmap source on disk, seed native state from it.
/// One-time (a non-empty roadmap is never touched) and opt-out via
/// `THINK_AND_SHIP_AUTO_IMPORT=false`. Called from `build_unified` before the
/// broadcaster/repo-sink are attached, so it produces no broadcast/commit noise.
fn maybe_auto_import_roadmap(engine: &mut crate::roadmap::RoadmapEngine) {
    let opted_out = std::env::var("THINK_AND_SHIP_AUTO_IMPORT")
        .map(|v| v == "false" || v == "0")
        .unwrap_or(false);
    if opted_out || !engine.roadmap().chunks.is_empty() {
        return;
    }
    let Ok(cwd) = std::env::current_dir() else {
        return;
    };
    let imported = crate::roadmap::import::import_project_full(&cwd);
    if imported.chunks.is_empty() {
        return;
    }
    let n = engine.seed_from_import(imported);
    if n > 0 {
        eprintln!(
            "think-and-ship: auto-imported {n} roadmap chunk(s) from {} (native roadmap was empty)",
            cwd.display()
        );
    }
}

/// Promote git-native trace records from the gitignored `local/` partition to
/// the committed `sessions/` partition. `step` filters to a single
/// reasoning step number; omit it to promote the whole session. Does not commit
/// — review + `git commit` (with the redaction hook) afterward.
pub fn promote(session: &str, step: Option<u32>, kind: Option<&str>) -> Result<()> {
    let cwd = std::env::current_dir().context("resolving current directory")?;
    let root = discover_repo_root(&cwd)
        .context("not inside a git repository — git-native traces require a repo")?;
    let sink = RepoSink::new(root);
    let out = sink
        .promote(session, step, kind)
        .with_context(|| format!("promoting session {session}"))?;
    let filter = match (step, kind) {
        (Some(n), Some(k)) => format!(" (step {n}, kind {k})"),
        (Some(n), None) => format!(" (step {n})"),
        (None, Some(k)) => format!(" (kind {k})"),
        (None, None) => String::new(),
    };
    println!(
        "promoted {} record(s){filter} in session '{session}' ({} kept local)",
        out.promoted, out.kept
    );
    if out.promoted > 0 {
        println!(
            "→ review .think-and-ship/sessions/{session}.jsonl, then `git add` + commit \
             (the pre-commit redaction hook will scan for secrets)."
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// tracker — mirror roadmap items into an issue tracker
// ---------------------------------------------------------------------------
//
// Two consents, both off by default and both required. `on`/`off` decide whether
// this project may talk to a tracker at all and where; `include`/`exclude` decide
// which items are in scope. Splitting them is what makes "I connected a repo" and
// "I meant to publish these forty items" two separate decisions rather than one
// accidental one.

/// Resolve the destination this project is configured to mirror into.
pub fn tracker_config() -> (std::path::PathBuf, String, crate::tracker::TrackerConfig) {
    let data_dir = crate::infra::PersistenceConfig::from_env().data_dir;
    let project_id = crate::infra::resolve_project_id(None);
    let config = crate::tracker::config::load(&data_dir, &project_id);
    (data_dir, project_id, config)
}

/// Build the adapter for the configured provider. Unknown providers are named
/// rather than silently ignored — a typo in `--provider` should not look like
/// "nothing to do".
fn tracker_port(
    config: &crate::tracker::TrackerConfig,
) -> Result<Box<dyn crate::tracker::TrackerPort>> {
    tracker_port_in(crate::tracker::registry::PROVIDERS, config)
}

/// `tracker_port` against an arbitrary registration table. See
/// [`build_tracker_port_in`] for why the table is a parameter at all.
pub fn tracker_port_in(
    registrations: &[crate::tracker::ProviderRegistration],
    config: &crate::tracker::TrackerConfig,
) -> Result<Box<dyn crate::tracker::TrackerPort>> {
    let provider = config.provider.as_deref().unwrap_or_default();
    build_tracker_port_in(registrations, config, tracker_credential(provider))
}

/// The async twin of `tracker_port`, for callers already inside a runtime.
/// Both funnel into [`build_tracker_port_in`], so the provider wiring stays a
/// single registration point — only the credential lookup differs.
pub async fn tracker_port_async(
    config: &crate::tracker::TrackerConfig,
) -> Result<Box<dyn crate::tracker::TrackerPort>> {
    tracker_port_async_in(crate::tracker::registry::PROVIDERS, config).await
}

/// [`tracker_port_async`] against an arbitrary registration table.
pub async fn tracker_port_async_in(
    registrations: &[crate::tracker::ProviderRegistration],
    config: &crate::tracker::TrackerConfig,
) -> Result<Box<dyn crate::tracker::TrackerPort>> {
    let provider = config.provider.as_deref().unwrap_or_default();
    build_tracker_port_in(
        registrations,
        config,
        tracker_credential_async(provider).await,
    )
}

/// Build the adapter registered under the config's provider, in a given table.
///
/// # Why the table is a parameter
///
/// This is the seam `tracker-port-test-seam` was filed for, and it is worth
/// saying what it is NOT. Two other ways to let a test reach a command were
/// considered: a `cfg(test)` fake arm in the dispatch, or a fake provider key
/// registered for real. Both were rejected. A `cfg(test)` arm means the path a
/// test executes is not the path that ships — an objection with no answer. A
/// production-reachable fake
/// key is a way for a typo to point a human's mirror at nothing.
///
/// Taking the TABLE instead has neither cost. There is ONE body — this one —
/// and there is no production-only wrapper around it to drift: `tracker_port`
/// and [`tracker_port_async`] pass [`PROVIDERS`], a test passes a table of its
/// own composition, and nothing else exists. The tested path is byte-identical
/// to the shipped one because it IS the shipped one, and the fake is
/// unreachable in production for the only durable reason — the shipped table
/// does not contain it. `tests/tracker_command_seam.rs` holds that as an
/// executing assertion rather than as this paragraph.
///
/// [`PROVIDERS`]: crate::tracker::registry::PROVIDERS
pub fn build_tracker_port_in(
    registrations: &[crate::tracker::ProviderRegistration],
    config: &crate::tracker::TrackerConfig,
    credential: Option<crate::tracker::credential::Credential>,
) -> Result<Box<dyn crate::tracker::TrackerPort>> {
    let provider = config.provider.as_deref().unwrap_or_default();
    // SEP-414 downstream half: resolved once here, so adding a provider gets
    // trace propagation by calling one builder rather than by remembering to.
    let project = crate::think::config::resolve_project_id();
    // NO PROVIDER IS NAMED IN THIS FILE. The single provider wiring point
    // lives in tracker::registry::PROVIDERS, and the list of known providers in
    // the refusal is rendered from that same table — so the two cannot drift.
    let request = crate::tracker::ProviderBuild {
        target: config.target.as_deref().unwrap_or_default(),
        project: &project,
        credential: credential.as_ref(),
    };
    Ok(crate::tracker::registry::build_in(
        registrations,
        provider,
        &request,
    )?)
}

pub fn tracker_status() -> Result<()> {
    let (data_dir, project_id, config) = tracker_config();
    let engine = load_roadmap_engine();

    match (&config.provider, &config.target) {
        (Some(p), Some(t)) if config.enabled => println!("mirroring {project_id} into {p} {t}"),
        (Some(p), Some(t)) => println!("mirroring is OFF (was set up for {p} {t})"),
        _ => println!("mirroring is OFF — nothing is sent anywhere"),
    }

    let provider = config.provider.as_deref().unwrap_or("github");

    // DID IT EVER WORK? The question that could not previously be answered by
    // looking — the unattended pusher logged success at `debug!` to a stderr
    // that, for an MCP server, is the client's log file. A cadence failing for
    // two days was indistinguishable from one that was working.
    match crate::tracker::receipt::load(&data_dir, &project_id).last(provider) {
        Some(r) => println!("last successful push: {}", r.summary()),
        // Stated plainly and without softening: "never" is the diagnosis, not
        // an absence of data.
        None => println!("last successful push: NEVER — no push to {provider} has ever succeeded"),
    }
    let included = engine.chunks_opted_in(provider);
    // THE DRIFT, said out loud. The included count alone is reassuring by
    // construction — it only ever goes up — so it cannot report a scope that
    // stopped growing. What is NOT covered is the number that can.
    let missing = engine.chunks_not_opted_in(provider);
    let inheriting = crate::tracker::inherited_opt_in(&config).is_some();

    if included.is_empty() {
        println!("\nno items are included, so nothing would be sent.");
        println!("include one with: think-and-ship tracker include --item <id>");
    } else {
        println!("\n{} item(s) included:", included.len());
        for chunk in included {
            match engine.tracker_link(&chunk.id, provider) {
                Some(link) => println!("  {}  ->  {}", chunk.id, link.external_id),
                None => println!("  {}  ->  (not sent yet)", chunk.id),
            }
        }
    }

    if missing.is_empty() {
        println!("\nevery active chunk is included — no drift.");
    } else {
        println!(
            "\n{} active chunk(s) are NOT included, so they are invisible to {provider}:",
            missing.len()
        );
        for chunk in missing.iter().take(10) {
            println!("  {}", chunk.id);
        }
        if missing.len() > 10 {
            println!("  … and {} more", missing.len() - 10);
        }
        println!(
            "include one with: think-and-ship tracker include --item <id> --provider {provider}"
        );
    }

    if inheriting {
        println!("\nnew chunks are born included (this project explicitly ran `tracker on`).");
    } else {
        println!(
            "\nnew chunks are born EXCLUDED — nothing grows this set on its own. \
             Run `tracker setup` to connect this project explicitly."
        );
    }
    Ok(())
}

pub fn tracker_on(
    provider: &str,
    into: &str,
    companion: Option<&str>,
    companion_into: Option<&str>,
) -> Result<()> {
    let (data_dir, project_id, _) = tracker_config();
    // Fail before recording anything if the destination is unusable, so the
    // stored config can never describe somewhere that does not exist.
    let probe = crate::tracker::TrackerConfig {
        enabled: true,
        provider: Some(provider.trim().to_ascii_lowercase()),
        target: Some(into.trim().to_string()),
        ..crate::tracker::TrackerConfig::default()
    };
    tracker_port(&probe)?;

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let config = crate::tracker::config::enable(&data_dir, &project_id, provider, into, &now)?;
    println!(
        "mirroring is ON: {} -> {} {}",
        project_id,
        config.provider.as_deref().unwrap_or_default(),
        config.target.as_deref().unwrap_or_default()
    );

    // The companion is set AFTER the primary, so `set_companion`'s
    // same-key refusal reads the provider this command just recorded rather
    // than whatever was configured before it.
    if let Some(name) = companion {
        // A companion with no destination is rejected here rather than stored:
        // the flags are separate, so forgetting one is the likely slip, and the
        // config layer would refuse it a second time anyway.
        let target = companion_into.unwrap_or_default();
        let config =
            crate::tracker::config::set_companion(&data_dir, &project_id, name, target, &now)?;
        match config.companion.as_ref() {
            Some(lane) => println!(
                "companion lane: {} {} — it takes its item identities from '{}'.",
                lane.provider,
                lane.target,
                config.provider.as_deref().unwrap_or_default()
            ),
            None => println!("companion lane cleared."),
        }
    }

    println!("nothing is sent until you include at least one item.");
    Ok(())
}

/// What a setup run was asked to do. A struct rather than eight positional
/// arguments so the test and the command cannot drift on argument order.
pub struct SetupRequest {
    pub provider: String,
    pub into: String,
    pub name: Option<String>,
    pub band: Option<String>,
    /// Name for the initiative roof. `None` keeps whatever is configured (or
    /// the directory-basename default 10b decided) — absent must never CLEAR a
    /// name a human chose.
    pub initiative: Option<String>,
    pub push: bool,
    pub push_secs: u64,
    pub yes: bool,
    pub dry_run: bool,
}

/// What a setup run actually did, in order. Returned so a test can assert on
/// the OUTCOME rather than on printed text, and so nothing has to be inferred
/// from stdout.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SetupOutcome {
    pub target_existed: bool,
    pub target_created: bool,
    /// The provider could not be asked. NOT the same as absent — see
    /// [`TargetPhase::Unverifiable`].
    pub target_unverified: bool,
    pub mirroring_enabled: bool,
    pub included: Vec<String>,
    pub already_included: usize,
    pub auto_push: Option<String>,
    /// Auto-push was wanted but there was no think-and-ship entry to merge into,
    /// i.e. `init` has not run here. Reported rather than silently skipped.
    pub auto_push_no_entry: bool,
    /// WHERE auto-push was written, so the human can go look at the right file
    /// — there are up to four candidates and the answer is not obvious.
    pub auto_push_at: Option<String>,
    /// The value was ALREADY what we wanted, so nothing changed and there is
    /// nothing to reconnect for. Distinguished from a write because "set 300s"
    /// when it was already 300s claims work that did not happen.
    pub auto_push_unchanged: bool,
    /// The entry was FOUND but lives in a config we will not rewrite. Carries
    /// the one-line instruction the human needs.
    pub auto_push_manual: Option<String>,
    pub pushed: Option<usize>,
}

/// `tracker setup` — the thin half: resolve config, build the port, hand off.
///
/// Split for the reason recorded in `tracker-port-test-seam`. That reason used
/// to be "`build_tracker_port` knows only github and linear"; it now binds
/// `tracker::registry::PROVIDERS`, the REAL table, so the debt is narrower but
/// not paid — a test still cannot inject a fake provider through this path,
/// only through `registry::build_in`. Keeping this function to resolution alone means the
/// ORCHESTRATION below is executable against a fake, instead of being asserted
/// by grepping this file.
pub fn tracker_setup(req: &SetupRequest) -> Result<()> {
    let (data_dir, project_id, _) = tracker_config();
    let probe = crate::tracker::TrackerConfig {
        enabled: true,
        provider: Some(req.provider.trim().to_ascii_lowercase()),
        target: Some(req.into.trim().to_string()),
        ..crate::tracker::TrackerConfig::default()
    };
    // Shape validation happens here, in the constructor, and is all a local
    // check can do. EXISTENCE is the port's job, below.
    let port = tracker_port(&probe)?;
    let mut engine = load_roadmap_engine();
    let runtime = tokio::runtime::Runtime::new()?;
    let outcome = runtime.block_on(run_setup(
        port.as_ref(),
        &mut engine,
        req,
        &data_dir,
        &project_id,
        &std::env::current_dir()?,
    ))?;
    let _ = outcome;
    Ok(())
}

/// Everything `tracker setup` does once a port exists — and therefore the whole
/// of it that a test can drive.
///
/// The ORDER is the design. The destination is verified (and created, if asked)
/// BEFORE any local config is written, so a run that cannot reach its
/// destination leaves nothing behind claiming it can. That is the defect this
/// chunk exists to fix: `tracker on` wrote its config first and discovered the
/// destination was fictional at the next push.
/// The engine arrives as a parameter for the same reason the port does: a test
/// that called `load_roadmap_engine` itself would read the DEVELOPER'S real
/// roadmap, and with persistence off it would read an empty one — so the include
/// stage would be untestable either way.
/// What the network half concluded about the destination.
#[derive(Debug, PartialEq, Eq)]
pub enum TargetPhase {
    /// It is there. Carry on.
    Present,
    /// It was not there and we made it.
    Created,
    /// The provider cannot be asked. Carry on, but do not claim it exists.
    Unverifiable,
    /// It was not there and we were not permitted to create it. STOP — writing
    /// local config now would name somewhere that does not exist.
    MissingAndNotCreated,
}

/// SETUP, HALF ONE: the destination. Network only.
///
/// Takes NO engine, and that is the entire point rather than a tidiness
/// preference. This half awaits on the network, and the MCP tool that calls it
/// holds `Arc<Mutex<RoadmapEngine>>` — holding that lock across an await is the
/// rule this codebase states as a constraint on every objective. A function
/// that cannot see the engine cannot break it.
///
/// `may_create` is a plain bool rather than a callback for the second, sharper
/// reason: `confirm` reads STDIN, and in a stdio MCP server stdin carries the
/// JSON-RPC frames. A prompt reachable from a tool handler would eat protocol
/// traffic and desynchronize the session. So the decision is made by the CALLER
/// — the CLI asks a human, the tool takes a parameter — and this function is
/// simply never in a position to prompt.
pub async fn setup_probe(
    port: &dyn crate::tracker::TrackerPort,
    display_name: &str,
    may_create: bool,
) -> Result<TargetPhase> {
    use crate::tracker::TrackerError;

    match port.probe_target().await {
        Ok(_) => Ok(TargetPhase::Present),
        Err(TrackerError::NotFound(_)) => {
            if !may_create {
                return Ok(TargetPhase::MissingAndNotCreated);
            }
            port.create_target(display_name).await.map_err(|e| {
                // A create failure is very often a permission one, and the
                // provider's own words are the only useful thing to say.
                anyhow::anyhow!(
                    "could not create the destination: {e}\n\n\
                     If that was a permission error: a personal API key acts \
                     as you, so you must be allowed to create one yourself. \
                     On the OAuth path (`tracker sign-in`) the token also \
                     needs the provider's admin scope — the default scopes \
                     cannot provision."
                )
            })?;
            Ok(TargetPhase::Created)
        }
        // "Cannot check" must not read as "missing". A provider that declines to
        // introspect is not evidence of a problem, and refusing to continue
        // would be worse than the bug this probe was added to fix.
        Err(TrackerError::Unsupported(_)) => Ok(TargetPhase::Unverifiable),
        Err(e) => Err(anyhow::anyhow!("could not check the destination: {e}")),
    }
}

async fn run_setup(
    port: &dyn crate::tracker::TrackerPort,
    engine: &mut crate::roadmap::engine::RoadmapEngine,
    req: &SetupRequest,
    data_dir: &std::path::Path,
    project_id: &str,
    cwd: &std::path::Path,
) -> Result<SetupOutcome> {
    let mut outcome = SetupOutcome::default();
    let provider = req.provider.trim().to_ascii_lowercase();
    let target = req.into.trim();

    println!("setting up {project_id} -> {provider} {target}\n");

    // ── 1. The destination ────────────────────────────────────────────────
    let display = req.name.as_deref().unwrap_or(target);

    // Probe with NO permission to create first, so a destination that already
    // exists is never preceded by a pointless "create it?" question. Only a
    // genuine miss reaches a human.
    let mut phase = setup_probe(port, display, false).await?;
    if phase == TargetPhase::MissingAndNotCreated && !req.dry_run {
        println!("  destination: NOT FOUND");
        // The CLI's consent gesture, and the ONLY place `confirm` is reachable
        // from. The second probe costs one extra call in the rare missing case,
        // which is the right price for keeping the prompt at the caller.
        if req.yes || confirm(&format!("  create '{display}' now?")) {
            phase = setup_probe(port, display, true).await?;
        }
    }

    match phase {
        TargetPhase::Present => {
            outcome.target_existed = true;
            println!("  destination: found");
        }
        TargetPhase::Created => {
            outcome.target_created = true;
            println!("  destination: CREATED — {display}");
        }
        TargetPhase::Unverifiable => {
            println!("  destination: unverified — this provider cannot describe its destination");
            println!(
                "               (this provider cannot be asked; a bad value will surface at push)"
            );
        }
        TargetPhase::MissingAndNotCreated => {
            if req.dry_run {
                println!("  destination: NOT FOUND — would offer to create '{display}'");
            } else {
                // Declining is a complete, successful answer — nothing was
                // written, so there is nothing to undo and nothing to warn about.
                println!("\nNothing was written. Create it yourself, then re-run this.");
                return Ok(outcome);
            }
        }
    }

    let local = setup_local(engine, req, data_dir, project_id, cwd)?;
    outcome.mirroring_enabled = local.mirroring_enabled;
    outcome.included = local.included;
    outcome.already_included = local.already_included;
    outcome.auto_push = local.auto_push;
    outcome.auto_push_no_entry = local.auto_push_no_entry;
    outcome.auto_push_at = local.auto_push_at;
    outcome.auto_push_unchanged = local.auto_push_unchanged;
    outcome.auto_push_manual = local.auto_push_manual.clone();

    // Rendering lives HERE, at the CLI entry point, because `setup_local` is
    // shared with a tool handler whose stdout is the JSON-RPC transport.
    println!(
        "\n  mirroring:   {}",
        if req.dry_run { "would turn ON" } else { "ON" }
    );
    let verb = if req.dry_run {
        "would include"
    } else {
        "included"
    };
    match (outcome.included.len(), outcome.already_included) {
        (0, 0) => println!("  items:       none are active, so nothing would be sent"),
        (0, n) => println!("  items:       {n} already included, nothing new"),
        (new, 0) => println!("  items:       {verb} {new}"),
        (new, n) => println!("  items:       {verb} {new} ({n} already included)"),
    }
    if let Some(secs) = &outcome.auto_push {
        if outcome.auto_push_unchanged {
            println!("  auto-push:   already {secs}s");
        } else {
            let what = if req.dry_run { "would set" } else { "set" };
            println!("  auto-push:   {what} {secs}s");
        }
        if let Some(where_at) = &outcome.auto_push_at {
            println!("               in {where_at}");
        }
    } else if let Some(manual) = &outcome.auto_push_manual {
        println!("  auto-push:   NOT SET — needs one line from you");
        println!("               {manual}");
    } else if outcome.auto_push_no_entry {
        println!("  auto-push:   SKIPPED — no think-and-ship entry found in this");
        println!("               project's .mcp.json/.cursor/.windsurf or ~/.claude.json");
        println!("               run `think-and-ship init` here first, then re-run this");
    }

    // ── 5. Optionally push now ────────────────────────────────────────────
    if req.push && !req.dry_run {
        let config = crate::tracker::config::load(data_dir, project_id);
        let run = push_once(data_dir, project_id, &config).await?;
        outcome.pushed = Some(run.report.outcomes.len());
        println!("  pushed:      {} item(s)", run.report.outcomes.len());
    }

    println!("\n{}", setup_epilogue(&outcome, req));
    Ok(outcome)
}

/// SETUP, HALF TWO: enable mirroring, include the items, wire auto-push.
///
/// Synchronous, and AWAITS NOTHING — that is the contract, not an accident of
/// what it happens to do today. It is the half that touches the engine, so a
/// caller holding `Arc<Mutex<RoadmapEngine>>` can run the whole thing inside one
/// brief lock without ever suspending while holding it. Adding an `.await` here
/// would silently reintroduce the lock-across-await defect for the MCP path.
///
/// SILENT, and that is the second half of the same contract. In a stdio MCP
/// server `run_stdio` hands STDOUT to the JSON-RPC transport, so a `println!`
/// reached from a tool handler would inject text into the protocol stream —
/// the same class of defect as `confirm` reading stdin, and just as fatal.
/// This function therefore returns everything it did in [`SetupOutcome`] and
/// renders nothing; the CLI prints, the tool serializes. (The engine already
/// observes this rule — note its load message uses `eprintln!`.)
pub fn setup_local(
    engine: &mut crate::roadmap::engine::RoadmapEngine,
    req: &SetupRequest,
    data_dir: &std::path::Path,
    project_id: &str,
    cwd: &std::path::Path,
) -> Result<SetupOutcome> {
    let mut outcome = SetupOutcome::default();
    let provider = req.provider.trim().to_ascii_lowercase();
    let target = req.into.trim();

    // ── 2. Local config, only now that the destination is real ────────────
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    if !req.dry_run {
        crate::tracker::config::enable(data_dir, project_id, &provider, target, &now)?;
        // The roof's name, when the caller chose one. After enable(), so the
        // explicit choice lands on top of the carried-through config; absent
        // means keep — enable() already preserves a prior name (1c14ceb).
        if let Some(name) = req.initiative.as_deref() {
            crate::tracker::config::set_initiative(data_dir, project_id, name, &now)?;
        }
        // Consent was granted a moment ago, to an engine that may be serving a
        // LIVE MCP session (the tracker_setup tool hands us its engine). Arm the
        // inheritance now, from the config we just wrote, or every chunk created
        // before the next restart would still be born invisible — the exact gap
        // this is fixing, reintroduced one restart wide.
        let armed =
            crate::tracker::inherited_opt_in(&crate::tracker::config::load(data_dir, project_id))
                .map(str::to_string);
        engine.set_opt_in_inheritance(armed);
    }
    outcome.mirroring_enabled = !req.dry_run;

    // ── 3. Bulk include ───────────────────────────────────────────────────
    let wanted: Vec<String> = {
        let already: std::collections::HashSet<String> = engine
            .chunks_opted_in(&provider)
            .into_iter()
            .map(|c| c.id.clone())
            .collect();
        let band = req.band.as_deref().map(str::trim);
        engine
            .roadmap()
            .chunks
            .iter()
            // ACTIVE only, by the shared rule — the same one a newly-born chunk
            // is judged by, so the bulk include and the inheritance can never
            // disagree about what counts as work.
            .filter(|c| c.status.is_active())
            .filter(|c| band.is_none_or(|b| crate::infra::coerce::priority_band(c.priority) == b))
            .filter(|c| {
                if already.contains(&c.id) {
                    outcome.already_included += 1;
                    false
                } else {
                    true
                }
            })
            .map(|c| c.id.clone())
            .collect()
    };

    for id in &wanted {
        if !req.dry_run {
            engine
                .set_tracker_opt_in(id, &provider, true)
                .map_err(anyhow::Error::msg)?;
        }
        outcome.included.push(id.clone());
    }

    // ── 4. Auto-push ──────────────────────────────────────────────────────
    //
    // SEARCH for the entry rather than guessing its path. The guess resolved by
    // IDE marker directory, so a repo with a `.cursor/` folder pointed at
    // `.cursor/mcp.json` even when the live entry was in `~/.claude.json` — and
    // then reported "run init here", which would have created another config
    // instead of touching the one in use.
    if req.push_secs > 0 {
        let secs = req.push_secs.to_string();
        match crate::cli::setup::find_server_entry(cwd) {
            None => outcome.auto_push_no_entry = true,
            Some(entry_at) => {
                match crate::cli::setup::merge_server_env(
                    &entry_at,
                    PUSH_INTERVAL_ENV,
                    &secs,
                    req.dry_run,
                )? {
                    crate::cli::setup::EnvMerge::Written { .. } => {
                        outcome.auto_push = Some(secs);
                        outcome.auto_push_at = Some(entry_at.describe());
                    }
                    crate::cli::setup::EnvMerge::AlreadySet => {
                        outcome.auto_push = Some(secs);
                        outcome.auto_push_at = Some(entry_at.describe());
                        outcome.auto_push_unchanged = true;
                    }
                    crate::cli::setup::EnvMerge::ExternalConfig { at } => {
                        outcome.auto_push_manual = Some(format!(
                            "Your think-and-ship entry is in {at}. That file holds \
                             every project you have, and rewriting it to set one \
                             variable would reorder and perturb the rest — so add \
                             this by hand:\n      \"{PUSH_INTERVAL_ENV}\": \"{secs}\"",
                        ));
                    }
                    // Only reachable if the file changed under us between the
                    // search and the write.
                    crate::cli::setup::EnvMerge::NoServerEntry => {
                        outcome.auto_push_no_entry = true;
                    }
                }
            }
        }
    }

    Ok(outcome)
}

/// The closing advice, kept separate so it is one testable string rather than a
/// scatter of `println!` calls whose combination nothing checks.
fn setup_epilogue(outcome: &SetupOutcome, req: &SetupRequest) -> String {
    if req.dry_run {
        return "Nothing was written. Drop --dry-run to do it.".into();
    }
    let mut lines = vec!["Done.".to_string()];
    if outcome.auto_push.is_some() && !outcome.auto_push_unchanged {
        // The single most common way this silently does nothing: cadences are
        // spawned once, at server startup. Only worth saying when something
        // actually changed — otherwise it is noise on every repeat run.
        lines.push("Reconnect the MCP server (/mcp) — the push cadence only starts there.".into());
    }
    if outcome.pushed.is_none() && !outcome.included.is_empty() {
        lines.push("Nothing has been sent yet; `tracker push` sends it now.".into());
    }
    lines.join("\n")
}

/// Every prompt in this module reads stdin through here, because the headless
/// rule is a property of READING STDIN and not of any one question.
///
/// The guarantee is a TTY CHECK BEFORE THE READ, and it cannot be anything
/// else. `read_line` reports a missing human two ways, and neither one arrives
/// on a non-interactive process: an `Err` needs a broken descriptor, and
/// `Ok(0)` needs EOF. A descriptor that is open and merely silent — an agent's
/// shell, a CI runner, `something | think-and-ship ...` — sends neither, so the
/// read blocks for as long as the pipe is held. That is not a slow prompt, it
/// is a hang: it is what stalled a full `cargo test` for twenty minutes with no
/// failure to read, and it is why the guard cannot be a timeout. A timeout is a
/// race that usually wins; `IsTerminal` is a fact about the process.
///
/// `None` means nobody is there to ask, and callers decide what that is worth:
/// [`confirm`] treats it as a no, the secret prompts refuse and name their
/// non-interactive path. Silence is never an answer.
///
/// The same rule, written as prose, already lives in [`otel_stack`]'s "headless
/// rule" module doc. Prose is why three of the four prompt sites in this crate
/// did not have it.
fn prompt_line(question: &str) -> Option<String> {
    prompt_line_from(
        question,
        std::io::stdin().is_terminal(),
        std::io::stdin().lock(),
    )
}

/// [`prompt_line`]'s rule with the terminal and the reader passed in, so a test
/// can hold both — in particular a reader that panics if it is ever consulted,
/// which is the only way to prove the tty check happens BEFORE the read rather
/// than beside it.
fn prompt_line_from<R: std::io::BufRead>(
    question: &str,
    stdin_is_terminal: bool,
    mut reader: R,
) -> Option<String> {
    use std::io::Write;
    // BEFORE the read, never after: once `read_line` is entered on a silent
    // descriptor, nothing left in this process can end the wait.
    if !stdin_is_terminal {
        return None;
    }
    eprint!("{question} ");
    std::io::stderr().flush().ok();
    let mut line = String::new();
    match reader.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line),
    }
}

/// The one wording for "this needed a person and there was nobody there".
///
/// Silently treating the absence as a no would produce a run that did nothing
/// and explained nothing — the objection [`otel_stack`]'s headless rule already
/// raises against exactly that behaviour. A refusal that names the
/// non-interactive path is the only outcome that leaves the operator better off
/// than before they ran it.
fn headless_refusal(needed: &str, instead: &str) -> String {
    format!(
        "{needed} has to come from somewhere, and this is not a terminal — there is nobody \
         here to ask. Run it again from a terminal, or {instead}."
    )
}

/// Turn "nobody was there to ask" into the refusal, for prompts whose answer the
/// command cannot proceed without.
///
/// This exists because the wording was not the part at risk. A deliberately
/// introduced bug that left [`headless_refusal`] untouched and simply stopped
/// calling it — headless `tracker connect` storing an empty key instead of
/// refusing — passed all 799 tests. The message builder attracts the gate; the disposal of the `None` is
/// what actually decides whether the operator gets a refusal or a silently
/// broken credential, and it needs its own.
fn require_prompted(answer: Option<String>, needed: &str, instead: &str) -> Result<String> {
    match answer {
        Some(line) => Ok(line),
        None => anyhow::bail!(headless_refusal(needed, instead)),
    }
}

/// Ask a yes/no question on the terminal. Anything but an explicit yes is a no,
/// including nobody being there to ask, so an automated run can never create
/// anything by default.
fn confirm(question: &str) -> bool {
    prompt_line(&format!("{question} [y/N]")).is_some_and(is_yes)
}

/// An explicit yes and nothing else. Split out from [`confirm`] only so it can
/// be driven by a test — [`confirm`] itself reads the process's real stdin, and
/// a test that called it would be asserting about the machine it runs on.
/// The production half of a CLI source file: everything before its first test
/// module. Shared by the two source-inspection gates so they cannot disagree
/// about what counts as live code, and so a call that only appears inside a
/// test cannot satisfy either of them.
/// The anchor is a test MODULE, not any `#[cfg(test)]` item. Splitting on the
/// bare attribute cut this file at *this function's own* attribute — above both
/// prompt sites — and the reachability gate below reported them missing. A
/// source gate whose window is wrong fails open just as easily as it fails
/// loudly; this one happened to fail loudly.
#[cfg(test)]
fn cli_production_source(whole: &str) -> &str {
    whole
        .split_once("\n#[cfg(test)]\nmod ")
        .map_or(whole, |(before, _)| before)
}

fn is_yes(line: impl AsRef<str>) -> bool {
    matches!(
        line.as_ref().trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    )
}

pub fn tracker_off() -> Result<()> {
    let (data_dir, project_id, _) = tracker_config();
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    crate::tracker::config::disable(&data_dir, &project_id, &now)?;
    println!("mirroring is OFF. Nothing will be sent.");
    Ok(())
}

pub fn tracker_include(item: &str, provider: &str, included: bool) -> Result<()> {
    let mut engine = load_roadmap_engine();
    engine
        .set_tracker_opt_in(item, provider, included)
        .map_err(anyhow::Error::msg)?;
    if included {
        println!("'{item}' is included and will be mirrored on the next push.");
    } else {
        println!("'{item}' is excluded and will no longer be mirrored.");
    }
    Ok(())
}

/// Build the credential resolver for this project.
///
/// The file store is the product path; the environment remains a documented
/// fallback and is consulted only when nothing has been connected.
///
/// Refresh configs are re-derived from what the store kept: the sign-in
/// command that knew the client id has exited by the time a token expires, so
/// without this peek every OAuth credential would die at its first expiry with
/// "no OAuth configuration is registered to refresh it".
fn credential_resolver() -> crate::tracker::credential::Resolver {
    use crate::tracker::credential::CredentialStore as _;

    let data_dir = crate::infra::PersistenceConfig::from_env().data_dir;
    let store = std::sync::Arc::new(crate::tracker::credential::FileCredentialStore::new(
        &data_dir,
    ));
    let mut resolver = crate::tracker::credential::Resolver::new(store.clone());
    for provider in ["linear", "jira"] {
        if let Ok(Some(stored)) = store.load(provider)
            && let Some(config) =
                crate::tracker::credential::authcode::stored_refresh_profile(&stored)
        {
            resolver = resolver.with_oauth(provider, config);
        }
    }
    resolver
}

/// Resolve a usable credential, preferring the stored one and falling back to
/// the environment. Returns `None` when neither is present, so callers can say
/// something useful instead of failing obscurely.
///
/// ASYNC because it must be callable from inside a runtime. The sweep runs from
/// three places that are already async — `tracker pull`, the realtime doorbell
/// and the unattended cadence — and the previous sync version built its own
/// runtime, which PANICS when one already exists. See [`tracker_credential`]
/// for the sync wrapper and the rule that keeps the two apart.
async fn tracker_credential_async(
    provider: &str,
) -> Option<crate::tracker::credential::Credential> {
    use crate::tracker::credential::{CredentialPort, CredentialStore};

    if let Ok(c) = credential_resolver().credential(provider).await {
        return Some(c);
    }
    // Documented fallback.
    let env = crate::tracker::credential::EnvCredentialStore;
    env.load(provider).ok().flatten().map(|s| s.as_credential())
}

/// The sync entry point, for callers genuinely OUTSIDE a runtime.
///
/// THE RULE: this must never be reachable from async code. Building a runtime
/// inside one panics with "Cannot start a runtime from within a runtime", and
/// because nothing in the suite executed the sweep, that panic shipped and sat
/// live in `tracker pull`, the doorbell and the unattended sweep for two
/// chunks. Async callers take [`tracker_credential_async`]; a test executes the
/// sweep inside a runtime so the panic cannot come back unnoticed.
fn tracker_credential(provider: &str) -> Option<crate::tracker::credential::Credential> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(tracker_credential_async(provider))
}

pub fn tracker_connect(provider: &str, key: Option<String>) -> Result<()> {
    let provider = provider.trim().to_ascii_lowercase();
    let key = match key {
        Some(k) => k,
        // Prompted rather than passed, so it never lands in shell history —
        // but only where there is somebody to prompt. Headless this refuses
        // and names `--key`, rather than blocking on a stdin nobody will type
        // into.
        None => require_prompted(
            prompt_line(&format!(
                "Paste the key for {provider} (it will not be echoed to history):"
            )),
            &format!("the key for {provider}"),
            "pass it with --key",
        )?,
    };
    let scheme = crate::tracker::credential::store::default_scheme_for(&provider);
    credential_resolver()
        .connect_personal_key(&provider, key.trim(), scheme)
        .map_err(anyhow::Error::new)?;
    println!("stored the key for {provider}.");
    println!("it is encrypted, kept outside the roadmap, and never appears in an export.");
    Ok(())
}

/// The environment variable carrying the Atlassian app's client secret.
///
/// An environment variable or a prompt, never a flag: a secret on a command
/// line lands in shell history and in every process listing on the machine.
const ATLASSIAN_SECRET_ENV: &str = "ATLASSIAN_CLIENT_SECRET";

/// The environment variable naming which Atlassian site to use, for accounts
/// that granted more than one. A cloudid, a site url, or a site name.
const ATLASSIAN_SITE_ENV: &str = "ATLASSIAN_SITE";

/// Read the Atlassian client secret from the environment, or ask for it.
///
/// Jira 3LO is a CONFIDENTIAL client: Atlassian requires the secret on the code
/// exchange AND on every refresh, and documents no PKCE. Linear needs none of
/// this, which is why the secret is asked for here rather than in the shared
/// flow.
fn atlassian_client_secret() -> Result<String> {
    if let Ok(secret) = std::env::var(ATLASSIAN_SECRET_ENV)
        && !secret.trim().is_empty()
    {
        return Ok(secret.trim().to_string());
    }
    // The env var above is this prompt's non-interactive path, so headless it
    // refuses and names it rather than blocking on a stdin nobody will type
    // into.
    let line = require_prompted(
        prompt_line(&format!(
            "Paste the client secret for your Atlassian app (it will not be echoed to history, \
             or set {ATLASSIAN_SECRET_ENV}):"
        )),
        "the Atlassian client secret",
        &format!("set {ATLASSIAN_SECRET_ENV}"),
    )?;
    let secret = line.trim().to_string();
    if secret.is_empty() {
        anyhow::bail!(
            "Jira 3LO is a confidential client — Atlassian rejects the token exchange without \
             a client secret. Find it in the developer console under your app's Settings."
        );
    }
    Ok(secret)
}

/// Sign in to a tracker in the browser and store what comes back.
///
/// Authorization-code with PKCE: Linear and Atlassian both require it and
/// neither offers a device flow, so a loopback receiver is the mechanism.
pub fn tracker_sign_in(
    provider: &str,
    app_id: &str,
    scopes: &str,
    actor: &str,
    print_only: bool,
) -> Result<()> {
    use crate::tracker::credential::{authcode, oauth};

    let provider = provider.trim().to_ascii_lowercase();
    let actor = actor.trim().to_ascii_lowercase();
    let config = match (provider.as_str(), actor.as_str()) {
        ("linear", "user") => authcode::linear_oauth(app_id),
        ("linear", "app") => authcode::linear_oauth_app(app_id),
        ("linear", other) => anyhow::bail!(
            "--actor must be 'user' or 'app', not '{other}' — it decides who the tracker's \
             writes are attributed to"
        ),
        // Atlassian has no app-actor equivalent: a 3LO token always acts as
        // the human who consented, so --actor is not consulted rather than
        // being silently accepted and ignored.
        ("jira", "user") => oauth::jira_3lo(app_id, &atlassian_client_secret()?),
        ("jira", other) => anyhow::bail!(
            "Jira 3LO always acts as the person who approves it, so --actor '{other}' cannot \
             be honoured — omit it, or use 'user'"
        ),
        (other, _) => anyhow::bail!(
            "'{other}' cannot be signed in to yet — use `tracker connect --provider {other}` \
             with a key from its settings page"
        ),
    };
    if actor == "app" {
        println!(
            "Signing in as the APPLICATION: issues and comments this tool creates will be \
             attributed to the app, not to you.\n"
        );
    }

    let receiver = authcode::LoopbackReceiver::bind(0).map_err(anyhow::Error::new)?;
    let redirect_uri = receiver.redirect_uri().map_err(anyhow::Error::new)?;
    let pkce = authcode::Pkce::generate();
    let state = authcode::new_state();
    let mut scope_list: Vec<&str> = scopes
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    // Atlassian issues a refresh token ONLY when offline_access is requested.
    // Added here rather than left to whoever typed --scopes, because omitting
    // it produces a credential that works for an hour and then signs the user
    // out with nothing to renew from — which is exactly the 24-hour credential
    // linear-app-actor-identity found.
    if provider == "jira" && !scope_list.contains(&oauth::JIRA_OFFLINE_SCOPE) {
        scope_list.push(oauth::JIRA_OFFLINE_SCOPE);
    }
    let url = authcode::authorize_url(&config, &pkce, &state, &redirect_uri, &scope_list);

    println!("Add this address to your {provider} application's allowed redirects:");
    println!("  {redirect_uri}\n");
    if print_only {
        println!("Then open this link to approve:\n  {url}\n");
    } else {
        println!("Opening your browser to approve...\n  {url}\n");
        // Best effort. If it fails the link above is still usable, which is why
        // it is printed either way rather than only on failure.
        let _ = std::process::Command::new(if cfg!(target_os = "macos") {
            "open"
        } else if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "xdg-open"
        })
        .arg(&url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
    }
    println!("Waiting for approval...");

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let runtime = tokio::runtime::Runtime::new()?;
    let http = reqwest::Client::new();
    let mut obtained = runtime.block_on(authcode::complete_authorization(
        &http, &config, &provider, receiver, &pkce, &state, &now,
    ))?;

    // THE CLOUDID HOP, and it happens BEFORE the save rather than after: a
    // record persisted without its site would be a credential nothing can
    // address, and a second save to add it would be the second persistence
    // path this port refuses to grow.
    if provider == "jira" {
        let sites = runtime
            .block_on(crate::tracker::credential::atlassian::accessible_resources(
                &http,
                obtained.access.expose(),
            ))
            .map_err(anyhow::Error::new)?;
        let wanted = std::env::var(ATLASSIAN_SITE_ENV).ok();
        let chosen = crate::tracker::credential::atlassian::select_site(&sites, wanted.as_deref())
            .map_err(|e| {
                anyhow::anyhow!(
                    "{e}\n  set {ATLASSIAN_SITE_ENV} to the one you mean, then sign in again"
                )
            })?;
        println!("Site: {}", chosen.describe());
        obtained.site = Some(chosen.id);
    }

    // The single persistence path — the same record refresh, rotation and
    // revoke already operate on.
    credential_resolver()
        .adopt(&obtained)
        .map_err(anyhow::Error::new)?;

    println!("\nSigned in to {provider}.");
    println!("the key is encrypted, kept outside the roadmap, and never appears in an export.");
    Ok(())
}

pub fn tracker_disconnect(provider: &str) -> Result<()> {
    let provider = provider.trim().to_ascii_lowercase();
    let runtime = tokio::runtime::Runtime::new()?;
    let told_provider = runtime.block_on(credential_resolver().revoke(&provider));

    match told_provider {
        Ok(true) => println!("{provider} was told to invalidate the key, and it is now forgotten."),
        Ok(false) => {
            println!("the key for {provider} is forgotten.");
            println!(
                "{provider} offers no way to invalidate it from here — revoke it in their settings too."
            );
        }
        // The local key is gone either way; say what is left to do.
        Err(e) => {
            println!("the key for {provider} is forgotten locally.");
            println!("but {provider} could not be told to invalidate it: {e}");
            println!("revoke it in their settings so it cannot be used.");
        }
    }
    Ok(())
}

pub fn tracker_push(dry_run: bool) -> Result<()> {
    tracker_push_in(crate::tracker::registry::PROVIDERS, dry_run)
}

/// `tracker push` against an arbitrary registration table.
///
/// The WHOLE command, not an extracted core: config resolution, the consent
/// gate, port construction and both the dry-run and the writing branch all run
/// here. That is the point — see [`build_tracker_port_in`] for why the table is
/// a parameter, and `tests/tracker_command_seam.rs` for the test this arity
/// exists to make possible. Every other tracker command still reaches its port
/// through the same builder, so extending this seam to them is a wrapper each,
/// not a redesign.
pub fn tracker_push_in(
    registrations: &[crate::tracker::ProviderRegistration],
    dry_run: bool,
) -> Result<()> {
    let (data_dir, project_id, config) = tracker_config();
    if !crate::tracker::should_project(&config) {
        println!("mirroring is off, so nothing was sent.");
        println!("turn it on with: think-and-ship tracker on --into <owner/repo>");
        return Ok(());
    }
    let provider = config.provider.clone().unwrap_or_default();

    if dry_run {
        let engine = load_roadmap_engine();
        let port = tracker_port_in(registrations, &config)?;
        let capabilities = port.capabilities();
        let planned: Vec<_> = engine
            .chunks_opted_in(&provider)
            .into_iter()
            .cloned()
            .collect();
        if planned.is_empty() {
            println!("no items are included, so nothing would be sent.");
            return Ok(());
        }
        println!(
            "would send {} item(s) to {} {}:\n",
            planned.len(),
            provider,
            config.target.as_deref().unwrap_or_default()
        );
        // The SAME predicate the projector is tested against. This block used
        // to reimplement the projector's first skip gate and know nothing of
        // its second, which is why it announced 7 of 46 live items as pending
        // updates that the real push had already correctly skipped.
        let policy = crate::tracker::ownership::Ownership::default();
        let mut promised = 0usize;
        for chunk in &planned {
            let item =
                crate::tracker::project::to_work_item(&engine, chunk, &provider, &capabilities);
            let verdict = crate::tracker::project::preview_verdict(
                engine.tracker_link(&chunk.id, &provider),
                &item,
                &policy,
            );
            if verdict.promises_a_write() {
                promised += 1;
            }
            println!("  {}  ({})", chunk.id, verdict.as_str());
        }
        println!(
            "\n{promised} of {} would actually be written.",
            planned.len()
        );
        return Ok(());
    }

    // The SAME body the unattended cadence runs, so the command and the
    // automation cannot drift apart. This caller is genuinely outside a runtime,
    // so it supplies one; `CliTrackerPusher` is already inside one and does not.
    let runtime = tokio::runtime::Runtime::new()?;
    let PushRun {
        report,
        companion,
        outbox,
        engine,
    } = runtime.block_on(push_once_in(registrations, &data_dir, &project_id, &config))?;

    let mut created = 0;
    let mut updated = 0;
    let mut unchanged = 0;
    for (id, outcome) in &report.outcomes {
        match outcome {
            crate::tracker::ProjectionOutcome::Created { external_id } => {
                created += 1;
                println!("  created  {id} -> {external_id}");
            }
            crate::tracker::ProjectionOutcome::Patched { external_id } => {
                updated += 1;
                println!("  updated  {id} -> {external_id}");
            }
            crate::tracker::ProjectionOutcome::Skipped { .. } => unchanged += 1,
            crate::tracker::ProjectionOutcome::Refused { reason } => {
                println!("  skipped  {id} — {reason}");
            }
            crate::tracker::ProjectionOutcome::Queued { reason } => {
                println!("  queued   {id} — will retry ({reason})");
            }
            crate::tracker::ProjectionOutcome::Rejected { reason } => {
                println!("  FAILED   {id} — {reason}");
            }
        }
    }
    // Divergence becomes a CONCERN here, at the caller, not inside the
    // projector — a detector that also writes is one nobody can reason about.
    // This is the point where "we noticed" turns into "somebody will see it".
    if !report.divergences.is_empty() {
        let mut signal = crate::signal::SignalEngine::new(project_id.clone()).with_persistence(
            InfraPersistence::new(&InfraPersistenceConfig::from_env(), Domain::Signal),
        );
        let raised =
            crate::tracker::emit_divergence_concerns(&mut signal, &engine, &provider, &report);
        if !raised.is_empty() {
            println!(
                "\n{} conflict(s) raised as concerns — see `signal pending`.",
                raised.len()
            );
            for (chunk_id, d) in &report.divergences {
                println!("  {chunk_id}: {}", d.summary());
            }
        }
    }

    println!("\n{created} created, {updated} updated, {unchanged} unchanged.");
    if let Some(name) = &report.initiative_ensured {
        println!("initiative '{name}' holds the projects.");
    }
    if let Some(why) = &report.initiative_failure {
        println!("initiative could not be ensured (projects still landed): {why}");
    }
    if !report.relations_degraded.is_empty() {
        println!(
            "{} item(s) had dependencies listed as text instead of links.",
            report.relations_degraded.len()
        );
    }
    if !outbox.is_empty() {
        println!("{} item(s) queued to retry on the next push.", outbox.len());
    }
    // The second lane, reported separately rather than folded into the counts
    // above — one number covering two destinations would hide a companion that
    // wrote nothing behind a primary that wrote plenty.
    if let Some(run) = &companion {
        println!("\ncompanion lane '{}':", run.provider);
        if run.seeded > 0 {
            println!(
                "  {} item identit(ies) seeded from '{provider}' — first push for those.",
                run.seeded
            );
        }
        match (&run.report, &run.failure) {
            (Some(r), _) => println!(
                "  {} created, {} updated, {} unchanged.",
                r.counts_of(crate::tracker::ProjectionOutcome::is_created),
                r.counts_of(crate::tracker::ProjectionOutcome::is_patched),
                r.counts_of(crate::tracker::ProjectionOutcome::is_skipped),
            ),
            (None, Some(why)) => println!("  nothing was sent: {why}"),
            (None, None) => println!("  nothing was sent."),
        }
    }
    Ok(())
}

/// One sweep: the shared core of `tracker pull` and the realtime doorbell.
///
/// Both entry points call THIS, so "a webhook-triggered reconcile uses the same
/// code path as the sweep" is a fact about the call graph rather than a promise
/// in a comment. Returns the provider, the window that was actually asked for,
/// and the report.
///
/// The engine is loaded fresh from disk rather than shared. That is not a
/// shortcut: the sweep only READS it and writes nothing (pinned by
/// `the_sweep_writes_nothing`), and a disk snapshot is what lets the doorbell
/// run inside the realtime subscriber at all — a mutex guard cannot be held
/// across the fetch's await in a spawned task.
async fn sweep_once(
    data_dir: &std::path::Path,
    project_id: &str,
    config: &crate::tracker::TrackerConfig,
) -> Result<SweepRun> {
    let provider = config.provider.clone().unwrap_or_default();
    let engine = load_roadmap_engine();
    // ASYNC port construction: this function runs inside a runtime from all
    // three of its callers, and the sync path would build a nested one.
    let port = tracker_port_async(config).await?;

    // The caller's clock, taken BEFORE any I/O. The watermark advances to the
    // instant the run STARTED, never to the newest record seen — anything
    // written while the sweep is in flight must fall inside the next window
    // rather than into a gap nobody asks for again.
    let run_start = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);

    // Read the window BEFORE sweeping. `reconcile` advances the watermark on
    // success, so asking afterwards would report the window we are about to
    // use next as though it were the one we just asked for.
    let marks = crate::tracker::sweep::load(data_dir, project_id);
    let asked_since = marks
        .since(&provider)
        .unwrap_or("the beginning of time")
        .to_string();

    let report = crate::tracker::reconcile(&engine, port.as_ref(), data_dir, &run_start).await?;
    Ok(SweepRun {
        provider,
        asked_since,
        report,
        engine,
    })
}

/// What one sweep produced, including the engine it read.
///
/// The read-side twin of [`PushRun`], and it hands back the engine for the same
/// reason: a *caller* may want to act on what the sweep found, and the detector
/// itself must not. `reconcile` decides nothing — see
/// [`crate::tracker::propose_status_from_sweep`], which is deliberately
/// caller-invoked so that a caller who only wants to know what moved gets that
/// with nothing written.
///
/// The engine is DETACHED — `load_roadmap_engine` reads it from disk and it is
/// never the live server's `Arc<Mutex<_>>`. Writing through it is safe for the
/// reason tracker-auto-push established: `RoadmapEngine::persist` goes through
/// `infra::locked_merge_write`, so a throwaway engine merges rather than
/// clobbers.
struct SweepRun {
    provider: String,
    asked_since: String,
    report: crate::tracker::SweepReport,
    engine: crate::roadmap::engine::RoadmapEngine,
}

/// The realtime doorbell, wired to the realtime subscriber.
///
/// Lives here rather than in `cloud::events` so that module stays ignorant of
/// trackers, and so the port is still constructed in exactly one place.
pub struct CliTrackerSweeper;

impl crate::cloud::events::TrackerSweeper for CliTrackerSweeper {
    async fn sweep(&self, provider: &str) {
        let (data_dir, project_id, config) = tracker_config();
        if !crate::tracker::should_project(&config) {
            return;
        }
        // A doorbell for a provider this project does not mirror is not an
        // error — several projects share one tenant's socket — so it is simply
        // not ours to answer.
        if config.provider.as_deref() != Some(provider) {
            return;
        }
        match sweep_once(&data_dir, &project_id, &config).await {
            // MAY propose, when the operator has said so. The old shape here
            // argued "no human in front of it to dispose of one" — but disposal
            // is asynchronous by design (machine proposes, a human disposes
            // whenever they next look at `roadmap status`), and
            // `propose_status` is idempotent so a cadence cannot restamp an old
            // suggestion into looking new. What the old comment actually
            // protected was CONSENT to a new unattended writer, and that is now
            // explicit: `THINK_AND_SHIP_TRACKER_PROPOSE`, default off
            // (tracker-pull-proposal-unattended). `tracker_pull` still proposes
            // unconditionally — a human typed that command.
            Ok(mut run) => {
                tracing::debug!(
                    target: "think_and_ship::cloud",
                    "doorbell: swept {}, {} fetched / {} remote",
                    run.provider,
                    run.report.fetched,
                    run.report.remote.len()
                );
                let proposed = propose_unattended(&mut run, unattended_propose_enabled());
                if !proposed.is_empty() {
                    tracing::info!(
                        target: "think_and_ship::cloud",
                        "unattended sweep proposed status changes for: {}",
                        proposed.join(", ")
                    );
                }
            }
            // Swallowed ON PURPOSE. A doorbell is an optimization; a failed one
            // must degrade to the next `tracker pull`, never take down the
            // subscriber that also carries think/roadmap/signal refreshes.
            Err(e) => tracing::warn!(
                target: "think_and_ship::cloud",
                "doorbell sweep failed (the next `tracker pull` will still find it): {e}"
            ),
        }
    }
}

/// The unattended half of what `report_pull` does: turn what the sweep found
/// into status AND title PROPOSALS — nothing more. No printing, no
/// transitions, no edits, and nothing at all unless `enabled`.
///
/// Split out for the same reason `report_pull` was: so a test can EXECUTE the
/// decision with a `SweepRun` built against `FakeTracker`, instead of asserting
/// the call exists in source — which is how `propose_status_from_sweep` shipped
/// dead for a long time. The sweeper passes `unattended_propose_enabled()`;
/// the parse behind it has its own tests.
///
/// Titles ride the SAME consent switch as statuses rather than a second knob:
/// what `THINK_AND_SHIP_TRACKER_PROPOSE` gates is "may an unattended sweep
/// write proposals at all", not "which kind" — both are the identical
/// machine-proposes-human-disposes shape, and a second switch would just be a
/// second thing to forget.
///
/// Returns the chunk ids that received a proposal (empty when disabled). A
/// chunk retitled AND moved in the same window appears once.
fn propose_unattended(run: &mut SweepRun, enabled: bool) -> Vec<String> {
    if !enabled {
        return Vec::new();
    }
    let SweepRun {
        provider,
        report,
        engine,
        ..
    } = run;
    let mut proposed = crate::tracker::propose_status_from_sweep(engine, provider, report);
    for id in crate::tracker::propose_titles_from_sweep(
        engine,
        provider,
        report,
        &crate::tracker::Ownership::default(),
    ) {
        if !proposed.contains(&id) {
            proposed.push(id);
        }
    }
    proposed
}

/// One projection run: replay the outbox, then push every opted-in chunk.
///
/// The engine is loaded fresh from disk rather than shared, and unlike
/// [`sweep_once`] this one WRITES to it — which is safe for exactly one reason,
/// worth stating because it is the whole design. `RoadmapEngine::persist` goes
/// through `infra::locked_merge_write`: an exclusive lock, then
/// `merge_roadmaps(memory, disk)`, then an atomic rename. Tracker links union by
/// `(chunk_id, provider)` under last-write-wins, and a link the other side has
/// never seen is appended rather than dropped. So a detached engine writing
/// links while the server's live engine also writes cannot clobber either side
/// — the family-stores-merge-on-save discipline already solved this, and no
/// mutex is held across a network call because no shared engine is touched.
///
/// ASYNC port construction, for the same reason as `sweep_once`: the cadence
/// calls this from inside a runtime, and the sync path would build a nested one
/// — the panic that shipped and was fixed in e5213c8.
async fn push_once(
    data_dir: &std::path::Path,
    project_id: &str,
    config: &crate::tracker::TrackerConfig,
) -> Result<PushRun> {
    push_once_in(
        crate::tracker::registry::PROVIDERS,
        data_dir,
        project_id,
        config,
    )
    .await
}

/// [`push_once`] against an arbitrary registration table. Both lanes take the
/// same table, so a test drives the companion ordering this body documents
/// rather than reading it.
async fn push_once_in(
    registrations: &[crate::tracker::ProviderRegistration],
    data_dir: &std::path::Path,
    project_id: &str,
    config: &crate::tracker::TrackerConfig,
) -> Result<PushRun> {
    let mut engine = load_roadmap_engine();
    let port = tracker_port_async_in(registrations, config).await?;
    let outbox = crate::tracker::TrackerOutbox::new(Some(crate::tracker::TrackerOutbox::path_for(
        data_dir, project_id,
    )));
    // Replay anything left over BEFORE sending anything new, so a chunk's
    // history reaches the tracker in the order it happened.
    outbox.flush(port.as_ref()).await;
    // The roof's name: the configured one, or the directory basename a human
    // would recognize. NEVER the derived project id — the hash suffix is a
    // machine identity, and `TrackerConfig::initiative` documents the decision.
    let initiative = config
        .initiative
        .clone()
        .or_else(crate::infra::project_display_name);
    let report = crate::tracker::project::project_all_with_policy(
        &mut engine,
        port.as_ref(),
        Some(&outbox),
        &crate::tracker::ownership::Ownership::default(),
        initiative.as_deref(),
    )
    .await?;
    // THE RECEIPT, written here rather than in `tracker_push`, because the
    // caller that most needs it is the one with no console: the unattended
    // cadence logged its successes at `debug!` to a stderr nobody reads, so a
    // push that had been failing for two days looked exactly like one that was
    // working. Both callers run this body, so both leave a trace.
    //
    // A failed run returns early above and records nothing, which is the point
    // — the stamp means "this succeeded", not "this was attempted".
    let receipt = crate::tracker::receipt::PushReceipt {
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        created: report.counts_of(crate::tracker::ProjectionOutcome::is_created),
        updated: report.counts_of(crate::tracker::ProjectionOutcome::is_patched),
        unchanged: report.counts_of(crate::tracker::ProjectionOutcome::is_skipped),
    };
    if let Err(e) = crate::tracker::receipt::record(
        data_dir,
        project_id,
        &config.provider.clone().unwrap_or_default(),
        &receipt,
    ) {
        // A receipt that cannot be written must not fail a push that already
        // succeeded — the tracker has the writes either way, and losing the
        // stamp costs one line of reporting.
        tracing::warn!(
            target: "think_and_ship::tracker",
            "push succeeded but its receipt could not be recorded: {e}"
        );
    }
    // THE COMPANION LANE, after the primary and never before it. Ordering is
    // the contract: the companion takes its item identities FROM the primary's
    // links, so a chunk the primary has just created reaches the companion on
    // this same run rather than one run later.
    //
    // A companion failure must not cost the primary push that already
    // succeeded, so this reports and returns rather than propagating — the
    // primary's writes are on the tracker either way.
    let companion = push_companion(
        registrations,
        data_dir,
        project_id,
        config,
        &mut engine,
        &outbox,
    )
    .await;

    Ok(PushRun {
        report,
        companion,
        outbox,
        engine,
    })
}

/// Seed the companion lane from the primary's links, then project into it.
///
/// `Ok(None)` is the answer for the overwhelmingly common single-lane project.
/// Everything else is reported to the caller rather than returned as an error
/// from the push, because the primary lane has already written by the time this
/// runs.
async fn push_companion(
    registrations: &[crate::tracker::ProviderRegistration],
    data_dir: &std::path::Path,
    project_id: &str,
    config: &crate::tracker::TrackerConfig,
    engine: &mut crate::roadmap::engine::RoadmapEngine,
    outbox: &crate::tracker::TrackerOutbox,
) -> Option<CompanionRun> {
    let primary = config.provider.clone().unwrap_or_default();
    let lane = match crate::tracker::companion_lane(config) {
        Ok(None) => return None,
        Ok(Some(lane)) => lane.clone(),
        Err(reason) => {
            tracing::warn!(
                target: "think_and_ship::tracker",
                "companion lane not projected: {reason}"
            );
            return Some(CompanionRun {
                provider: config
                    .companion
                    .as_ref()
                    .map(|c| c.provider.clone())
                    .unwrap_or_default(),
                seeded: 0,
                report: None,
                failure: Some(reason),
            });
        }
    };

    // The seed BEFORE the port is built: a lane whose credential is missing
    // still gets its links recorded, so the next run with a working credential
    // starts from identities rather than from refusals.
    let seeded = match crate::tracker::seed_links_from(engine, &primary, &lane.provider) {
        Ok(report) => report.seeded.len(),
        Err(reason) => {
            tracing::warn!(
                target: "think_and_ship::tracker",
                "companion lane not seeded: {reason}"
            );
            return Some(CompanionRun {
                provider: lane.provider,
                seeded: 0,
                report: None,
                failure: Some(reason),
            });
        }
    };

    // The companion's own destination and its own credential — a synthesized
    // config rather than a second constructor, so the port is still built in
    // exactly one place.
    let lane_config = crate::tracker::TrackerConfig {
        provider: Some(lane.provider.clone()),
        target: Some(lane.target.clone()),
        companion: None,
        ..config.clone()
    };
    let port = match tracker_port_async_in(registrations, &lane_config).await {
        Ok(port) => port,
        Err(e) => {
            return Some(CompanionRun {
                provider: lane.provider,
                seeded,
                report: None,
                failure: Some(e.to_string()),
            });
        }
    };

    let initiative = lane_config
        .initiative
        .clone()
        .or_else(crate::infra::project_display_name);
    match crate::tracker::project::project_all_with_policy(
        engine,
        port.as_ref(),
        Some(outbox),
        &crate::tracker::ownership::Ownership::default(),
        initiative.as_deref(),
    )
    .await
    {
        Ok(report) => {
            let receipt = crate::tracker::receipt::PushReceipt {
                at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                created: report.counts_of(crate::tracker::ProjectionOutcome::is_created),
                updated: report.counts_of(crate::tracker::ProjectionOutcome::is_patched),
                unchanged: report.counts_of(crate::tracker::ProjectionOutcome::is_skipped),
            };
            // Filed under the COMPANION's key, so the two lanes' receipts do not
            // overwrite each other and "when did the board last update?" has an
            // answer that is not the issues lane's.
            if let Err(e) =
                crate::tracker::receipt::record(data_dir, project_id, &lane.provider, &receipt)
            {
                tracing::warn!(
                    target: "think_and_ship::tracker",
                    "companion push succeeded but its receipt could not be recorded: {e}"
                );
            }
            Some(CompanionRun {
                provider: lane.provider,
                seeded,
                report: Some(report),
                failure: None,
            })
        }
        Err(e) => Some(CompanionRun {
            provider: lane.provider,
            seeded,
            report: None,
            failure: Some(e.to_string()),
        }),
    }
}

/// What the companion lane did on one push, when a project has one.
struct CompanionRun {
    provider: String,
    seeded: usize,
    report: Option<crate::tracker::ProjectionReport>,
    failure: Option<String>,
}

/// Everything one [`push_once`] produced. A struct rather than a tuple because
/// the command needs all three — the report to print, the outbox to say what
/// will retry, and the engine to raise divergence concerns from.
struct PushRun {
    report: crate::tracker::ProjectionReport,
    /// The second lane's outcome, when this project has one. `None` is every
    /// project that does not, which is almost all of them.
    companion: Option<CompanionRun>,
    outbox: crate::tracker::TrackerOutbox,
    engine: crate::roadmap::engine::RoadmapEngine,
}

/// The unattended push (tracker-auto-push), wired to the cadence.
///
/// Lives here rather than in `cloud::events` for the same reason as
/// [`CliTrackerSweeper`]: that module stays ignorant of trackers, and the port
/// is constructed in exactly one place.
pub struct CliTrackerPusher;

impl crate::cloud::events::TrackerPusher for CliTrackerPusher {
    async fn push(&self, provider: &str) {
        let (data_dir, project_id, config) = tracker_config();
        // Re-read consent every cycle rather than trusting the value captured at
        // spawn: `tracker off` must stop the writing, not merely stop the next
        // restart from starting it.
        if !crate::tracker::should_project(&config) {
            return;
        }
        if config.provider.as_deref() != Some(provider) {
            return;
        }
        match push_once(&data_dir, &project_id, &config).await {
            Ok(run) => tracing::debug!(
                target: "think_and_ship::cloud",
                "auto-push: {} item(s) projected to {provider}, {} divergence(s)",
                run.report.outcomes.len(),
                run.report.divergences.len()
            ),
            // Swallowed ON PURPOSE, like the doorbell's. An unattended push is a
            // convenience over `tracker push`; a failed one must degrade to the
            // next cycle rather than take down the task that carries it.
            Err(e) => tracing::warn!(
                target: "think_and_ship::cloud",
                "auto-push failed (the next cycle or `tracker push` will retry): {e}"
            ),
        }
    }
}

/// The inbound half of [`tracker_push`] — the entry point the sweep
/// never had.
///
/// The sweep itself was correct and tested from the day it was written, but
/// nothing ever ran it: there was no command, no timer, no caller outside the
/// test suite. So "state recovers without webhooks" was true of the library and
/// false of the product, and every argument resting on it — including the later
/// decision to let a Linear delivery drop — was resting on something nobody
/// could run.
/// This is that gap closed, at the smallest surface that closes it.
///
/// Deliberately a command rather than a daemon. A schedule is `cron`, a
/// `/loop`, or a keypress, and none of those need to live in here; a background
/// thread would be a second thing to reason about for no property this does not
/// already have.
pub fn tracker_pull() -> Result<()> {
    let (data_dir, project_id, config) = tracker_config();
    if !crate::tracker::should_project(&config) {
        println!("mirroring is off, so there is nothing to check.");
        println!("turn it on with: think-and-ship tracker on --into <owner/repo>");
        return Ok(());
    }
    let runtime = tokio::runtime::Runtime::new()?;
    let mut run = runtime.block_on(sweep_once(&data_dir, &project_id, &config))?;
    report_pull(&mut run);
    Ok(())
}

/// Everything `tracker pull` does once the sweep has come back: say what moved,
/// and turn what moved into status and title PROPOSALS.
///
/// Split out from [`tracker_pull`] so it can be EXECUTED by a test rather than
/// inspected. `tracker_pull` itself is unreachable in a test — its first act is
/// to build a real GitHub or Linear port, and `build_tracker_port` knows only
/// those two, so there is no seam to inject a fake at. Everything after that
/// point lives here, which means a test drives a real sweep against
/// `FakeTracker` and then runs the ACTUAL production body, not a copy of it.
/// The one line left uncovered is the port construction, and that already has
/// its own probe (`sweep_once_does_not_build_a_runtime_inside_a_runtime`).
///
/// That distinction is the whole reason this split exists: a source-level call
/// is what shipped `propose_status_from_sweep` dead for a long time.
///
/// Returns the chunk ids that received a proposal, so a caller (and a test) can
/// see what the run decided. Takes `&mut SweepRun` rather than consuming it so a
/// test can inspect the ENGINE afterwards — asserting on the returned ids alone
/// would prove the function computed a proposal, not that it recorded one.
fn report_pull(run: &mut SweepRun) -> Vec<String> {
    let SweepRun {
        provider,
        asked_since,
        report,
        engine,
    } = run;

    println!("checked {provider} for changes since {asked_since}:\n");
    println!("  {} item(s) came back", report.fetched);
    println!("  {} were our own writes echoing back", report.echoes);

    if !report.drifted.is_empty() {
        // An echo whose content hash disagrees with what we recorded is
        // evidence the adapter's round trip is LOSSY — not evidence anybody
        // edited anything. Worth saying out loud, because it looks like a
        // remote change and is not one.
        println!(
            "\n{} echo(es) came back altered — the round trip is losing detail:",
            report.drifted.len()
        );
        for item in &report.drifted {
            println!("  {}", item.external_id.as_deref().unwrap_or("(no id)"));
        }
    }

    let mut proposed = Vec::new();
    if report.remote.is_empty() {
        println!("\nNothing changed on the tracker that we did not already know about.");
    } else {
        println!("\n{} genuine remote change(s):", report.remote.len());
        for item in &report.remote {
            println!(
                "  {}  {}",
                item.external_id.as_deref().unwrap_or("(no id)"),
                item.title
            );
        }
        // A remote change becomes a PROPOSAL here, at the caller, not inside
        // the sweep — the same seam the push side raises its divergence concerns
        // at, and the point where "we noticed" turns into "somebody will see
        // it". The sweep itself still decides nothing.
        //
        // (Named obliquely on purpose: `pull_does_not_write_to_the_roadmap`
        // greps this body for the push-side writer's name, so spelling it here
        // would trip a gate that is right to be literal.)
        //
        // This is the ONE write a read makes, and the exemption is narrow on
        // purpose (tracker-status-proposal-unreachable). A proposal
        // is not a transition: no chunk's real `status` moves, so pull still
        // cannot change what it read. What it can do is write down what it saw,
        // in the slot the roadmap already has for exactly that — the same
        // machine-proposes-human-disposes shape as `propose_reprioritize`.
        // Without this the whole chain was dead in production: the promise "a
        // tracker close proposes a status" was true of the library and false of
        // the product.
        proposed = crate::tracker::propose_status_from_sweep(engine, provider, report);
        // Same seam, same exemption, second field: a remote RETITLE with no
        // local edit reaches the plan through no other path — push cheap-skips
        // an unchanged chunk before reconcile can see the divergence, so
        // without this the durable-concession machinery
        // (tracker-contested-memory) was unreachable for a pull-only user.
        // The ownership table gates it (only a Contested title proposes), and
        // the echo fence already kept our own writes out of `report.remote`.
        let titled = crate::tracker::propose_titles_from_sweep(
            engine,
            provider,
            report,
            &crate::tracker::Ownership::default(),
        );
        if proposed.is_empty() && titled.is_empty() {
            println!("\nNothing was changed locally. These are for you to decide about.");
        }
        if !proposed.is_empty() {
            println!(
                "\n{} status proposal(s) recorded — nothing transitioned, these are \
                 suggestions for you to accept or reject:",
                proposed.len()
            );
            for chunk_id in &proposed {
                let suggested = engine
                    .roadmap()
                    .chunks
                    .iter()
                    .find(|c| &c.id == chunk_id)
                    .and_then(|c| c.status_proposal.as_ref())
                    .map(|p| format!("{:?}", p.suggested_status))
                    .unwrap_or_else(|| "(unknown)".to_string());
                println!("  {chunk_id} -> {suggested} (proposed, NOT applied)");
            }
            println!("\nNo chunk's status was changed. See `roadmap status`.");
        }
        if !titled.is_empty() {
            println!(
                "\n{} title proposal(s) recorded — no chunk was renamed, these are \
                 contested titles for you to accept or reject:",
                titled.len()
            );
            for chunk_id in &titled {
                let suggested = engine
                    .roadmap()
                    .chunks
                    .iter()
                    .find(|c| &c.id == chunk_id)
                    .and_then(|c| c.title_proposal.as_ref())
                    .map(|p| p.suggested_title.clone())
                    .unwrap_or_else(|| "(unknown)".to_string());
                println!("  {chunk_id} -> \"{suggested}\" (proposed, NOT applied)");
            }
            println!(
                "\nNo chunk's title was changed. Accept adopts the tracker's title; \
                 reject lets the plan's title flow again."
            );
        }
        for id in titled {
            if !proposed.contains(&id) {
                proposed.push(id);
            }
        }
    }

    if let Some(at) = &report.advanced_to {
        println!("\nNext check will ask for changes since {at}.");
    }
    proposed
}

/// The sweep is only a backstop if something can actually run it.
///
/// Source inspection rather than behaviour, for the same reason as
/// `project.rs`'s policy gate: the property is "the inbound path is REACHABLE",
/// and it was silently false for a long stretch of the project's history — the
/// sweep was correct, tested, and called by nothing outside the test suite. No
/// runtime assertion catches that, because a function nobody calls passes every
/// test it has.
#[cfg(test)]
mod sweep_reachability_tests {
    /// Scan only the production half — this module's own assertion strings name
    /// the calls it counts, and including them would make the test lie.
    fn production_source() -> &'static str {
        let whole = include_str!("mod.rs");
        whole
            .split_once("mod sweep_reachability_tests")
            .map_or(whole, |(before, _)| before)
    }

    #[test]
    fn the_sweep_has_a_production_caller() {
        let src = production_source();
        assert!(
            src.contains("crate::tracker::reconcile("),
            "nothing in the CLI calls the sweep. It has passed its tests since \
             the day it was written, and until `tracker pull` existed no \
             user could ever cause it to run — so 'state recovers without \
             webhooks' was a property of the library and not of the product"
        );
    }

    #[test]
    fn the_window_is_read_before_the_sweep_advances_it() {
        // A real bug, caught while building this and pinned here. `reconcile`
        // advances the watermark on success, so reading it afterwards reports
        // the window we will use NEXT as though it were the one just queried —
        // which reads as "no changes since <a moment ago>" forever.
        let src = production_source();
        let read_at = src
            .find("let asked_since")
            .expect("tracker_pull must read the watermark into `asked_since`");
        let sweep_at = src
            .find("crate::tracker::reconcile(")
            .expect("tracker_pull must call the sweep");
        assert!(
            read_at < sweep_at,
            "the watermark is read AFTER the sweep advanced it, so the reported \
             window is the next one rather than the one just asked for"
        );
    }

    /// THE TEST THAT SHOULD HAVE EXISTED MUCH EARLIER.
    ///
    /// Every other check here reads SOURCE — it proves the sweep is reachable,
    /// which is necessary and turned out not to be sufficient. `tracker pull`,
    /// the doorbell and the unattended cadence all reached `sweep_once`, and
    /// all three PANICKED on "Cannot start a runtime from within a runtime",
    /// because port construction built a nested runtime. Every gate stayed
    /// green the whole time; only running the installed binary found it.
    ///
    /// So this one EXECUTES the path inside a runtime. It needs no credential
    /// and no network: the panic happened during port construction, long before
    /// any request. A `Result` is fine — an error means it got far enough to
    /// fail honestly. A panic is the regression.
    #[tokio::test]
    async fn sweep_once_does_not_build_a_runtime_inside_a_runtime() {
        let config = crate::tracker::TrackerConfig {
            enabled: true,
            provider: Some("github".into()),
            target: Some("acme/widgets".into()),
            ..crate::tracker::TrackerConfig::default()
        };
        let dir = std::env::temp_dir().join("ts-sweep-runtime-probe");
        let _ = std::fs::create_dir_all(&dir);

        // Deliberately NOT unwrapped: reaching a Result at all is the pass.
        let outcome = super::sweep_once(&dir, "probe-project", &config).await;
        let _ = outcome;
    }

    /// The same probe for the WRITE side (tracker-auto-push).
    ///
    /// `tracker_push` carried the identical latent defect the pull side shipped:
    /// it built its own runtime and called the SYNC `tracker_port`. That never
    /// panicked only because nothing spawned it — the moment a cadence called it
    /// from inside the server's runtime it would have. This executes the path so
    /// the regression cannot come back the way it came the first time.
    ///
    /// Needs no credential and no network: the panic happened during port
    /// construction, long before any request. A `Result` is the pass; a panic is
    /// the regression.
    #[tokio::test]
    async fn push_once_does_not_build_a_runtime_inside_a_runtime() {
        let config = crate::tracker::TrackerConfig {
            enabled: true,
            provider: Some("github".into()),
            target: Some("acme/widgets".into()),
            ..crate::tracker::TrackerConfig::default()
        };
        let dir = std::env::temp_dir().join("ts-push-runtime-probe");
        let _ = std::fs::create_dir_all(&dir);

        let outcome = super::push_once(&dir, "probe-project", &config).await;
        let _ = outcome;
    }

    #[test]
    fn the_unattended_push_is_actually_spawned() {
        // Same check as `the_unattended_sweep_is_actually_spawned`: a cadence
        // nothing starts is a feature that does not exist. This is the exact
        // defect class untested-reachability-sweep was about.
        let src = production_source();
        assert!(
            src.contains("spawn_push_schedule("),
            "nothing starts the periodic push, so the roadmap still only reaches \
             the tracker when a human types `tracker push`"
        );
    }

    #[test]
    fn the_push_is_gated_on_consent_as_well_as_on_a_cadence() {
        // Two gates, both load-bearing. `should_project` is the per-project
        // consent the whole tracker feature rests on; the interval is what keeps
        // an unattended WRITER off by default.
        let src = production_source();
        let block = src
            .split_once("// The outbound twin (tracker-auto-push)")
            .expect("the auto-push spawn block must exist")
            .1;
        let block = block.split_once("Ok((").map_or(block, |(b, _)| b);
        assert!(
            block.contains("should_project(&tracker_cfg)"),
            "the unattended push must be gated on per-project consent"
        );
        assert!(
            block.contains("push_schedule_interval()"),
            "the unattended push must require an explicitly configured cadence, \
             or an upgrade silently starts writing to somebody's tracker"
        );
    }

    #[test]
    fn the_push_cadence_is_off_unless_somebody_asks_for_it() {
        // THE posture criterion. Absent means OFF here, unlike the sweep, where
        // absent means the default cadence. Reading on a timer and WRITING on a
        // timer are different things to consent to.
        assert_eq!(
            super::parse_push_interval(None),
            None,
            "an unset push interval must mean OFF — a user who upgrades and does \
             nothing must see no new network writes"
        );
        assert_eq!(
            super::parse_push_interval(Some("0")),
            None,
            "0 is the off switch"
        );
        assert_eq!(
            super::parse_push_interval(Some("  0 ")),
            None,
            "whitespace must not defeat the off switch"
        );
        assert_eq!(
            super::parse_push_interval(Some("nonsense")),
            None,
            "an unparseable PUSH interval must fall back to OFF, the opposite of \
             the sweep's fallback — an unattended writer starting on a typo is \
             worse than one that stays quiet"
        );
        assert_eq!(
            super::parse_push_interval(Some("300")),
            Some(std::time::Duration::from_secs(300)),
            "an explicit interval is the only thing that turns it on"
        );
    }

    #[test]
    fn the_propose_switch_announces_itself_at_startup() {
        // The visibility criterion (tracker-propose-switch-visibility): a
        // configured-and-on unattended WRITER must say so at startup, the way
        // the sweep and push cadences do. Default-off silence is allowed;
        // silence with the switch ON is not — an operator who set
        // THINK_AND_SHIP_TRACKER_PROPOSE=1 must see a line naming the side
        // effect they consented to.
        let src = production_source();
        let block = src
            .split_once("// The propose switch's announcement")
            .expect("the propose announcement block must exist in build_unified")
            .1;
        let block = block.split_once("\n    Ok((").map_or(block, |(b, _)| b);
        assert!(
            block.contains("should_project(&tracker_cfg)"),
            "the announcement must be gated on the tracker being configured — \
             a propose switch with no tracker gates nothing worth announcing"
        );
        assert!(
            block.contains("unattended_propose_enabled()"),
            "the announcement must be gated on the switch itself, or it would \
             announce an unattended writer that is off"
        );
        assert!(
            block.contains("eprintln!") && block.contains("tracker propose:"),
            "the configured-and-on propose switch must announce itself in the \
             same voice as the sweep/push startup lines"
        );
    }

    #[test]
    fn the_unattended_sweep_is_actually_spawned() {
        // Same check as `the_sweep_has_a_production_caller`, one level up: a
        // cadence nothing starts is a floor that does not exist.
        let src = production_source();
        assert!(
            src.contains("spawn_sweep_schedule("),
            "nothing starts the periodic sweep, so convergence still depends on \
             a human typing `tracker pull`"
        );
    }

    #[test]
    fn the_floor_is_gated_on_the_tracker_not_on_the_cloud() {
        // The trap this test guards against: spawn_realtime (and the
        // doorbell it carries) only runs with cloud sync configured, so hanging
        // the floor there would miss the local-only tracker user entirely —
        // the one person with no doorbell at all.
        let src = production_source();
        let block = src
            .split_once("let (_, _, tracker_cfg) = tracker_config();")
            .expect("the floor must resolve the tracker config")
            .1;
        let block = block.split_once("\n    Ok((").map_or(block, |(b, _)| b);

        assert!(
            block.contains("should_project(&tracker_cfg)"),
            "the floor must be gated on the tracker being configured"
        );
        assert!(
            !block.contains("cloud_client"),
            "the floor must NOT depend on cloud sync — that is the doorbell's \
             gate, and it excludes exactly the user who needs a floor most"
        );
    }

    #[test]
    fn pull_does_not_write_to_the_roadmap() {
        // The sweep is a detector. `tracker push` legitimately writes (and
        // emits concerns); pull must not, or the two directions stop being
        // separable and a read starts changing what it read.
        let src = production_source();
        let after = src
            .split_once("pub fn tracker_pull()")
            .expect("tracker_pull must exist")
            .1;

        // `tracker_pull`'s own body, and then the whole pull PATH — the command
        // plus the `report_pull` seam it delegates to. Both slices are taken
        // explicitly rather than left to chance: `report_pull` happens to sit
        // inside the old single window, and a gate that guards the right code by
        // accident is the failure mode this whole test is about.
        let own_body = after
            .split_once("\nfn report_pull(")
            .map_or(after, |(body, _)| body);
        let pull = after
            .split_once("\npub fn ")
            .map_or(after, |(body, _)| body);
        assert!(
            pull.contains("\nfn report_pull("),
            "report_pull left the scanned pull path, so everything this test \
             asserts about proposing and transitioning is now vacuous"
        );
        // Matched WITH its indentation, so the call must be an unconditional
        // top-level statement of `tracker_pull` and not a mention buried in a
        // branch. Found by deliberately breaking it: wrapping the call in
        // `if config.enabled && !config.enabled` left every other test in this
        // module green, because a grep cannot tell a call from a dead call.
        // Four spaces can.
        //
        // The residual limit is real and worth stating rather than papering over:
        // no source scan proves a call EXECUTES. The link from `tracker_pull` to
        // `report_pull` is the one hop this module can only assert structurally,
        // because `tracker_pull`'s first act is building a real GitHub or Linear
        // port and there is no seam to inject a fake at. Everything past that hop
        // is covered by the execution probes below.
        assert!(
            own_body.contains("\n    report_pull(&mut run);"),
            "tracker_pull does not call report_pull as an unconditional statement, \
             so `tracker pull` may report and propose NOTHING while every other \
             assertion here stays green — which is the failure mode this test \
             exists to close"
        );

        for forbidden in ["project_all", "upsert_item", "emit_divergence_concerns"] {
            assert!(
                !pull.contains(forbidden),
                "tracker_pull calls `{forbidden}`, which writes. Pull reports; \
                 deciding what a remote change MEANS belongs to a human"
            );
        }

        // THE ONE EXEMPTION, and it is listed rather than merely absent.
        //
        // This gate FAILED OPEN for a long time: it named
        // three writers and `propose_status_from_sweep` was not among them, so
        // wiring the proposal in would have passed silently and the invariant
        // everyone believed was guarded would have been unguarded by accident.
        // That is the same failed-open shape the sweep-reachability hunt found
        // twice. Naming it here converts an accident into a decision.
        //
        // The exemption is narrow and load-bearing: a proposal is NOT a
        // transition. `propose_status` only fills `status_proposal`; a chunk's
        // real `status` never moves, so pull still does not change what it read.
        // If that ever stops being true this assertion is the thing that must be
        // re-argued, not quietly deleted.
        assert!(
            pull.contains("propose_status_from_sweep"),
            "tracker_pull no longer proposes a status. If that was deliberate, the \
             whole chain (propose_status_from_sweep, RoadmapEngine::propose_status, \
             Chunk::status_proposal) is now dead in production again and should go \
             with it — that is exactly the state \
             tracker-status-proposal-unreachable was filed about"
        );

        // THE SECOND EXEMPTION, argued separately rather than waved through on
        // the first one's ticket, because the first one's argument does not
        // automatically cover it. Re-made here in full: a title proposal is NOT
        // a rename. `propose_title` only fills `title_proposal`; the chunk's
        // real `title` never moves, so pull still cannot change what it read.
        // And it must live on the PULL side or nowhere: a remote retitle with
        // no local edit never reaches push's reconcile (the cheap-skip fires
        // before any I/O), so the sweep is the only path on which that edit is
        // ever seen. Without this exemption the durable-concession machinery
        // (tracker-contested-memory) is unreachable for a pull-only user —
        // true of the library, false of the product, the same disease as
        // tracker-status-proposal-unreachable in a second field.
        assert!(
            pull.contains("propose_titles_from_sweep"),
            "tracker_pull no longer proposes a title. If that was deliberate, the \
             pull-only user has lost their ONLY path into the title-concession \
             machinery (propose_titles_from_sweep, RoadmapEngine::propose_title, \
             Chunk::title_proposal) — a remote retitle with no local edit never \
             reaches push's reconcile, so that chain is dead in production again; \
             tracker-sweep-title-proposal is the chunk that was filed about \
             exactly this"
        );
        assert!(
            !pull.contains("set_status") && !pull.contains("complete_chunk"),
            "tracker_pull TRANSITIONS a chunk. The proposal exemption covers \
             writing down a suggestion, never applying one — a close means the \
             ticket is finished, not that the acceptance criteria were met"
        );
        assert!(
            !pull.contains("resolve_title_proposal") && !pull.contains("update_chunk"),
            "tracker_pull RESOLVES or EDITS a chunk. The title exemption covers \
             recording a contested suggestion, never adopting one — accepting a \
             tracker's title into the plan is the human act both proposals exist \
             to preserve"
        );
    }

    // ----------------------------------------------------------------------
    // The EXECUTION probes. Everything above this line inspects source, and
    // source inspection is precisely what let `propose_status_from_sweep` ship
    // dead for a long time: it had passing tests the whole time, because a
    // function nobody calls passes every test it has. These run the production
    // body instead.
    // ----------------------------------------------------------------------

    /// A real engine holding one opted-in chunk, projected into a fake tracker
    /// so the link the proposal needs actually exists.
    ///
    /// No persistence: `RoadmapEngine::persist` is a no-op without it, so these
    /// tests can never touch the developer's real roadmap on disk.
    async fn projected(
        chunk_id: &str,
        provider: &str,
        tracker: &crate::tracker::FakeTracker,
    ) -> crate::roadmap::engine::RoadmapEngine {
        let mut e = crate::roadmap::engine::RoadmapEngine::new("pull-probe".into());
        e.add_chunk(
            chunk_id.into(),
            format!("Chunk {chunk_id}"),
            crate::roadmap::domain::ChunkStatus::Pending,
            10,
            format!("why {chunk_id} exists"),
            vec![format!("{chunk_id} works")],
            vec![],
            false,
        )
        .expect("add chunk");
        e.set_tracker_opt_in(chunk_id, provider, true)
            .expect("opt in");
        crate::tracker::project::project_all(&mut e, tracker, None)
            .await
            .expect("project");
        e
    }

    /// THE promise this feature exists to make true, executed rather than read.
    ///
    /// Somebody closes the ticket in the tracker. `tracker pull`'s real body
    /// runs. The chunk gets a PROPOSAL and does NOT move — because a close means
    /// the ticket is finished, not that the acceptance criteria were met.
    #[tokio::test]
    async fn pull_turns_a_remote_close_into_a_proposal_and_moves_nothing() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let provider = "fake";
        let tracker = crate::tracker::FakeTracker::new(provider);
        tracker.set_clock("2026-07-26T10:00:00+00:00");
        let engine = projected("c1", provider, &tracker).await;

        let link = engine.tracker_link("c1", provider).expect("link").clone();
        tracker.set_clock("2026-07-26T11:00:00+00:00");
        tracker.remote_edit(&link.external_id, |item| {
            item.state = crate::tracker::WorkItemState::Done;
        });

        let report =
            crate::tracker::reconcile(&engine, &tracker, dir.path(), "2026-07-26T12:00:00+00:00")
                .await
                .expect("sweep");
        assert!(
            !report.remote.is_empty(),
            "the fixture must produce a genuine remote change, or this test \
             asserts nothing"
        );

        // The ACTUAL production body — not a reimplementation of it.
        let mut run = super::SweepRun {
            provider: provider.into(),
            asked_since: "the beginning of time".into(),
            report,
            engine,
        };
        let proposed = super::report_pull(&mut run);

        assert_eq!(
            proposed,
            vec!["c1".to_string()],
            "a remote close on a linked chunk must produce exactly one proposal"
        );
    }

    /// The other half of the same promise, and the one the pinned invariant
    /// cares about: pull may write a suggestion, never a transition.
    #[tokio::test]
    async fn the_proposal_is_recorded_and_surfaced_without_the_status_moving() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let provider = "fake";
        let tracker = crate::tracker::FakeTracker::new(provider);
        tracker.set_clock("2026-07-26T10:00:00+00:00");
        let mut engine = projected("c2", provider, &tracker).await;

        let link = engine.tracker_link("c2", provider).expect("link").clone();
        tracker.set_clock("2026-07-26T11:00:00+00:00");
        tracker.remote_edit(&link.external_id, |item| {
            item.state = crate::tracker::WorkItemState::Done;
        });
        let report =
            crate::tracker::reconcile(&engine, &tracker, dir.path(), "2026-07-26T12:00:00+00:00")
                .await
                .expect("sweep");

        // Drive the production body, then inspect the engine it wrote through.
        let mut run = super::SweepRun {
            provider: provider.into(),
            asked_since: "whenever".into(),
            report,
            engine,
        };
        super::report_pull(&mut run);
        engine = run.engine;

        let chunk = engine
            .roadmap()
            .chunks
            .iter()
            .find(|c| c.id == "c2")
            .expect("chunk survives the pull");

        let proposal = chunk
            .status_proposal
            .as_ref()
            .expect("the proposal must be RECORDED, not merely returned");
        assert_eq!(
            proposal.suggested_status,
            crate::roadmap::domain::ChunkStatus::Done,
            "a closed ticket suggests done"
        );
        assert_eq!(
            proposal.source,
            format!("ext:{provider}/{}", link.external_id),
            "the proposal must say which ticket caused it, or a human cannot \
             check the evidence"
        );

        // THE INVARIANT. Pull wrote a suggestion and nothing else.
        assert_eq!(
            chunk.status,
            crate::roadmap::domain::ChunkStatus::Pending,
            "pull TRANSITIONED the chunk. A proposal is never a transition — \
             transitioning silently removes the one moment a human was going to \
             look at the evidence"
        );

        // And it is VISIBLE. A proposal no surface reports is the same defect as
        // a proposal nothing can write: still false of the product.
        let status = engine.status();
        let seen = status["chunks"]
            .as_array()
            .expect("chunks array")
            .iter()
            .find(|c| c["id"] == "c2")
            .expect("c2 is active, so it is listed");
        assert_eq!(
            seen["has_status_proposal"], true,
            "the proposal was written but `roadmap status` does not report it, \
             so no human can act on it"
        );
        assert_eq!(
            seen["status"], "pending",
            "the surfaced status must still be the real one"
        );
    }

    // ── unattended proposing (tracker-pull-proposal-unattended) ───────────

    /// The one hop the execution probes below cannot cover: the sweeper
    /// actually handing its run to the decision. Structural, with indentation,
    /// for the reason `pull_does_not_write_to_the_roadmap` records: a bare
    /// grep cannot tell a live call from a dead one, but the exact indented
    /// statement can only appear as the sweeper's own unconditional line.
    #[test]
    fn the_sweeper_hands_the_decision_to_propose_unattended() {
        let src = production_source();
        let sweeper = src
            .split_once("impl crate::cloud::events::TrackerSweeper for CliTrackerSweeper")
            .expect("the sweeper impl must exist")
            .1;
        let sweeper = sweeper.split_once("\n}\n").map_or(sweeper, |(b, _)| b);
        assert!(
            sweeper.contains(
                "\n                let proposed = \
                 propose_unattended(&mut run, unattended_propose_enabled());"
            ),
            "CliTrackerSweeper::sweep no longer hands its run to \
             propose_unattended under the parsed switch — the unattended \
             proposal path is dead in production again, which is the exact \
             state tracker-pull-proposal-unattended was filed to end"
        );
    }

    #[test]
    fn the_propose_switch_is_off_unless_somebody_asks_for_it() {
        // The writer's fallback direction, same as the push cadence: a user who
        // upgrades and does nothing must see no new writes, and a typo must
        // fail quiet rather than start a writer.
        //
        // Since `mcp-elicitation-consent` the parse lives in
        // `tracker::propose_consent` because there are now two sources; these
        // claims are the ENV half, held against an undecided remembered answer
        // so that this stays a test of the switch alone.
        use crate::tracker::propose_consent::{ProposeConsent, resolve};
        let undecided = ProposeConsent::default();
        assert!(!resolve(None, &undecided), "unset must mean OFF");
        assert!(!resolve(Some("0"), &undecided), "0 is the off switch");
        assert!(
            !resolve(Some("false"), &undecided),
            "false is the off switch"
        );
        assert!(
            !resolve(Some("nonsense"), &undecided),
            "an unparseable value must fall back to OFF — an unattended writer \
             starting on a typo is worse than one that stays quiet"
        );
        assert!(resolve(Some("1"), &undecided), "an explicit 1 turns it on");
        assert!(
            resolve(Some(" true "), &undecided),
            "an explicit true turns it on, whitespace notwithstanding"
        );
    }

    /// The unattended path EXECUTED, both postures. Disabled: the sweep found a
    /// genuine remote change and wrote nothing. Enabled: the same finding
    /// becomes a recorded proposal — and never a transition.
    #[tokio::test]
    async fn the_unattended_sweep_proposes_only_when_switched_on() {
        let provider = "fake";
        let tracker = crate::tracker::FakeTracker::new(provider);
        tracker.set_clock("2026-07-26T10:00:00+00:00");
        let engine = projected("c3", provider, &tracker).await;

        let link = engine.tracker_link("c3", provider).expect("link").clone();
        tracker.set_clock("2026-07-26T11:00:00+00:00");
        tracker.remote_edit(&link.external_id, |item| {
            item.state = crate::tracker::WorkItemState::Done;
        });

        // DISABLED (the default posture): the decision runs and declines.
        let dir_off = tempfile::TempDir::new().expect("tempdir");
        let report = crate::tracker::reconcile(
            &engine,
            &tracker,
            dir_off.path(),
            "2026-07-26T12:00:00+00:00",
        )
        .await
        .expect("sweep");
        assert!(!report.remote.is_empty(), "fixture must find the change");
        let mut run = super::SweepRun {
            provider: provider.into(),
            asked_since: "whenever".into(),
            report,
            engine,
        };
        let proposed = super::propose_unattended(&mut run, false);
        assert!(proposed.is_empty(), "disabled must mean NOTHING is written");
        assert!(
            run.engine
                .roadmap()
                .chunks
                .iter()
                .find(|c| c.id == "c3")
                .and_then(|c| c.status_proposal.as_ref())
                .is_none(),
            "disabled, yet a proposal landed on the chunk"
        );

        // ENABLED: the same finding becomes a recorded proposal. A fresh
        // watermark dir re-delivers the change (reconcile advanced the first
        // dir's watermark).
        let dir_on = tempfile::TempDir::new().expect("tempdir");
        let report = crate::tracker::reconcile(
            &run.engine,
            &tracker,
            dir_on.path(),
            "2026-07-26T12:00:00+00:00",
        )
        .await
        .expect("sweep");
        run.report = report;
        let proposed = super::propose_unattended(&mut run, true);
        assert_eq!(proposed, vec!["c3".to_string()]);

        let chunk = run
            .engine
            .roadmap()
            .chunks
            .iter()
            .find(|c| c.id == "c3")
            .expect("chunk");
        assert_eq!(
            chunk
                .status_proposal
                .as_ref()
                .expect("the proposal must be RECORDED")
                .suggested_status,
            crate::roadmap::domain::ChunkStatus::Done
        );
        assert_eq!(
            chunk.status,
            crate::roadmap::domain::ChunkStatus::Pending,
            "unattended may suggest, never transition"
        );
    }

    /// A cadence fires every few minutes forever, so the criterion is not "it
    /// proposes" but "proposing twice changes nothing": `proposed_at` must not
    /// advance when the suggestion has not changed, or every old suggestion
    /// looks perpetually new.
    #[tokio::test]
    async fn an_unattended_proposal_is_idempotent_across_cycles() {
        let provider = "fake";
        let tracker = crate::tracker::FakeTracker::new(provider);
        tracker.set_clock("2026-07-26T10:00:00+00:00");
        let engine = projected("c4", provider, &tracker).await;

        let link = engine.tracker_link("c4", provider).expect("link").clone();
        tracker.set_clock("2026-07-26T11:00:00+00:00");
        tracker.remote_edit(&link.external_id, |item| {
            item.state = crate::tracker::WorkItemState::Done;
        });

        // Each cycle sweeps with its own watermark dir, so the SAME remote
        // change is delivered both times — two processes, or a doorbell and a
        // cadence, seeing one event.
        let mut run = super::SweepRun {
            provider: provider.into(),
            asked_since: "whenever".into(),
            report: Default::default(),
            engine,
        };
        let mut stamps = Vec::new();
        for _ in 0..2 {
            let dir = tempfile::TempDir::new().expect("tempdir");
            run.report = crate::tracker::reconcile(
                &run.engine,
                &tracker,
                dir.path(),
                "2026-07-26T12:00:00+00:00",
            )
            .await
            .expect("sweep");
            super::propose_unattended(&mut run, true);
            stamps.push(
                run.engine
                    .roadmap()
                    .chunks
                    .iter()
                    .find(|c| c.id == "c4")
                    .and_then(|c| c.status_proposal.as_ref())
                    .expect("proposal recorded")
                    .proposed_at
                    .clone(),
            );
        }
        assert_eq!(
            stamps[0], stamps[1],
            "an unchanged suggestion was restamped — a cadence would make every \
             old suggestion look perpetually new"
        );
    }

    // ── title proposals from the sweep (tracker-sweep-title-proposal) ─────

    /// THE promise this feature exists to make true, executed end-to-end: a
    /// human retitles the ticket in the tracker and the plan NEVER edits that
    /// chunk locally, so push cheap-skips and reconcile never sees the
    /// divergence — `tracker pull` is the only path on which the retitle is
    /// ever seen, and its real body must turn it into a recorded, visible
    /// TitleProposal without renaming anything.
    #[tokio::test]
    async fn pull_turns_a_remote_retitle_into_a_title_proposal_and_renames_nothing() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let provider = "fake";
        let tracker = crate::tracker::FakeTracker::new(provider);
        tracker.set_clock("2026-07-26T10:00:00+00:00");
        let engine = projected("c5", provider, &tracker).await;

        let link = engine.tracker_link("c5", provider).expect("link").clone();
        tracker.set_clock("2026-07-26T11:00:00+00:00");
        // ONLY the title moves. The state stays Todo (= the chunk's Pending),
        // so anything this test observes came through the title path alone.
        tracker.remote_edit(&link.external_id, |item| {
            item.title = "A human's better name".into();
        });

        let report =
            crate::tracker::reconcile(&engine, &tracker, dir.path(), "2026-07-26T12:00:00+00:00")
                .await
                .expect("sweep");
        assert!(
            !report.remote.is_empty(),
            "the fixture must produce a genuine remote change, or this test \
             asserts nothing"
        );

        // The ACTUAL production body — not a reimplementation of it.
        let mut run = super::SweepRun {
            provider: provider.into(),
            asked_since: "whenever".into(),
            report,
            engine,
        };
        let proposed = super::report_pull(&mut run);
        assert_eq!(
            proposed,
            vec!["c5".to_string()],
            "a remote retitle on a linked chunk must produce exactly one proposal"
        );

        let chunk = run
            .engine
            .roadmap()
            .chunks
            .iter()
            .find(|c| c.id == "c5")
            .expect("chunk survives the pull");
        let p = chunk
            .title_proposal
            .as_ref()
            .expect("the proposal must be RECORDED, not merely returned");
        assert_eq!(p.suggested_title, "A human's better name");
        assert_eq!(
            p.source,
            format!("ext:{provider}/{}", link.external_id),
            "the proposal must say which ticket caused it, or a human cannot \
             check the evidence"
        );
        assert!(
            chunk.status_proposal.is_none(),
            "the state did not move, so a status proposal here means the title \
             path leaked into the status path"
        );

        // THE INVARIANT. Pull wrote a suggestion and nothing else.
        assert_eq!(
            chunk.title, "Chunk c5",
            "pull RENAMED the chunk. A proposal is never an edit — adopting a \
             tracker's title is the human act the proposal exists to preserve"
        );

        // And it is VISIBLE — a proposal no surface reports is still false of
        // the product.
        let status = run.engine.status();
        let seen = status["chunks"]
            .as_array()
            .expect("chunks array")
            .iter()
            .find(|c| c["id"] == "c5")
            .expect("c5 is active, so it is listed");
        assert_eq!(
            seen["has_title_proposal"], true,
            "the proposal was written but `roadmap status` does not report it, \
             so no human can act on it"
        );
    }

    /// The unattended path, both postures, titles specifically: the same
    /// consent switch that gates status proposals gates these — a user who
    /// upgrades and does nothing must see no new unattended writer.
    #[tokio::test]
    async fn the_unattended_sweep_proposes_titles_only_when_switched_on() {
        let provider = "fake";
        let tracker = crate::tracker::FakeTracker::new(provider);
        tracker.set_clock("2026-07-26T10:00:00+00:00");
        let engine = projected("c6", provider, &tracker).await;

        let link = engine.tracker_link("c6", provider).expect("link").clone();
        tracker.set_clock("2026-07-26T11:00:00+00:00");
        tracker.remote_edit(&link.external_id, |item| {
            item.title = "A human's better name".into();
        });

        // DISABLED (the default posture): the decision runs and declines.
        let dir_off = tempfile::TempDir::new().expect("tempdir");
        let report = crate::tracker::reconcile(
            &engine,
            &tracker,
            dir_off.path(),
            "2026-07-26T12:00:00+00:00",
        )
        .await
        .expect("sweep");
        assert!(!report.remote.is_empty(), "fixture must find the change");
        let mut run = super::SweepRun {
            provider: provider.into(),
            asked_since: "whenever".into(),
            report,
            engine,
        };
        let proposed = super::propose_unattended(&mut run, false);
        assert!(proposed.is_empty(), "disabled must mean NOTHING is written");
        assert!(
            run.engine
                .roadmap()
                .chunks
                .iter()
                .find(|c| c.id == "c6")
                .and_then(|c| c.title_proposal.as_ref())
                .is_none(),
            "disabled, yet a title proposal landed on the chunk"
        );

        // ENABLED: the same finding becomes a recorded proposal — and never a
        // rename. A fresh watermark dir re-delivers the change.
        let dir_on = tempfile::TempDir::new().expect("tempdir");
        run.report = crate::tracker::reconcile(
            &run.engine,
            &tracker,
            dir_on.path(),
            "2026-07-26T12:00:00+00:00",
        )
        .await
        .expect("sweep");
        let proposed = super::propose_unattended(&mut run, true);
        assert_eq!(proposed, vec!["c6".to_string()]);

        let chunk = run
            .engine
            .roadmap()
            .chunks
            .iter()
            .find(|c| c.id == "c6")
            .expect("chunk");
        assert_eq!(
            chunk
                .title_proposal
                .as_ref()
                .expect("the proposal must be RECORDED")
                .suggested_title,
            "A human's better name"
        );
        assert_eq!(
            chunk.title, "Chunk c6",
            "unattended may suggest, never rename"
        );
    }

    // ── tracker setup (tracker-setup-seamless) ────────────────────────────
    //
    // These drive `run_setup`, which is the WHOLE of `tracker setup` after the
    // port is built. The split exists so these can run at all — see
    // tracker-port-test-seam.

    fn setup_req(into: &str) -> super::SetupRequest {
        super::SetupRequest {
            provider: "fake".into(),
            into: into.into(),
            name: None,
            band: None,
            initiative: None,
            push: false,
            // 0 so the default case never touches an editor config; the
            // auto-push tests opt in explicitly.
            push_secs: 0,
            yes: false,
            dry_run: false,
        }
    }

    /// The roof's name, reachable (tracker-initiative-name-reachable): a name
    /// given at setup round-trips into the config trimmed; a re-setup WITHOUT
    /// the flag preserves it (the enable() carry-through from 1c14ceb,
    /// regression-guarded here); a blank value is a no-op rather than a roof
    /// named "".
    #[tokio::test]
    async fn an_initiative_named_at_setup_round_trips_and_survives_a_re_setup() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let tracker = crate::tracker::FakeTracker::new("fake");
        let mut engine = setup_engine();
        let cwd = dir.path().join("absent.json");

        let mut req = setup_req("ENG");
        req.initiative = Some("  Roadmap 2027  ".into());
        super::run_setup(&tracker, &mut engine, &req, dir.path(), "setup-probe", &cwd)
            .await
            .expect("setup");
        assert_eq!(
            crate::tracker::config::load(dir.path(), "setup-probe")
                .initiative
                .as_deref(),
            Some("Roadmap 2027"),
            "the chosen name must round-trip into the config, trimmed"
        );

        // Re-setup with NO initiative: the chosen name survives.
        super::run_setup(
            &tracker,
            &mut engine,
            &setup_req("ENG"),
            dir.path(),
            "setup-probe",
            &cwd,
        )
        .await
        .expect("re-setup");
        assert_eq!(
            crate::tracker::config::load(dir.path(), "setup-probe")
                .initiative
                .as_deref(),
            Some("Roadmap 2027"),
            "a re-setup without the flag silently discarded the human's name"
        );

        // A blank value is a no-op, not a roof named "".
        let mut blank = setup_req("ENG");
        blank.initiative = Some("   ".into());
        super::run_setup(
            &tracker,
            &mut engine,
            &blank,
            dir.path(),
            "setup-probe",
            &cwd,
        )
        .await
        .expect("blank setup");
        assert_eq!(
            crate::tracker::config::load(dir.path(), "setup-probe")
                .initiative
                .as_deref(),
            Some("Roadmap 2027"),
            "a whitespace-only value must not clear or blank the name"
        );
    }

    /// An engine holding one chunk per band plus a done one, with no
    /// persistence — so nothing here can reach the developer's real roadmap.
    fn setup_engine() -> crate::roadmap::engine::RoadmapEngine {
        use crate::roadmap::domain::ChunkStatus;
        let mut e = crate::roadmap::engine::RoadmapEngine::new("setup-probe".into());
        for (id, priority, status) in [
            ("crit", 50u32, ChunkStatus::Pending),
            ("high", 150, ChunkStatus::Backlog),
            ("finished", 60, ChunkStatus::Pending),
        ] {
            e.add_chunk(
                id.into(),
                format!("Chunk {id}"),
                status,
                priority,
                format!("why {id}"),
                vec![format!("{id} works")],
                vec![],
                false,
            )
            .expect("add chunk");
        }
        // Drive the third to done through the real lifecycle, so "active only"
        // is tested against a genuinely completed chunk.
        e.set_status("finished", ChunkStatus::InProgress)
            .expect("start");
        e.set_status("finished", ChunkStatus::Done).expect("finish");
        e
    }

    /// THE seamless promise: one call verifies the destination, turns mirroring
    /// on, and includes the active items.
    #[tokio::test]
    async fn setup_verifies_the_destination_then_includes_every_active_item() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let tracker = crate::tracker::FakeTracker::new("fake");
        let mut engine = setup_engine();

        let outcome = super::run_setup(
            &tracker,
            &mut engine,
            &setup_req("ENG"),
            dir.path(),
            "setup-probe",
            &dir.path().join("absent.json"),
        )
        .await
        .expect("setup");

        assert!(outcome.target_existed, "the fake's destination is present");
        assert!(
            !outcome.target_created,
            "nothing may be provisioned when the destination already exists"
        );
        let mut included = outcome.included.clone();
        included.sort();
        assert_eq!(
            included,
            vec!["crit".to_string(), "high".to_string()],
            "every ACTIVE chunk, and the done one left alone — mirroring finished \
             work would bury the tracker"
        );
        assert!(
            crate::tracker::should_project(&crate::tracker::config::load(
                dir.path(),
                "setup-probe"
            )),
            "mirroring must actually be ON afterwards"
        );
    }

    /// The defect this fix exists to end, from the other side: a destination
    /// that is not there must stop the run BEFORE any config is written.
    #[tokio::test]
    async fn a_missing_destination_writes_no_config_when_creation_is_declined() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let tracker = crate::tracker::FakeTracker::new("fake");
        tracker.set_target_missing();
        let mut engine = setup_engine();

        // `yes` stays false and stdin is not a TTY under `cargo test`, so
        // `confirm` answers no — which is exactly the non-interactive guarantee.
        //
        // That reason used to be written here as "stdin is not a tty, so the
        // read fails", and it was false. A non-tty does not fail: `read_line`
        // errors on a broken descriptor and returns `Ok(0)` at EOF, and an
        // open-but-silent stdin sends neither. Run this very test with a live
        // pipe on stdin before `prompt_line` existed and it did not decline —
        // it hung, for as long as the pipe was held. The guarantee is the tty
        // check in `prompt_line`, not anything about the read.
        let outcome = super::run_setup(
            &tracker,
            &mut engine,
            &setup_req("ZZZ"),
            dir.path(),
            "setup-probe",
            &dir.path().join("absent.json"),
        )
        .await
        .expect("declining is a successful outcome, not an error");

        assert!(!outcome.target_created, "declined means not created");
        assert!(
            tracker.created_targets().is_empty(),
            "nothing may be provisioned without an explicit yes"
        );
        assert!(
            outcome.included.is_empty(),
            "no items may be included when the destination was never reached"
        );
        assert!(
            !crate::tracker::should_project(&crate::tracker::config::load(
                dir.path(),
                "setup-probe"
            )),
            "THE BUG: config must NOT be written for a destination that does not \
             exist. `tracker on` wrote it first and discovered the truth at push"
        );
    }

    /// With an explicit yes, the destination is provisioned exactly once and the
    /// run continues through to inclusion.
    #[tokio::test]
    async fn an_explicit_yes_provisions_the_destination_once_and_carries_on() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let tracker = crate::tracker::FakeTracker::new("fake");
        tracker.set_target_missing();
        let mut engine = setup_engine();

        let mut req = setup_req("ENG");
        req.yes = true;
        req.name = Some("Engineering".into());

        let outcome = super::run_setup(
            &tracker,
            &mut engine,
            &req,
            dir.path(),
            "setup-probe",
            &dir.path().join("absent.json"),
        )
        .await
        .expect("setup");

        assert!(outcome.target_created, "it had to be created");
        assert_eq!(
            tracker.created_targets(),
            vec!["Engineering".to_string()],
            "created ONCE, with the display name, not the key"
        );
        assert_eq!(outcome.included.len(), 2, "and the run continued");
    }

    /// `--dry-run` is a promise about the whole flow, not just the local half:
    /// nothing upstream, nothing on disk.
    #[tokio::test]
    async fn dry_run_provisions_nothing_and_writes_no_config() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let tracker = crate::tracker::FakeTracker::new("fake");
        tracker.set_target_missing();
        let mut engine = setup_engine();

        let mut req = setup_req("ENG");
        req.dry_run = true;
        // Even with a yes: --dry-run outranks it.
        req.yes = true;

        let outcome = super::run_setup(
            &tracker,
            &mut engine,
            &req,
            dir.path(),
            "setup-probe",
            &dir.path().join("absent.json"),
        )
        .await
        .expect("setup");

        assert!(
            tracker.created_targets().is_empty(),
            "--dry-run must never write UPSTREAM, which is the write that cannot \
             be undone"
        );
        assert!(!outcome.target_created);
        assert!(
            !crate::tracker::should_project(&crate::tracker::config::load(
                dir.path(),
                "setup-probe"
            )),
            "--dry-run must not enable mirroring"
        );
        assert!(
            engine.chunks_opted_in("fake").is_empty(),
            "--dry-run must not opt anything in"
        );
        assert!(
            !outcome.included.is_empty(),
            "but it must still REPORT what it would have included, or it is not a \
             preview of anything"
        );
    }

    /// Running it twice is a normal thing to do, so the second run must add
    /// nothing and say so.
    #[tokio::test]
    async fn a_second_run_includes_nothing_new() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let tracker = crate::tracker::FakeTracker::new("fake");
        let mut engine = setup_engine();
        let cfg = dir.path().join("absent.json");

        super::run_setup(
            &tracker,
            &mut engine,
            &setup_req("ENG"),
            dir.path(),
            "setup-probe",
            &cfg,
        )
        .await
        .expect("first");

        let second = super::run_setup(
            &tracker,
            &mut engine,
            &setup_req("ENG"),
            dir.path(),
            "setup-probe",
            &cfg,
        )
        .await
        .expect("second");

        assert!(
            second.included.is_empty(),
            "a repeat run must include nothing new"
        );
        assert_eq!(
            second.already_included, 2,
            "and must account for what was already there rather than reporting zero work"
        );
    }

    /// `--band` narrows, so a big roadmap can be onboarded a slice at a time.
    #[tokio::test]
    async fn band_narrows_the_bulk_include() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let tracker = crate::tracker::FakeTracker::new("fake");
        let mut engine = setup_engine();

        let mut req = setup_req("ENG");
        req.band = Some("critical".into());

        let outcome = super::run_setup(
            &tracker,
            &mut engine,
            &req,
            dir.path(),
            "setup-probe",
            &dir.path().join("absent.json"),
        )
        .await
        .expect("setup");

        assert_eq!(
            outcome.included,
            vec!["crit".to_string()],
            "only the critical band; `high` must be left for a later run"
        );
    }

    /// A provider that cannot introspect must not be treated as broken — the
    /// run continues and says the destination is unverified.
    #[tokio::test]
    async fn a_provider_that_cannot_be_probed_still_sets_up() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let mut engine = setup_engine();

        // The refusing DEFAULTS, i.e. exactly what an adapter that implemented
        // neither verb gets. This is also the compile-time proof that adding the
        // verbs broke no existing implementor.
        struct NoProvisioning;
        #[async_trait::async_trait]
        impl crate::tracker::TrackerPort for NoProvisioning {
            fn provider(&self) -> &str {
                "unprobeable"
            }
            fn capabilities(&self) -> crate::tracker::TrackerCapabilities {
                crate::tracker::TrackerCapabilities::full()
            }
            async fn upsert_item(
                &self,
                _: &crate::tracker::WorkItem,
            ) -> Result<crate::tracker::UpsertOutcome, crate::tracker::TrackerError> {
                unreachable!("setup does not upsert")
            }
            async fn fetch_since(
                &self,
                _: &str,
            ) -> Result<Vec<crate::tracker::WorkItem>, crate::tracker::TrackerError> {
                Ok(vec![])
            }
        }

        let outcome = super::run_setup(
            &NoProvisioning,
            &mut engine,
            &setup_req("whatever"),
            dir.path(),
            "setup-probe",
            &dir.path().join("absent.json"),
        )
        .await
        .expect("an unprobeable provider is not a failure");

        assert!(
            !outcome.target_existed,
            "we did not confirm it exists — claiming otherwise would be a lie"
        );
        assert!(!outcome.target_created);
        assert_eq!(
            outcome.included.len(),
            2,
            "'cannot check' must not read as 'missing' — the run proceeds"
        );
    }

    /// The quiet case, which is most of them: the tracker agrees with the plan,
    /// so a read stays a read and writes nothing at all.
    #[tokio::test]
    async fn pull_proposes_nothing_when_the_tracker_agrees() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let provider = "fake";
        let tracker = crate::tracker::FakeTracker::new(provider);
        tracker.set_clock("2026-07-26T10:00:00+00:00");
        let engine = projected("c3", provider, &tracker).await;

        // No remote_edit: nothing moved upstream.
        let report =
            crate::tracker::reconcile(&engine, &tracker, dir.path(), "2026-07-26T12:00:00+00:00")
                .await
                .expect("sweep");

        let mut run = super::SweepRun {
            provider: provider.into(),
            asked_since: "whenever".into(),
            report,
            engine,
        };
        assert!(
            super::report_pull(&mut run).is_empty(),
            "an unchanged tracker must produce no proposals — otherwise every \
             pull restamps a suggestion and an old one looks perpetually new"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_interval_defaults_when_unset_and_turns_off_at_zero() {
        assert_eq!(
            parse_sweep_interval(None),
            Some(crate::cloud::events::SWEEP_INTERVAL),
            "absent means default, not off"
        );
        assert_eq!(parse_sweep_interval(Some("0")), None, "0 is the off switch");
        assert_eq!(
            parse_sweep_interval(Some("  0 ")),
            None,
            "whitespace must not defeat the off switch"
        );
        assert_eq!(
            parse_sweep_interval(Some("60")),
            Some(std::time::Duration::from_secs(60))
        );
    }

    #[test]
    fn an_unparseable_interval_falls_back_to_the_default_not_to_off() {
        // A backstop that goes silently quiet on a typo is worse than one that
        // ignores the typo: nothing downstream notices a floor that vanished.
        for junk in ["", "abc", "-5", "60s", "1.5"] {
            assert_eq!(
                parse_sweep_interval(Some(junk)),
                Some(crate::cloud::events::SWEEP_INTERVAL),
                "{junk:?} should fall back to the default, never disable the floor"
            );
        }
    }

    #[test]
    fn parse_http_addr_accepts_full_host_port() {
        let got = parse_http_addr("0.0.0.0:9000").unwrap();
        assert_eq!(got.to_string(), "0.0.0.0:9000");
    }

    #[test]
    fn parse_http_addr_accepts_colon_port_shorthand() {
        let got = parse_http_addr(":8080").unwrap();
        assert_eq!(got.to_string(), "127.0.0.1:8080");
    }

    #[test]
    fn parse_http_addr_accepts_bare_port() {
        let got = parse_http_addr("8080").unwrap();
        assert_eq!(got.to_string(), "127.0.0.1:8080");
    }

    #[test]
    fn parse_http_addr_rejects_garbage() {
        assert!(parse_http_addr("not-an-address").is_err());
    }

    #[test]
    fn is_authorized_enforces_bearer_allowlist() {
        let allowed: HashSet<String> = ["good-token".to_string(), "second".to_string()]
            .into_iter()
            .collect();

        // Accept: exact token, case-insensitive scheme.
        assert!(is_authorized(Some("Bearer good-token"), &allowed));
        assert!(is_authorized(Some("bearer second"), &allowed));
        assert!(is_authorized(Some("BEARER good-token"), &allowed));

        // Reject: missing header, wrong scheme, empty token, unknown token.
        assert!(!is_authorized(None, &allowed));
        assert!(!is_authorized(Some("Basic good-token"), &allowed));
        assert!(!is_authorized(Some("Bearer "), &allowed));
        assert!(!is_authorized(Some("good-token"), &allowed)); // no scheme
        assert!(!is_authorized(Some("Bearer wrong"), &allowed));
        // An empty allowlist authorizes nothing (but the layer isn't mounted
        // when the env var is unset, so this never happens in practice).
        assert!(!is_authorized(Some("Bearer good-token"), &HashSet::new()));
    }

    // Process env is shared — fold the four parse_csv_env scenarios into
    // one sequential test so they don't race on the same key.
    #[test]
    fn parse_csv_env_covers_unset_empty_single_and_list() {
        const KEY: &str = "THINK_AND_SHIP_TEST_CSV_PARSE";

        // (1) Unset → None
        unsafe { std::env::remove_var(KEY) };
        assert_eq!(parse_csv_env(KEY), None);

        // (2) Empty / whitespace-only → None
        unsafe { std::env::set_var(KEY, "   ,  ,") };
        assert_eq!(parse_csv_env(KEY), None);

        // (3) Single value, with surrounding whitespace
        unsafe { std::env::set_var(KEY, "  https://app.example.com  ") };
        assert_eq!(
            parse_csv_env(KEY),
            Some(vec!["https://app.example.com".to_string()])
        );

        // (4) Comma-separated list with mixed whitespace + an empty slot
        unsafe { std::env::set_var(KEY, "a.example.com, b.example.com,,c.example.com ") };
        assert_eq!(
            parse_csv_env(KEY),
            Some(vec![
                "a.example.com".to_string(),
                "b.example.com".to_string(),
                "c.example.com".to_string(),
            ])
        );

        unsafe { std::env::remove_var(KEY) };
    }
}

/// The headless rule: no prompt in this crate may block a process that has
/// nobody to answer it.
///
/// This is the defect the rule was created against, and it was reported as an
/// intermittent test flake. It is neither intermittent nor a test problem. The
/// sweep-reachability test reaches `confirm` through `run_setup`, and with an
/// open-but-silent stdin — a live pipe, an agent's shell, a CI runner — the old
/// `confirm` blocked there forever. A full `cargo test` sat for twenty minutes
/// and had to be killed by hand, which is the worst shape this can take:
/// `ship_check` runs `cargo test` itself, so an honest gate and a stuck one
/// look identical from the outside.
#[cfg(test)]
mod headless_prompt_tests {
    use super::{is_yes, prompt_line_from, require_prompted};

    /// A reader that cannot be read. Consulting it is the failure.
    struct NeverReads;

    impl std::io::Read for NeverReads {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!(
                "prompt_line_from read stdin with no terminal attached. In production that read \
                 does not fail and does not see EOF — it BLOCKS, for as long as the descriptor \
                 is held open, which is the twenty-minute hang this rule exists to make \
                 impossible. The tty check must come BEFORE the read, not beside it."
            )
        }
    }

    /// THE test the headless rule exists for.
    ///
    /// Both halves live here on purpose. The first is load-bearing and comes
    /// first so nothing can shadow it; the second is what stops the first from
    /// passing vacuously, because a `prompt_line_from` that returned `None`
    /// unconditionally would satisfy the tty half perfectly while asking
    /// nobody anything, ever.
    #[test]
    fn the_terminal_check_happens_before_the_read() {
        // LOAD-BEARING: with no terminal, the reader is never touched. If the
        // check moves after the read — or disappears — `NeverReads` panics.
        assert_eq!(
            prompt_line_from("anyone there?", false, std::io::BufReader::new(NeverReads)),
            None,
            "a non-interactive process must be answered by the tty check, not by the read"
        );

        // And the positive: given a terminal, the answer is genuinely read
        // back. Without this, returning `None` always would pass the above.
        assert_eq!(
            prompt_line_from("anyone there?", true, "yes\n".as_bytes()),
            Some("yes\n".to_string()),
            "with a terminal the prompt must actually consult the reader"
        );

        // EOF is a real end-of-input and means nobody answered — distinct from
        // the silent descriptor above, and it must not hand back an empty
        // string that a caller could mistake for a keypress.
        assert_eq!(
            prompt_line_from("anyone there?", true, "".as_bytes()),
            None,
            "EOF is nobody answering, not an empty answer"
        );
    }

    /// `confirm`'s half of the rule: absence is a no, and so is everything that
    /// is not an explicit yes.
    #[test]
    fn only_an_explicit_yes_is_a_yes() {
        assert!(is_yes("y\n"), "a bare y is the answer this prompt asks for");
        assert!(is_yes("  YES  \n"), "case and whitespace are not the point");
        for no in ["n\n", "\n", "", "ye", "yes please", "0"] {
            assert!(!is_yes(no), "{no:?} is not an explicit yes");
        }
    }

    /// The two secret prompts refuse rather than decline. Silently answering
    /// "no" to a secret prompt produces a run that did nothing and explained
    /// nothing — the objection `otel_stack`'s headless rule already raises.
    #[test]
    fn an_unanswerable_secret_prompt_refuses_and_names_the_non_interactive_path() {
        // LOAD-BEARING: no answer is an ERROR, not an empty string. A `None`
        // disposed of as `String::new()` stores a blank credential and reports
        // success — which is exactly what the deliberately introduced bug that
        // forced this gate did, with every one of the other 799 tests still
        // green.
        let key = match require_prompted(None, "the key for github", "pass it with --key") {
            Ok(got) => panic!("a prompt nobody answered returned {got:?} instead of refusing"),
            Err(e) => e.to_string(),
        };
        assert!(
            key.contains("--key"),
            "the refusal must name the flag that makes this work unattended; got: {key}"
        );

        let secret = match require_prompted(
            None,
            "the Atlassian client secret",
            "set ATLASSIAN_CLIENT_SECRET",
        ) {
            Ok(got) => panic!("a prompt nobody answered returned {got:?} instead of refusing"),
            Err(e) => e.to_string(),
        };
        assert!(
            secret.contains("ATLASSIAN_CLIENT_SECRET"),
            "the refusal must name the env var that makes this work unattended; got: {secret}"
        );
        for message in [&key, &secret] {
            assert!(
                message.contains("not a terminal"),
                "the refusal must say WHY nothing was asked, or it reads as a bug; got: {message}"
            );
        }

        // And an answered prompt still passes its answer through untouched —
        // without this, refusing unconditionally would satisfy all of the above.
        match require_prompted(Some("lin_api_xxx\n".into()), "the key", "pass --key") {
            Ok(got) => assert_eq!(
                got, "lin_api_xxx\n",
                "an answered prompt returns the answer"
            ),
            Err(e) => panic!("an answered prompt must not refuse: {e}"),
        }
    }

    /// The reachability half, and the reason it exists: a deliberately
    /// introduced bug that deleted the refusal from `tracker_connect` —
    /// leaving `headless_refusal` itself untouched and simply not calling it —
    /// passed all 799 tests.
    ///
    /// Nothing behavioural can see that. A test of `require_prompted` cannot
    /// see a caller that stopped calling it, and the stdin ratchet below cannot
    /// either, because not-prompting reads no stdin at all. Driving the real
    /// prompts is not an option — they read the machine's own stdin, which is
    /// the failure this whole module is about. Source inspection is what is
    /// left.
    #[test]
    fn both_secret_prompts_dispose_of_an_unanswered_prompt_through_the_refusal() {
        let calls: Vec<&str> = super::cli_production_source(include_str!("mod.rs"))
            .lines()
            .map(str::trim_start)
            // Live call sites only: `//` and `///` are not code, and the
            // definition is not one of its own callers.
            .filter(|live| {
                live.contains("require_prompted(")
                    && !live.starts_with("//")
                    && !live.starts_with("fn require_prompted(")
            })
            .collect();

        // POSITIVE FIRST: the disposal must survive as a live call at all,
        // otherwise the count below is satisfied by there being no prompt left
        // to refuse.
        assert!(
            !calls.is_empty(),
            "no live call to `require_prompted` survives in mod.rs — the secret prompts have \
             stopped refusing headless"
        );
        assert_eq!(
            calls.len(),
            2,
            "exactly two prompts cannot proceed without an answer: `tracker connect`'s key and \
             the Atlassian client secret. Both must dispose of an unanswered prompt through \
             `require_prompted`, or a headless run stores a blank credential and calls it \
             success. Found: {calls:#?}"
        );
    }

    /// THE RATCHET. A fifth prompt must not be able to grow its own stdin read.
    ///
    /// Source inspection rather than behaviour, for the same reason as
    /// `sweep_reachability_tests`: the property is "no OTHER code reads stdin",
    /// and no runtime assertion can see a read that this test run never
    /// executes. It was silently false for three of four sites.
    ///
    /// The two `otel_stack` entries are the documented exception and are
    /// counted, not excused: its `ask` blocks exactly like the old `confirm`
    /// did, but its only caller branches on `is_terminal` first — the guard is
    /// at the caller. Counting it means moving that guard breaks this test too.
    #[test]
    fn the_only_stdin_reads_in_the_cli_are_the_guarded_ones() {
        const SOURCES: [(&str, &str); 7] = [
            ("mod.rs", include_str!("mod.rs")),
            ("args.rs", include_str!("args.rs")),
            ("connect.rs", include_str!("connect.rs")),
            ("otel_stack.rs", include_str!("otel_stack.rs")),
            ("setup.rs", include_str!("setup.rs")),
            ("skills.rs", include_str!("skills.rs")),
            ("store_health.rs", include_str!("store_health.rs")),
        ];

        // Live lines only: a commented-out call must not satisfy this, and a
        // test module's own mention of `stdin()` is not a prompt.
        let mut sites: Vec<String> = Vec::new();
        for (name, whole) in SOURCES {
            for line in super::cli_production_source(whole).lines() {
                let live = line.trim_start();
                if live.starts_with("//") || !live.contains("stdin()") {
                    continue;
                }
                sites.push(format!("{name}: {live}"));
            }
        }

        // POSITIVE FIRST: the guard itself must be present and live. Without
        // this, deleting `prompt_line` outright would shrink the list and the
        // count assertion below would be satisfied by having no prompts at all.
        assert!(
            sites
                .iter()
                .any(|s| s.starts_with("mod.rs:") && s.contains("stdin().is_terminal()")),
            "prompt_line's terminal check is gone from mod.rs — found: {sites:#?}"
        );
        assert!(
            sites
                .iter()
                .any(|s| s == "otel_stack.rs: let interactive = std::io::stdin().is_terminal();"),
            "the wizard's caller-side guard is gone — found: {sites:#?}"
        );

        assert_eq!(
            sites.len(),
            4,
            "the CLI may read stdin in exactly four places: prompt_line's tty check and its \
             lock, otel_stack's `ask` and the `wizard` guard that makes `ask` unreachable \
             headless. A new one here is a new way to hang a process that has nobody to \
             answer it — route it through `prompt_line` instead. Found: {sites:#?}"
        );
    }
}
