//! The command-line grammar.
//!
//! This lives in the library rather than in `main.rs` so the grammar is
//! testable: `main.rs` is a binary crate, and nothing in `tests/` can reach a
//! type declared there. Restructuring commands with no test coverage is how you
//! silently break `export`.
//!
//! # The grammar
//!
//! A **group is a noun, a command is a verb** — the rule Nix's CLI guideline,
//! Microsoft's System.CommandLine guidance ("a command with subcommands should
//! function as an area or grouping identifier rather than specify an action"),
//! and Thoughtworks (`platform-cli [noun] [verb]`) all land on independently.
//!
//! So the areas are nouns — `roadmap`, `trace`, `corpus`, `skills`, `sync`,
//! `telemetry`, `project` — and only verbs that act on the *installation
//! itself* stay at the top level: `serve`, `init`, `doctor`, `status`,
//! `connect`, `repair`.
//!
//! # Compatibility
//!
//! `export`, `import`, `hygiene`, `promote`, and `eval` were top-level verbs
//! through v0.3.x. They still parse, are hidden from `--help`, and
//! [`Command::canonicalize`] rewrites them to their noun-grouped form while
//! handing back a one-line note for stderr. Shell history and muscle memory are
//! part of the interface.

use clap::{Parser, Subcommand};

#[derive(Parser, Debug, PartialEq)]
#[command(
    name = "think-and-ship",
    version,
    about = "Unified MCP server for structured reasoning + execution tracking"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum Command {
    /// Run as an MCP server (stdio by default; --http for Streamable HTTP).
    Serve {
        /// Bind a Streamable HTTP listener at the given address (e.g. ":8080").
        #[arg(long, value_name = "ADDR")]
        http: Option<String>,
    },
    /// Set this project up: write the MCP config your editor reads.
    Init {
        /// Also write CLAUDE.md.
        #[arg(long)]
        with_claude_md: bool,
        /// MCP config and CLAUDE.md in one shot.
        #[arg(long)]
        full: bool,
        /// Preview what would be written without touching any files.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite an existing think-and-ship entry / CLAUDE.md section.
        #[arg(long)]
        force: bool,
    },
    /// Work with this repository's identity: the id every store is keyed by.
    Project {
        #[command(subcommand)]
        action: ProjectAction,
    },
    /// Install the bundled agent skills (/roadmap, /signals, /craft, …) into a
    /// coding agent's user-level skills directory.
    Skills {
        #[command(subcommand)]
        action: SkillsAction,
    },
    /// Work with the project roadmap: the plan the agent implements chunk by chunk.
    Roadmap {
        #[command(subcommand)]
        action: RoadmapAction,
    },
    /// Remove records that belong to a different project, in any family.
    ///
    /// Lists what it would remove and changes nothing unless you pass --apply.
    /// A record whose origin cannot be proven is NEVER removed on its own —
    /// name it with --matching if you're sure.
    Prune {
        /// Which family to prune: think, signal, roadmap, or all.
        #[arg(value_enum, default_value_t = PruneFamily::All)]
        family: PruneFamily,
        /// Also remove unprovable-origin records whose id starts with one of
        /// these comma-separated prefixes. Use only for ids you recognize.
        #[arg(long, value_name = "PREFIXES", value_delimiter = ',')]
        matching: Vec<String>,
        /// Actually remove them. Without this, nothing is written.
        #[arg(long)]
        apply: bool,
        /// Also remove records this project CLAIMS as its own, but ONLY where
        /// another project's store claims the very same id — the contradiction
        /// `doctor` reports. Requires --matching, because a contested id proves
        /// one of the two stamps is false and never which one.
        #[arg(long)]
        contested: bool,
    },
    /// Claim this project's unprovable-origin records as its own.
    ///
    /// The inverse of `prune`, and the only thing that empties the
    /// unprovable-origin row: records written before the origin stamp existed
    /// carry no owner, so every future `prune` has to ask about them again.
    /// Refuses outright while the store still holds a record that provably
    /// belongs to another project — prune those first, or adoption makes the
    /// bleed permanent. Lists what it would claim and changes nothing unless
    /// you pass --apply.
    Adopt {
        /// Which family to adopt: signal, roadmap, or all.
        #[arg(value_enum, default_value_t = PruneFamily::All)]
        family: PruneFamily,
        /// Claim only unprovable records whose id starts with one of these
        /// comma-separated prefixes. Omit to claim all of them.
        #[arg(long, value_name = "PREFIXES", value_delimiter = ',')]
        matching: Vec<String>,
        /// Actually write the stamps. Without this, nothing is written.
        #[arg(long)]
        apply: bool,
    },
    /// Diagnose setup issues.
    Doctor,
    /// Show project info and config state.
    Status,
    /// Share the trace with your team, or export it to another tool.
    Trace {
        #[command(subcommand)]
        action: TraceAction,
    },
    /// Run the agent's trace against a real telemetry stack, locally.
    ///
    /// `otel wizard` is the guided path: it writes a docker-compose for a local
    /// Jaeger, starts it, and sends this project's trace — no docker or curl
    /// typing. Every step is also a standalone command.
    Otel {
        #[command(subcommand)]
        action: OtelAction,
    },
    /// Export and evaluate the event history behind "what should I do next?".
    Corpus {
        #[command(subcommand)]
        action: CorpusAction,
    },
    /// Push your local history to the cloud workspace.
    Sync {
        #[command(subcommand)]
        action: SyncAction,
    },
    /// Show how many times each tool has been called on this machine.
    ///
    /// Local only — these counts are never transmitted, and no code path
    /// exists that could transmit them. That is why this is NOT a `telemetry`
    /// subcommand: spelling it `telemetry calls` would imply the counter is
    /// the thing the 2026-06-10 opt-in decision governs, and it is not. It
    /// reports on the installation, like `status` and `doctor`.
    /// Turn it off with `THINK_AND_SHIP_CALL_COUNTS=off`.
    Calls {
        /// Emit JSON instead of a table.
        #[arg(long)]
        json: bool,
        /// Ask whether ONE verb is cold, e.g. `calls signal_research`.
        ///
        /// Answers through the soak verdict rather than the raw number: a zero
        /// read before the observation window is met comes back as "not yet
        /// evidence", which is the misreading the counter exists to prevent.
        tool: Option<String>,
    },
    /// Decide whether to share anonymized usage data. Off unless you turn it on.
    Telemetry {
        #[command(subcommand)]
        action: TelemetryAction,
    },
    /// Mirror roadmap items into an issue tracker. Off unless you turn it on.
    Tracker {
        #[command(subcommand)]
        action: TrackerAction,
    },
    /// Clean up duplicated reasoning steps, so the trace reads as one history.
    ///
    /// Two sessions writing at once could each save the same step under a
    /// different number. This keeps the earliest copy of each and drops the
    /// rest; pinned steps stay pinned.
    Repair {
        /// List what would be removed without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Sign in to a cloud workspace and configure this machine for it.
    ///
    /// Opens a browser to confirm the login, then writes the MCP config with
    /// the credentials filled in — nothing to copy and paste.
    Connect {
        /// The cloud backend URL to connect to.
        ///
        /// There is no built-in default. Omit it and `TAS_CLOUD_URL` is used;
        /// with neither set, connect stops and names both.
        #[arg(long, value_name = "URL")]
        url: Option<String>,
        /// Show what would be written without signing in or changing anything.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite an existing think-and-ship MCP entry.
        #[arg(long)]
        force: bool,
        /// Name the client you are running in, when it cannot be detected.
        ///
        /// Detection uses the environment variables a client sets plus its
        /// marker directory. Windsurf publishes neither a documented variable
        /// nor anything else we can read, and a client we have never heard of
        /// publishes nothing by definition — so say so, and it gets configured
        /// alongside whatever else is here. Repeatable.
        #[arg(long, value_name = "NAME")]
        client: Vec<String>,
    },

    /// Print this project's cloud agent token.
    ///
    /// The credential store keeps an envelope, not a bare token, and the
    /// envelope has three dot-separated segments — so decoding it as a JWT
    /// succeeds and hands back the real claims of the token inside it. A wrong
    /// read therefore looks exactly like a revoked credential, with
    /// corroborating detail. This prints the token itself so nobody has to know
    /// the shape.
    ///
    /// Writes the token and nothing else to stdout, for piping into a header.
    Token,

    /// Stop syncing this project to the cloud.
    ///
    /// Forgets the agent token from this machine's credential store and removes
    /// the cloud settings from the MCP entry. Both, so you are not left with a
    /// server that tries to sync and fails, or a live long-lived token on a
    /// machine you believe is disconnected.
    Disconnect {
        /// Show what would be forgotten and removed, without changing anything.
        #[arg(long)]
        dry_run: bool,
    },

    // ── Retired top-level spellings ──────────────────────────────────────────
    // Hidden from --help, still parsed, rewritten by `canonicalize`.
    #[command(hide = true)]
    Export {
        #[arg(long, default_value = "markdown")]
        format: String,
    },
    #[command(hide = true)]
    Import {
        #[arg(long)]
        file: Option<String>,
        #[arg(long)]
        shared: bool,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        merge: bool,
    },
    #[command(hide = true)]
    Hygiene {
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value_t = 7)]
        stall_days: i64,
        #[arg(long, default_value_t = 7)]
        idle_days: i64,
    },
    #[command(hide = true)]
    Promote {
        #[arg(long, value_name = "ID")]
        session: String,
        #[arg(long, value_name = "N")]
        step: Option<u32>,
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
    },
    #[command(hide = true)]
    Eval {
        #[arg(long, value_name = "FILE")]
        corpus: Option<String>,
        #[arg(long)]
        learned: bool,
        #[arg(long = "as-you-go", alias = "prequential", conflicts_with = "learned")]
        prequential: bool,
        #[arg(long, default_value_t = 20, requires = "prequential")]
        warmup: usize,
        #[arg(long, value_name = "FILE", requires = "learned")]
        weights_out: Option<String>,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum ProjectAction {
    /// Declare this repository's identity, and nothing else.
    ///
    /// Writes `.think-and-ship/project.json` with the id this project ALREADY
    /// resolves to, so every reasoning step, chunk and signal it holds stays
    /// its own. Commit the file: it is what keeps a project itself when the
    /// directory is renamed, moved, cloned, or entered from a subdirectory.
    ///
    /// `init` seeds the same file for a fresh project, but also writes the MCP
    /// config for every client it finds. This writes the identity alone.
    Mark {
        /// Show what would be declared without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// The display name to record. Omit to leave it derived.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum RoadmapAction {
    /// Render the roadmap as a ROADMAP.md-shaped markdown view (or json).
    Export {
        /// Output format: markdown or json.
        #[arg(long, default_value = "markdown")]
        format: String,
    },
    /// Seed roadmap chunks from an existing roadmap file (one-time).
    Import {
        /// Roadmap file to parse (markdown or YAML). Omit to auto-discover every
        /// roadmap source in the project and merge them.
        #[arg(long)]
        file: Option<String>,
        /// Mark imported chunks as shared (committed) rather than local.
        #[arg(long)]
        shared: bool,
        /// Parse and print without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Merge into an existing roadmap: backfill notes/narrative onto existing
        /// chunks WITHOUT changing their status/priority (safe re-import).
        #[arg(long)]
        merge: bool,
    },
    /// Show the next ready chunk: the most urgent pending chunk (smallest priority number) that
    /// carries no blocker and whose dependencies are all done.
    Next,
    /// Show the roadmap at a glance: counts by status and what's next.
    Status,
    /// Remove chunks that belong to a different project.
    ///
    /// Lists what it would remove and changes nothing unless you pass --apply.
    /// Chunks recorded before origin tracking existed are never removed on
    /// their own — name them with --matching if you're sure.
    Prune {
        /// Also remove untracked-origin chunks whose id starts with one of
        /// these comma-separated prefixes. Use only for ids you recognize.
        #[arg(long, value_name = "PREFIXES", value_delimiter = ',')]
        matching: Vec<String>,
        /// Actually remove them. Without this, nothing is written.
        #[arg(long)]
        apply: bool,
        /// Also remove chunks this project CLAIMS, where another project's store
        /// claims the same id. Requires --matching. See `doctor`.
        #[arg(long)]
        contested: bool,
    },
    /// Audit the region map — the places the tech-tree canvas is navigated by —
    /// or re-author it from a file.
    ///
    /// With no arguments it reports the live map against the constraints in
    /// the canvas constraints and changes nothing. With
    /// `--file` it reads a JSON object of region name to chunk ids, and still
    /// changes nothing unless you pass --apply.
    Regions {
        /// A JSON map of region name to the chunk ids belonging to it. Chunks
        /// the file does not mention keep the region they have.
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
        /// Actually write the regions from --file. Without this, nothing is
        /// written.
        #[arg(long)]
        apply: bool,
    },
    /// Flag stalled and ready-but-idle chunks as signals in the triage inbox.
    Hygiene {
        /// Print findings without capturing signals.
        #[arg(long)]
        dry_run: bool,
        /// Days an in_progress chunk may go untouched before it counts as stalled.
        #[arg(long, default_value_t = 7)]
        stall_days: i64,
        /// Days a ready pending chunk may sit idle before it gets flagged.
        #[arg(long, default_value_t = 7)]
        idle_days: i64,
    },
    /// Record why a chunk cannot be worked, when the answer is not another
    /// chunk.
    ///
    /// A dependency on another chunk is `deps`. This is for everything else:
    /// a premise the work rests on turned out to be false, a premise that is
    /// not met YET, a decision only a human can make, or a third party we are
    /// waiting on. Stating it here means nobody has to re-derive it from the
    /// title. Re-running this on an already-blocked chunk restates the blocker.
    Block {
        /// The chunk id to block.
        #[arg(long)]
        id: String,
        /// Why the work cannot proceed: premise_refuted, premise_unmet,
        /// awaiting_human, or external. Anything else is refused.
        #[arg(long)]
        kind: String,
        /// The blocker in a sentence. Required, and it may not be blank.
        #[arg(long)]
        reason: String,
        /// Optional proof — a cross-ref such as think:42, chunk:some-id or
        /// task:some-task.
        #[arg(long)]
        evidence: Option<String>,
    },
    /// Retract a chunk's blocker — it is no longer true.
    ///
    /// Deliberately as short to type as `block`, and its own verb rather than
    /// a flag on one. Every blocker eventually stops being true, and if saying
    /// so is the awkward half of the pair, people edit the title instead and
    /// the record rots. Errors when the chunk has no blocker, so a clear that
    /// prints success always means something changed.
    Unblock {
        /// The chunk id to unblock.
        #[arg(long)]
        id: String,
    },
}

/// Which record family a `prune` applies to.
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum PruneFamily {
    Think,
    Signal,
    Roadmap,
    All,
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum SkillsAction {
    /// Write the bundled skills into each detected agent's skills directory.
    ///
    /// Installs the CORE profile by default: switch-work and advance-work.
    /// Destinations differ per agent and per scope; see docs/HARNESSES.md,
    /// which records each one against the first-party page it came from.
    Install {
        /// Target one agent by key or alias (claude-code, codex, copilot,
        /// cursor, gemini, windsurf, opencode, cline, roo, amp, goose, kiro),
        /// or `all` to write every known agent whether or not it is installed.
        /// Omit to install for each agent detected under your home directory.
        #[arg(long, value_name = "NAME")]
        client: Option<String>,
        /// Where to install: `user` (default, every project) or `project`
        /// (this repository only).
        #[arg(long, value_name = "SCOPE")]
        scope: Option<String>,
        /// Which set of skills: `core` (default), `legacy`, or `all`.
        #[arg(long, value_name = "PROFILE")]
        profile: Option<String>,
        /// Install just one skill by name. Overrides --profile.
        #[arg(long, value_name = "SKILL")]
        only: Option<String>,
        /// Show what would be written without touching any files.
        #[arg(long)]
        dry_run: bool,
        /// Overwrite skills that already exist with local edits (files you
        /// added alongside the bundled ones are kept).
        #[arg(long)]
        force: bool,
    },
    /// List the bundled skills, their profile, and where each agent stands.
    List {
        /// Report the `user` (default) or `project` destinations.
        #[arg(long, value_name = "SCOPE")]
        scope: Option<String>,
    },
    /// Retire skill directories this installer no longer writes.
    ///
    /// Dry-run by default. Only removes directories proven byte-identical to
    /// this version's own render; anything that differs is reported and kept,
    /// because a local edit and an older version's copy are indistinguishable.
    Migrate {
        /// Which destinations to inspect: `user` (default) or `project`.
        #[arg(long, value_name = "SCOPE")]
        scope: Option<String>,
        /// Actually remove. Without this, nothing is written or deleted.
        #[arg(long)]
        apply: bool,
        /// With --apply, also remove directories that differ. This can delete
        /// your own edits; it is never the default.
        #[arg(long)]
        force: bool,
    },
    /// Build an agent's own plugin package for the core skills.
    ///
    /// Generated from the same canonical source `install` renders, so the two
    /// cannot disagree. Builds only — nothing here publishes anywhere.
    Package {
        /// Which agent's plugin format: `claude-code` or `codex`.
        #[arg(long, value_name = "NAME")]
        client: String,
        /// Directory to build into.
        #[arg(long, value_name = "DIR")]
        out: std::path::PathBuf,
        /// Show what would be written without touching any files.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Subcommands for the local OpenTelemetry stack.
#[derive(Subcommand, Debug, PartialEq)]
pub enum OtelAction {
    /// Guided setup: generate the stack, start it, and send the trace.
    ///
    /// Without a terminal this prints the plan and changes nothing, so it can
    /// never hang an automated session.
    Wizard {
        /// Where to keep the generated docker-compose file.
        #[arg(long, value_name = "DIR")]
        dir: Option<String>,
        /// OTLP/HTTP port to publish (Jaeger's receiver).
        #[arg(long, default_value_t = crate::cli::otel_stack::DEFAULT_OTLP_PORT)]
        otlp_port: u16,
        /// Port to publish the Jaeger UI on.
        #[arg(long, default_value_t = crate::cli::otel_stack::DEFAULT_UI_PORT)]
        ui_port: u16,
        /// Run every step without asking. Required for a non-interactive run.
        #[arg(long)]
        yes: bool,
    },
    /// Write the compose file and start the local collector.
    Up {
        #[arg(long, value_name = "DIR")]
        dir: Option<String>,
        #[arg(long, default_value_t = crate::cli::otel_stack::DEFAULT_OTLP_PORT)]
        otlp_port: u16,
        #[arg(long, default_value_t = crate::cli::otel_stack::DEFAULT_UI_PORT)]
        ui_port: u16,
    },
    /// Stop the local collector. Not an error if it was never started.
    Down {
        #[arg(long, value_name = "DIR")]
        dir: Option<String>,
    },
    /// Export this project's trace and POST it to an OTLP endpoint.
    Send {
        /// Full OTLP traces endpoint. Defaults to the local stack.
        #[arg(long, value_name = "URL")]
        endpoint: Option<String>,
        #[arg(long, default_value_t = crate::cli::otel_stack::DEFAULT_OTLP_PORT)]
        otlp_port: u16,
    },
    /// Is docker up, is the stack running, and is there anything to send?
    Status {
        #[arg(long, value_name = "DIR")]
        dir: Option<String>,
        #[arg(long, default_value_t = crate::cli::otel_stack::DEFAULT_OTLP_PORT)]
        otlp_port: u16,
        #[arg(long, default_value_t = crate::cli::otel_stack::DEFAULT_UI_PORT)]
        ui_port: u16,
    },
}
#[derive(Subcommand, Debug, PartialEq)]
pub enum TraceAction {
    /// Export the trace for a tracing tool to read (OpenTelemetry format).
    ///
    /// Send the result to any OTLP endpoint — for example a local Jaeger on
    /// port 4318 — to see the agent's work as a timeline.
    Export {
        /// Write to this file. Omit to print to the terminal.
        #[arg(long, value_name = "FILE")]
        out: Option<String>,
    },
    /// Share a private session with your team by moving it into the committed
    /// part of the repo.
    ///
    /// Records start private (gitignored). This moves them where teammates can
    /// read them; review and commit afterwards. Needs the repo-backed trace
    /// storage turned on.
    Promote {
        /// Which session to share (the file name without its extension).
        #[arg(long, value_name = "ID")]
        session: String,
        /// Share only this one reasoning step.
        #[arg(long, value_name = "N")]
        step: Option<u32>,
        /// Share only records of this kind: step | objective | task | action | check.
        #[arg(long, value_name = "KIND")]
        kind: Option<String>,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum CorpusAction {
    /// Build the corpus from the local stores and write JSONL.
    Export {
        /// Output file. Omit to print to stdout.
        #[arg(long, value_name = "FILE")]
        out: Option<String>,
    },
    /// Measure how well "what should I do next?" would have predicted the
    /// chunks you actually worked on.
    Eval {
        /// Score this exported corpus file. Omit to build one from your local
        /// history.
        #[arg(long, value_name = "FILE")]
        corpus: Option<String>,
        /// Also train a predictor on the older 70% of your history and score it
        /// against the newer 30% it has never seen.
        #[arg(long)]
        learned: bool,
        /// Score every prediction against only the history that preceded it —
        /// the honest way to measure a predictor that learns as it goes.
        #[arg(long = "as-you-go", alias = "prequential", conflicts_with = "learned")]
        prequential: bool,
        /// How many early cases to learn from before scoring starts.
        #[arg(long, default_value_t = 20, requires = "prequential")]
        warmup: usize,
        /// Save the trained predictor to this file.
        #[arg(long, value_name = "FILE", requires = "learned")]
        weights_out: Option<String>,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum TelemetryAction {
    /// Show the current consent state and what telemetry would contain.
    Status,
    /// Enable anonymized structural telemetry (prints the full disclosure first).
    On,
    /// Disable telemetry.
    Off,
    /// Send the workspace's structural shape now (consent-gated; one-shot).
    Push {
        /// Print the shape that would be sent without sending it.
        #[arg(long)]
        dry_run: bool,
    },
}

/// Verbs for the tracker noun.
///
/// Two separate consents, because they answer different questions. `on`/`off`
/// decide whether this project may talk to a tracker at all and where; `include`
/// and `exclude` decide which items are in scope. Both are required before
/// anything is mirrored, and both start off.
#[derive(Subcommand, Debug, PartialEq)]
pub enum TrackerAction {
    /// Show where items would be mirrored and which ones are included.
    Status,
    /// Set mirroring up in one go: check the destination, include your items,
    /// and turn on unattended pushing.
    ///
    /// Everything `on` + `include` + `push` would do, in the right order, with
    /// the destination VERIFIED before anything is written — `on` only checks
    /// that a destination looks well-formed, so a typo used to surface much
    /// later as a failed push.
    Setup {
        /// Which tracker to mirror into.
        #[arg(long, default_value = "github")]
        provider: String,
        /// Where to write: a Linear team key (`ENG`) or a GitHub `owner/repo`.
        #[arg(long)]
        into: String,
        /// Human-readable name to use if the destination has to be created.
        /// Defaults to the key itself.
        #[arg(long)]
        name: Option<String>,
        /// Only include items in this priority band
        /// (critical|high|medium|low|later). Default: every active item.
        #[arg(long)]
        band: Option<String>,
        /// Name for the initiative (the roof the mirrored projects file under),
        /// for providers that have one. Default: this directory's name.
        #[arg(long)]
        initiative: Option<String>,
        /// Push straight away instead of waiting for the first unattended cycle.
        #[arg(long)]
        push: bool,
        /// Seconds between unattended pushes. 0 leaves auto-push alone.
        #[arg(long, default_value = "300")]
        push_secs: u64,
        /// Answer yes to creating the destination. Without this you are asked,
        /// and a non-interactive run stops rather than creating anything.
        #[arg(long)]
        yes: bool,
        /// Say what would happen. Writes nothing, locally or upstream.
        #[arg(long)]
        dry_run: bool,
    },
    /// Start mirroring for this project. Nothing is sent until you also include
    /// at least one item.
    On {
        /// Which tracker to mirror into.
        #[arg(long, default_value = "github")]
        provider: String,
        /// Where to write, in the form the tracker uses (`owner/repo`).
        #[arg(long)]
        into: String,
        /// A SECOND tracker to mirror into alongside the first, taking its item
        /// identities from it. This is how a GitHub Projects v2 board is
        /// reached: a board item wraps an issue that already exists, so the
        /// board is a companion to an issues lane rather than a replacement for
        /// one. Pass an empty value to clear it.
        #[arg(long)]
        companion: Option<String>,
        /// Where the companion writes — a board is addressed by project URL,
        /// not by the primary's `owner/repo`. Required with `--companion`.
        #[arg(long)]
        companion_into: Option<String>,
    },
    /// Stop mirroring for this project, keeping the destination for later.
    Off,
    /// Include one roadmap item, so it is mirrored from now on.
    Include {
        /// The item's id.
        #[arg(long)]
        item: String,
        #[arg(long, default_value = "github")]
        provider: String,
    },
    /// Exclude one roadmap item, so it stops being mirrored.
    Exclude {
        /// The item's id.
        #[arg(long)]
        item: String,
        #[arg(long, default_value = "github")]
        provider: String,
    },
    /// Mirror the included items now, and replay anything that failed earlier.
    Push {
        /// Show what would be sent without sending it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Check the tracker for changes made since the last check, and report them.
    ///
    /// This is the inbound half of `push`. It CHANGES NOTHING in your roadmap —
    /// it fetches what moved, separates your own writes coming back from
    /// genuine edits somebody else made, and prints the result. Deciding what
    /// to do about a remote edit stays yours.
    ///
    /// Run it whenever you want, or on a schedule. It is also the path a
    /// webhook would shortcut: a webhook can only ever make this happen
    /// sooner, never instead.
    Pull,
    /// Store the key this project uses to sign in to the tracker.
    ///
    /// The key is kept encrypted, outside the roadmap, and never appears in an
    /// export. Reading it from the environment instead is still supported and
    /// documented, but this is the path that keeps it off your disk in plain
    /// text.
    Connect {
        #[arg(long, default_value = "github")]
        provider: String,
        /// The key to store. Omit it and you will be prompted, so it never
        /// reaches your shell history.
        #[arg(long)]
        key: Option<String>,
    },
    /// Sign in to the tracker in your browser, instead of pasting a key.
    ///
    /// Opens a consent page, catches the answer on a local port, and stores the
    /// result. Nothing is copied by hand.
    ///
    /// `linear` and `jira` can be signed in to. Jira is a confidential client,
    /// so it also needs a client secret — supplied by the
    /// `ATLASSIAN_CLIENT_SECRET` environment variable or a prompt, never a flag,
    /// because a secret on a command line lands in shell history. An account
    /// with several Jira sites picks one with `ATLASSIAN_SITE` (a cloudid, a
    /// site url or a site name); with one site nothing is needed.
    SignIn {
        #[arg(long, default_value = "linear")]
        provider: String,
        /// The application id the tracker issued you.
        #[arg(long)]
        app_id: String,
        /// What to ask permission for. Jira's `offline_access` is added for
        /// you — without it Atlassian issues no refresh token at all.
        #[arg(long, default_value = "read,write,issues:create")]
        scopes: String,
        /// Who the tracker's writes are attributed to: `user` (you) or `app`
        /// (the application itself). With `app`, issues and comments this tool
        /// creates show as the app's — revoking your own key later does not
        /// sever them.
        #[arg(long, default_value = "user")]
        actor: String,
        /// Print the sign-in link instead of opening a browser — for a remote
        /// or headless session.
        #[arg(long)]
        print_only: bool,
    },
    /// Forget the stored key and tell the tracker to invalidate it.
    Disconnect {
        #[arg(long, default_value = "github")]
        provider: String,
    },
}

#[derive(Subcommand, Debug, PartialEq)]
pub enum SyncAction {
    /// One-shot back-fill: push the existing local corpus (think/ship/roadmap/
    /// signal) to the cloud. Idempotent + resumable. Needs
    /// THINK_AND_SHIP_CLOUD_URL + THINK_AND_SHIP_CLOUD_TOKEN.
    Push {
        /// Count the records that would be pushed without contacting the cloud.
        #[arg(long)]
        dry_run: bool,
        /// Push every project with a store on this machine, not only the one
        /// this directory resolves to. A record's cloud copy is rewritten only
        /// on mutation, so a field added after a project went quiet never
        /// reaches its records until someone pushes from that project — this
        /// flag is that push, for all of them at once.
        #[arg(long)]
        all_projects: bool,
    },
}

impl Command {
    /// Rewrite a retired top-level spelling into its noun-grouped form.
    ///
    /// Returns the canonical command plus, when the caller used a retired
    /// spelling, a one-line note to print on stderr. Dispatch therefore only
    /// ever sees canonical variants — there is exactly one code path per
    /// command, so an alias can't drift away from the thing it aliases.
    pub fn canonicalize(self) -> (Command, Option<&'static str>) {
        let moved = |note| Some(note);
        match self {
            Command::Export { format } => (
                Command::Roadmap {
                    action: RoadmapAction::Export { format },
                },
                moved("note: `export` is now `roadmap export`"),
            ),
            Command::Import {
                file,
                shared,
                dry_run,
                merge,
            } => (
                Command::Roadmap {
                    action: RoadmapAction::Import {
                        file,
                        shared,
                        dry_run,
                        merge,
                    },
                },
                moved("note: `import` is now `roadmap import`"),
            ),
            Command::Hygiene {
                dry_run,
                stall_days,
                idle_days,
            } => (
                Command::Roadmap {
                    action: RoadmapAction::Hygiene {
                        dry_run,
                        stall_days,
                        idle_days,
                    },
                },
                moved("note: `hygiene` is now `roadmap hygiene`"),
            ),
            Command::Promote {
                session,
                step,
                kind,
            } => (
                Command::Trace {
                    action: TraceAction::Promote {
                        session,
                        step,
                        kind,
                    },
                },
                moved("note: `promote` is now `trace promote`"),
            ),
            Command::Eval {
                corpus,
                learned,
                prequential,
                warmup,
                weights_out,
            } => (
                Command::Corpus {
                    action: CorpusAction::Eval {
                        corpus,
                        learned,
                        prequential,
                        warmup,
                        weights_out,
                    },
                },
                moved("note: `eval` is now `corpus eval`"),
            ),
            canonical => (canonical, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(args: &[&str]) -> Command {
        Cli::try_parse_from(args).expect("should parse").command
    }

    /// Every retired spelling must resolve to exactly the command its
    /// replacement produces — same variant, same fields. This is what makes the
    /// aliases safe: they can't quietly diverge from the thing they alias.
    #[test]
    fn retired_spellings_resolve_to_their_replacement() {
        let pairs: [(&[&str], &[&str]); 5] = [
            (
                &["think-and-ship", "export", "--format", "json"],
                &["think-and-ship", "roadmap", "export", "--format", "json"],
            ),
            (
                &["think-and-ship", "import", "--file", "R.md", "--merge"],
                &[
                    "think-and-ship",
                    "roadmap",
                    "import",
                    "--file",
                    "R.md",
                    "--merge",
                ],
            ),
            (
                &["think-and-ship", "hygiene", "--stall-days", "3"],
                &["think-and-ship", "roadmap", "hygiene", "--stall-days", "3"],
            ),
            (
                &[
                    "think-and-ship",
                    "promote",
                    "--session",
                    "s1",
                    "--step",
                    "4",
                ],
                &[
                    "think-and-ship",
                    "trace",
                    "promote",
                    "--session",
                    "s1",
                    "--step",
                    "4",
                ],
            ),
            (
                &["think-and-ship", "eval", "--learned"],
                &["think-and-ship", "corpus", "eval", "--learned"],
            ),
        ];

        for (old, new) in pairs {
            let (from_old, note) = parse(old).canonicalize();
            let (from_new, no_note) = parse(new).canonicalize();
            assert_eq!(
                from_old,
                from_new,
                "`{}` should resolve exactly like `{}`",
                old.join(" "),
                new.join(" ")
            );
            assert!(
                note.is_some(),
                "`{}` should tell the user where it moved",
                old.join(" ")
            );
            assert!(no_note.is_none(), "the canonical spelling needs no note");
        }
    }

    /// The retired spellings must not be advertised — they're a courtesy for
    /// existing muscle memory, not part of the grammar we teach.
    #[test]
    fn retired_spellings_are_hidden_from_help() {
        let help = Cli::command().render_long_help().to_string();
        for retired in ["export", "import", "hygiene", "promote", "eval"] {
            let listed_as_command = help
                .lines()
                .any(|l| l.trim_start().starts_with(&format!("{retired} ")));
            assert!(
                !listed_as_command,
                "retired spelling `{retired}` should be hidden from --help"
            );
        }
        // …while the noun groups that replaced them are advertised.
        for noun in ["roadmap", "trace", "corpus", "skills"] {
            assert!(
                help.contains(noun),
                "noun group `{noun}` should appear in --help"
            );
        }
    }

    /// Every area noun must actually be a group with verbs under it. A noun that
    /// takes no subcommand is a verb wearing a noun's name.
    #[test]
    fn every_area_noun_is_a_group_with_verbs() {
        let cli = Cli::command();
        for noun in [
            "roadmap",
            "trace",
            "corpus",
            "skills",
            "sync",
            "telemetry",
            "tracker",
        ] {
            let sub = cli
                .get_subcommands()
                .find(|c| c.get_name() == noun)
                .unwrap_or_else(|| panic!("`{noun}` should be a subcommand"));
            assert!(
                sub.get_subcommands().count() > 0,
                "`{noun}` is a noun, so it must group verbs"
            );
        }
    }

    #[test]
    fn roadmap_group_covers_the_whole_roadmap_surface() {
        assert_eq!(
            parse(&["think-and-ship", "roadmap", "next"]),
            Command::Roadmap {
                action: RoadmapAction::Next
            }
        );
        assert_eq!(
            parse(&["think-and-ship", "roadmap", "status"]),
            Command::Roadmap {
                action: RoadmapAction::Status
            }
        );
    }

    /// Help text is the first thing a user reads, so it must not be written for
    /// whoever built the feature. Internal phase numbers (the word "phase"
    /// followed by a build number) mean nothing outside this repo's history,
    /// and implementation nouns describe a mechanism rather than what the
    /// command does for the reader.
    ///
    /// The denylist is deliberately concrete — every entry shipped in real help
    /// output before this test existed.
    #[test]
    fn help_text_is_free_of_repo_internal_jargon() {
        let help = full_help_text();

        let phase_number = help.lines().find(|line| {
            let l = line.to_ascii_lowercase();
            l.contains("phase ") && l.chars().any(|c| c.is_ascii_digit())
        });
        assert!(
            phase_number.is_none(),
            "help text ships an internal phase number: {phase_number:?}"
        );

        for jargon in [
            "renumbered duplicate step clones",
            "git-native",
            "THINK_AND_SHIP_SYNC_TARGET",
            "OTLP/HTTP JSON",
            "prequential",
            "digest-verified",
            "structural event stream",
            "WorkOS",
        ] {
            assert!(
                !help.contains(jargon),
                "help text still says {jargon:?} — describe what it does for the reader instead"
            );
        }
    }

    /// Render every command's long help, recursively — the text a user can
    /// actually reach with `--help`.
    fn full_help_text() -> String {
        fn walk(cmd: &mut clap::Command, out: &mut String) {
            out.push_str(&cmd.render_long_help().to_string());
            let names: Vec<String> = cmd
                .get_subcommands()
                .map(|s| s.get_name().to_string())
                .collect();
            for name in names {
                if name == "help" {
                    continue;
                }
                if let Some(sub) = cmd.find_subcommand_mut(&name) {
                    walk(sub, out);
                }
            }
        }
        let mut out = String::new();
        walk(&mut Cli::command(), &mut out);
        out
    }

    /// `status` (the installation) and `roadmap status` (the plan) are different
    /// commands and must stay distinguishable.
    #[test]
    fn top_level_status_is_not_roadmap_status() {
        assert_eq!(parse(&["think-and-ship", "status"]), Command::Status);
        assert_ne!(
            parse(&["think-and-ship", "status"]),
            parse(&["think-and-ship", "roadmap", "status"])
        );
    }
}
