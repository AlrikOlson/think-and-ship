//! The roadmap engine: owns the in-memory [`Roadmap`] and mediates every
//! mutation, mirroring the shape of `ship::engine::ShipEngine` (a plain struct
//! whose concurrency guard lives at the service layer; builder-style
//! `with_persistence`; persist-on-mutation).
//!
//! Dependency direction: this module depends on `crate::roadmap::domain` (pure)
//! and `crate::infra` (persistence) — never the reverse (DIP).

use chrono::Utc;

use crate::cloud::client::CloudClient;
use crate::infra::{CrossRef, Persistence};
use crate::roadmap::broadcast::{Broadcaster, RoadmapFrame};
use crate::roadmap::domain::{
    BlockedBy, BlockerKind, Chunk, ChunkStatus, ContainerKind, ContainerLink, Focus, FocusMode,
    RefreshNote, ReprioritizeProposal, Roadmap, TrackerLink, TrackerOptIn, validate_lane,
};
use crate::roadmap::region;

pub struct RoadmapEngine {
    roadmap: Roadmap,
    project_id: String,
    persistence: Option<Persistence>,
    broadcaster: Option<Broadcaster>,
    /// Optional git-native trace mirror, reusing the same mirror core as the
    /// think/ship trace stores. When set, every mutation mirrors into
    /// `.think-and-ship/` as an Agent Trace
    /// JSONL record; the session commits on chunk completion/obsoletion. All
    /// git/IO runs on the worker's own thread, never under this engine's lock.
    /// `None` = the default Local behaviour. Fire-and-forget.
    mirror: Option<crate::infra::MirrorWorker>,
    /// Whether mirrored records are `shared` (committed `sessions/`) vs `local`
    /// (gitignored). Default `false`. Only meaningful with `mirror`.
    repo_shared: bool,
    /// Optional cloud sync client. When set, every mutation
    /// fire-and-forget pushes the chunk envelope to the cloud backend. `None`
    /// (default) = no cloud sync.
    cloud: Option<CloudClient>,
    /// The provider a newly-born ACTIVE chunk inherits an opt-in to
    /// (tracker-optin-never-grows). `None` — the default, and what every test
    /// and every unconfigured project gets — means chunks are born silent.
    ///
    /// A plain `Option<String>` rather than a `TrackerConfig` on purpose: the
    /// engine must not learn that `crate::tracker::config` exists, so the
    /// DECISION is made by [`crate::tracker::config::inherited_opt_in`] at the
    /// composition root and only its answer is handed down. That also means the
    /// engine cannot accidentally widen the rule.
    inherit_opt_in: Option<String>,
}

/// What a caller's workstream name resolved to.
///
/// Three outcomes, and only one of them is allowed to change anything. The
/// other two carry the exact candidate list, because "no such workstream" is
/// useless on its own and "did you mean" is the entire remedy — a caller that
/// gets a bare refusal will guess again, and guessing is what produced the
/// wrong name in the first place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupResolution {
    /// Exactly one workstream matched. Carries the STORED name, not what the
    /// caller typed, so everything downstream compares against one spelling.
    Exact(String),
    /// Several matched. Carries them, in stored order.
    Ambiguous(Vec<String>),
    /// None matched. Carries every workstream in play, which is the only
    /// useful thing to say to someone who named one that does not exist.
    Unknown(Vec<String>),
}

impl GroupResolution {
    /// The stored name, when there is exactly one.
    #[must_use]
    pub fn exact(&self) -> Option<&str> {
        match self {
            Self::Exact(name) => Some(name.as_str()),
            _ => None,
        }
    }

    /// The refusal a caller sees, naming what they typed and what exists.
    #[must_use]
    pub fn explain(&self, query: &str) -> String {
        let list = |names: &[String]| {
            if names.is_empty() {
                "(none — no chunk in this roadmap has a workstream yet)".to_string()
            } else {
                names.join(", ")
            }
        };
        match self {
            Self::Exact(name) => format!("'{query}' resolved to '{name}'"),
            Self::Ambiguous(names) => format!(
                "'{query}' is ambiguous — it matches {}. Focus is unchanged; name one exactly.",
                list(names)
            ),
            Self::Unknown(names) => format!(
                "no workstream matches '{query}'. Focus is unchanged. Workstreams in play: {}",
                list(names)
            ),
        }
    }
}

impl RoadmapEngine {
    pub fn new(project_id: String) -> Self {
        Self {
            roadmap: Roadmap {
                project_id: project_id.clone(),
                preamble: String::new(),
                chunks: Vec::new(),
                notes: Vec::new(),
                refreshes: Vec::new(),
                links: Vec::new(),
                tracker_opt_ins: Vec::new(),
                container_links: Vec::new(),
                focuses: Vec::new(),
            },
            project_id,
            persistence: None,
            broadcaster: None,
            mirror: None,
            repo_shared: false,
            cloud: None,
            inherit_opt_in: None,
        }
    }

    /// Arm opt-in inheritance for chunks born from here on: every new ACTIVE
    /// chunk is opted in to `provider`. `None` disarms it.
    ///
    /// Builder half, for a composition root that knows the answer up front
    /// ([`crate::cli`]). Takes the resolved provider, never the config — see the
    /// `inherit_opt_in` field for why the engine is kept ignorant.
    #[must_use]
    pub fn with_opt_in_inheritance(mut self, provider: Option<String>) -> Self {
        self.set_opt_in_inheritance(provider);
        self
    }

    /// The setter half, for the moment consent is granted to an engine that is
    /// ALREADY RUNNING — `tracker setup` over MCP hands the live server's engine
    /// to `setup_local`. Without this, a setup would appear to work and then not
    /// take effect until the next restart, which is the same class of silent gap
    /// this setter exists to close.
    pub fn set_opt_in_inheritance(&mut self, provider: Option<String>) {
        self.inherit_opt_in = provider
            .map(|p| p.trim().to_ascii_lowercase())
            .filter(|p| !p.is_empty());
    }

    /// The provider new chunks inherit, if any. Reads back what was armed.
    #[must_use]
    pub fn opt_in_inheritance(&self) -> Option<&str> {
        self.inherit_opt_in.as_deref()
    }

    /// The project this engine belongs to — the reconcile filter's identity
    /// (sync-project-scope): only records stamped with this id merge in.
    #[must_use]
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn with_broadcaster(mut self, broadcaster: Broadcaster) -> Self {
        self.broadcaster = Some(broadcaster);
        self
    }

    /// Attach a cloud client so every mutation fire-and-forget pushes the chunk
    /// envelope to the cloud backend. Wired by `cli::build_unified`.
    pub fn with_cloud(mut self, client: CloudClient) -> Self {
        self.cloud = Some(client);
        self
    }

    /// Attach a git-native trace sink. `shared` selects the
    /// committed `sessions/` partition (`true`) vs the gitignored `local/`
    /// partition (`false`). Wired by `cli::build_unified`.
    pub fn with_repo_sink(mut self, sink: crate::infra::RepoSink, shared: bool) -> Self {
        self.mirror = Some(crate::infra::MirrorWorker::spawn(sink));
        self.repo_shared = shared;
        self
    }

    /// Block until the git-native mirror has drained every mutation submitted so
    /// far. No-op without a mirror. For graceful shutdown and deterministic tests.
    pub fn flush_mirror(&self) {
        if let Some(m) = &self.mirror {
            m.flush();
        }
    }

    /// Fire-and-forget: mirror the mutation into the git-native trace, then
    /// fan it out to the broadcast socket. A sink/broadcast error is
    /// logged at WARN and dropped — a mutation is never failed by it. The
    /// frame→record mapping lives here (engine-side) so `infra::repo_sync`
    /// stays domain-free.
    fn record_event(&self, frame: RoadmapFrame) {
        self.mirror_frame_to_repo(&frame);
        if let Some(b) = &self.broadcaster {
            b.emit(&frame);
        }
        self.cloud_push_frame(&frame);
    }

    /// Fire-and-forget: push the frame's chunk envelope to the cloud backend.
    /// No-op without a cloud client OR outside a tokio runtime
    /// (so sync unit tests never panic); a push error is logged and dropped, so
    /// a mutation is never failed by it.
    fn cloud_push_frame(&self, frame: &RoadmapFrame) {
        let Some(client) = &self.cloud else {
            return;
        };
        let chunk = match frame {
            RoadmapFrame::ChunkAdded { chunk }
            | RoadmapFrame::ChunkChanged { chunk }
            | RoadmapFrame::ChunkCompleted { chunk }
            | RoadmapFrame::ChunkObsoleted { chunk } => chunk,
            RoadmapFrame::RefreshRecorded { .. } => return,
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        // The chunk's tracker state rides its envelope (see build::from_chunk),
        // so a peer inherits the binding instead of minting a second twin.
        let links: Vec<_> = self
            .roadmap
            .links
            .iter()
            .filter(|l| l.chunk_id == chunk.id)
            .cloned()
            .collect();
        let opt_ins: Vec<_> = self
            .roadmap
            .tracker_opt_ins
            .iter()
            .filter(|o| o.chunk_id == chunk.id)
            .cloned()
            .collect();
        let envelope = crate::cloud::build::from_chunk(&self.project_id, chunk, &links, &opt_ins);
        let client = client.clone();
        handle.spawn(async move {
            if let Err(e) = client.push(&envelope).await {
                tracing::warn!(target: "think_and_ship::cloud", "roadmap cloud push failed: {e}");
            }
        });
    }

    fn mirror_frame_to_repo(&self, frame: &RoadmapFrame) {
        let Some(mirror) = &self.mirror else {
            return;
        };
        let (kind, closes) = frame.record_meta();
        let payload = match frame {
            RoadmapFrame::ChunkAdded { chunk }
            | RoadmapFrame::ChunkChanged { chunk }
            | RoadmapFrame::ChunkCompleted { chunk }
            | RoadmapFrame::ChunkObsoleted { chunk } => {
                serde_json::to_value(chunk).unwrap_or(serde_json::Value::Null)
            }
            RoadmapFrame::RefreshRecorded {
                summary,
                think_steps,
            } => serde_json::json!({ "summary": summary, "think_steps": think_steps }),
        };

        // Hand off to the worker thread; building the record (which shells git),
        // the append, and any commit all happen off this engine's lock.
        mirror.submit(crate::infra::MirrorJob {
            family: "roadmap",
            kind,
            session_id: self.project_id.clone(),
            shared: self.repo_shared,
            payload,
            files: vec![],
            closes,
        });
    }

    /// Attach a persistence handle, loading any prior roadmap for this project
    /// off disk first (so state accumulates across conversations).
    pub fn with_persistence(mut self, persistence: Persistence) -> Self {
        match persistence.load::<Roadmap>(&self.project_id) {
            Ok(Some(loaded)) => {
                eprintln!(
                    "think-and-ship: loaded roadmap with {} chunk(s) from disk",
                    loaded.chunks.len()
                );
                self.roadmap = loaded;
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("think-and-ship: roadmap load failed: {e}"),
        }
        self.persistence = Some(persistence);
        // Before the name fill: a chunk with no id cannot even seed a name,
        // and the known specimens predate the add-door rejection.
        let (minted, removed) = self.repair_missing_ids();
        if minted > 0 {
            eprintln!("think-and-ship: minted an id for {minted} chunk(s) that had none");
        }
        if removed > 0 {
            eprintln!("think-and-ship: removed {removed} contentless chunk(s) that had no id");
        }
        // Every chunk written before `name` existed loads without one. Filling
        // them here — after the store is attached, so the fill persists — is
        // what makes "no chunk is nameless" true of the existing roadmap and
        // not just of the ones added from now on.
        let filled = self.backfill_names();
        if filled > 0 {
            eprintln!("think-and-ship: named {filled} chunk(s) that had no name");
        }
        // After the fill, because a name seeded a moment ago can collide the
        // same way a name seeded a year ago does.
        let separated = self.repair_label_collisions();
        if separated > 0 {
            eprintln!(
                "think-and-ship: relabeled {separated} chunk(s) whose derived labels collided"
            );
        }
        self
    }

    /// Whether anything written here survives this process.
    ///
    /// Reported on every focus answer rather than left implicit, because a
    /// focus that silently evaporates at restart is worse than no focus at
    /// all: the caller would keep believing it had one. A person who is told
    /// "not persistent" can decide to re-focus each session; a person who is
    /// not told simply loses their place without explanation.
    #[must_use]
    pub const fn persistence_enabled(&self) -> bool {
        self.persistence.is_some()
    }

    /// Locked merge-on-save (family-stores-merge-on-save): chunks another
    /// live process acked are folded in, never clobbered. Merge policy:
    /// [`crate::roadmap::domain::merge_roadmaps`].
    fn persist(&self) {
        if let Some(p) = &self.persistence
            && let Err(e) = p.save_merging(
                &self.project_id,
                &self.roadmap,
                crate::roadmap::domain::merge_roadmaps,
            )
        {
            tracing::warn!("think-and-ship: roadmap persist failed: {e}");
        }
    }

    fn now() -> String {
        Utc::now().to_rfc3339()
    }

    fn index_of(&self, id: &str) -> Result<usize, String> {
        self.roadmap
            .chunks
            .iter()
            .position(|c| c.id == id)
            .ok_or_else(|| format!("chunk '{id}' not found"))
    }

    /// Read-only access for the wire/family layer.
    pub fn roadmap(&self) -> &Roadmap {
        &self.roadmap
    }

    /// Merge a chunk pulled from the cloud: insert when absent;
    /// replace an existing copy only when the incoming one is strictly NEWER
    /// (`domain::chunk_wins` — the same updated_at rule as the disk merge).
    /// Cloud-wins-unconditionally was the original policy and clobbered fresh
    /// local mutations when a realtime refresh raced their own push
    /// (reconcile-recency-guard). A SILENT merge — it does NOT
    /// emit a mutation frame or push back to the cloud; a reconcile that
    /// re-emitted would loop the pull straight back into a push. Persists.
    pub fn upsert_chunk(&mut self, chunk: Chunk) {
        // The same refusal as `add_chunk_with_content`, at the cloud door: a
        // record with no id cannot be addressed by anything downstream. The
        // push lane already skips these (`cloud::backfill`), so accepting one
        // here would only re-import a defect the other doors now reject.
        if chunk.id.trim().is_empty() {
            return;
        }
        match self.index_of(&chunk.id) {
            Ok(idx) => {
                if !crate::roadmap::domain::chunk_wins(&chunk, &self.roadmap.chunks[idx]) {
                    return; // local copy is as new or newer — nothing to do
                }
                self.roadmap.chunks[idx] = chunk;
            }
            Err(_) => self.roadmap.chunks.push(chunk),
        }
        self.persist();
    }

    /// Add a new chunk. Errors on a duplicate id.
    #[allow(clippy::too_many_arguments)]
    pub fn add_chunk(
        &mut self,
        id: String,
        title: String,
        status: ChunkStatus,
        priority: u32,
        description: String,
        acceptance: Vec<String>,
        deps: Vec<String>,
        shared: bool,
    ) -> Result<&Chunk, String> {
        self.add_chunk_with_content(
            id,
            title,
            String::new(),
            status,
            priority,
            description,
            acceptance,
            deps,
            shared,
            None,
            None,
        )
    }

    /// [`Self::add_chunk`] carrying the optional structured body — the rich
    /// rendering sidecar the MCP seam steers writers toward. Validated by the
    /// caller; `None` is every legacy writer.
    #[allow(clippy::too_many_arguments)]
    pub fn add_chunk_with_content(
        &mut self,
        id: String,
        title: String,
        name: String,
        status: ChunkStatus,
        priority: u32,
        description: String,
        acceptance: Vec<String>,
        deps: Vec<String>,
        shared: bool,
        content: Option<crate::content::StructuredContent>,
        group: Option<String>,
    ) -> Result<&Chunk, String> {
        // The id is the one field every other verb keys on: a chunk without
        // one cannot be linked, mutated, mirrored, or drawn. Two real stores
        // held such a record — both admitted here, back when this door only
        // checked for duplicates. This check is what makes `name::derive`'s
        // "the engine rejects that id upstream" true.
        if id.trim().is_empty() {
            return Err(
                "a chunk needs an id — it is the field every other verb addresses it by"
                    .to_string(),
            );
        }
        if self.roadmap.chunks.iter().any(|c| c.id == id) {
            return Err(format!("chunk '{id}' already exists"));
        }
        // "Required or derived, never absent": a caller may omit the name and
        // get a usable one, but no path exists that creates a nameless chunk.
        // An over-budget name IS rejected — the whole point of the field is that
        // a sentence cannot be what a node wears, and accepting one here would
        // be the leak that reopens the defect.
        let name = match name.trim() {
            "" => crate::roadmap::name::derive(&id),
            given => {
                if let Some(why) = crate::roadmap::name::why_unfit(given) {
                    return Err(format!("chunk '{id}': {why}"));
                }
                given.to_string()
            }
        };
        // The workstream is validated BEFORE anything is written, and rejected
        // as a whole call rather than dropped. A chunk born ungrouped because
        // its group was quietly ignored is invisible to every focused read —
        // `next_in_group` will not offer it and `group_status` will not count
        // it — and nothing about the created chunk says why. Refusing here is
        // the only outcome a caller can act on.
        let group = Self::clean_group(group);
        if let Some(name) = &group
            && let Some(why) = self.why_group_unfit(name, Some(&id))
        {
            return Err(format!("cannot group chunk '{id}': {why}"));
        }
        let now = Self::now();
        // Captured before `id` moves into the chunk: the inherited opt-in below
        // needs to name it, and the status decides whether it qualifies.
        let born = id.clone();
        let born_active = status.is_active();
        self.roadmap.chunks.push(Chunk {
            tier: None,
            id,
            title,
            name,
            status,
            priority,
            description,
            content,
            notes: String::new(),
            group,
            acceptance,
            deps,
            cross_refs: Vec::new(),
            shared,
            reprioritize: None,
            status_proposal: None,
            title_proposal: None,
            obsoleted_reason: None,
            blocked_by: None,
            // Stamped at birth: this engine is this project, so a chunk it
            // records is provably ours and every later copy carries the proof.
            project_id: Some(self.project_id.clone()),
            created_at: now.clone(),
            updated_at: now,
        });
        self.persist();
        let snapshot = self.roadmap.chunks.last().unwrap().clone();
        self.record_event(RoadmapFrame::ChunkAdded { chunk: snapshot });

        // THE INHERITANCE, and the only place it happens
        // (tracker-optin-never-grows). A project whose owner explicitly ran
        // `tracker on` should not have to re-consent to every chunk the plan
        // grows; the argument for reversing the silent-by-default rule, and the
        // reason it costs nothing, is written at
        // [`crate::tracker::config::inherited_opt_in`], which is what decides
        // whether this field is set at all.
        //
        // That this is inside `add_chunk` — reachable only by a chunk coming
        // into existence — is what makes "never a retroactive sweep" structural
        // rather than a promise: there is no loop here that could reach a chunk
        // that already exists, so there is nothing to accidentally point at one.
        if born_active && let Some(provider) = self.inherit_opt_in.clone() {
            // Ignored deliberately: the chunk exists (we just made it) and the
            // provider is non-empty (the setter filters), so the only way this
            // errors is a future refactor, and failing to opt in must never
            // fail the creation of the chunk itself.
            let _ = self.set_tracker_opt_in(&born, &provider, true);
        }

        self.roadmap
            .chunks
            .iter()
            .find(|c| c.id == born)
            .ok_or_else(|| "chunk vanished after add".to_string())
    }

    /// Move a chunk to a new status, validated against the transition table.
    ///
    /// An explicit transition also DISPOSES any open status proposal — the
    /// field-scoped disposal rule ("Proposals, and what disposes them", above
    /// [`Self::propose_status`]). Moving to the suggested status accepts the
    /// suggestion; moving anywhere else supersedes it. Either way the human
    /// acted on the field, which is all a proposal ever asks for.
    pub fn set_status(&mut self, id: &str, to: ChunkStatus) -> Result<&Chunk, String> {
        let idx = self.index_of(id)?;
        let from = self.roadmap.chunks[idx].status;
        if !from.allows(to) {
            return Err(format!(
                "illegal transition for chunk '{id}': {from:?} -> {to:?}"
            ));
        }
        let c = &mut self.roadmap.chunks[idx];
        c.status = to;
        c.status_proposal = None;
        c.updated_at = Self::now();
        self.persist();
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    /// Patch a chunk's descriptive fields (status is handled by `set_status`).
    /// Any `None` argument leaves that field unchanged — including `content`,
    /// which is replace-only here (like `title`); nothing needs to clear it.
    #[allow(clippy::too_many_arguments)]
    pub fn update_chunk(
        &mut self,
        id: &str,
        title: Option<String>,
        priority: Option<u32>,
        description: Option<String>,
        acceptance: Option<Vec<String>>,
        deps: Option<Vec<String>>,
        content: Option<crate::content::StructuredContent>,
    ) -> Result<&Chunk, String> {
        let idx = self.index_of(id)?;
        let c = &mut self.roadmap.chunks[idx];
        if let Some(t) = title {
            // An explicit title edit is the human speaking about exactly the
            // field a title proposal is about, so it disposes the proposal —
            // otherwise the projector would keep sending the conceded value
            // and the edit the human just made could never reach the tracker.
            if c.title != t {
                c.title_proposal = None;
            }
            c.title = t;
        }
        if let Some(p) = priority {
            // Same rule as the title edit above, for the reprioritize sibling:
            // an explicit priority edit is the human acting on exactly the
            // field the proposal is about, so it disposes the proposal.
            if c.priority != p {
                c.reprioritize = None;
            }
            c.priority = p;
        }
        if let Some(d) = description {
            c.description = d;
        }
        if let Some(a) = acceptance {
            c.acceptance = a;
        }
        if let Some(d) = deps {
            c.deps = d;
        }
        if let Some(sc) = content {
            c.content = Some(sc);
        }
        c.updated_at = Self::now();
        self.persist();
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    /// Put a chunk in a workstream, or take it out of one.
    ///
    /// A verb of its own rather than a seventh `Option` on [`Self::update_chunk`],
    /// for a reason that is not style: that function reads `None` as "leave
    /// alone", so it cannot express CLEARING a field. Grouping needs both, and
    /// `Option<Option<String>>` to get them would be a worse interface than a
    /// named method.
    ///
    /// An empty or whitespace-only name clears rather than creating a container
    /// called "" upstream.
    ///
    /// A name that repeats a chunk id prefix is REFUSED here rather than left
    /// for `doctor` to object to later. The engine holds every chunk, so it is
    /// the only place that can tell a caller at the moment it happens; the
    /// alternative was a red diagnostic hours afterwards with no memory of who
    /// wrote it. The phrase returned is the one [`region::why_unfit`] prints.
    /// Why `name` cannot be a workstream, or `None` if it can.
    ///
    /// THE one place the rule lives, so the two doors into it — naming a
    /// workstream when a chunk is created, and re-naming one later — cannot
    /// drift apart. They must agree exactly: a group `add_chunk` accepts and
    /// `set_group` would reject is a backdoor around the rule, and the caller
    /// would only discover it the next time they touched the chunk.
    ///
    /// `prospective_id` is the id of a chunk that does not exist YET. Passing
    /// it matters: `region::why_unfit` compares the name against every chunk's
    /// id prefix, so validating a birth without the newborn's own prefix would
    /// accept `Billing` for a chunk called `billing-1` — which `set_group`
    /// refuses, because a region has to name a place rather than repeat a slug.
    fn why_group_unfit(&self, name: &str, prospective_id: Option<&str>) -> Option<String> {
        let prefixes: Vec<String> = self
            .roadmap
            .chunks
            .iter()
            .map(|c| region::id_prefix(&c.id).to_string())
            .chain(prospective_id.map(|id| region::id_prefix(id).to_string()))
            .collect();
        region::why_unfit(name, prefixes.iter().map(String::as_str))
    }

    /// Trim a caller's group to `None` (ungrouped) or a real name.
    fn clean_group(group: Option<String>) -> Option<String> {
        group
            .map(|g| g.trim().to_string())
            .filter(|g| !g.is_empty())
    }

    pub fn set_group(&mut self, id: &str, group: Option<String>) -> Result<&Chunk, String> {
        let idx = self.index_of(id)?;
        let cleaned = Self::clean_group(group);
        if let Some(name) = &cleaned
            && let Some(why) = self.why_group_unfit(name, None)
        {
            return Err(format!("cannot group chunk '{id}': {why}"));
        }
        let c = &mut self.roadmap.chunks[idx];
        if c.group == cleaned {
            // No-op writes still cost a persist and an event, and a projector
            // that re-groups every push would churn the tracker for nothing.
            return Ok(&self.roadmap.chunks[idx]);
        }
        c.group = cleaned;
        c.updated_at = Self::now();
        self.persist();
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    /// Rewrite the short label a node wears, leaving the sentence `title`
    /// alone.
    ///
    /// Its own verb rather than another argument on [`Self::update_chunk`],
    /// which already carries seven and is called from twenty-five places. This
    /// mirrors [`Self::set_group`], the other field a human corrects one at a
    /// time.
    ///
    /// Passing an empty name re-seeds from the id rather than clearing it: a
    /// nameless chunk is exactly what this field exists to make unreachable.
    pub fn set_name(&mut self, id: &str, name: &str) -> Result<&Chunk, String> {
        let idx = self.index_of(id)?;
        let cleaned = match name.trim() {
            "" => crate::roadmap::name::derive(id),
            given => {
                if let Some(why) = crate::roadmap::name::why_unfit(given) {
                    return Err(format!("chunk '{id}': {why}"));
                }
                given.to_string()
            }
        };
        let c = &mut self.roadmap.chunks[idx];
        if c.name == cleaned {
            // No-op writes still cost a persist and an event, and the projector
            // would churn the tracker for nothing — same reasoning as set_group.
            return Ok(&self.roadmap.chunks[idx]);
        }
        c.name = cleaned;
        c.updated_at = Self::now();
        self.persist();
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    /// Deal with every chunk that has no id at all, and report how many were
    /// re-identified and how many removed.
    ///
    /// The id is the field every other verb keys on, so an id-less record can
    /// only sit in the store: it cannot be linked, mutated, mirrored, or
    /// drawn. Two real stores held one — both admitted by an `add_chunk` that
    /// did not yet reject an empty id. Runs on load (see
    /// [`Self::with_persistence`]) before [`Self::backfill_names`], so a
    /// minted id has its name seeded in the same pass.
    ///
    /// Disposition, decided from the two observed records rather than by
    /// abstract rule: a record carrying content (a title, description, notes,
    /// acceptance, or cross-refs) gets the id the import lane would have
    /// minted for that content — `import::derive_id` over
    /// its title (description when the title is empty too), uniquified
    /// against the store — because import is the one lane that already knows
    /// how to turn prose into a stable id. A record carrying nothing at all
    /// is removed: there is no information to key, and the one observed
    /// specimen's own `obsoleted_reason` recorded the intent ("removing")
    /// that could not complete against an unaddressable record. Idempotent:
    /// after one pass no empty id remains.
    pub fn repair_missing_ids(&mut self) -> (usize, usize) {
        if !self.roadmap.chunks.iter().any(|c| c.id.trim().is_empty()) {
            return (0, 0);
        }
        let mut seen: std::collections::HashSet<String> = self
            .roadmap
            .chunks
            .iter()
            .filter(|c| !c.id.trim().is_empty())
            .map(|c| c.id.clone())
            .collect();
        let mut minted = 0;
        let mut removed = 0;
        let mut kept = Vec::with_capacity(self.roadmap.chunks.len());
        for mut c in std::mem::take(&mut self.roadmap.chunks) {
            if !c.id.trim().is_empty() {
                kept.push(c);
                continue;
            }
            let contentless = c.title.trim().is_empty()
                && c.description.trim().is_empty()
                && c.notes.trim().is_empty()
                && c.acceptance.is_empty()
                && c.cross_refs.is_empty();
            if contentless {
                removed += 1;
                continue;
            }
            let source = if c.title.trim().is_empty() {
                &c.description
            } else {
                &c.title
            };
            c.id = crate::roadmap::import::uniquify(
                crate::roadmap::import::derive_id(source),
                &mut seen,
            );
            minted += 1;
            kept.push(c);
        }
        self.roadmap.chunks = kept;
        self.persist();
        (minted, removed)
    }

    /// Give a name to every chunk that arrived without one, and report how many
    /// were filled.
    ///
    /// Runs on load (see [`Self::with_persistence`]) rather than as a one-off
    /// command, because chunks arrive from more than one direction: the local
    /// store, a cloud pull, an import. A migration that only covers the file on
    /// disk leaves the other two doors open.
    ///
    /// Only ever fills a gap. A name already present is left exactly as written,
    /// including one that is over budget — silently rewriting somebody's label
    /// would hide the very thing the gate is there to report.
    pub fn backfill_names(&mut self) -> usize {
        let mut filled = 0;
        for c in &mut self.roadmap.chunks {
            if c.name.trim().is_empty() {
                c.name = crate::roadmap::name::derive(&c.id);
                filled += 1;
            }
        }
        if filled > 0 {
            self.persist();
        }
        filled
    }

    /// Separate chunks whose derived labels collided, and report how many were
    /// relabeled. The other half of the same migration as [`Self::backfill_names`],
    /// run beside it on load for the same reason: colliding seeds arrive from
    /// the local store, cloud pulls, and imports alike.
    ///
    /// Delegates the whole decision to [`crate::roadmap::name::repair_collisions`],
    /// which only ever proposes a rewrite for a name still equal to its own
    /// seed — an authored label, and any label that does not collide, is out of
    /// reach by construction. Idempotent: a relabeled chunk no longer matches
    /// its seed, so the next load leaves it alone.
    pub fn repair_label_collisions(&mut self) -> usize {
        let rewrites = {
            let table: Vec<(&str, &str)> = self
                .roadmap
                .chunks
                .iter()
                .map(|c| (c.id.as_str(), c.name.as_str()))
                .collect();
            crate::roadmap::name::repair_collisions(&table)
        };
        let relabeled = rewrites.len();
        for (id, name) in rewrites {
            if let Some(c) = self.roadmap.chunks.iter_mut().find(|c| c.id == id) {
                c.name = name;
            }
        }
        if relabeled > 0 {
            self.persist();
        }
        relabeled
    }

    /// Every chunk whose name cannot serve as a node label, with the reason.
    /// The data-side half of constraint C8 — `doctor` reports it, and it is the
    /// gate that stops the sentence style leaking back in.
    #[must_use]
    pub fn chunks_without_usable_names(&self) -> Vec<(String, String)> {
        self.roadmap
            .chunks
            .iter()
            .filter_map(|c| crate::roadmap::name::why_unfit(&c.name).map(|why| (c.id.clone(), why)))
            .collect()
    }

    /// Every distinct group in play, sorted, ignoring ungrouped chunks.
    #[must_use]
    pub fn groups(&self) -> Vec<String> {
        let mut seen: Vec<String> = self
            .roadmap
            .chunks
            .iter()
            .filter_map(|c| c.group.clone())
            .collect();
        seen.sort();
        seen.dedup();
        seen
    }

    // ── Focus: what one lane is currently working on ───────────────────
    //
    // Every verb below is keyed by lane. There is deliberately no lane-less
    // variant of any of them: a convenience overload defaulting the lane is
    // the project-global focus this design exists to make impossible, and it
    // would be one commit away at all times if the door were left open.

    /// What `lane` is focused on, or `None` if it has never focused anything.
    ///
    /// Read-only, and it does not validate the lane: asking after a nonsense
    /// lane is honestly answered "that lane has no focus", which is true and is
    /// what a caller reconstructing its own state needs to hear.
    #[must_use]
    pub fn focus_get(&self, lane: &str) -> Option<&Focus> {
        let lane = lane.trim();
        self.roadmap.focuses.iter().find(|f| f.lane == lane)
    }

    /// Point `lane` at `group` in `mode`.
    ///
    /// Validates BEFORE it writes, and the ordering is the contract: a call
    /// naming an unknown or ambiguous group leaves the store exactly as it was,
    /// so a mistyped switch can never strand a lane somewhere it did not ask to
    /// be. Chunk status is untouched — focus points at work, it does not start
    /// any.
    pub fn focus_set(&mut self, lane: &str, group: &str, mode: FocusMode) -> Result<Focus, String> {
        let lane = validate_lane(lane)?;
        let resolved = match self.resolve_group(group) {
            GroupResolution::Exact(name) => name,
            other => return Err(other.explain(group)),
        };
        let now = Self::now();
        let focus = match self.roadmap.focuses.iter_mut().find(|f| f.lane == lane) {
            Some(existing) => {
                existing.group = resolved;
                existing.mode = mode;
                existing.updated_at = now;
                existing.clone()
            }
            None => {
                let focus = Focus {
                    lane,
                    group: resolved,
                    mode,
                    set_at: now.clone(),
                    updated_at: now,
                };
                self.roadmap.focuses.push(focus.clone());
                focus
            }
        };
        self.persist();
        Ok(focus)
    }

    /// Drop `lane`'s focus. `true` when there was one to drop.
    pub fn focus_clear(&mut self, lane: &str) -> bool {
        let lane = lane.trim();
        let before = self.roadmap.focuses.len();
        self.roadmap.focuses.retain(|f| f.lane != lane);
        let removed = self.roadmap.focuses.len() != before;
        if removed {
            self.persist();
        }
        removed
    }

    /// Resolve what a caller typed into exactly one group, or refuse.
    ///
    /// Three ways to name a workstream, tried in order of decreasing certainty:
    /// the exact stored name, a case-insensitive match of it, then an
    /// unambiguous substring. The substring pass exists because "authentication"
    /// should find "Authentication and identity" — but it deliberately does not
    /// pick a winner among several matches, because "closest match" is how a
    /// caller ends up focused on a workstream it did not name and cannot see it
    /// did not name.
    #[must_use]
    pub fn resolve_group(&self, query: &str) -> GroupResolution {
        let q = query.trim();
        if q.is_empty() {
            return GroupResolution::Unknown(self.groups());
        }
        let groups = self.groups();
        if let Some(hit) = groups.iter().find(|g| g.as_str() == q) {
            return GroupResolution::Exact(hit.clone());
        }
        let lower = q.to_lowercase();
        let ci: Vec<&String> = groups
            .iter()
            .filter(|g| g.to_lowercase() == lower)
            .collect();
        if ci.len() == 1 {
            return GroupResolution::Exact(ci[0].clone());
        }
        let partial: Vec<String> = groups
            .iter()
            .filter(|g| g.to_lowercase().contains(&lower))
            .cloned()
            .collect();
        match partial.len() {
            0 => GroupResolution::Unknown(groups),
            1 => GroupResolution::Exact(partial.into_iter().next().expect("length checked")),
            _ => GroupResolution::Ambiguous(partial),
        }
    }

    /// The next ready chunk INSIDE `group` — same three readiness conditions as
    /// [`Self::next`], with membership added.
    ///
    /// It cannot return a chunk from another group, and that is the property the
    /// whole focused loop rests on: an empty answer here means "nothing ready in
    /// this workstream", never "look elsewhere". Callers are expected to stop.
    #[must_use]
    pub fn next_in_group(&self, group: &str) -> Option<&Chunk> {
        self.roadmap
            .chunks
            .iter()
            .filter(|c| c.group.as_deref() == Some(group))
            .filter(|c| {
                c.status == ChunkStatus::Pending && c.blocked_by.is_none() && self.deps_satisfied(c)
            })
            .min_by_key(|c| c.priority)
    }

    /// A focused frontier report for one group: counts, what is ready, what is
    /// blocked and why, and the next candidate.
    ///
    /// Separate from [`Self::status`] rather than a parameter on it, because the
    /// two answer different questions at different scales — `status` is bounded
    /// and elides, while a single workstream is small enough to report whole.
    #[must_use]
    pub fn group_status(&self, group: &str) -> serde_json::Value {
        let members: Vec<&Chunk> = self
            .roadmap
            .chunks
            .iter()
            .filter(|c| c.group.as_deref() == Some(group))
            .collect();
        let count = |s: ChunkStatus| members.iter().filter(|c| c.status == s).count();
        let ready: Vec<&&Chunk> = members
            .iter()
            .filter(|c| {
                c.status == ChunkStatus::Pending && c.blocked_by.is_none() && self.deps_satisfied(c)
            })
            .collect();
        // "Blocked" here is the reader's meaning, not the enum's: a chunk that
        // cannot be worked right now for ANY reason — a recorded blocker, an
        // unfinished dependency, or the Blocked status itself. Reporting only
        // the enum would say zero on a workstream nobody can move.
        let blocked: Vec<serde_json::Value> = members
            .iter()
            .filter(|c| {
                c.status == ChunkStatus::Blocked
                    || c.blocked_by.is_some()
                    || (c.status == ChunkStatus::Pending && !self.deps_satisfied(c))
            })
            .map(|c| {
                let mut row = serde_json::json!({ "id": c.id, "name": c.name });
                if let Some(b) = &c.blocked_by {
                    row["blocker_kind"] = serde_json::json!(b.kind.as_wire());
                    row["reason"] = serde_json::json!(b.reason);
                } else if c.status == ChunkStatus::Pending {
                    let unmet: Vec<&str> = c
                        .deps
                        .iter()
                        .filter(|d| {
                            !self
                                .roadmap
                                .chunks
                                .iter()
                                .any(|x| &&x.id == d && x.status == ChunkStatus::Done)
                        })
                        .map(String::as_str)
                        .collect();
                    row["unmet_deps"] = serde_json::json!(unmet);
                }
                row
            })
            .collect();
        serde_json::json!({
            "group": group,
            "counts": {
                "backlog": count(ChunkStatus::Backlog),
                "pending": count(ChunkStatus::Pending),
                "in_progress": count(ChunkStatus::InProgress),
                "blocked": count(ChunkStatus::Blocked),
                "done": count(ChunkStatus::Done),
                "obsoleted": count(ChunkStatus::Obsoleted),
                "total": members.len(),
            },
            "ready": ready.iter().map(|c| serde_json::json!({
                "id": c.id, "name": c.name, "priority": c.priority,
            })).collect::<Vec<_>>(),
            "ready_count": ready.len(),
            "blocked_count": blocked.len(),
            "blocked": blocked,
            "next": self.next_in_group(group).map(|c| c.id.clone()),
        })
    }

    /// Which ungrouped chunks share an id prefix, offered for a person to name.
    ///
    /// This used to assign the prefix as the group, and could not be repaired
    /// into doing so correctly. [`region::why_unfit`] rejects a region name
    /// contained in any chunk id prefix, and a prefix contains itself, so every
    /// name that guess could produce is rejected — always, not on unlucky
    /// slugs. That is where this roadmap's `saas`, `line`, `signal` and `iac`
    /// came from, and a fresh project running the seeder walked into the same
    /// red diagnostic with no account of what it had done.
    ///
    /// So the grouping survives and the naming does not. Read-only by
    /// consequence rather than by preference: there is nothing left it could
    /// honestly write.
    ///
    /// The floor is unchanged and still the point: measured on this roadmap, 17
    /// prefixes covered 239 of 317 chunks while 78 sat in a tail of 56 prefixes
    /// used once or twice, and a container per one-off slug is worse than no
    /// grouping at all.
    #[must_use]
    pub fn propose_groups_from_ids(&self, min_shared: usize) -> Vec<region::Cluster> {
        region::cluster_by_prefix(
            self.roadmap
                .chunks
                .iter()
                .map(|c| (c.id.as_str(), c.group.as_deref())),
            min_shared,
        )
    }

    /// Mark a chunk obsoleted with a reason. Kept for history, never `next()`-ed.
    pub fn obsolete_chunk(&mut self, id: &str, reason: String) -> Result<&Chunk, String> {
        let idx = self.index_of(id)?;
        let from = self.roadmap.chunks[idx].status;
        if !from.allows(ChunkStatus::Obsoleted) {
            return Err(format!("cannot obsolete chunk '{id}' from status {from:?}"));
        }
        let c = &mut self.roadmap.chunks[idx];
        c.status = ChunkStatus::Obsoleted;
        // The one status write outside `set_status`, so the field-scoped
        // disposal rule applies here too: obsoleting is the strongest possible
        // human statement about the status field.
        c.status_proposal = None;
        c.obsoleted_reason = Some(reason);
        c.updated_at = Self::now();
        self.persist();
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkObsoleted { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    // ── Proposals, and what disposes them ──────────────────────────────────
    //
    // Three proposal types live on a chunk, and all three obey ONE disposal
    // rule: a proposal is disposed by the human's explicit act on the field it
    // proposes about. Nothing else clears it, and nothing needs to — the act
    // the proposal was soliciting is itself the disposal.
    //
    // | Proposal      | Set by                    | Disposed by                                   |
    // |---------------|---------------------------|-----------------------------------------------|
    // | status        | propose_status            | set_status / obsolete_chunk (any transition)  |
    // | title         | propose_title             | resolve_title_proposal, or a differing title  |
    // |               |                           | edit in update_chunk                          |
    // | reprioritize  | propose_reprioritize      | a differing priority edit in update_chunk     |
    //
    // The one asymmetry is ARGUED, not accidental: only the TITLE gets an
    // explicit accept/reject verb, because only the title proposal has the two
    // properties that make a verb earn its surface — it is CONSULTED while
    // open (the projector keeps sending the conceded title, so "reject" must
    // exist to make the plan's title flow again), and "accept" has a
    // WRITE-BACK effect (the tracker's title is adopted into the plan). A
    // status or reprioritize proposal has neither: nothing consults it, and
    // accepting it is exactly the transition/priority edit the human can
    // already make — a verb would just be that same act with a second name.
    //
    // Eager disposal is safe because proposals REGENERATE: the sweep re-mints
    // on its next cycle while a divergence persists, so a disposal costs at
    // most one sweep interval, while a proposal that nothing clears costs the
    // truthfulness of every status view forever (the defect
    // tracker-status-proposal-disposal was filed about).
    //
    // Disposal is FIELD-SCOPED on purpose: a transition does not touch a title
    // proposal — a contested title stays contested when its chunk completes,
    // and the projector may be actively honoring that concession.

    /// Record a STATUS proposal. Never changes the chunk's real `status`.
    ///
    /// The mirror of [`Self::propose_reprioritize`], deliberately: reusing the
    /// established shape means a human already knows what a proposal is and
    /// where to look for it, instead of learning a second mechanism that does
    /// the same job with different words.
    ///
    /// A proposal identical to the one already recorded is a no-op, so a sweep
    /// that runs every hour does not restamp `proposed_at` forever and make an
    /// old suggestion look perpetually new.
    pub fn propose_status(
        &mut self,
        id: &str,
        suggested_status: ChunkStatus,
        reason: String,
        source: String,
    ) -> Result<&Chunk, String> {
        let idx = self.index_of(id)?;
        let unchanged = self.roadmap.chunks[idx]
            .status_proposal
            .as_ref()
            .is_some_and(|p| {
                p.suggested_status == suggested_status && p.reason == reason && p.source == source
            });
        if unchanged {
            return Ok(&self.roadmap.chunks[idx]);
        }

        let c = &mut self.roadmap.chunks[idx];
        c.status_proposal = Some(crate::roadmap::domain::StatusProposal {
            suggested_status,
            reason,
            proposed_at: Self::now(),
            source,
        });
        c.updated_at = Self::now();
        self.persist();
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    /// Record a conceded contested TITLE. Never changes the chunk's real
    /// `title` — but unlike its proposal siblings it is CONSULTED: while open,
    /// the projector keeps sending `suggested_title` instead of re-asserting
    /// the plan's, which is what makes the concession durable
    /// (tracker-contested-memory).
    ///
    /// Idempotent the same way [`Self::propose_status`] is: an identical
    /// suggestion is a no-op, so a projection running every few minutes does
    /// not restamp `proposed_at` and make an old disagreement look new. A
    /// remote that moves AGAIN to a different title is a different
    /// disagreement and replaces the proposal.
    pub fn propose_title(
        &mut self,
        id: &str,
        suggested_title: String,
        reason: String,
        source: String,
    ) -> Result<&Chunk, String> {
        let idx = self.index_of(id)?;
        let unchanged = self.roadmap.chunks[idx]
            .title_proposal
            .as_ref()
            .is_some_and(|p| {
                p.suggested_title == suggested_title && p.reason == reason && p.source == source
            });
        if unchanged {
            return Ok(&self.roadmap.chunks[idx]);
        }

        let c = &mut self.roadmap.chunks[idx];
        c.title_proposal = Some(crate::roadmap::domain::TitleProposal {
            suggested_title,
            reason,
            proposed_at: Self::now(),
            source,
        });
        c.updated_at = Self::now();
        self.persist();
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    /// Resolve an open title proposal — the human act with both outcomes.
    ///
    /// `accept: true` adopts the tracker's title into the plan and clears the
    /// proposal; `accept: false` clears it so the plan's title flows again on
    /// the next projection. Erring on a chunk with no open proposal is loud
    /// rather than a silent no-op: "resolved" must mean something happened.
    pub fn resolve_title_proposal(&mut self, id: &str, accept: bool) -> Result<&Chunk, String> {
        let idx = self.index_of(id)?;
        let Some(proposal) = self.roadmap.chunks[idx].title_proposal.take() else {
            return Err(format!("chunk '{id}' has no open title proposal"));
        };
        let c = &mut self.roadmap.chunks[idx];
        if accept {
            c.title = proposal.suggested_title;
        }
        c.updated_at = Self::now();
        self.persist();
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    /// Record a re-prioritization *proposal*. This never changes the chunk's
    /// real `priority` — it only attaches a suggestion for a human to accept,
    /// keeping "never auto-reorder" a type-level invariant.
    ///
    /// Idempotent the same way its two siblings are: an identical suggestion
    /// is a no-op, so a repeated proposal does not restamp `proposed_at` and
    /// make an old suggestion look perpetually new. It was the one sibling
    /// WITHOUT this guard — an accidental asymmetry, ended when the disposal
    /// rules were unified (tracker-status-proposal-disposal).
    pub fn propose_reprioritize(
        &mut self,
        id: &str,
        suggested_priority: u32,
        reason: String,
    ) -> Result<&Chunk, String> {
        let idx = self.index_of(id)?;
        let unchanged = self.roadmap.chunks[idx]
            .reprioritize
            .as_ref()
            .is_some_and(|p| p.suggested_priority == suggested_priority && p.reason == reason);
        if unchanged {
            return Ok(&self.roadmap.chunks[idx]);
        }

        let c = &mut self.roadmap.chunks[idx];
        c.reprioritize = Some(ReprioritizeProposal {
            suggested_priority,
            reason,
            proposed_at: Self::now(),
        });
        c.updated_at = Self::now();
        self.persist();
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    // ── Cross-family integration ────────────────────────────────────────────
    //
    // These keep the families decoupled: the roadmap records links and hands
    // back the `chunk:<id>` backref for the agent to wire into ship/think. No
    // roadmap tool reaches into the ship or think engines.

    /// Mark a chunk in progress (Pending/Blocked → InProgress). Returns the
    /// chunk; the caller derives the `chunk:<id>` backref to pass to
    /// `ship_set_objective` / a think `execution_ref`.
    pub fn start_chunk(&mut self, id: &str) -> Result<&Chunk, String> {
        self.set_status(id, ChunkStatus::InProgress)
    }

    /// Mark a chunk done, optionally attaching the proof-of-ship cross-ref
    /// (e.g. `task:<id>` or `check:<name>`). The ref is validated and stored
    /// in normalized form.
    pub fn complete_chunk(&mut self, id: &str, ship_ref: Option<&str>) -> Result<&Chunk, String> {
        if let Some(raw) = ship_ref {
            // Validate before transitioning so a bad ref doesn't half-apply.
            self.normalize_ref(raw)?;
        }
        self.set_status(id, ChunkStatus::Done)?;
        if let Some(raw) = ship_ref {
            self.push_cross_ref(id, raw)?;
        }
        let idx = self.index_of(id)?;
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkCompleted { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    /// Attach any validated cross-reference (`think:N`, `task:X`, `action:N`,
    /// `check:X`, `chunk:X`) to a chunk. Duplicates are ignored.
    pub fn link_chunk(&mut self, id: &str, cross_ref: &str) -> Result<&Chunk, String> {
        self.push_cross_ref(id, cross_ref)?;
        let idx = self.index_of(id)?;
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    /// Bind a chunk to its twin in an external tracker, or update the binding
    /// after a write (tracker-port-seam).
    ///
    /// Upserts by `(chunk_id, provider)` — one chunk has at most one twin per
    /// provider — and stamps the content hash and version that later make an
    /// idempotent re-projection and an echo fence possible. Also attaches the
    /// `ext:<provider>/<id>` cross-ref to the chunk, so the binding is visible
    /// in the same provenance graph as `think:`/`task:` refs rather than in a
    /// side table only this module knows about.
    ///
    /// `content_hash` is `WorkItem::content_hash()` of the payload just written.
    pub fn record_tracker_link(
        &mut self,
        chunk_id: &str,
        provider: &str,
        external_id: &str,
        content_hash: &str,
        version: Option<&str>,
    ) -> Result<&TrackerLink, String> {
        // Validate the chunk exists before mutating anything.
        self.index_of(chunk_id)?;
        let provider = provider.trim().to_ascii_lowercase();
        let external_id = external_id.trim().to_string();
        if provider.is_empty() || external_id.is_empty() {
            return Err("tracker link needs a provider and an external id".to_string());
        }
        let now = Self::now();

        match self
            .roadmap
            .links
            .iter_mut()
            .find(|l| l.chunk_id == chunk_id && l.provider == provider)
        {
            Some(link) => {
                link.external_id = external_id.clone();
                link.our_last_write_hash = content_hash.to_string();
                link.last_seen_version = version.map(str::to_string);
                link.updated_at = now;
            }
            None => self.roadmap.links.push(TrackerLink {
                chunk_id: chunk_id.to_string(),
                provider: provider.clone(),
                external_id: external_id.clone(),
                our_last_write_hash: content_hash.to_string(),
                last_seen_version: version.map(str::to_string),
                our_last_relations_hash: None,
                our_last_authored_hash: None,
                created_at: now.clone(),
                updated_at: now,
            }),
        }

        // Idempotent: push_cross_ref dedupes, so re-linking the same twin does
        // not accumulate refs.
        let wire = CrossRef::external(&provider, &external_id).to_wire();
        self.push_cross_ref(chunk_id, &wire)?;
        self.touch_chunk_for_link(chunk_id)?;
        self.persist();

        let idx = self.index_of(chunk_id)?;
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });

        self.tracker_link(chunk_id, &provider)
            .ok_or_else(|| "tracker link vanished after write".to_string())
    }

    /// Bind a container (group or roof) to its upstream twin, or refresh the
    /// binding after a push (tracker-group-ownership).
    ///
    /// Upserts by `(kind, name, provider)`. `created_by_us` is STICKY: once a
    /// binding remembers that we minted the container, a later resolve must not
    /// erase that fact, because it is what a future cleanup uses to tell our
    /// empty containers from a human's.
    pub fn record_container_link(
        &mut self,
        kind: ContainerKind,
        name: &str,
        provider: &str,
        external_id: &str,
        created_by_us: bool,
    ) -> Result<(), String> {
        let provider = provider.trim().to_ascii_lowercase();
        let external_id = external_id.trim().to_string();
        if name.trim().is_empty() || provider.is_empty() || external_id.is_empty() {
            return Err("container link needs a name, a provider and an external id".to_string());
        }
        let now = Self::now();

        match self
            .roadmap
            .container_links
            .iter_mut()
            .find(|c| c.kind == kind && c.name == name && c.provider == provider)
        {
            Some(link) => {
                link.external_id = external_id;
                link.created_by_us = link.created_by_us || created_by_us;
                link.updated_at = now;
            }
            None => self.roadmap.container_links.push(ContainerLink {
                kind,
                name: name.to_string(),
                provider,
                external_id,
                created_by_us,
                created_at: now.clone(),
                updated_at: now,
            }),
        }
        self.persist();
        Ok(())
    }

    /// The container's upstream binding, if one was ever recorded.
    #[must_use]
    pub fn container_link(
        &self,
        kind: ContainerKind,
        name: &str,
        provider: &str,
    ) -> Option<&ContainerLink> {
        self.roadmap
            .container_links
            .iter()
            .find(|c| c.kind == kind && c.name == name && c.provider == provider)
    }

    /// The chunk's twin on `provider`, if it has one.
    #[must_use]
    pub fn tracker_link(&self, chunk_id: &str, provider: &str) -> Option<&TrackerLink> {
        let provider = provider.trim().to_ascii_lowercase();
        self.roadmap
            .links
            .iter()
            .find(|l| l.chunk_id == chunk_id && l.provider == provider)
    }

    /// The link that binds `external_id` on `provider` back to a chunk.
    ///
    /// The inbound direction of [`Self::tracker_link`]: a sweep or a webhook
    /// knows the provider's id and needs to find out whether the item is ours.
    /// Deliberately searches ALL links rather than only opted-in chunks — a
    /// chunk opted OUT after projection still owns its twin, and forgetting
    /// that would make our own writes read as remote changes forever.
    #[must_use]
    pub fn tracker_link_by_external_id(
        &self,
        provider: &str,
        external_id: &str,
    ) -> Option<&TrackerLink> {
        let provider = provider.trim().to_ascii_lowercase();
        self.roadmap
            .links
            .iter()
            .find(|l| l.provider == provider && l.external_id == external_id)
    }

    /// Every tracker binding a chunk has, across providers.
    #[must_use]
    pub fn tracker_links_for(&self, chunk_id: &str) -> Vec<&TrackerLink> {
        self.roadmap
            .links
            .iter()
            .filter(|l| l.chunk_id == chunk_id)
            .collect()
    }

    /// Bump the chunk's `updated_at` because one of its tracker records changed.
    ///
    /// Links and opt-ins travel to other machines inside the chunk's cloud
    /// envelope, and both the disk merge and the cloud reconcile resolve chunks
    /// by strict `updated_at` recency. A link write that left the stamp alone
    /// would produce an envelope the peer declines as not-newer, so the binding
    /// would never propagate and each machine would mint its own twin — the
    /// exact duplicate-ticket failure the link record exists to prevent.
    /// [`Self::push_cross_ref`] stamps only when the ref is genuinely new, so it
    /// cannot carry this on a re-projection.
    fn touch_chunk_for_link(&mut self, chunk_id: &str) -> Result<(), String> {
        let idx = self.index_of(chunk_id)?;
        self.roadmap.chunks[idx].updated_at = Self::now();
        Ok(())
    }

    /// Record the fingerprint of the blocking-link set last declared for this
    /// chunk on `provider`. Kept apart from [`Self::record_tracker_link`] so the
    /// content fence and the relation fence move independently: a projection can
    /// change content without changing deps, and the reverse.
    ///
    /// Fails when no link exists yet — relations are declared between items that
    /// have already been created, so a link is always the prerequisite.
    pub fn record_tracker_relations(
        &mut self,
        chunk_id: &str,
        provider: &str,
        relations_hash: &str,
    ) -> Result<&TrackerLink, String> {
        self.index_of(chunk_id)?;
        let provider = provider.trim().to_ascii_lowercase();
        let now = Self::now();

        let Some(link) = self
            .roadmap
            .links
            .iter_mut()
            .find(|l| l.chunk_id == chunk_id && l.provider == provider)
        else {
            return Err(format!(
                "no tracker link for chunk '{chunk_id}' on provider '{provider}' — \
                 relations are declared between items that already exist"
            ));
        };
        link.our_last_relations_hash = Some(relations_hash.to_string());
        link.updated_at = now;

        self.touch_chunk_for_link(chunk_id)?;
        self.persist();

        let idx = self.index_of(chunk_id)?;
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });

        self.tracker_link(chunk_id, &provider)
            .ok_or_else(|| "tracker link vanished after write".to_string())
    }

    /// Record the digest of just the fields we AUTHORED in the write that
    /// [`Self::record_tracker_link`] just stamped.
    ///
    /// A sibling setter rather than a sixth parameter on `record_tracker_link`,
    /// for the reason [`Self::record_tracker_relations`] is one: the fences move
    /// independently and every existing caller keeps compiling. It also means an
    /// adapter path that records a link without an authored digest leaves `None`
    /// — which the preview reads as "cannot tell", the honest answer, rather
    /// than as a confident wrong one.
    ///
    /// Fails when no link exists yet: the authored digest describes a write, and
    /// a write always produces the link first.
    pub fn record_tracker_authored(
        &mut self,
        chunk_id: &str,
        provider: &str,
        authored_hash: &str,
    ) -> Result<&TrackerLink, String> {
        self.index_of(chunk_id)?;
        let provider = provider.trim().to_ascii_lowercase();
        let now = Self::now();

        let Some(link) = self
            .roadmap
            .links
            .iter_mut()
            .find(|l| l.chunk_id == chunk_id && l.provider == provider)
        else {
            return Err(format!(
                "no tracker link for chunk '{chunk_id}' on provider '{provider}' — \
                 the authored digest describes a write, which creates the link first"
            ));
        };
        link.our_last_authored_hash = Some(authored_hash.to_string());
        link.updated_at = now;

        self.touch_chunk_for_link(chunk_id)?;
        self.persist();

        let idx = self.index_of(chunk_id)?;
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });

        self.tracker_link(chunk_id, &provider)
            .ok_or_else(|| "tracker link vanished after write".to_string())
    }

    /// Opt a chunk in or out of projection into `provider`. Opting out is
    /// recorded rather than deleted so an explicit refusal wins the recency
    /// merge against a peer still holding the older opt-in.
    pub fn set_tracker_opt_in(
        &mut self,
        chunk_id: &str,
        provider: &str,
        enabled: bool,
    ) -> Result<&TrackerOptIn, String> {
        self.index_of(chunk_id)?;
        let provider = provider.trim().to_ascii_lowercase();
        if provider.is_empty() {
            return Err("tracker opt-in needs a provider".to_string());
        }
        let now = Self::now();

        match self
            .roadmap
            .tracker_opt_ins
            .iter_mut()
            .find(|o| o.chunk_id == chunk_id && o.provider == provider)
        {
            Some(opt_in) => {
                opt_in.enabled = enabled;
                opt_in.updated_at = now;
            }
            None => self.roadmap.tracker_opt_ins.push(TrackerOptIn {
                chunk_id: chunk_id.to_string(),
                provider: provider.clone(),
                enabled,
                updated_at: now,
            }),
        }

        self.touch_chunk_for_link(chunk_id)?;
        self.persist();

        let idx = self.index_of(chunk_id)?;
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });

        self.tracker_opt_in(chunk_id, &provider)
            .ok_or_else(|| "tracker opt-in vanished after write".to_string())
    }

    /// Whether this chunk has explicitly consented to `provider`. Absent means
    /// no — silence is the default.
    #[must_use]
    pub fn tracker_opt_in(&self, chunk_id: &str, provider: &str) -> Option<&TrackerOptIn> {
        let provider = provider.trim().to_ascii_lowercase();
        self.roadmap
            .tracker_opt_ins
            .iter()
            .find(|o| o.chunk_id == chunk_id && o.provider == provider)
    }

    /// Adopt tracker state that arrived from another machine inside a chunk
    /// envelope (see `cloud::build::from_chunk`).
    ///
    /// Merged by `(chunk_id, provider)` on strict `updated_at` recency — the
    /// same rule [`crate::roadmap::domain::merge_roadmaps`] uses on disk, so a
    /// record never means one thing over the wire and another on the filesystem.
    /// A stale peer copy cannot clobber a fresher local one. Returns how many
    /// records were actually adopted.
    pub fn adopt_tracker_state(
        &mut self,
        links: Vec<TrackerLink>,
        opt_ins: Vec<TrackerOptIn>,
    ) -> usize {
        let mut adopted = 0;
        for link in links {
            match self
                .roadmap
                .links
                .iter_mut()
                .find(|l| l.key() == link.key())
            {
                Some(existing) => {
                    if crate::roadmap::domain::rfc3339_newer(&link.updated_at, &existing.updated_at)
                    {
                        *existing = link;
                        adopted += 1;
                    }
                }
                None => {
                    self.roadmap.links.push(link);
                    adopted += 1;
                }
            }
        }
        for opt_in in opt_ins {
            match self
                .roadmap
                .tracker_opt_ins
                .iter_mut()
                .find(|o| o.key() == opt_in.key())
            {
                Some(existing) => {
                    if crate::roadmap::domain::rfc3339_newer(
                        &opt_in.updated_at,
                        &existing.updated_at,
                    ) {
                        *existing = opt_in;
                        adopted += 1;
                    }
                }
                None => {
                    self.roadmap.tracker_opt_ins.push(opt_in);
                    adopted += 1;
                }
            }
        }
        if adopted > 0 {
            self.persist();
        }
        adopted
    }

    /// The chunk ids opted in to `provider`, in roadmap order so a projection
    /// run is deterministic.
    #[must_use]
    pub fn chunks_opted_in(&self, provider: &str) -> Vec<&Chunk> {
        let provider = provider.trim().to_ascii_lowercase();
        self.roadmap
            .chunks
            .iter()
            .filter(|c| {
                self.roadmap
                    .tracker_opt_ins
                    .iter()
                    .any(|o| o.chunk_id == c.id && o.provider == provider && o.enabled)
            })
            .collect()
    }

    /// THE DRIFT: active chunks that are NOT opted in to `provider`, in roadmap
    /// order.
    ///
    /// The complement of [`Self::chunks_opted_in`], and it exists because the
    /// count that was reported for a year was the wrong one. `tracker status`
    /// said "46 items included" every day while the roadmap grew to 354 chunks,
    /// and 46 is a perfectly reassuring number — nothing said that 20 active
    /// chunks were invisible. A scope that can silently stop growing needs a
    /// readout of what it is NOT covering, or the next gap is found the same way
    /// this one was: by a human noticing months later that the tracker is stale.
    ///
    /// Done and obsoleted chunks are not drift — they are deliberately out of
    /// scope ([`ChunkStatus::is_active`]).
    #[must_use]
    pub fn chunks_not_opted_in(&self, provider: &str) -> Vec<&Chunk> {
        let provider = provider.trim().to_ascii_lowercase();
        self.roadmap
            .chunks
            .iter()
            .filter(|c| c.status.is_active())
            .filter(|c| {
                !self
                    .roadmap
                    .tracker_opt_ins
                    .iter()
                    .any(|o| o.chunk_id == c.id && o.provider == provider && o.enabled)
            })
            .collect()
    }

    /// Append a datestamped refresh note recording a roadmap mutation and the
    /// think step ids that motivated it. Returns the new note count.
    pub fn record_refresh(&mut self, summary: String, think_steps: Vec<u32>) -> usize {
        self.roadmap.refreshes.push(RefreshNote {
            at: Self::now(),
            summary: summary.clone(),
            think_steps: think_steps.clone(),
        });
        self.persist();
        self.record_event(RoadmapFrame::RefreshRecorded {
            summary,
            think_steps,
        });
        self.roadmap.refreshes.len()
    }

    /// Validate + normalize a wire cross-ref string via [`CrossRef`].
    fn normalize_ref(&self, raw: &str) -> Result<String, String> {
        CrossRef::from_wire(raw)
            .map(|r| r.to_wire())
            .map_err(|e| format!("invalid cross-ref '{raw}': {e}"))
    }

    /// Build a validated [`BlockedBy`], or say exactly what is wrong with it.
    ///
    /// This lives in the engine rather than on the type because
    /// `roadmap::domain` is pure data with no `crate::infra` imports (its module
    /// header states the rule), and [`CrossRef`] is infra. Rather than write a
    /// second parser for a wire format that already has one — which would drift
    /// the moment either changed — the domain stores `evidence` as a String and
    /// this reuses the same `normalize_ref` helper `cross_refs` goes through,
    /// so both fields accept exactly the same vocabulary.
    ///
    /// Two refusals, both about the same failure. A blank `reason` is rejected
    /// because a blocker nobody wrote a reason for is the prose-in-the-title
    /// problem wearing a struct; an unparseable `evidence` is rejected because a
    /// ref that resolves to nothing is worse than the honest `None`.
    ///
    /// Note this VALIDATES and does not store: attaching a blocker to a chunk,
    /// and clearing it again, is `blocked-by-set-and-cleared`. Keeping the
    /// stored-schema change and the write path in separate chunks is what lets
    /// each be verified on its own.
    pub fn validate_blocked_by(
        &self,
        kind: BlockerKind,
        reason: String,
        evidence: Option<String>,
    ) -> Result<BlockedBy, String> {
        if reason.trim().is_empty() {
            return Err(
                "a blocker needs a reason: the point of blocked_by is that nobody has to \
                 re-derive the blocker from the title"
                    .to_string(),
            );
        }
        let evidence = evidence
            .map(|raw| self.normalize_ref(&raw))
            .transpose()
            .map_err(|e| format!("invalid blocker evidence: {e}"))?;
        Ok(BlockedBy {
            kind,
            reason: reason.trim().to_string(),
            evidence,
            blocked_at: Self::now(),
        })
    }

    /// Attach a blocker to a chunk, replacing any blocker already there.
    ///
    /// Validation happens in [`Self::validate_blocked_by`] and is not repeated
    /// here: a caller that has already built a `BlockedBy` has been through the
    /// refusals, and re-checking would be a second place for the rules to drift.
    ///
    /// Replacing rather than refusing an existing blocker is deliberate. The
    /// reason a chunk is stuck changes — a premise that was merely unmet gets
    /// refuted, an external wait becomes a wait on a person — and making the
    /// re-statement of a blocker require a clear first would put a gesture
    /// between a human and the truth, which is how records go stale.
    pub fn set_blocked_by(&mut self, id: &str, blocked_by: BlockedBy) -> Result<&Chunk, String> {
        let idx = self.index_of(id)?;
        let c = &mut self.roadmap.chunks[idx];
        c.blocked_by = Some(blocked_by);
        c.updated_at = Self::now();
        self.persist();
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    /// Retract a chunk's blocker — the half that decides whether the other half
    /// gets used at all.
    ///
    /// Every blocker eventually becomes wrong: the human answers, the tenant
    /// appears, the refuted premise is re-tested and holds. If saying so is
    /// harder than recording it in the first place, the field rots into the
    /// stale prose it was built to replace, so this is a first-class verb and
    /// not a special case of the setter.
    ///
    /// Clearing removes the key entirely rather than storing an emptied husk,
    /// so a chunk that was unblocked is indistinguishable on disk from one that
    /// was never blocked — the property `clearing_leaves_no_trace_on_disk`
    /// asserts exactly that.
    ///
    /// Erring on a chunk with no blocker is loud, matching
    /// [`Self::resolve_title_proposal`]: "cleared" has to mean something
    /// happened, and a caller who believes there is a blocker to retract when
    /// there is none is working from a stale picture worth interrupting.
    pub fn clear_blocked_by(&mut self, id: &str) -> Result<&Chunk, String> {
        let idx = self.index_of(id)?;
        if self.roadmap.chunks[idx].blocked_by.take().is_none() {
            return Err(format!("chunk '{id}' has no blocker to clear"));
        }
        let c = &mut self.roadmap.chunks[idx];
        c.updated_at = Self::now();
        self.persist();
        let snapshot = self.roadmap.chunks[idx].clone();
        self.record_event(RoadmapFrame::ChunkChanged { chunk: snapshot });
        Ok(&self.roadmap.chunks[idx])
    }

    /// Validate `raw`, then push the normalized form onto the chunk's
    /// `cross_refs` unless already present.
    fn push_cross_ref(&mut self, id: &str, raw: &str) -> Result<(), String> {
        let normalized = self.normalize_ref(raw)?;
        let idx = self.index_of(id)?;
        let chunk = &mut self.roadmap.chunks[idx];
        if !chunk.cross_refs.contains(&normalized) {
            chunk.cross_refs.push(normalized);
            chunk.updated_at = Self::now();
            self.persist();
        }
        Ok(())
    }

    /// Whether every dependency of `chunk` is satisfied (exists and is `Done`).
    /// An unknown dependency id is treated as unsatisfied (fail-safe).
    fn deps_satisfied(&self, chunk: &Chunk) -> bool {
        chunk.deps.iter().all(|dep| {
            self.roadmap
                .chunks
                .iter()
                .any(|c| &c.id == dep && c.status == ChunkStatus::Done)
        })
    }

    /// The next chunk to work: the lowest-`priority` `Pending` chunk that
    /// carries no blocker and whose dependencies are all `Done`. Ties break by
    /// insertion order. `None` when nothing is ready.
    ///
    /// Three conditions, and the middle one is the youngest. An unfinished
    /// dependency used to be the only way the scheduler could refuse a chunk,
    /// so anything else it should not pick up had to be hidden by demoting its
    /// status — and a demoted chunk loses the ranking that said how much it
    /// mattered. A [`Chunk::blocked_by`] chunk is SKIPPED, not hidden: it keeps
    /// its priority, its band and its place in [`Self::status`], and is simply
    /// never the answer to "what should I do next".
    pub fn next(&self) -> Option<&Chunk> {
        self.roadmap
            .chunks
            .iter()
            .filter(|c| {
                c.status == ChunkStatus::Pending && c.blocked_by.is_none() && self.deps_satisfied(c)
            })
            .min_by_key(|c| c.priority)
    }

    /// A JSON snapshot: status counts, the next-ready chunk, and the **actionable**
    /// chunk list. Deliberately bounded so the result never exceeds the MCP
    /// tool-output limit on a large roadmap (a real 294-chunk roadmap
    /// serialized to ~120 KB and `roadmap_status` hard-errored, leaving the agent
    /// blind). The list carries only active chunks (backlog/pending/in_progress/
    /// blocked) with truncated titles, capped at `Self::STATUS_LIST_CAP`; done
    /// and obsoleted chunks appear only in `counts` plus a short `recent_done`
    /// tail. Any elision is reported (`omitted_active`, `note`) — never silent.
    pub fn status(&self) -> serde_json::Value {
        use ChunkStatus::*;
        let count = |s: ChunkStatus| self.roadmap.chunks.iter().filter(|c| c.status == s).count();

        let summary = |c: &Chunk| {
            let mut row = serde_json::json!({
                "id": c.id,
                "title": Self::truncate_title(&c.title),
                // The label, beside the truncated sentence. This is the surface
                // that renders every chunk at once, so it is the one that most
                // needs something shorter than a claim to show.
                "name": c.name,
                "status": c.status,
                "priority": c.priority,
                // The band names the number. Additive — `priority` keeps its
                // meaning and its place as the sort key.
                "band": crate::infra::coerce::priority_band(c.priority),
                "deps": c.deps,
                "has_reprioritize_proposal": c.reprioritize.is_some(),
                // Its twin, and it was missing: a status proposal was writable
                // and readable by NOTHING (tracker-status-proposal-unreachable).
                // A proposal a human cannot see is the same defect as a proposal
                // nothing can write — the capability stays false of the product.
                "has_status_proposal": c.status_proposal.is_some(),
                // The third sibling, visible from birth — the projector obeys
                // an open title proposal, so a human must be able to see the
                // thing that is steering their tracker's titles.
                "has_title_proposal": c.title_proposal.is_some(),
            });
            // Why the row carries anything about the blocker at all: a reader
            // building their own frontier from this list (which is what
            // /roadmap-run does) never calls `next`, so without it they would
            // re-derive readiness from status + deps and hand out the very
            // chunk the scheduler refused.
            //
            // Why the KIND and not the `has_blocker: bool` this replaces. The
            // boolean was exactly `blocked_by.is_some()`, so the two could
            // never legally disagree and one of them was a fifth wheel. The
            // kind subsumes it — absent means unblocked — and it answers the
            // question the boolean left open, which matters most precisely
            // here: `title` above is CUT AT `STATUS_TITLE_LEN`, and on a real
            // roadmap the blocker is usually stated in the tail of the
            // sentence, so the truncation destroys the prose and this token is
            // what survives it.
            //
            // Why the reason is not here: it is a sentence, and this is the
            // surface that renders sixty rows at once. The token belongs in
            // the list; the sentence belongs in the record and in the markdown
            // export, both of which carry it.
            //
            // Absent rather than null when there is no blocker: the common row
            // then pays nothing, which is the difference between a field that
            // costs ~20 bytes on every one of sixty rows and one that costs
            // only where there is something to say.
            if let Some(b) = &c.blocked_by {
                row["blocker_kind"] = serde_json::json!(b.kind.as_wire());
            }
            // The workstream, on the surface that renders every chunk at once.
            // Without it a reader rebuilding a focused frontier from this list
            // cannot tell which rows belong to the workstream they are in, and
            // would have to fetch each record to find out — which is the same
            // trap `blocker_kind` above was added to close.
            //
            // Absent rather than null when ungrouped, for the same reason as
            // the blocker: the common row should pay nothing.
            if let Some(g) = &c.group {
                row["group"] = serde_json::json!(g);
            }
            row
        };

        // Active = anything still workable, priority-sorted.
        let mut active: Vec<&Chunk> = self
            .roadmap
            .chunks
            .iter()
            .filter(|c| matches!(c.status, Backlog | Pending | InProgress | Blocked))
            .collect();
        active.sort_by_key(|c| c.priority);
        let active_total = active.len();
        let omitted_active = active_total.saturating_sub(Self::STATUS_LIST_CAP);
        let chunks: Vec<_> = active
            .iter()
            .take(Self::STATUS_LIST_CAP)
            .map(|c| summary(c))
            .collect();

        // A short tail of the most-recently-touched done/obsoleted chunks for
        // context, instead of dumping all of them.
        let mut finished: Vec<&Chunk> = self
            .roadmap
            .chunks
            .iter()
            .filter(|c| matches!(c.status, Done | Obsoleted))
            .collect();
        finished.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        let recent_done: Vec<_> = finished
            .iter()
            .take(Self::STATUS_RECENT_DONE)
            .map(|c| summary(c))
            .collect();

        let mut note = String::new();
        if omitted_active > 0 {
            note = format!(
                "{omitted_active} more active chunk(s) not shown (cap {}); call roadmap_export for the full list.",
                Self::STATUS_LIST_CAP
            );
        }

        // Blockers counted ACROSS the status partition rather than inside it.
        // A blocker-carrying chunk keeps its status — that is the whole point
        // of the skip — so it is still counted in `backlog` or `pending` above,
        // and this cannot be another cell beside them. It has to be its own
        // sub-object, or the reader is back to the situation this chunk exists
        // to end: a disproven chunk and an undecided one indistinguishable
        // because both were counted as backlog.
        //
        // Counted over the ACTIVE board only. `complete_chunk` does not clear
        // `blocked_by`, so a finished chunk can still carry the blocker that
        // once held it up; including it would answer a question nobody asked,
        // because "how much of my board cannot be scheduled" is about work that
        // is still waiting to be done.
        let mut blocked_by = serde_json::Map::new();
        // Seeded from the vocabulary rather than from what happens to be on
        // the board, for two reasons: a kind with no chunks reports 0 instead
        // of vanishing (absence and zero are different answers), and a fifth
        // kind added to `BlockerKind` shows up here without this function
        // being edited to remember it.
        for kind in BlockerKind::ALL {
            blocked_by.insert(kind.as_wire().to_string(), serde_json::json!(0));
        }
        let mut blocked_by_total: u64 = 0;
        for c in active.iter().filter(|c| c.blocked_by.is_some()) {
            let kind = c.blocked_by.as_ref().expect("filtered on is_some").kind;
            let slot = blocked_by
                .get_mut(kind.as_wire())
                .expect("every kind was seeded from BlockerKind::ALL");
            *slot = serde_json::json!(slot.as_u64().unwrap_or(0) + 1);
            blocked_by_total += 1;
        }
        blocked_by.insert("total".to_string(), serde_json::json!(blocked_by_total));

        serde_json::json!({
            "project_id": self.project_id,
            "counts": {
                "backlog": count(Backlog),
                "pending": count(Pending),
                "in_progress": count(InProgress),
                "blocked": count(Blocked),
                "done": count(Done),
                "obsoleted": count(Obsoleted),
                "total": self.roadmap.chunks.len(),
                "blocked_by": blocked_by,
            },
            "next": self.next().map(|c| c.id.clone()),
            "active_total": active_total,
            "omitted_active": omitted_active,
            "chunks": chunks,
            "recent_done": recent_done,
            "note": note,
        })
    }

    pub fn export(&self, format: &str) -> String {
        match format {
            "json" => serde_json::to_string_pretty(&self.roadmap).unwrap_or_default(),
            _ => self.export_markdown(),
        }
    }

    /// Render a ROADMAP.md-shaped markdown *view* of the roadmap: chunks grouped
    /// under status sections, priority-ordered within each. This is a generated
    /// projection of native state — not the source of truth.
    fn export_markdown(&self) -> String {
        use ChunkStatus::*;

        let mut out = format!("# Roadmap — {}\n\n", self.project_id);
        // Preamble: the intro prose carried from the imported source.
        if !self.roadmap.preamble.is_empty() {
            out.push_str(&self.roadmap.preamble);
            out.push_str("\n\n");
        }
        let mut listed: Vec<&Chunk> = self.roadmap.chunks.iter().collect();
        listed.sort_by_key(|c| c.priority);

        // (heading, statuses that fall under it), in display order. Blocked
        // chunks render alongside Pending so the "what's next" view stays whole.
        let sections: [(&str, &[ChunkStatus]); 5] = [
            ("In progress", &[InProgress]),
            ("Pending", &[Pending, Blocked]),
            ("Done", &[Done]),
            ("Backlog", &[Backlog]),
            ("Obsoleted", &[Obsoleted]),
        ];

        for (heading, statuses) in sections {
            let mut wrote_heading = false;
            let mut current_band: Option<&str> = None;
            for c in listed.iter().filter(|c| statuses.contains(&c.status)) {
                if !wrote_heading {
                    out.push_str(&format!("## {heading}\n\n"));
                    wrote_heading = true;
                }
                // Within a status section, break the run of chunks by band.
                // `listed` is already priority-sorted, so bands come out in
                // order and each one starts exactly once.
                let band = crate::infra::coerce::priority_band(c.priority);
                if current_band != Some(band) {
                    if current_band.is_some() {
                        out.push('\n');
                    }
                    out.push_str(&format!("### {band}\n\n"));
                    current_band = Some(band);
                }
                let icon = Self::status_icon(c.status);
                if c.description.is_empty() {
                    out.push_str(&format!("- {icon} **{}** ({})\n", c.title, c.priority));
                } else {
                    out.push_str(&format!(
                        "- {icon} **{}** ({}) — {}\n",
                        c.title, c.priority, c.description
                    ));
                }
                // The name rides as a sub-bullet beside deps and acceptance
                // rather than replacing the bold title, so the export a human
                // reads still leads with the claim and an import round-trip
                // recovers both.
                if !c.name.is_empty() {
                    out.push_str(&format!("  - name: {}\n", c.name));
                }
                if !c.deps.is_empty() {
                    out.push_str(&format!("  - deps: {}\n", c.deps.join(", ")));
                }
                // The blocker, beside deps, because a reader asking "why is
                // this not moving" is asking one question and deps answer only
                // half of it. A generated ROADMAP.md that omits this is a
                // document that quietly deletes its own negative results: a
                // chunk whose premise was DISPROVEN reads identically to one
                // nobody has got to yet.
                //
                // Both the kind and the reason, unlike the status list above,
                // which carries only the token. This view has no per-row budget
                // and no truncation, so the sentence that says why can be here
                // in full — and it is the sentence, not the token, that stops
                // the next reader re-deriving the blocker from scratch.
                if let Some(b) = &c.blocked_by {
                    out.push_str(&format!(
                        "  - blocked by: {} — {}",
                        b.kind.as_wire(),
                        b.reason
                    ));
                    if let Some(ev) = &b.evidence {
                        out.push_str(&format!(" ({ev})"));
                    }
                    out.push('\n');
                }
                for a in &c.acceptance {
                    out.push_str(&format!("  - acceptance: {a}\n"));
                }
                // The hand-written narrative, indented under the chunk.
                for line in c.notes.lines() {
                    out.push_str(&format!("  {line}\n"));
                }
            }
            if wrote_heading {
                out.push('\n');
            }
        }

        // Doc-level note sections (Research notes, Vision, …) preserved verbatim.
        for note in &self.roadmap.notes {
            out.push_str(&format!("## {}\n\n", note.heading));
            if !note.body.is_empty() {
                out.push_str(&note.body);
                out.push_str("\n\n");
            }
        }
        out
    }

    /// Merge an imported source into a **non-empty** roadmap, backfilling
    /// narrative without disturbing reconciled state: existing chunks keep their
    /// `status`/`priority` but gain `notes`/`description` (when blank) and any
    /// new `acceptance` items; new chunks are added; the preamble + note sections
    /// are filled in. Returns `(added, updated)`. This is the safe "reseed" — it
    /// captures notes for projects already imported by an older parser without
    /// reverting hand-made status changes.
    pub fn merge_from_import(
        &mut self,
        imported: crate::roadmap::import::ImportedRoadmap,
    ) -> (usize, usize) {
        if self.roadmap.preamble.is_empty() && !imported.preamble.is_empty() {
            self.roadmap.preamble = imported.preamble;
        }
        let existing_notes: std::collections::HashSet<String> = self
            .roadmap
            .notes
            .iter()
            .map(|n| n.heading.to_ascii_lowercase())
            .collect();
        for n in imported.notes {
            if !existing_notes.contains(&n.heading.to_ascii_lowercase()) {
                self.roadmap.notes.push(n);
            }
        }
        let now = Self::now();
        let (mut added, mut updated) = (0, 0);
        for c in imported.chunks {
            if let Some(existing) = self.roadmap.chunks.iter_mut().find(|x| x.id == c.id) {
                let mut changed = false;
                if existing.notes.is_empty() && !c.notes.is_empty() {
                    existing.notes = c.notes;
                    changed = true;
                }
                if existing.description.is_empty() && !c.description.is_empty() {
                    existing.description = c.description;
                    changed = true;
                }
                for a in c.acceptance {
                    if !existing.acceptance.contains(&a) {
                        existing.acceptance.push(a);
                        changed = true;
                    }
                }
                if changed {
                    existing.updated_at = now.clone();
                    updated += 1;
                }
            } else {
                let name = c.resolved_name();
                self.roadmap.chunks.push(Chunk {
                    tier: None,
                    id: c.id,
                    title: c.title,
                    name,
                    status: c.status,
                    priority: c.priority,
                    description: c.description,
                    content: None,
                    notes: c.notes,
                    group: None,
                    acceptance: c.acceptance,
                    deps: c.deps,
                    cross_refs: Vec::new(),
                    shared: false,
                    reprioritize: None,
                    status_proposal: None,
                    title_proposal: None,
                    obsoleted_reason: None,
                    blocked_by: None,
                    project_id: Some(self.project_id.clone()),
                    created_at: now.clone(),
                    updated_at: now.clone(),
                });
                added += 1;
            }
        }
        self.persist();
        (added, updated)
    }

    /// Seed the roadmap from an imported source (Phase: richer-export). Sets the
    /// `preamble` if empty, appends new note sections (dedup by heading), and
    /// adds chunks whose id is new — carrying their `notes`. Persists once.
    /// Returns the number of chunks added.
    pub fn seed_from_import(&mut self, imported: crate::roadmap::import::ImportedRoadmap) -> usize {
        if self.roadmap.preamble.is_empty() && !imported.preamble.is_empty() {
            self.roadmap.preamble = imported.preamble;
        }
        let existing_notes: std::collections::HashSet<String> = self
            .roadmap
            .notes
            .iter()
            .map(|n| n.heading.to_ascii_lowercase())
            .collect();
        for n in imported.notes {
            if !existing_notes.contains(&n.heading.to_ascii_lowercase()) {
                self.roadmap.notes.push(n);
            }
        }
        let now = Self::now();
        let mut added = 0;
        for c in imported.chunks {
            if self.roadmap.chunks.iter().any(|x| x.id == c.id) {
                continue;
            }
            let name = c.resolved_name();
            self.roadmap.chunks.push(Chunk {
                tier: None,
                id: c.id,
                title: c.title,
                name,
                status: c.status,
                priority: c.priority,
                description: c.description,
                content: None,
                notes: c.notes,
                group: None,
                acceptance: c.acceptance,
                deps: c.deps,
                cross_refs: Vec::new(),
                shared: false,
                reprioritize: None,
                status_proposal: None,
                title_proposal: None,
                obsoleted_reason: None,
                blocked_by: None,
                project_id: Some(self.project_id.clone()),
                created_at: now.clone(),
                updated_at: now.clone(),
            });
            added += 1;
        }
        self.persist();
        added
    }

    /// The most ids [`Self::get`] will answer in one call.
    ///
    /// DERIVED FROM A MEASUREMENT of this project's own board, not chosen for
    /// roundness — serializing all 570 stored chunks gives mean 2,158 B,
    /// median 1,840, p90 3,499, max 6,885. What matters is the WORST case a
    /// caller can ask for, against the ~25,000-token tool-result limit that is
    /// the reason this tool exists at all:
    ///
    /// ```text
    /// n=15   76,948 B  (~19.2k tok)
    /// n=20   97,469 B  (~24.4k tok)   <- the largest N that still fits
    /// n=25  117,487 B  (~29.4k tok)   OVER
    /// ```
    ///
    /// So 20 is the answer the measurement gives. A typical 20 is 43,173 B
    /// (~10.8k tok); the cap binds on the pathological call, which is what a
    /// cap is for. Re-measure before moving it: the number is a property of
    /// how fat the records have grown, not a preference.
    ///
    /// # Why the cap counts DISTINCT ids, and why that is not a detail
    ///
    /// The table above is the N largest *distinct* records. The first version
    /// of this code answered a repeated id twice, and a live probe against the
    /// real store showed what that costs: naming the single largest record 20
    /// times returned **137,772 B (~34.4k tok)** — 41% past the very bound this
    /// cap was derived from. The worst case was never `sum of the top N`, it
    /// was `N × max`, and picking the wrong extreme instance is how a measured
    /// gate ends up guarding a number nothing enforces.
    ///
    /// Deduplicating is what makes the derivation true, so `ids` is a SET: the
    /// cap is checked against the distinct count and each record is answered
    /// once. That is also the more honest reading of the request — asking for
    /// the same record twice is asking for one record — and it is stated in the
    /// tool description rather than left for a caller to discover.
    pub const GET_ID_CAP: usize = 20;

    /// Fetch the FULL stored record for a named set of chunk ids, optionally
    /// projected to a subset of fields.
    ///
    /// The gap this fills: `status()` returns every chunk as one row with the
    /// title cut at `STATUS_TITLE_LEN` (100 chars) and no description, content
    /// or acceptance, while `export()` returns everything — 1.5 MB on this board,
    /// too large to be a tool result at all. Reading three records had no
    /// verb between those two.
    ///
    /// # Every refusal here is a refusal to answer silently
    ///
    /// The three ways this call can go wrong fail the same way if handled
    /// casually: they return a plausible response that is quietly incomplete,
    /// which is worse than an error because the caller acts on it.
    ///
    /// * More ids than the cap → an error naming both numbers. Truncating to
    ///   the first N would read to the caller as "those chunks do not exist".
    /// * An id that matches nothing → reported in `unknown`, never dropped.
    ///   Omission would make "no such chunk" and "that chunk has no summary"
    ///   the same answer.
    /// * A field outside [`Chunk::PROJECTABLE_FIELDS`] → an error naming the
    ///   field and the vocabulary. JSON:API, the prior art for `fields`,
    ///   explicitly permits EITHER rejecting or ignoring an unrecognised name
    ///   (its RFC says a server "MAY respond with a 400"), so this is a choice
    ///   and not an inheritance: ignoring it would hand back a response
    ///   projected to nothing but `id`, indistinguishable from a board where
    ///   every named chunk is genuinely bare.
    ///
    /// `id` survives every projection, as it does in JSON:API — a record you
    /// cannot attribute to a chunk is not a record.
    pub fn get(&self, ids: &[String], fields: &[String]) -> Result<serde_json::Value, String> {
        if ids.is_empty() {
            return Err(
                "ids must name at least one chunk — roadmap_status lists the ids, \
                 roadmap_export dumps every record"
                    .to_string(),
            );
        }
        // `ids` is a SET. Deduplicating here rather than at the end is what
        // makes `GET_ID_CAP`'s derivation true — see the constant, and the live
        // probe that caught the difference between `sum of the top N` and
        // `N × max`. First-occurrence order is kept so the response still
        // reads in the order the caller wrote.
        let mut distinct: Vec<&String> = Vec::with_capacity(ids.len());
        for id in ids {
            if !distinct.iter().any(|seen| *seen == id.as_str()) {
                distinct.push(id);
            }
        }
        if distinct.len() > Self::GET_ID_CAP {
            return Err(format!(
                "ids names {} distinct chunks, over the cap of {} — split the call \
                 rather than expecting a short answer, because a truncated response \
                 reads as 'those chunks do not exist'",
                distinct.len(),
                Self::GET_ID_CAP,
            ));
        }
        for f in fields {
            if !Chunk::PROJECTABLE_FIELDS.contains(&f.as_str()) {
                return Err(format!(
                    "unknown field '{f}' — roadmap_get projects one of: {}",
                    Chunk::PROJECTABLE_FIELDS.join(", "),
                ));
            }
        }

        let mut records = Vec::new();
        let mut unknown = Vec::new();
        for id in distinct {
            match self.roadmap.chunks.iter().find(|c| c.id == *id) {
                Some(chunk) => records.push(Self::project(chunk, fields)),
                None => unknown.push(id.clone()),
            }
        }

        Ok(serde_json::json!({
            "records": records,
            "returned": records.len(),
            "unknown": unknown,
            // Echoed so a caller can see WHICH projection produced this shape.
            // An empty list means the whole record, and saying so beats leaving
            // the reader to infer it from the keys that happen to be present.
            "fields": fields,
        }))
    }

    /// Serialize one chunk, keeping only `fields` (plus `id`, always). An
    /// empty `fields` is the whole record — JSON:API's own default.
    fn project(chunk: &Chunk, fields: &[String]) -> serde_json::Value {
        let mut value = serde_json::to_value(chunk).unwrap_or(serde_json::Value::Null);
        if fields.is_empty() {
            return value;
        }
        if let Some(map) = value.as_object_mut() {
            map.retain(|k, _| k == "id" || fields.iter().any(|f| f == k));
        }
        value
    }

    /// Max active chunks listed by `status()` before the rest are summarized as a
    /// count. Keeps the snapshot bounded regardless of roadmap size.
    const STATUS_LIST_CAP: usize = 60;
    /// How many recently-finished chunks `status()` includes for context.
    const STATUS_RECENT_DONE: usize = 8;
    /// Max title length echoed in a `status()` summary row.
    const STATUS_TITLE_LEN: usize = 100;

    /// Truncate a title to [`Self::STATUS_TITLE_LEN`] chars for the status
    /// snapshot, appending `…` when cut. A defensive cap independent of the
    /// import-side [`crate::roadmap::import::headline`] split, so even
    /// hand-entered long titles can't bloat the snapshot.
    fn truncate_title(title: &str) -> String {
        if title.chars().count() <= Self::STATUS_TITLE_LEN {
            return title.to_string();
        }
        let cut: String = title.chars().take(Self::STATUS_TITLE_LEN).collect();
        format!("{}…", cut.trim_end())
    }

    /// Checkbox-style status glyph for the markdown projection.
    fn status_icon(status: ChunkStatus) -> &'static str {
        match status {
            ChunkStatus::Done => "[x]",
            ChunkStatus::InProgress => "[~]",
            ChunkStatus::Blocked => "[!]",
            ChunkStatus::Obsoleted => "[-]",
            ChunkStatus::Backlog | ChunkStatus::Pending => "[ ]",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::{Domain, Persistence, PersistenceConfig};
    use crate::roadmap::domain::merge_roadmaps;
    use tempfile::TempDir;

    fn engine() -> RoadmapEngine {
        RoadmapEngine::new("proj".into())
    }

    fn add(e: &mut RoadmapEngine, id: &str, priority: u32, deps: Vec<String>) {
        e.add_chunk(
            id.into(),
            format!("Chunk {id}"),
            ChunkStatus::Pending,
            priority,
            String::new(),
            Vec::new(),
            deps,
            false,
        )
        .unwrap();
    }

    /// The seam, not the diagnostic. A region named after an id prefix used to
    /// be written without complaint and objected to hours later by `doctor`,
    /// which by then could not say who had written it or why.
    #[test]
    fn set_group_refuses_a_name_that_repeats_a_slug() {
        let mut e = engine();
        add(&mut e, "berth-allocation-races", 1, vec![]);
        add(&mut e, "berth-draft-limits", 2, vec![]);

        let err = e
            .set_group("berth-allocation-races", Some("berth".into()))
            .expect_err("a slug is not a place");
        assert!(
            err.contains("berth"),
            "the caller must be told which: {err}"
        );
        assert!(
            e.roadmap.chunks.iter().all(|c| c.group.is_none()),
            "a refused name must not be written"
        );

        // Capitalizing it is the same failure, and so is using a piece of it.
        assert!(
            e.set_group("berth-draft-limits", Some("Berth".into()))
                .is_err()
        );
        assert!(
            e.set_group("berth-draft-limits", Some("bert".into()))
                .is_err()
        );

        // A real place is accepted, and so is the one the code itself names.
        e.set_group("berth-draft-limits", Some("Quayside".into()))
            .expect("a place name is fine");
        e.set_group("berth-allocation-races", Some(region::UNPLACED.into()))
            .expect("the unplaced region must survive its own gate");
    }

    /// Clearing is not naming, so it cannot fail the name check.
    #[test]
    fn set_group_still_clears() {
        let mut e = engine();
        add(&mut e, "berth-draft-limits", 1, vec![]);
        e.set_group("berth-draft-limits", Some("Quayside".into()))
            .unwrap();
        let cleared = e.set_group("berth-draft-limits", None).unwrap();
        assert!(cleared.group.is_none());
    }

    /// The seeder's replacement: it groups and it does not name, and nothing it
    /// returns has been written to a chunk.
    #[test]
    fn proposing_groups_places_nothing() {
        let mut e = engine();
        for id in [
            "berth-allocation-races",
            "berth-draft-limits",
            "berth-pilot-handoff",
            "crane-idle-telemetry",
        ] {
            add(&mut e, id, 1, vec![]);
        }

        let proposed = e.propose_groups_from_ids(3);
        assert_eq!(proposed.len(), 1);
        assert_eq!(proposed[0].prefix, "berth");
        assert!(proposed[0].why_prefix_is_unfit.contains("berth"));
        assert!(
            e.roadmap.chunks.iter().all(|c| c.group.is_none()),
            "proposing must place nothing"
        );
    }

    #[test]
    fn add_chunk_rejects_duplicate_id() {
        let mut e = engine();
        add(&mut e, "a", 1, vec![]);
        let err = e
            .add_chunk(
                "a".into(),
                "dup".into(),
                ChunkStatus::Pending,
                2,
                String::new(),
                vec![],
                vec![],
                false,
            )
            .unwrap_err();
        assert!(err.contains("already exists"));
    }

    #[test]
    fn set_status_enforces_transition_table() {
        let mut e = engine();
        add(&mut e, "a", 1, vec![]);
        e.set_status("a", ChunkStatus::InProgress).unwrap();
        e.set_status("a", ChunkStatus::Done).unwrap();
        // Done -> Obsoleted is forbidden.
        let err = e.set_status("a", ChunkStatus::Obsoleted).unwrap_err();
        assert!(err.contains("illegal transition"));
    }

    #[test]
    fn next_picks_lowest_priority_ready_pending() {
        let mut e = engine();
        add(&mut e, "low", 30, vec![]);
        add(&mut e, "high", 10, vec![]);
        add(&mut e, "mid", 20, vec![]);
        assert_eq!(e.next().unwrap().id, "high");
    }

    #[test]
    fn next_skips_chunk_with_unsatisfied_dep() {
        let mut e = engine();
        add(&mut e, "blocker", 1, vec![]); // pending, not done
        add(&mut e, "dependent", 5, vec!["blocker".into()]);
        // dependent has lower priority would-be pick at idx but its dep isn't Done;
        // blocker (priority 1) is the ready pick instead.
        assert_eq!(e.next().unwrap().id, "blocker");
        // Now finish the blocker; dependent becomes ready.
        e.set_status("blocker", ChunkStatus::InProgress).unwrap();
        e.set_status("blocker", ChunkStatus::Done).unwrap();
        assert_eq!(e.next().unwrap().id, "dependent");
    }

    #[test]
    fn unknown_dep_is_treated_as_unsatisfied() {
        let mut e = engine();
        add(&mut e, "a", 1, vec!["ghost".into()]);
        assert!(e.next().is_none());
    }

    #[test]
    fn propose_reprioritize_does_not_change_priority() {
        let mut e = engine();
        add(&mut e, "a", 50, vec![]);
        e.propose_reprioritize("a", 1, "should be first".into())
            .unwrap();
        let c = &e.roadmap().chunks[0];
        assert_eq!(c.priority, 50, "real priority must be untouched");
        assert_eq!(c.reprioritize.as_ref().unwrap().suggested_priority, 1);
        // The proposal is surfaced but does not reorder next().
        add(&mut e, "b", 10, vec![]);
        assert_eq!(e.next().unwrap().id, "b");
    }

    #[test]
    fn obsolete_records_reason() {
        let mut e = engine();
        add(&mut e, "a", 1, vec![]);
        e.obsolete_chunk("a", "overtaken by events".into()).unwrap();
        let c = &e.roadmap().chunks[0];
        assert_eq!(c.status, ChunkStatus::Obsoleted);
        assert_eq!(c.obsoleted_reason.as_deref(), Some("overtaken by events"));
    }

    // ── proposal disposal (tracker-status-proposal-disposal) ──────────────

    /// The defect this test was written against: a human ACCEPTS a status
    /// suggestion by transitioning to it, and the proposal used to survive —
    /// `has_status_proposal` stayed true in every status view forever.
    #[test]
    fn a_transition_disposes_the_status_proposal_either_way() {
        let mut e = engine();
        add(&mut e, "a", 1, vec![]);

        // Differing transition: the human moved the field somewhere else, so
        // the suggestion is superseded.
        e.propose_status("a", ChunkStatus::Done, "ticket closed".into(), "t".into())
            .unwrap();
        assert!(e.roadmap().chunks[0].status_proposal.is_some());
        e.set_status("a", ChunkStatus::InProgress).unwrap();
        assert!(
            e.roadmap().chunks[0].status_proposal.is_none(),
            "a differing transition must dispose the proposal — the human \
             acted on the field, which is all a proposal ever asks for"
        );

        // And the status view stops lying (asserted while the chunk is still
        // active — done chunks leave the reported list entirely).
        let status = e.status();
        let seen = status["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == "a")
            .expect("an in-progress chunk is listed");
        assert_eq!(
            seen["has_status_proposal"], false,
            "the disposal must reach the reporting surface"
        );

        // Matching transition: accepting the suggestion IS the disposal.
        e.propose_status("a", ChunkStatus::Done, "ticket closed".into(), "t".into())
            .unwrap();
        e.set_status("a", ChunkStatus::Done).unwrap();
        assert!(
            e.roadmap().chunks[0].status_proposal.is_none(),
            "accepting a suggestion by transitioning to it must dispose it"
        );
    }

    #[test]
    fn obsoleting_disposes_the_status_proposal_too() {
        // The one status write outside set_status must obey the same rule.
        let mut e = engine();
        add(&mut e, "a", 1, vec![]);
        e.propose_status("a", ChunkStatus::Done, "r".into(), "s".into())
            .unwrap();
        e.obsolete_chunk("a", "overtaken".into()).unwrap();
        assert!(e.roadmap().chunks[0].status_proposal.is_none());
    }

    /// Disposal is FIELD-SCOPED: a transition says nothing about the title,
    /// and a contested title may be actively honored by the projector.
    #[test]
    fn a_transition_leaves_the_title_proposal_alone() {
        let mut e = engine();
        add(&mut e, "a", 1, vec![]);
        e.propose_title("a", "Their name".into(), "r".into(), "s".into())
            .unwrap();
        e.set_status("a", ChunkStatus::InProgress).unwrap();
        assert!(
            e.roadmap().chunks[0].title_proposal.is_some(),
            "a status act must not dispose a TITLE proposal — clearing it \
             would silently drop a concession the projector is honoring"
        );
    }

    /// The reprioritize sibling of the update_chunk title-edit rule: an
    /// explicit differing priority edit disposes the proposal (setting the
    /// suggested value is exactly how a human accepts it); an edit that
    /// touches other fields, or restates the current priority, does not.
    #[test]
    fn a_differing_priority_edit_disposes_a_reprioritize_proposal() {
        let mut e = engine();
        add(&mut e, "a", 50, vec![]);
        e.propose_reprioritize("a", 10, "should be earlier".into())
            .unwrap();

        // A description edit is not about the priority field.
        e.update_chunk("a", None, None, Some("new words".into()), None, None, None)
            .unwrap();
        assert!(e.roadmap().chunks[0].reprioritize.is_some());

        // Restating the current priority is not a decision about the proposal.
        e.update_chunk("a", None, Some(50), None, None, None, None)
            .unwrap();
        assert!(e.roadmap().chunks[0].reprioritize.is_some());

        // Accepting it — an explicit edit to the suggested value — disposes.
        e.update_chunk("a", None, Some(10), None, None, None, None)
            .unwrap();
        let c = &e.roadmap().chunks[0];
        assert_eq!(c.priority, 10);
        assert!(
            c.reprioritize.is_none(),
            "an explicit priority edit must dispose the reprioritize proposal"
        );
    }

    /// The third sibling gains the no-restamp guard the other two always had:
    /// an unchanged suggestion must not make an old disagreement look new.
    #[test]
    fn an_unchanged_reprioritize_suggestion_does_not_restamp() {
        let mut e = engine();
        add(&mut e, "a", 50, vec![]);
        e.propose_reprioritize("a", 10, "should be earlier".into())
            .unwrap();
        let first = e.roadmap().chunks[0]
            .reprioritize
            .as_ref()
            .unwrap()
            .proposed_at
            .clone();
        e.propose_reprioritize("a", 10, "should be earlier".into())
            .unwrap();
        let second = e.roadmap().chunks[0]
            .reprioritize
            .as_ref()
            .unwrap()
            .proposed_at
            .clone();
        assert_eq!(first, second, "an unchanged suggestion was restamped");

        // A different suggestion IS a new disagreement and replaces it.
        e.propose_reprioritize("a", 5, "even earlier".into())
            .unwrap();
        assert_eq!(
            e.roadmap().chunks[0]
                .reprioritize
                .as_ref()
                .unwrap()
                .suggested_priority,
            5
        );
    }

    #[test]
    fn status_counts_and_next_are_reported() {
        let mut e = engine();
        add(&mut e, "a", 1, vec![]);
        add(&mut e, "b", 2, vec![]);
        e.set_status("b", ChunkStatus::InProgress).unwrap();
        let s = e.status();
        assert_eq!(s["counts"]["total"], 2);
        assert_eq!(s["counts"]["pending"], 1);
        assert_eq!(s["counts"]["in_progress"], 1);
        assert_eq!(s["next"], "a");
    }

    #[test]
    fn status_is_bounded_on_a_large_roadmap() {
        let mut e = engine();
        // 200 done chunks + 100 active — the shape that blew the output limit.
        for i in 0..200 {
            add(&mut e, &format!("done-{i}"), i, vec![]);
            e.set_status(&format!("done-{i}"), ChunkStatus::InProgress)
                .unwrap();
            e.set_status(&format!("done-{i}"), ChunkStatus::Done)
                .unwrap();
        }
        for i in 0..100 {
            add(&mut e, &format!("act-{i}"), 1000 + i, vec![]);
        }
        let s = e.status();
        // Counts still reflect the whole roadmap.
        assert_eq!(s["counts"]["total"], 300);
        assert_eq!(s["counts"]["done"], 200);
        // The listed chunks are active-only and capped.
        let listed = s["chunks"].as_array().unwrap();
        assert_eq!(listed.len(), RoadmapEngine::STATUS_LIST_CAP);
        assert!(listed.iter().all(|c| c["status"] == "pending"));
        assert_eq!(s["active_total"], 100);
        assert_eq!(s["omitted_active"], 100 - RoadmapEngine::STATUS_LIST_CAP);
        assert!(s["note"].as_str().unwrap().contains("not shown"));
        // A bounded tail of finished work, never the full 200.
        assert_eq!(
            s["recent_done"].as_array().unwrap().len(),
            RoadmapEngine::STATUS_RECENT_DONE
        );
        // Whole snapshot stays small — the regression guard.
        let bytes = serde_json::to_string(&s).unwrap().len();
        assert!(bytes < 16_000, "status snapshot too large: {bytes} bytes");
    }

    #[test]
    fn status_truncates_long_titles() {
        let mut e = engine();
        let long = "x".repeat(500);
        e.add_chunk(
            "big".into(),
            long,
            ChunkStatus::Pending,
            1,
            String::new(),
            Vec::new(),
            Vec::new(),
            false,
        )
        .unwrap();
        let s = e.status();
        let title = s["chunks"][0]["title"].as_str().unwrap();
        assert!(title.chars().count() <= RoadmapEngine::STATUS_TITLE_LEN + 1);
        assert!(title.ends_with('…'));
    }

    #[test]
    fn persistence_round_trips_across_engine_instances() {
        let tmp = TempDir::new().unwrap();
        let cfg = PersistenceConfig::from_env()
            .with_data_dir(tmp.path().to_path_buf())
            .enabled(true);

        {
            let mut e = RoadmapEngine::new("proj".into())
                .with_persistence(Persistence::new(&cfg, Domain::Roadmap));
            add(&mut e, "a", 1, vec![]);
            e.set_status("a", ChunkStatus::InProgress).unwrap();
        }

        let e2 = RoadmapEngine::new("proj".into())
            .with_persistence(Persistence::new(&cfg, Domain::Roadmap));
        assert_eq!(e2.roadmap().chunks.len(), 1);
        assert_eq!(e2.roadmap().chunks[0].status, ChunkStatus::InProgress);
    }

    // ── Focus ──────────────────────────────────────────────────────────

    /// Put `id` in `group`, bypassing `set_group`'s region-name rules — those
    /// are that verb's business, and a focus test that had to satisfy them
    /// would be testing region naming instead of focus.
    fn grouped(e: &mut RoadmapEngine, id: &str, priority: u32, group: &str) {
        add(e, id, priority, vec![]);
        e.roadmap
            .chunks
            .iter_mut()
            .find(|c| c.id == id)
            .expect("just added")
            .group = Some(group.to_string());
    }

    /// THE concurrency property, and the reason focus is a vector rather than a
    /// field: two lanes hold different workstreams at once, and neither call
    /// disturbs the other's record. A project-global focus fails this on the
    /// second `focus_set` — which is why the assertion is written against both
    /// lanes AFTER both writes, not against each one as it lands.
    #[test]
    fn two_lanes_focus_different_groups_without_interference() {
        let mut e = engine();
        grouped(&mut e, "auth-1", 10, "Authentication");
        grouped(&mut e, "bill-1", 20, "Billing");

        e.focus_set("/work/tree-a", "Authentication", FocusMode::Build)
            .unwrap();
        e.focus_set("/work/tree-b", "Billing", FocusMode::Shape)
            .unwrap();

        let a = e.focus_get("/work/tree-a").expect("lane a is focused");
        let b = e.focus_get("/work/tree-b").expect("lane b is focused");
        assert_eq!(
            (a.group.as_str(), a.mode),
            ("Authentication", FocusMode::Build)
        );
        assert_eq!((b.group.as_str(), b.mode), ("Billing", FocusMode::Shape));
        assert_eq!(e.roadmap().focuses.len(), 2, "one record per lane");

        // Re-focusing one lane still leaves the other alone.
        e.focus_set("/work/tree-a", "Billing", FocusMode::Listen)
            .unwrap();
        assert_eq!(e.focus_get("/work/tree-b").unwrap().group, "Billing");
        assert_eq!(e.focus_get("/work/tree-b").unwrap().mode, FocusMode::Shape);
        assert_eq!(
            e.roadmap().focuses.len(),
            2,
            "re-focus updates, never appends"
        );
    }

    /// THE DEFECT THIS FIXES, stated as the symptom that was actually reported:
    /// chunks created for a workstream landed ungrouped and were invisible to
    /// every focused read, so a focused agent saw an empty frontier and had no
    /// way to tell that from a workstream with genuinely nothing ready.
    #[test]
    fn a_chunk_born_with_a_group_is_immediately_visible_to_the_focused_frontier() {
        let mut e = engine();
        e.add_chunk_with_content(
            "auth-1".into(),
            "Rotate sessions".into(),
            String::new(),
            ChunkStatus::Pending,
            10,
            String::new(),
            vec![],
            vec![],
            false,
            None,
            Some("Authentication".into()),
        )
        .expect("created");

        // One call, not two. Before this, the chunk existed and the frontier
        // was empty until a separate set_group landed.
        assert_eq!(
            e.next_in_group("Authentication").map(|c| c.id.as_str()),
            Some("auth-1")
        );
        assert_eq!(e.group_status("Authentication")["ready_count"], 1);
        assert_eq!(e.groups(), vec!["Authentication"]);

        // And it is reachable by focusing, which is the path that matters.
        e.focus_set("lane", "Authentication", FocusMode::Build)
            .unwrap();
        assert_eq!(e.focus_get("lane").unwrap().group, "Authentication");
    }

    /// Omitting the group still works, and blank is the same as omitted — an
    /// ungrouped chunk is a legitimate answer, not a gap to fill.
    #[test]
    fn a_chunk_born_without_a_group_is_ungrouped_and_blank_means_the_same() {
        let mut e = engine();
        for (id, group) in [("a", None), ("b", Some("   ".to_string()))] {
            e.add_chunk_with_content(
                id.into(),
                "t".into(),
                String::new(),
                ChunkStatus::Pending,
                10,
                String::new(),
                vec![],
                vec![],
                false,
                None,
                group,
            )
            .expect("created");
        }
        assert!(e.roadmap().chunks.iter().all(|c| c.group.is_none()));
        assert!(e.groups().is_empty());
    }

    /// Both doors into grouping enforce the SAME rule, so neither is a backdoor
    /// around the other — and the birth door refuses the whole call rather than
    /// silently dropping the group, because a chunk that is ungrouped for an
    /// unstated reason is exactly the defect being fixed.
    #[test]
    fn an_unusable_group_is_refused_at_birth_and_creates_nothing() {
        let mut e = engine();
        // "Billing" repeats the id prefix of `billing-1`, which names a slug
        // rather than a place. set_group has always refused this.
        let err = e
            .add_chunk_with_content(
                "billing-1".into(),
                "t".into(),
                String::new(),
                ChunkStatus::Pending,
                10,
                String::new(),
                vec![],
                vec![],
                false,
                None,
                Some("Billing".into()),
            )
            .expect_err("an unusable group must refuse");
        assert!(
            err.contains("billing"),
            "the error must name the clash: {err}"
        );
        assert!(
            e.roadmap().chunks.is_empty(),
            "a refused group must not leave a chunk behind — least of all an ungrouped one"
        );

        // The same name, via the other door, is refused identically.
        add(&mut e, "billing-1", 10, vec![]);
        let err2 = e
            .set_group("billing-1", Some("Billing".into()))
            .expect_err("set_group refuses it too");
        assert_eq!(
            err.trim_end_matches('.'),
            err2.trim_end_matches('.'),
            "the two doors must give the same refusal, or one is a backdoor"
        );
    }

    /// A lane that has focused nothing is answered honestly rather than being
    /// handed somebody else's focus.
    #[test]
    fn an_unfocused_lane_has_no_focus() {
        let mut e = engine();
        grouped(&mut e, "auth-1", 10, "Authentication");
        e.focus_set("lane-a", "Authentication", FocusMode::Build)
            .unwrap();
        assert!(e.focus_get("lane-b").is_none());
    }

    /// The refusal that keeps focus per-caller. A blank lane must NOT become a
    /// shared default — that is the project-global bug wearing a per-lane type.
    #[test]
    fn a_blank_lane_is_refused_with_a_recipe_rather_than_defaulted() {
        let mut e = engine();
        grouped(&mut e, "auth-1", 10, "Authentication");

        for blank in ["", "   ", "\t"] {
            let err = e
                .focus_set(blank, "Authentication", FocusMode::Build)
                .expect_err("a blank lane must be refused");
            assert!(err.contains("a lane is required"), "unhelpful error: {err}");
            assert!(
                err.contains("worktree") && err.contains("session"),
                "the refusal must say how to produce a real lane: {err}"
            );
        }
        assert!(
            e.roadmap().focuses.is_empty(),
            "a refused focus must write nothing"
        );
        // And nothing sneaks in under a whitespace key either.
        assert!(e.focus_get("").is_none());
    }

    #[test]
    fn an_over_budget_or_control_character_lane_is_refused() {
        let mut e = engine();
        grouped(&mut e, "auth-1", 10, "Authentication");
        let long = "x".repeat(crate::roadmap::domain::LANE_BUDGET + 1);
        assert!(
            e.focus_set(&long, "Authentication", FocusMode::Build)
                .is_err()
        );
        assert!(
            e.focus_set("lane\nb", "Authentication", FocusMode::Build)
                .is_err()
        );
        assert!(e.roadmap().focuses.is_empty());
    }

    /// An unknown group changes nothing AND hands back the real candidates.
    #[test]
    fn an_unknown_group_mutates_nothing_and_lists_what_exists() {
        let mut e = engine();
        grouped(&mut e, "auth-1", 10, "Authentication");
        grouped(&mut e, "bill-1", 20, "Billing");
        e.focus_set("lane", "Authentication", FocusMode::Build)
            .unwrap();

        let err = e
            .focus_set("lane", "Payments", FocusMode::Build)
            .expect_err("unknown group must refuse");
        assert!(err.contains("Authentication") && err.contains("Billing"));
        assert!(err.contains("Focus is unchanged"));
        // The prior focus survived the failed switch untouched.
        let f = e.focus_get("lane").unwrap();
        assert_eq!(
            (f.group.as_str(), f.mode),
            ("Authentication", FocusMode::Build)
        );
    }

    /// An ambiguous fragment refuses too, and names exactly the candidates —
    /// picking the "closest" is how a caller ends up in a workstream it never
    /// named.
    #[test]
    fn an_ambiguous_group_fragment_mutates_nothing_and_lists_the_candidates() {
        let mut e = engine();
        grouped(&mut e, "a1", 10, "Billing core");
        grouped(&mut e, "b1", 20, "Billing reports");

        match e.resolve_group("billing") {
            GroupResolution::Ambiguous(names) => {
                assert_eq!(names, vec!["Billing core", "Billing reports"]);
            }
            other => panic!("expected ambiguity, got {other:?}"),
        }
        let err = e
            .focus_set("lane", "billing", FocusMode::Build)
            .expect_err("ambiguous group must refuse");
        assert!(err.contains("ambiguous"));
        assert!(e.roadmap().focuses.is_empty());
    }

    /// The three resolution paths, in decreasing certainty.
    #[test]
    fn a_group_resolves_by_exact_name_by_case_and_by_unambiguous_fragment() {
        let mut e = engine();
        grouped(&mut e, "a1", 10, "Authentication and identity");
        grouped(&mut e, "b1", 20, "Billing");

        assert_eq!(
            e.resolve_group("Authentication and identity").exact(),
            Some("Authentication and identity")
        );
        assert_eq!(
            e.resolve_group("billing").exact(),
            Some("Billing"),
            "case-insensitive exact match"
        );
        assert_eq!(
            e.resolve_group("identity").exact(),
            Some("Authentication and identity"),
            "an unambiguous fragment resolves"
        );
        // The resolved value is the STORED spelling, not what was typed.
        let f = e.focus_set("lane", "identity", FocusMode::Shape).unwrap();
        assert_eq!(f.group, "Authentication and identity");
    }

    /// Focusing is not starting. Nothing about any chunk moves.
    #[test]
    fn focusing_changes_no_chunk_status() {
        let mut e = engine();
        grouped(&mut e, "auth-1", 10, "Authentication");
        let before = e.roadmap().chunks.clone();
        e.focus_set("lane", "Authentication", FocusMode::Build)
            .unwrap();
        e.focus_set("lane", "Authentication", FocusMode::Listen)
            .unwrap();
        e.focus_clear("lane");
        assert_eq!(e.roadmap().chunks, before, "focus must not touch chunks");
    }

    /// `next_in_group` cannot escape its workstream, and says nothing rather
    /// than something from elsewhere.
    #[test]
    fn next_in_group_never_returns_a_chunk_from_another_group() {
        let mut e = engine();
        // Deliberately higher priority (lower number) OUTSIDE the group, so a
        // naive implementation that forgot the filter would return this one.
        grouped(&mut e, "bill-1", 1, "Billing");
        grouped(&mut e, "auth-1", 500, "Authentication");
        add(&mut e, "ungrouped", 2, vec![]);

        assert_eq!(e.next().map(|c| c.id.as_str()), Some("bill-1"));
        assert_eq!(
            e.next_in_group("Authentication").map(|c| c.id.as_str()),
            Some("auth-1")
        );
        // An empty workstream is an empty answer, never a fallthrough.
        assert!(e.next_in_group("Payments").is_none());
    }

    /// A group whose only chunk is blocked reports it as blocked rather than
    /// ready, and next_in_group refuses it.
    #[test]
    fn group_status_counts_readiness_blockers_and_unmet_deps() {
        let mut e = engine();
        grouped(&mut e, "one", 10, "Auth");
        grouped(&mut e, "two", 20, "Auth");
        e.roadmap
            .chunks
            .iter_mut()
            .find(|c| c.id == "two")
            .unwrap()
            .deps = vec!["one".into()];

        let s = e.group_status("Auth");
        assert_eq!(s["ready_count"], 1, "only `one` is ready");
        assert_eq!(s["next"], "one");
        assert_eq!(s["blocked_count"], 1, "`two` waits on `one`");
        assert_eq!(s["blocked"][0]["unmet_deps"][0], "one");
        assert_eq!(s["counts"]["total"], 2);

        // Finish the dependency and the frontier moves.
        e.set_status("one", ChunkStatus::InProgress).unwrap();
        e.set_status("one", ChunkStatus::Done).unwrap();
        let s = e.group_status("Auth");
        assert_eq!(s["next"], "two");
        assert_eq!(s["blocked_count"], 0);
    }

    /// Focus survives a restart when persistence is on — the property that
    /// makes `switch-work` worth calling once instead of every turn.
    #[test]
    fn focus_persists_and_reloads_across_engine_instances() {
        let tmp = TempDir::new().unwrap();
        let cfg = PersistenceConfig::from_env()
            .with_data_dir(tmp.path().to_path_buf())
            .enabled(true);

        {
            let mut e = RoadmapEngine::new("proj".into())
                .with_persistence(Persistence::new(&cfg, Domain::Roadmap));
            grouped(&mut e, "auth-1", 10, "Authentication");
            grouped(&mut e, "bill-1", 20, "Billing");
            e.focus_set("lane-a", "Authentication", FocusMode::Build)
                .unwrap();
            e.focus_set("lane-b", "Billing", FocusMode::Listen).unwrap();
        }

        let e2 = RoadmapEngine::new("proj".into())
            .with_persistence(Persistence::new(&cfg, Domain::Roadmap));
        assert_eq!(
            e2.focus_get("lane-a").map(|f| (f.group.clone(), f.mode)),
            Some(("Authentication".to_string(), FocusMode::Build))
        );
        assert_eq!(
            e2.focus_get("lane-b").map(|f| (f.group.clone(), f.mode)),
            Some(("Billing".to_string(), FocusMode::Listen))
        );
    }

    /// A store written before focus existed loads unchanged. The fixture is a
    /// literal pre-focus document rather than a serialized modern one, because
    /// a round-trip through today's type cannot prove yesterday's bytes parse.
    #[test]
    fn a_roadmap_persisted_before_focus_existed_loads_with_no_focuses() {
        let pre_focus = serde_json::json!({
            "project_id": "proj",
            "preamble": "",
            "chunks": [{
                "id": "a", "title": "A", "name": "A", "status": "pending",
                "priority": 1, "description": "", "notes": "",
                "acceptance": [], "deps": [], "cross_refs": [], "shared": false,
                "created_at": "2026-01-01T00:00:00+00:00",
                "updated_at": "2026-01-01T00:00:00+00:00"
            }],
            "notes": [], "refreshes": [], "links": [], "tracker_opt_ins": []
        });
        let loaded: Roadmap =
            serde_json::from_value(pre_focus).expect("a pre-focus store must still parse");
        assert_eq!(loaded.chunks.len(), 1);
        assert!(
            loaded.focuses.is_empty(),
            "absent focuses means nobody had focused anything, not a parse failure"
        );
    }

    /// The merge rule: two processes each holding their OWN lane union rather
    /// than clobber, and the same lane edited twice keeps the newer.
    #[test]
    fn merging_two_stores_unions_lanes_and_keeps_the_newer_per_lane() {
        let focus = |lane: &str, group: &str, at: &str| Focus {
            lane: lane.into(),
            group: group.into(),
            mode: FocusMode::Build,
            set_at: at.into(),
            updated_at: at.into(),
        };
        let mut memory = Roadmap {
            project_id: "p".into(),
            ..Default::default()
        };
        memory.focuses = vec![
            focus("lane-a", "Authentication", "2026-01-02T00:00:00+00:00"),
            focus("lane-shared", "Billing", "2026-01-02T00:00:00+00:00"),
        ];
        let mut disk = Roadmap {
            project_id: "p".into(),
            ..Default::default()
        };
        disk.focuses = vec![
            focus("lane-b", "Billing", "2026-01-01T00:00:00+00:00"),
            // Older copy of a lane memory also holds: memory must win.
            focus("lane-shared", "Payments", "2026-01-01T00:00:00+00:00"),
        ];

        let merged = merge_roadmaps(&memory, disk);
        let by_lane = |lane: &str| {
            merged
                .focuses
                .iter()
                .find(|f| f.lane == lane)
                .map(|f| f.group.clone())
        };
        assert_eq!(merged.focuses.len(), 3, "disk-only lanes survive the union");
        assert_eq!(by_lane("lane-a").as_deref(), Some("Authentication"));
        assert_eq!(by_lane("lane-b").as_deref(), Some("Billing"));
        assert_eq!(
            by_lane("lane-shared").as_deref(),
            Some("Billing"),
            "the newer copy of a contested lane wins"
        );
    }

    #[test]
    fn mode_parsing_is_closed_and_names_the_alternatives() {
        assert_eq!(FocusMode::from_wire("build").unwrap(), FocusMode::Build);
        assert_eq!(FocusMode::from_wire("  SHAPE ").unwrap(), FocusMode::Shape);
        assert_eq!(FocusMode::from_wire("listen").unwrap(), FocusMode::Listen);
        let err = FocusMode::from_wire("implement").unwrap_err();
        assert!(err.contains("shape") && err.contains("build") && err.contains("listen"));
        // Wire spellings round-trip through serde exactly as typed.
        for m in FocusMode::ALL {
            assert_eq!(serde_json::to_value(m).unwrap(), m.as_wire());
        }
    }

    #[test]
    fn start_chunk_moves_to_in_progress() {
        let mut e = engine();
        add(&mut e, "a", 1, vec![]);
        let c = e.start_chunk("a").unwrap();
        assert_eq!(c.status, ChunkStatus::InProgress);
        // The agent's backref is derived from the chunk id.
        assert_eq!(
            crate::infra::CrossRef::RoadmapChunk("a".into()).to_wire(),
            "chunk:a"
        );
    }

    #[test]
    fn link_chunk_validates_and_dedupes() {
        let mut e = engine();
        add(&mut e, "a", 1, vec![]);
        e.link_chunk("a", "think:42").unwrap();
        e.link_chunk("a", "task:auth").unwrap();
        e.link_chunk("a", "think:42").unwrap(); // duplicate ignored
        assert_eq!(
            e.roadmap().chunks[0].cross_refs,
            vec!["think:42", "task:auth"]
        );
        // A malformed ref is rejected.
        let err = e.link_chunk("a", "not-a-ref").unwrap_err();
        assert!(err.contains("invalid cross-ref"));
    }

    #[test]
    fn complete_chunk_attaches_proof_of_ship() {
        let mut e = engine();
        add(&mut e, "a", 1, vec![]);
        e.start_chunk("a").unwrap();
        let c = e.complete_chunk("a", Some("task:build-it")).unwrap();
        assert_eq!(c.status, ChunkStatus::Done);
        assert!(c.cross_refs.contains(&"task:build-it".to_string()));
    }

    #[test]
    fn complete_chunk_rejects_bad_ref_before_transition() {
        let mut e = engine();
        add(&mut e, "a", 1, vec![]);
        e.start_chunk("a").unwrap();
        let err = e.complete_chunk("a", Some("bogus")).unwrap_err();
        assert!(err.contains("invalid cross-ref"));
        // The status must NOT have advanced (validation happens first).
        assert_eq!(e.roadmap().chunks[0].status, ChunkStatus::InProgress);
    }

    #[test]
    fn record_refresh_appends_a_note() {
        let mut e = engine();
        let n = e.record_refresh("revisited priorities".into(), vec![1, 2, 3]);
        assert_eq!(n, 1);
        let note = &e.roadmap().refreshes[0];
        assert_eq!(note.summary, "revisited priorities");
        assert_eq!(note.think_steps, vec![1, 2, 3]);
    }

    #[test]
    fn export_markdown_groups_by_status_section() {
        let mut e = engine();
        e.add_chunk(
            "ship-me".into(),
            "Ship me".into(),
            ChunkStatus::Pending,
            1,
            "do the work".into(),
            vec!["it ships".into()],
            vec![],
            false,
        )
        .unwrap();
        e.add_chunk(
            "did-it".into(),
            "Did it".into(),
            ChunkStatus::Done,
            0,
            String::new(),
            vec![],
            vec![],
            false,
        )
        .unwrap();

        let md = e.export("markdown");
        // Section headings present for the statuses that have chunks.
        assert!(md.contains("## Pending"), "missing Pending section: {md}");
        assert!(md.contains("## Done"), "missing Done section: {md}");
        // Empty sections are omitted.
        assert!(!md.contains("## Obsoleted"), "empty section leaked: {md}");
        // A chunk with a description renders the em-dash tail; one without omits
        // it. Both carry their priority number, named by the band heading above.
        assert!(md.contains("**Ship me** (1) — do the work"));
        assert!(md.contains("- [x] **Did it** (0)\n"));
        assert!(md.contains("  - acceptance: it ships"));
        // Both fixtures sit in the `critical` band (1 and 0), so each status
        // section opens with one band heading and no other.
        assert_eq!(
            md.matches("### critical").count(),
            2,
            "each status section should name its band exactly once: {md}"
        );
    }

    /// The bands are a lens over the same data: chunks appear under the band
    /// their stored priority falls in, in the same order as before, with the
    /// number still visible. A reader who only trusts numbers loses nothing.
    #[test]
    fn export_markdown_groups_each_status_section_by_band() {
        let mut e = engine();
        for (id, priority) in [
            ("crit", 50),
            ("hi", 150),
            ("med", 250),
            ("lo", 350),
            ("later", 900),
        ] {
            e.add_chunk(
                id.into(),
                id.to_uppercase(),
                ChunkStatus::Pending,
                priority,
                String::new(),
                vec![],
                vec![],
                false,
            )
            .unwrap();
        }

        let md = e.export("markdown");
        let order: Vec<&str> = ["critical", "high", "medium", "low", "later"]
            .into_iter()
            .filter(|band| md.contains(&format!("### {band}")))
            .collect();
        assert_eq!(
            order,
            ["critical", "high", "medium", "low", "later"],
            "bands should appear in priority order: {md}"
        );

        // Each chunk keeps its raw number next to it.
        for (title, priority) in [("CRIT", 50), ("HI", 150), ("LATER", 900)] {
            assert!(
                md.contains(&format!("**{title}** ({priority})")),
                "chunk {title} should show its stored priority {priority}: {md}"
            );
        }
    }

    /// Bands are presentation only. Adding them must not touch what is stored
    /// or the order `next()` walks — the whole safety argument for this change.
    #[test]
    fn banding_changes_no_stored_priority_and_no_ordering() {
        let mut e = engine();
        for (id, priority) in [("c", 400), ("a", 100), ("b", 250)] {
            e.add_chunk(
                id.into(),
                id.into(),
                ChunkStatus::Pending,
                priority,
                String::new(),
                vec![],
                vec![],
                false,
            )
            .unwrap();
        }

        let stored: Vec<(String, u32)> = e
            .roadmap()
            .chunks
            .iter()
            .map(|c| (c.id.clone(), c.priority))
            .collect();
        assert_eq!(
            stored,
            vec![
                ("c".to_string(), 400),
                ("a".to_string(), 100),
                ("b".to_string(), 250)
            ],
            "stored priorities and insertion order are untouched"
        );

        // The projections still order by the number, not by the band name
        // (alphabetically "critical" < "high" < "low" < "medium" — a band-name
        // sort would put `low` before `medium` and be wrong).
        let status = e.status();
        let ids: Vec<&str> = status["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["a", "b", "c"], "priority order is preserved");
        assert_eq!(status["chunks"][0]["priority"], 100);
        assert_eq!(status["chunks"][0]["band"], "critical");
        assert_eq!(e.next().map(|c| c.id.as_str()), Some("a"));
    }

    #[test]
    fn merge_from_import_backfills_notes_without_reverting_status() {
        use crate::roadmap::import::{ImportedChunk, ImportedRoadmap};

        let mut e = engine();
        // A chunk reconciled to Done by hand (the case re-import must not revert).
        e.add_chunk(
            "p1".into(),
            "Stage 1".into(),
            ChunkStatus::Done,
            0,
            String::new(),
            vec![],
            vec![],
            false,
        )
        .unwrap();

        let imported = ImportedRoadmap {
            preamble: "the intro".into(),
            notes: vec![crate::roadmap::domain::NoteSection {
                heading: "Research".into(),
                body: "a finding".into(),
            }],
            chunks: vec![
                ImportedChunk {
                    id: "p1".into(),
                    title: "Stage 1".into(),
                    name: String::new(),
                    status: ChunkStatus::Pending, // stale source status — must be ignored
                    priority: 0,
                    description: "desc".into(),
                    notes: "narrative".into(),
                    acceptance: vec!["criterion".into()],
                    deps: vec![],
                },
                ImportedChunk {
                    id: "p2".into(),
                    title: "Stage 2".into(),
                    name: String::new(),
                    status: ChunkStatus::Pending,
                    priority: 1,
                    description: String::new(),
                    notes: String::new(),
                    acceptance: vec![],
                    deps: vec![],
                },
            ],
        };

        let (added, updated) = e.merge_from_import(imported);
        assert_eq!((added, updated), (1, 1));
        let p1 = e.roadmap().chunks.iter().find(|c| c.id == "p1").unwrap();
        assert_eq!(p1.status, ChunkStatus::Done, "status must NOT revert");
        assert_eq!(p1.notes, "narrative");
        assert_eq!(p1.description, "desc");
        assert_eq!(p1.acceptance, vec!["criterion".to_string()]);
        assert_eq!(e.roadmap().preamble, "the intro");
        assert_eq!(e.roadmap().notes.len(), 1);
        assert!(e.roadmap().chunks.iter().any(|c| c.id == "p2")); // new chunk added
    }

    // ---- Node labels (constraint C8) ----

    /// The chunk with this id. The engine has no single-chunk getter, and every
    /// assertion below is about one chunk's fields.
    fn got<'a>(e: &'a RoadmapEngine, id: &str) -> &'a Chunk {
        e.roadmap()
            .chunks
            .iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("no chunk '{id}'"))
    }

    /// The creation path a caller uses when it has nothing to say about the
    /// label. "Required or derived, never absent" is only true if this holds.
    #[test]
    fn a_chunk_added_without_a_name_still_gets_one() {
        let mut e = engine();
        e.add_chunk(
            "the-quota-ceiling-is-never-enforced".into(),
            "The quota ceiling is never enforced, so a runaway job bills forever".into(),
            ChunkStatus::Pending,
            1,
            String::new(),
            vec![],
            vec![],
            false,
        )
        .unwrap();
        let c = got(&e, "the-quota-ceiling-is-never-enforced");
        assert_eq!(c.name, "Quota ceiling");
        // The claim is untouched — this field is additive, not a rewrite.
        assert!(c.title.starts_with("The quota ceiling is never enforced"));
        assert!(crate::roadmap::name::fits(&c.name));
    }

    /// The leak this chunk exists to close: a sentence must not be installable
    /// as a label, or the defect reopens one careless call at a time.
    #[test]
    fn a_sentence_is_refused_as_a_name() {
        let mut e = engine();
        let err = e
            .add_chunk_with_content(
                "c1".into(),
                "A claim".into(),
                "Every chunk's label is a sentence, so nothing can be a node".into(),
                ChunkStatus::Pending,
                1,
                String::new(),
                vec![],
                vec![],
                false,
                None,
                None,
            )
            .unwrap_err();
        assert!(err.contains("over the"), "{err}");
        assert!(
            !e.roadmap().chunks.iter().any(|c| c.id == "c1"),
            "a rejected name creates no chunk"
        );
    }

    #[test]
    fn set_name_rewrites_the_label_and_leaves_the_title_alone() {
        let mut e = engine();
        e.add_chunk(
            "c1".into(),
            "A sentence stating the claim".into(),
            ChunkStatus::Pending,
            1,
            String::new(),
            vec![],
            vec![],
            false,
        )
        .unwrap();
        e.set_name("c1", "Quota ceiling").unwrap();
        assert_eq!(got(&e, "c1").name, "Quota ceiling");
        assert_eq!(got(&e, "c1").title, "A sentence stating the claim");

        // Empty re-seeds rather than clears: a nameless chunk stays unreachable.
        e.set_name("c1", "  ").unwrap();
        assert_eq!(got(&e, "c1").name, "C1");

        assert!(e.set_name("c1", &"x".repeat(80)).is_err());
    }

    /// The migration. Chunks persisted before the field existed deserialize
    /// with no name, and the whole roadmap has to come back usable.
    #[test]
    fn a_roadmap_saved_before_names_existed_loads_and_is_backfilled() {
        let legacy = serde_json::json!({
            "project_id": "proj",
            "chunks": [
                {
                    "id": "shipping-labels-print-blank",
                    "title": "Shipping labels print blank on thermal printers",
                    "status": "pending",
                    "priority": 100,
                    "created_at": "2026-01-01T00:00:00Z",
                    "updated_at": "2026-01-01T00:00:00Z"
                }
            ]
        });
        let roadmap: crate::roadmap::domain::Roadmap =
            serde_json::from_value(legacy).expect("a roadmap with no name key must still load");
        assert_eq!(roadmap.chunks[0].name, "");

        let mut e = engine();
        e.roadmap = roadmap;
        assert_eq!(e.backfill_names(), 1);
        assert_eq!(
            got(&e, "shipping-labels-print-blank").name,
            "Shipping labels print"
        );
        assert!(e.chunks_without_usable_names().is_empty());
        // Idempotent: a second pass has nothing left to fill.
        assert_eq!(e.backfill_names(), 0);
    }

    /// The collision half of the load-time migration: seeds that tie are
    /// separated, the family head keeps its label, and a hand-written name in
    /// the same store is not touched. Driven by ids this roadmap has never held.
    #[test]
    fn colliding_derived_labels_are_separated_on_load() {
        let mut e = engine();
        for id in [
            "tidepool-sensor-array",
            "tidepool-sensor-array-calibration",
            "tidepool-sensor-array-mounting",
        ] {
            e.add_chunk(
                id.into(),
                format!("A claim about {id}"),
                ChunkStatus::Pending,
                1,
                String::new(),
                vec![],
                vec![],
                false,
            )
            .unwrap();
        }
        e.add_chunk(
            "kelp-survey".into(),
            "A claim".into(),
            ChunkStatus::Pending,
            1,
            String::new(),
            vec![],
            vec![],
            false,
        )
        .unwrap();
        e.set_name("kelp-survey", "Hand written").unwrap();

        // The premise: all three seeds tie.
        assert_eq!(
            got(&e, "tidepool-sensor-array").name,
            "Tidepool sensor array"
        );
        assert_eq!(
            got(&e, "tidepool-sensor-array-calibration").name,
            "Tidepool sensor array"
        );

        assert_eq!(e.repair_label_collisions(), 2);
        assert_eq!(
            got(&e, "tidepool-sensor-array").name,
            "Tidepool sensor array"
        );
        assert_eq!(
            got(&e, "tidepool-sensor-array-calibration").name,
            "Array calibration"
        );
        assert_eq!(
            got(&e, "tidepool-sensor-array-mounting").name,
            "Array mounting"
        );
        assert_eq!(got(&e, "kelp-survey").name, "Hand written");

        // Idempotent: the relabeled names no longer match their seeds.
        assert_eq!(e.repair_label_collisions(), 0);
    }

    /// The door itself: an id-less chunk cannot enter through `add_chunk`,
    /// which is how the two observed specimens got in. Drives the function,
    /// not the type — a blank and a whitespace id both bounce, and the store
    /// stays empty.
    #[test]
    fn add_chunk_rejects_an_empty_id() {
        let mut e = engine();
        for id in ["", "   "] {
            let err = e
                .add_chunk(
                    id.into(),
                    "A real claim arriving without an id".into(),
                    ChunkStatus::Pending,
                    1,
                    String::new(),
                    Vec::new(),
                    Vec::new(),
                    false,
                )
                .unwrap_err();
            assert!(err.contains("id"), "the refusal must name the id: {err}");
        }
        assert!(e.roadmap().chunks.is_empty());
    }

    /// The cloud door: a pulled record with no id is dropped, not inserted.
    #[test]
    fn upsert_ignores_a_chunk_with_no_id() {
        let mut e = engine();
        let ghost: crate::roadmap::domain::Chunk = serde_json::from_value(serde_json::json!({
            "id": "", "title": "Pulled from a cloud copy that predates the door",
            "status": "pending", "priority": 1,
            "created_at": "2026-06-08T00:00:00Z", "updated_at": "2026-06-08T00:00:00Z"
        }))
        .unwrap();
        e.upsert_chunk(ghost);
        assert!(e.roadmap().chunks.is_empty());
    }

    /// The load-time disposition, driven by a store this engine never wrote:
    /// a contentful id-less record gets the id the import lane would have
    /// minted for its title (uniquified past a squatter), a contentless husk
    /// is removed, and everyone else is untouched. Second pass finds nothing.
    #[test]
    fn empty_id_records_are_repaired_on_load() {
        let foreign = serde_json::json!({
            "project_id": "proj",
            "chunks": [
                {"id": "orchard-frost-watch", "title": "A chunk with an ordinary id",
                 "status": "pending", "priority": 1,
                 "created_at": "t", "updated_at": "t"},
                // A squatter already wearing the id the mint would produce.
                {"id": "orchard-drone-the-frost-sensor-misreads-at",
                 "title": "The squatter", "status": "pending", "priority": 1,
                 "created_at": "t", "updated_at": "t"},
                // The contentful specimen: real title, live status, a cross-ref.
                {"id": "", "title": "Orchard drone — the frost sensor misreads at dawn",
                 "status": "pending", "priority": 118,
                 "cross_refs": ["task:frost-audit"],
                 "created_at": "t", "updated_at": "t"},
                // The husk: nothing anywhere, obsoleted at birth.
                {"id": "", "title": "", "status": "obsoleted", "priority": 0,
                 "obsoleted_reason": "Accidental empty chunk; removing.",
                 "created_at": "t", "updated_at": "t"}
            ]
        });
        let mut e = engine();
        e.roadmap = serde_json::from_value(foreign).unwrap();

        assert_eq!(e.repair_missing_ids(), (1, 1));

        let ids: Vec<&str> = e.roadmap().chunks.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "orchard-frost-watch",
                "orchard-drone-the-frost-sensor-misreads-at",
                // import::derive_id over the title, pushed past the squatter.
                "orchard-drone-the-frost-sensor-misreads-at-2",
            ]
        );
        // The record kept everything but gained an identity; the name gap is
        // backfill_names' job, seeded from the minted id on the same load.
        let repaired = got(&e, "orchard-drone-the-frost-sensor-misreads-at-2");
        assert_eq!(repaired.cross_refs, vec!["task:frost-audit".to_string()]);
        assert_eq!(repaired.priority, 118);
        assert!(e.backfill_names() > 0);
        assert!(
            !got(&e, "orchard-drone-the-frost-sensor-misreads-at-2")
                .name
                .is_empty()
        );

        // Idempotent: one pass leaves no empty id behind.
        assert_eq!(e.repair_missing_ids(), (0, 0));
    }

    /// Never overwrites a label somebody wrote — including one over budget,
    /// which doctor reports rather than silently repairing.
    #[test]
    fn backfill_only_fills_gaps() {
        let mut e = engine();
        e.add_chunk(
            "c1".into(),
            "A claim".into(),
            ChunkStatus::Pending,
            1,
            String::new(),
            vec![],
            vec![],
            false,
        )
        .unwrap();
        e.set_name("c1", "Hand written").unwrap();
        assert_eq!(e.backfill_names(), 0);
        assert_eq!(got(&e, "c1").name, "Hand written");
    }

    /// The export/import round trip. Both fields have to survive, and the name
    /// must not come back as an acceptance criterion.
    #[test]
    fn a_name_survives_export_and_reimport() {
        let mut e = engine();
        e.add_chunk(
            "reconciliation-statements-duplicate".into(),
            "Reconciliation statements duplicate every line after a retry".into(),
            ChunkStatus::Pending,
            1,
            "the description".into(),
            vec!["a criterion a human wrote".into()],
            vec![],
            false,
        )
        .unwrap();
        e.set_name(
            "reconciliation-statements-duplicate",
            "Statements duplicate",
        )
        .unwrap();

        let md = e.export("markdown");
        assert!(md.contains("- name: Statements duplicate"), "{md}");

        let back = crate::roadmap::import::markdown::parse_markdown(&md);
        let c = back
            .iter()
            .find(|c| c.title.starts_with("Reconciliation statements duplicate"))
            .expect("the chunk must survive the round trip");
        assert_eq!(c.name, "Statements duplicate");
        // The label is a field, not a criterion — it must not land in acceptance.
        assert!(
            !c.acceptance
                .iter()
                .any(|a| a.contains("Statements duplicate")),
            "the name leaked into acceptance: {:?}",
            c.acceptance
        );
        assert!(
            c.acceptance
                .iter()
                .any(|a| a.contains("a criterion a human wrote"))
        );
    }

    /// Import is the other door a chunk arrives through, and it must not create
    /// a nameless one either.
    #[test]
    fn an_imported_chunk_with_no_name_is_seeded_from_its_id() {
        let mut e = engine();
        let imported = crate::roadmap::import::ImportedRoadmap {
            preamble: String::new(),
            notes: vec![],
            chunks: vec![crate::roadmap::import::ImportedChunk::new(
                "payroll-export-breaks-on-leap-day".into(),
                "Payroll export breaks on a leap day and drops the last run".into(),
                ChunkStatus::Pending,
                0,
            )],
        };
        assert_eq!(e.seed_from_import(imported), 1);
        assert_eq!(
            got(&e, "payroll-export-breaks-on-leap-day").name,
            "Payroll export breaks"
        );
        assert!(e.chunks_without_usable_names().is_empty());
    }

    // ---- validate_blocked_by ----

    /// A blocker with no reason is the failure this whole feature replaces,
    /// wearing a struct instead of a title. Whitespace does not count as one.
    #[test]
    fn validate_blocked_by_refuses_a_reason_that_says_nothing() {
        let e = engine();
        for blank in ["", "   ", "\t", "\n  \n"] {
            let err = e
                .validate_blocked_by(BlockerKind::AwaitingHuman, blank.into(), None)
                .expect_err("a blank reason must be refused");
            assert!(
                err.contains("reason"),
                "the error must say what is missing: {err}"
            );
        }
    }

    /// Evidence goes through the SAME parser `cross_refs` uses. Checked by
    /// driving the whole CrossRef vocabulary rather than one happy example, so
    /// this cannot pass while silently accepting only the form I had in mind.
    #[test]
    fn validate_blocked_by_accepts_every_cross_ref_form_and_normalizes_it() {
        let e = engine();
        for raw in [
            "think:42",
            "chunk:some-other-chunk",
            "task:verify-blockedby",
            "check:cargo test",
            "action:4",
            "ext:linear/THI-30",
        ] {
            let b = e
                .validate_blocked_by(
                    BlockerKind::PremiseUnmet,
                    "waiting on the world".into(),
                    Some(raw.into()),
                )
                .unwrap_or_else(|e| panic!("'{raw}' is a legal cross-ref but was refused: {e}"));
            assert_eq!(
                b.evidence.as_deref(),
                Some(CrossRef::from_wire(raw).unwrap().to_wire().as_str()),
                "evidence must be stored in the parser's normalized form, not verbatim"
            );
        }
    }

    /// An evidence ref that resolves to nothing is worse than no evidence: it
    /// LOOKS like the blocker is sourced. Refuse it rather than store it.
    #[test]
    fn validate_blocked_by_refuses_evidence_that_is_not_a_cross_ref() {
        let e = engine();
        for bad in ["1763", "think:", "just some prose", "think:not-a-number"] {
            let err = e
                .validate_blocked_by(
                    BlockerKind::PremiseRefuted,
                    "measured dead".into(),
                    Some(bad.into()),
                )
                .expect_err("an unparseable evidence ref must be refused");
            assert!(
                err.contains("evidence"),
                "the error must say which field is wrong: {err}"
            );
        }
    }

    /// Absent evidence is a first-class answer, not a gap to fill. A blocker
    /// nobody has a citation for is still worth recording.
    #[test]
    fn validate_blocked_by_allows_a_blocker_with_no_evidence() {
        let e = engine();
        let b = e
            .validate_blocked_by(
                BlockerKind::External,
                "  nobody holds the signing key  ".into(),
                None,
            )
            .expect("evidence is optional");
        assert_eq!(b.evidence, None);
        assert_eq!(b.kind, BlockerKind::External);
        assert_eq!(
            b.reason, "nobody holds the signing key",
            "the reason is trimmed on the way in"
        );
        assert!(
            !b.blocked_at.is_empty(),
            "a blocker must be timestamped, so 'how long has this been stuck' is answerable"
        );
    }

    /// The scope fence, flipped — and kept rather than replaced.
    ///
    /// `chunk-blocked-by-vocabulary` added the field, validated it, and
    /// deliberately did NOT teach the scheduler about it; this test asserted
    /// that blindness on purpose, so the day it ended would have to be a
    /// deliberate day. `roadmap-next-skips-blocked-by` is that day. Inverting
    /// the assertion in place — same test, opposite expectation — keeps the
    /// record that the blindness was once a choice, which deleting the test
    /// would have thrown away along with it.
    #[test]
    fn next_skips_a_chunk_carrying_a_blocker() {
        let mut e = engine();
        add(&mut e, "a", 1, vec![]);

        assert_eq!(
            e.next().map(|c| c.id.as_str()),
            Some("a"),
            "unblocked, this chunk is exactly what next() should answer — \
             which is what makes the assertion below about the blocker and \
             nothing else"
        );

        let b = e
            .validate_blocked_by(BlockerKind::AwaitingHuman, "needs a person".into(), None)
            .unwrap();
        let idx = e.roadmap().chunks.iter().position(|c| c.id == "a").unwrap();
        e.roadmap.chunks[idx].blocked_by = Some(b);

        assert_eq!(
            e.next(),
            None,
            "a blocker is now a refusal, and with nothing else on the board \
             the honest answer is nothing at all — not the blocked chunk"
        );
    }

    /// A roadmap this project has never had, and never will: `(id, priority,
    /// blocked)` for a greenhouse-controls backlog.
    ///
    /// Foreign on purpose. A gate that says "next skips blockers" while
    /// reading think-and-ship's own chunks can pass on an accident of how the
    /// real plan happens to be ordered; this table is the only data these
    /// tests see, and [`engine_from`] takes it as a parameter so a second
    /// table can be driven through the same code without editing it.
    const GREENHOUSE: &[(&str, u32, bool)] = &[
        ("mist-line-pressure-drops", 40, false),
        ("dosing-pump-runs-dry", 5, true),
        ("vent-motor-stalls-in-heat", 12, true),
        ("substrate-ec-drifts-overnight", 25, false),
        ("light-recipe-ignores-dli", 60, false),
    ];

    /// Build a board from a table: every row a `Pending`, dependency-free
    /// chunk, blocked or not as the row says. Dependency-free deliberately —
    /// deps are the refusal that already worked, and leaving them out of the
    /// table means nothing but the blocker can explain a skip.
    fn engine_from(table: &[(&str, u32, bool)]) -> RoadmapEngine {
        let mut e = engine();
        for (id, priority, blocked) in table {
            add(&mut e, id, *priority, vec![]);
            if *blocked {
                let b = e
                    .validate_blocked_by(
                        BlockerKind::External,
                        format!("{id} waits on somebody else"),
                        None,
                    )
                    .expect("a well-formed blocker");
                let idx = e
                    .roadmap()
                    .chunks
                    .iter()
                    .position(|c| &c.id == id)
                    .expect("just added");
                e.roadmap.chunks[idx].blocked_by = Some(b);
            }
        }
        e
    }

    /// The worst case is COMPUTED from the table, never named.
    ///
    /// Naming the sample is how this kind of gate rots: the table gets a new
    /// row, the named chunk stops being the hard case, and the test keeps
    /// passing for a reason that has nothing to do with what it claims. So the
    /// test derives two answers — what a scheduler blind to blockers would say
    /// (the most urgent chunk on the board — smallest priority number) and what a seeing one must say
    /// (the most urgent UNBLOCKED chunk) — and asserts the premise that
    /// makes them differ before asserting anything about `next`.
    #[test]
    fn next_skips_the_hardest_case_the_table_can_produce() {
        let table = GREENHOUSE;
        let e = engine_from(table);

        let blind = table
            .iter()
            .min_by_key(|(_, priority, _)| *priority)
            .expect("a non-empty table");
        let seeing = table
            .iter()
            .filter(|(_, _, blocked)| !blocked)
            .min_by_key(|(_, priority, _)| *priority)
            .expect("a table with somewhere to go");

        // The premise, asserted so a re-ordering fails loudly instead of
        // quietly passing: unless the board's top chunk IS a blocked one, both
        // answers coincide and the assertion below proves nothing.
        assert!(
            blind.2,
            "the most urgent chunk in the table must be a blocked one, \
             or this test cannot tell a seeing scheduler from a blind one; \
             got '{}'",
            blind.0
        );
        assert_ne!(blind.0, seeing.0, "the two answers must be distinguishable");

        assert_eq!(
            e.next().map(|c| c.id.as_str()),
            Some(seeing.0),
            "next() must step over every blocker above '{}' — including '{}', \
             which outranks it",
            seeing.0,
            blind.0
        );
    }

    /// The other end of the same rule: a board with nothing takeable answers
    /// nothing, rather than falling back to the best blocked chunk. Derived
    /// from the same table so the two tests cannot disagree about the data.
    #[test]
    fn a_board_of_nothing_but_blockers_answers_nothing() {
        let all_blocked: Vec<(&str, u32, bool)> = GREENHOUSE
            .iter()
            .map(|(id, priority, _)| (*id, *priority, true))
            .collect();
        let e = engine_from(&all_blocked);

        assert_eq!(
            e.next(),
            None,
            "with every chunk blocked the answer is None — a scheduler that \
             relaxes its own rule when the board looks empty is worse than one \
             that admits there is nothing to do"
        );
    }

    /// Skipped is not hidden, which is the entire point of the field: a chunk
    /// `next` refuses keeps its status, its priority and its place in the
    /// status list, and says out loud that it is blocked. The alternative —
    /// demoting it out of the way — is exactly the workaround this line of
    /// work exists to make unnecessary.
    #[test]
    fn a_skipped_chunk_keeps_its_priority_and_its_place_in_status() {
        let e = engine_from(GREENHOUSE);
        let status = e.status();
        let listed = status["chunks"].as_array().expect("a chunk list");

        for (id, priority, blocked) in GREENHOUSE {
            let seen = listed
                .iter()
                .find(|c| c["id"] == *id)
                .unwrap_or_else(|| panic!("'{id}' vanished from status: {status}"));
            assert_eq!(
                seen["priority"].as_u64(),
                Some(u64::from(*priority)),
                "'{id}' must keep the priority that says how much it matters"
            );
            assert_eq!(
                seen["status"], "pending",
                "'{id}' must not have been demoted to earn the skip"
            );
            // The row says WHY, not merely whether: `blocker_kind` replaced the
            // `has_blocker` boolean this assertion used to read, because the
            // two could never legally disagree and only one of them answers
            // the reader's actual question. Absent is the unblocked answer.
            assert_eq!(
                seen.get("blocker_kind").and_then(|k| k.as_str()),
                blocked.then_some(BlockerKind::External.as_wire()),
                "'{id}' must say why it is blocked, or a reader building their \
                 own frontier from this list cannot apply the skip"
            );
        }

        assert_eq!(
            status["counts"]["pending"].as_u64(),
            Some(GREENHOUSE.len() as u64),
            "every chunk is still counted as pending: {status}"
        );
    }

    /// A night-operations backlog for an observatory: `(id, priority, headline,
    /// tail, kind)`. The title a chunk actually wears is the headline, an
    /// em-dash and the tail; an unblocked row has an empty tail.
    ///
    /// Foreign on purpose, like [`GREENHOUSE`], and SPLIT on purpose. Splitting
    /// the title is what lets the truncation gate derive which row is the hard
    /// case and assert that the blocker prose really does fall past the cut,
    /// instead of trusting a hand-counted character offset that the next edit
    /// to these strings would silently invalidate.
    ///
    /// [`BlockerKind::External`] is deliberately absent: a vocabulary entry with
    /// no chunks is the case that separates "reports 0" from "vanishes", and
    /// only a table missing one can tell them apart.
    #[allow(clippy::type_complexity)]
    const OBSERVATORY: &[(&str, u32, &str, &str, Option<BlockerKind>)] = &[
        (
            "seeing-monitor-and-guider-disagree",
            30,
            "The seeing monitor and the guide camera disagree about the sky on nights of high cirrus, and the scheduler trusts whichever answered last",
            "and the monitor's premise did not survive the measurement",
            Some(BlockerKind::PremiseRefuted),
        ),
        (
            "periodic-drift-the-encoders-deny",
            10,
            "Every exposure longer than four hundred seconds shows a periodic drift the mount's own encoders insist is not happening",
            "and nothing can be done until the new encoder firmware exists",
            Some(BlockerKind::PremiseUnmet),
        ),
        (
            "dome-slit-lags-near-the-meridian",
            45,
            "The dome slit tracks the telescope with a lag that only matters near the meridian, where the flip already costs the most time",
            "and somebody has to stand in the dome and watch it happen",
            Some(BlockerKind::AwaitingHuman),
        ),
        (
            "dusk-and-dawn-flats-disagree",
            20,
            "Flat fields taken at dusk and at dawn differ by more than the calibration pipeline is willing to tolerate in one night",
            "and the decision about which night to sacrifice is a person's to make",
            Some(BlockerKind::AwaitingHuman),
        ),
        (
            "queue-ranks-targets-by-airmass-alone",
            55,
            "The queue scheduler ranks targets by airmass alone and will happily spend a whole night on one object that is already setting",
            "",
            None,
        ),
    ];

    /// The title a row wears: headline, em-dash, tail.
    fn observatory_title(headline: &str, tail: &str) -> String {
        if tail.is_empty() {
            headline.to_string()
        } else {
            format!("{headline} — {tail}")
        }
    }

    /// The blocker's reason, built so it CANNOT be found anywhere but the
    /// blocker record. A reason echoing the title would make every "the view
    /// shows the reason" assertion below pass on the title alone.
    fn observatory_reason(id: &str) -> String {
        format!("the night log for {id} says so, and its title does not")
    }

    /// Build a board from the table, writing blockers through the real engine
    /// verbs (`validate_blocked_by` then `set_blocked_by`) rather than poking
    /// the field, so what these tests read back is what a human typing
    /// `roadmap block` would have produced.
    #[allow(clippy::type_complexity)]
    fn observatory_from(table: &[(&str, u32, &str, &str, Option<BlockerKind>)]) -> RoadmapEngine {
        let mut e = engine();
        for (id, priority, headline, tail, kind) in table {
            e.add_chunk(
                (*id).into(),
                observatory_title(headline, tail),
                ChunkStatus::Pending,
                *priority,
                String::new(),
                Vec::new(),
                Vec::new(),
                false,
            )
            .expect("a well-formed chunk");
            if let Some(k) = kind {
                // A refutation cites the measurement that refuted it; the other
                // kinds have nothing to point at, which exercises both arms of
                // the optional evidence.
                let evidence = (*k == BlockerKind::PremiseRefuted)
                    .then(|| "check:mirror-recoating-audit".to_string());
                let b = e
                    .validate_blocked_by(*k, observatory_reason(id), evidence)
                    .expect("a well-formed blocker");
                e.set_blocked_by(id, b).expect("the chunk was just added");
            }
        }
        e
    }

    /// THE SHARP ONE. A title is cut at [`RoadmapEngine::STATUS_TITLE_LEN`], and
    /// on a real roadmap the blocker is stated in the title's TAIL — so the
    /// truncation destroys exactly the thing the reader needs, which is the
    /// measured symptom this chunk exists to fix.
    ///
    /// The hard case is COMPUTED, never named: among the blocked rows, the one
    /// whose headline is longest is the one whose blocker prose is pushed
    /// furthest past the cut. Naming it is how this rots — the table gains a
    /// row, the named chunk stops being the hard case, and the test keeps
    /// passing for a reason unrelated to its claim.
    #[test]
    fn the_blocker_kind_survives_the_truncation_that_destroys_the_prose() {
        let table = OBSERVATORY;
        let e = observatory_from(table);
        let status = e.status();
        let listed = status["chunks"].as_array().expect("a chunk list");

        let worst = table
            .iter()
            .filter(|(_, _, _, _, kind)| kind.is_some())
            .max_by_key(|(_, _, headline, _, _)| headline.chars().count())
            .expect("a table with at least one blocker");
        let (id, _, headline, tail, kind) = worst;
        let kind = kind.expect("filtered on is_some");

        // The premise, asserted before anything about the output: unless the
        // headline alone already fills the budget, part of the tail survives
        // the cut and this test proves nothing about what truncation destroys.
        assert!(
            headline.chars().count() >= RoadmapEngine::STATUS_TITLE_LEN,
            "'{id}' must have a headline at least {} chars long, or its blocker \
             prose is not actually lost to truncation and the gate is vacuous; \
             got {}",
            RoadmapEngine::STATUS_TITLE_LEN,
            headline.chars().count()
        );

        let row = listed
            .iter()
            .find(|c| c["id"] == *id)
            .unwrap_or_else(|| panic!("'{id}' vanished from status: {status}"));
        let shown = row["title"].as_str().expect("a title");

        // The loss, observed rather than assumed.
        assert!(
            shown.ends_with('…'),
            "'{id}' must actually have been cut, or nothing was destroyed: {shown}"
        );
        assert!(
            !shown.contains(tail),
            "the blocker prose must be GONE from the truncated title — that is \
             the failure being repaired, and if it survives this test is \
             measuring the wrong thing: {shown}"
        );

        // And the token that survives it.
        assert_eq!(
            row.get("blocker_kind").and_then(|k| k.as_str()),
            Some(kind.as_wire()),
            "'{id}' loses its blocker to truncation and must still say what \
             kind it is: {row}"
        );

        // The complement, so absence is a fact about the blocker and not about
        // the title being short: an UNBLOCKED row that is also cut says nothing.
        let unblocked_and_cut = table
            .iter()
            .filter(|(_, _, headline, _, kind)| {
                kind.is_none() && headline.chars().count() >= RoadmapEngine::STATUS_TITLE_LEN
            })
            .map(|(id, ..)| *id)
            .collect::<Vec<_>>();
        assert!(
            !unblocked_and_cut.is_empty(),
            "the table must hold an unblocked row long enough to be truncated, \
             or the complement below is untested"
        );
        for id in unblocked_and_cut {
            let row = listed.iter().find(|c| c["id"] == id).expect("listed");
            assert!(
                row.get("blocker_kind").is_none(),
                "'{id}' is not blocked and must carry no kind: {row}"
            );
        }
    }

    /// The counts, derived from the vocabulary rather than from a list of four
    /// names — so a kind added to [`BlockerKind`] and forgotten in `status()`
    /// fails here by name instead of being silently uncounted.
    #[test]
    fn every_kind_in_the_vocabulary_is_counted_and_the_status_buckets_do_not_move() {
        let table = OBSERVATORY;
        let e = observatory_from(table);
        let status = e.status();
        let tally = &status["counts"]["blocked_by"];

        let expected = |kind: BlockerKind| {
            table
                .iter()
                .filter(|(_, _, _, _, k)| *k == Some(kind))
                .count() as u64
        };

        // The premise. Two ways this gate can go vacuous: if every kind has the
        // same tally, a `status()` that answered one number for all four would
        // pass; and if no kind has zero, "reports 0 rather than vanishing" is
        // never exercised.
        let mut tallies: Vec<u64> = BlockerKind::ALL.iter().map(|k| expected(*k)).collect();
        assert!(
            tallies.contains(&0),
            "the table must leave some kind unused, or a vanishing key reads \
             the same as an absent one"
        );
        tallies.sort_unstable();
        tallies.dedup();
        assert!(
            tallies.len() >= 3,
            "the kinds must have distinguishable tallies, or one number \
             answered for all of them would pass: {tallies:?}"
        );

        for kind in BlockerKind::ALL {
            assert_eq!(
                tally[kind.as_wire()].as_u64(),
                Some(expected(kind)),
                "'{}' must be counted, at 0 if nothing carries it: {tally}",
                kind.as_wire()
            );
        }
        assert_eq!(
            tally["total"].as_u64(),
            Some(table.iter().filter(|(_, _, _, _, k)| k.is_some()).count() as u64),
            "the total must agree with the breakdown: {tally}"
        );

        // Cross-cutting, not a seventh bucket: nothing was moved out of the
        // status partition to earn a place in the tally. This is the criterion
        // "counted separately from `backlog` and from `blocked`" — a blocked
        // chunk is still pending, and `blocked` (the STATUS) is still zero.
        assert_eq!(
            status["counts"]["pending"].as_u64(),
            Some(table.len() as u64),
            "every chunk is still counted as pending: {status}"
        );
        assert_eq!(
            status["counts"]["blocked"].as_u64(),
            Some(0),
            "no chunk was demoted to the Blocked STATUS to earn its tally: {status}"
        );
    }

    /// The tally counts the ACTIVE board, and the reason that is a real choice
    /// rather than a convenience: `complete_chunk` does not clear `blocked_by`,
    /// so a finished chunk really can still carry the blocker that once held it
    /// up. The premise is asserted, because if completion ever DID clear the
    /// field this test would pass while proving nothing about the filter.
    #[test]
    fn finishing_a_chunk_takes_it_out_of_the_unschedulable_count_without_clearing_its_blocker() {
        let mut e = observatory_from(OBSERVATORY);
        let before = e.status()["counts"]["blocked_by"]["total"]
            .as_u64()
            .expect("a total");

        // Derived: the blocked row the board would reach first.
        let victim = OBSERVATORY
            .iter()
            .filter(|(_, _, _, _, kind)| kind.is_some())
            .min_by_key(|(_, priority, ..)| *priority)
            .map(|(id, ..)| *id)
            .expect("a blocked row");

        e.start_chunk(victim).expect("pending starts");
        e.complete_chunk(victim, None)
            .expect("in-progress completes");

        assert!(
            e.roadmap()
                .chunks
                .iter()
                .find(|c| c.id == victim)
                .expect("still stored")
                .blocked_by
                .is_some(),
            "the premise: completion must NOT clear the blocker, or the active \
             filter has nothing to exclude and this test is vacuous"
        );
        assert_eq!(
            e.status()["counts"]["blocked_by"]["total"].as_u64(),
            Some(before - 1),
            "a finished chunk is not part of what cannot be scheduled, however \
             stale the blocker it still carries"
        );
    }

    /// The markdown view is the one surface with no per-row budget, so it is the
    /// one that carries the SENTENCE. A generated ROADMAP.md that omits it is a
    /// document that quietly deletes its own negative results.
    #[test]
    fn the_markdown_view_carries_the_reason_the_status_list_cannot_afford() {
        let e = observatory_from(OBSERVATORY);
        let md = e.export("markdown");

        for (id, _, headline, tail, kind) in OBSERVATORY {
            let Some(kind) = kind else {
                continue;
            };
            let reason = observatory_reason(id);
            // The premise that keeps this from passing on the title: the reason
            // appears NOWHERE in what the view would print anyway.
            let title = observatory_title(headline, tail);
            assert!(
                !title.contains(&reason),
                "'{id}' — the reason must not be recoverable from the title, or \
                 the assertion below proves only that titles are printed"
            );
            assert!(
                md.contains(&format!("blocked by: {} — {reason}", kind.as_wire())),
                "'{id}' must state its blocker and why in the view a human \
                 reads: {md}"
            );
        }

        // Evidence rides along when there is any, and nothing is invented when
        // there is not.
        let cited = OBSERVATORY
            .iter()
            .filter(|(_, _, _, _, k)| *k == Some(BlockerKind::PremiseRefuted))
            .count();
        assert_eq!(
            md.matches("(check:mirror-recoating-audit)").count(),
            cited,
            "each refutation cites its measurement, and only those: {md}"
        );
        assert_eq!(
            md.matches("- blocked by:").count(),
            OBSERVATORY
                .iter()
                .filter(|(_, _, _, _, k)| k.is_some())
                .count(),
            "an unblocked chunk must not grow a blocker line: {md}"
        );
    }
}

#[cfg(test)]
mod tracker_state_tests {
    use super::*;

    fn engine_with(id: &str) -> RoadmapEngine {
        let mut e = RoadmapEngine::new("proj".into());
        e.add_chunk(
            id.into(),
            format!("Chunk {id}"),
            ChunkStatus::Pending,
            10,
            String::new(),
            Vec::new(),
            Vec::new(),
            false,
        )
        .expect("add chunk");
        e
    }

    /// Silence is the default and it has to be structural, not a policy applied
    /// somewhere else: a chunk nobody opted in is simply not in the projection
    /// set, so there is no code path that could decide otherwise.
    #[test]
    fn nothing_is_opted_in_by_default() {
        let e = engine_with("c1");
        assert!(e.tracker_opt_in("c1", "github").is_none());
        assert!(e.chunks_opted_in("github").is_empty());
    }

    #[test]
    fn opting_in_and_out_is_recorded_not_deleted() {
        let mut e = engine_with("c1");
        e.set_tracker_opt_in("c1", "GitHub", true).expect("opt in");
        assert_eq!(e.chunks_opted_in("github").len(), 1);

        e.set_tracker_opt_in("c1", "github", false)
            .expect("opt out");
        assert!(e.chunks_opted_in("github").is_empty());
        assert!(
            e.tracker_opt_in("c1", "github").is_some(),
            "the refusal must persist so a peer cannot re-enable it"
        );
    }

    /// The consequence of putting tracker state in the chunk envelope: both the
    /// disk merge and the cloud reconcile resolve chunks by strict recency, so
    /// a tracker write that left `updated_at` alone would produce an envelope
    /// the peer declines — and each machine would keep minting its own twin.
    #[test]
    fn a_tracker_write_bumps_the_chunks_stamp() {
        let mut e = engine_with("c1");
        let before = e.roadmap().chunks[0].updated_at.clone();

        e.record_tracker_link("c1", "github", "owner/repo#12", "hash-1", Some("1"))
            .expect("link");
        let after_link = e.roadmap().chunks[0].updated_at.clone();
        assert!(after_link > before, "the link write must bump the chunk");

        // The second projection reuses the SAME external id, so push_cross_ref
        // dedupes and cannot be what carries the stamp.
        e.record_tracker_link("c1", "github", "owner/repo#12", "hash-2", Some("2"))
            .expect("re-link");
        assert!(
            e.roadmap().chunks[0].updated_at > after_link,
            "a re-projection with an unchanged ref must still bump the chunk"
        );
    }

    #[test]
    fn relations_are_fenced_separately_from_content() {
        let mut e = engine_with("c1");
        e.record_tracker_link("c1", "github", "owner/repo#12", "hash-1", None)
            .expect("link");
        assert!(
            e.tracker_link("c1", "github")
                .expect("link")
                .our_last_relations_hash
                .is_none(),
            "no relation declared yet is distinct from an empty one"
        );

        e.record_tracker_relations("c1", "github", "rel-1")
            .expect("relations");
        let link = e.tracker_link("c1", "github").expect("link");
        assert_eq!(link.our_last_relations_hash.as_deref(), Some("rel-1"));
        // Recording relations must not disturb the content fence.
        assert_eq!(link.our_last_write_hash, "hash-1");
    }

    /// Relations are declared between items that already exist, so there is no
    /// meaningful "record relations for a chunk we never projected".
    #[test]
    fn relations_require_a_link_first() {
        let mut e = engine_with("c1");
        assert!(e.record_tracker_relations("c1", "github", "rel-1").is_err());
    }

    /// The cross-machine story end to end: what a peer receives in the chunk
    /// envelope is adopted under the same recency rule as everything else, so a
    /// second machine inherits the binding instead of creating a twin.
    #[test]
    fn adopted_state_follows_the_recency_rule() {
        let mut e = engine_with("c1");
        e.record_tracker_link("c1", "github", "local", "h", None)
            .expect("link");

        let stale = TrackerLink {
            chunk_id: "c1".into(),
            provider: "github".into(),
            external_id: "stale".into(),
            our_last_write_hash: "h".into(),
            last_seen_version: None,
            our_last_relations_hash: None,
            our_last_authored_hash: None,
            created_at: "2020-01-01T00:00:00+00:00".into(),
            updated_at: "2020-01-01T00:00:00+00:00".into(),
        };
        assert_eq!(e.adopt_tracker_state(vec![stale], vec![]), 0);
        assert_eq!(
            e.tracker_link("c1", "github").expect("link").external_id,
            "local",
            "a stale peer copy must not clobber a fresher local binding"
        );

        let fresh = TrackerLink {
            external_id: "peer".into(),
            updated_at: "2099-01-01T00:00:00+00:00".into(),
            ..e.tracker_link("c1", "github").expect("link").clone()
        };
        assert_eq!(e.adopt_tracker_state(vec![fresh], vec![]), 1);
        assert_eq!(
            e.tracker_link("c1", "github").expect("link").external_id,
            "peer"
        );
    }
}
