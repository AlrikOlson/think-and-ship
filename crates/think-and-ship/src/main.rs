//! The `think-and-ship` binary: parse, canonicalize, dispatch.
//!
//! The grammar itself lives in `think_and_ship::cli::args` so it can be tested
//! (a binary crate's types are unreachable from `tests/`). This file stays a
//! thin dispatcher — one arm per canonical command, and none for the retired
//! spellings, which [`Command::canonicalize`] has already rewritten by the time
//! the match runs.

use anyhow::Result;
use clap::Parser;
use think_and_ship::cli;
use think_and_ship::cli::args::{
    Cli, Command, CorpusAction, OtelAction, ProjectAction, RoadmapAction, SkillsAction, SyncAction,
    TelemetryAction, TraceAction, TrackerAction,
};

fn main() -> Result<()> {
    let (command, moved_note) = Cli::parse().command.canonicalize();
    if let Some(note) = moved_note {
        eprintln!("{note}");
    }

    match command {
        Command::Serve { http } => cli::serve(http),
        Command::Init {
            with_claude_md,
            full,
            dry_run,
            force,
        } => cli::init(with_claude_md, full, dry_run, force),
        Command::Project { action } => match action {
            ProjectAction::Mark { dry_run, name } => cli::project_mark(name.as_deref(), dry_run),
        },
        Command::Skills { action } => match action {
            SkillsAction::Install {
                client,
                scope,
                profile,
                only,
                dry_run,
                force,
            } => cli::skills_install(
                client.as_deref(),
                scope.as_deref(),
                profile.as_deref(),
                only.as_deref(),
                dry_run,
                force,
            ),
            SkillsAction::List { scope } => cli::skills_list(scope.as_deref()),
            SkillsAction::Package {
                client,
                out,
                dry_run,
            } => cli::skills_package(&client, &out, dry_run),
            SkillsAction::Migrate {
                scope,
                apply,
                force,
            } => cli::skills_migrate(scope.as_deref(), apply, force),
        },
        Command::Roadmap { action } => match action {
            RoadmapAction::Export { format } => cli::export(&format),
            RoadmapAction::Import {
                file,
                shared,
                dry_run,
                merge,
            } => cli::import(file.as_deref(), shared, dry_run, merge),
            RoadmapAction::Next => cli::roadmap_next(),
            RoadmapAction::Status => cli::roadmap_status(),
            RoadmapAction::Prune {
                matching,
                apply,
                contested,
            } => cli::roadmap_prune(&matching, apply, contested),
            RoadmapAction::Regions { file, apply } => cli::roadmap_regions(file.as_deref(), apply),
            RoadmapAction::Hygiene {
                dry_run,
                stall_days,
                idle_days,
            } => cli::hygiene(dry_run, stall_days, idle_days),
            RoadmapAction::Block {
                id,
                kind,
                reason,
                evidence,
            } => cli::roadmap_block(&id, &kind, reason, evidence),
            RoadmapAction::Unblock { id } => cli::roadmap_unblock(&id),
        },
        Command::Prune {
            family,
            matching,
            apply,
            contested,
        } => cli::prune(family, &matching, apply, contested),
        Command::Adopt {
            family,
            matching,
            apply,
        } => cli::adopt(family, &matching, apply),
        Command::Doctor => cli::doctor(),
        Command::Status => cli::status(),
        Command::Trace { action } => match action {
            TraceAction::Export { out } => cli::trace_export_otel(out.as_deref()),
            TraceAction::Promote {
                session,
                step,
                kind,
            } => cli::promote(&session, step, kind.as_deref()),
        },
        Command::Otel { action } => match action {
            OtelAction::Wizard {
                dir,
                otlp_port,
                ui_port,
                yes,
            } => cli::otel_stack::wizard(dir.as_deref(), otlp_port, ui_port, yes),
            OtelAction::Up {
                dir,
                otlp_port,
                ui_port,
            } => cli::otel_stack::up(dir.as_deref(), otlp_port, ui_port),
            OtelAction::Down { dir } => cli::otel_stack::down(dir.as_deref()),
            OtelAction::Send {
                endpoint,
                otlp_port,
            } => cli::otel_stack::send(endpoint.as_deref(), otlp_port),
            OtelAction::Status {
                dir,
                otlp_port,
                ui_port,
            } => cli::otel_stack::status(dir.as_deref(), otlp_port, ui_port),
        },
        Command::Corpus { action } => match action {
            CorpusAction::Export { out } => cli::corpus_export(out.as_deref()),
            CorpusAction::Eval {
                corpus,
                learned,
                prequential,
                warmup,
                weights_out,
            } => cli::eval_run(
                corpus.as_deref(),
                learned,
                prequential,
                warmup,
                weights_out.as_deref(),
            ),
        },
        Command::Sync { action } => match action {
            SyncAction::Push {
                dry_run,
                all_projects,
            } => cli::sync_push(dry_run, all_projects),
        },
        Command::Calls { json, tool } => cli::calls(json, tool.as_deref()),
        Command::Telemetry { action } => match action {
            TelemetryAction::Status => cli::telemetry_status(),
            TelemetryAction::On => cli::telemetry_set(true),
            TelemetryAction::Off => cli::telemetry_set(false),
            TelemetryAction::Push { dry_run } => cli::telemetry_push(dry_run),
        },
        Command::Tracker { action } => match action {
            TrackerAction::Status => cli::tracker_status(),
            TrackerAction::On {
                provider,
                into,
                companion,
                companion_into,
            } => cli::tracker_on(
                &provider,
                &into,
                companion.as_deref(),
                companion_into.as_deref(),
            ),
            TrackerAction::Setup {
                provider,
                into,
                name,
                band,
                initiative,
                push,
                push_secs,
                yes,
                dry_run,
            } => cli::tracker_setup(&cli::SetupRequest {
                provider,
                into,
                name,
                band,
                initiative,
                push,
                push_secs,
                yes,
                dry_run,
            }),
            TrackerAction::Off => cli::tracker_off(),
            TrackerAction::Include { item, provider } => {
                cli::tracker_include(&item, &provider, true)
            }
            TrackerAction::Exclude { item, provider } => {
                cli::tracker_include(&item, &provider, false)
            }
            TrackerAction::Push { dry_run } => cli::tracker_push(dry_run),
            TrackerAction::Pull => cli::tracker_pull(),
            TrackerAction::Connect { provider, key } => cli::tracker_connect(&provider, key),
            TrackerAction::SignIn {
                provider,
                app_id,
                scopes,
                actor,
                print_only,
            } => cli::tracker_sign_in(&provider, &app_id, &scopes, &actor, print_only),
            TrackerAction::Disconnect { provider } => cli::tracker_disconnect(&provider),
        },
        Command::Repair { dry_run } => cli::repair(dry_run),
        Command::Connect {
            url,
            dry_run,
            force,
            client,
        } => cli::connect(url.as_deref(), dry_run, force, &client),
        Command::Token => cli::print_token(),
        Command::Disconnect { dry_run } => cli::disconnect(dry_run),

        // `canonicalize` rewrote every retired spelling above; reaching one here
        // would mean a new alias was added without a rewrite rule.
        retired @ (Command::Export { .. }
        | Command::Import { .. }
        | Command::Hygiene { .. }
        | Command::Promote { .. }
        | Command::Eval { .. }) => unreachable!(
            "retired spelling reached dispatch without being canonicalized: {retired:?}"
        ),
    }
}
