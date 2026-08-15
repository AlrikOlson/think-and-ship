//! The signal engine: owns the in-memory [`Signals`] store and mediates every
//! mutation, mirroring `roadmap::engine::RoadmapEngine` (a plain struct whose
//! concurrency guard lives at the service layer; builder-style
//! `with_persistence`; persist-on-mutation).
//!
//! Scope: local capture + read + validated status transitions. The
//! local store is a CACHE of the cloud system-of-record; the write-through to
//! the cloud belongs to the cloud client (`SyncTarget::Cloud`), a documented
//! no-op here.
//!
//! Dependency direction: depends on `crate::signal::domain` (pure) and
//! `crate::infra` (persistence) — never the reverse (DIP).

use chrono::Utc;
use uuid::Uuid;

use crate::cloud::client::CloudClient;
use crate::infra::Persistence;
use crate::signal::broadcast::{Broadcaster, SignalFrame};
use crate::signal::domain::{Enrichment, Signal, SignalKind, SignalStatus, Signals};

pub struct SignalEngine {
    signals: Signals,
    project_id: String,
    persistence: Option<Persistence>,
    broadcaster: Option<Broadcaster>,
    /// Optional cloud sync client. When set, every mutation
    /// fire-and-forget pushes the signal envelope to the cloud backend. `None`
    /// (default) = no cloud sync.
    cloud: Option<CloudClient>,
}

impl SignalEngine {
    pub fn new(project_id: String) -> Self {
        Self {
            signals: Signals {
                project_id: project_id.clone(),
                signals: Vec::new(),
            },
            project_id,
            persistence: None,
            broadcaster: None,
            cloud: None,
        }
    }

    /// The project this engine belongs to — the reconcile filter's identity
    /// (sync-project-scope): only records stamped with this id merge in.
    #[must_use]
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Attach the shared broadcaster so mutations emit `family="signal"` frames
    /// through the one socket. Wired by `cli::build_unified`.
    pub fn with_broadcaster(mut self, broadcaster: Broadcaster) -> Self {
        self.broadcaster = Some(broadcaster);
        self
    }

    /// Attach a cloud client so every mutation fire-and-forget pushes the signal
    /// envelope to the cloud backend. Wired by `cli::build_unified`.
    pub fn with_cloud(mut self, client: CloudClient) -> Self {
        self.cloud = Some(client);
        self
    }

    /// Fire-and-forget: fan a mutation frame out to the broadcast socket, then
    /// push the signal envelope to the cloud. A broadcast/push error is logged
    /// and dropped — a mutation is never failed by it.
    fn record_event(&self, frame: SignalFrame) {
        if let Some(b) = &self.broadcaster {
            b.emit(&frame);
        }
        self.cloud_push_frame(&frame);
    }

    /// Fire-and-forget cloud push of the frame's signal. No-op
    /// without a cloud client OR outside a tokio runtime (so sync unit tests
    /// never panic); a push error is logged and dropped.
    fn cloud_push_frame(&self, frame: &SignalFrame) {
        let Some(client) = &self.cloud else {
            return;
        };
        let signal = match frame {
            SignalFrame::SignalCaptured { signal }
            | SignalFrame::SignalChanged { signal }
            | SignalFrame::SignalDismissed { signal } => signal,
        };
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let envelope = crate::cloud::build::from_signal(&self.project_id, signal);
        let client = client.clone();
        handle.spawn(async move {
            if let Err(e) = client.push(&envelope).await {
                tracing::warn!(target: "think_and_ship::cloud", "signal cloud push failed: {e}");
            }
        });
    }

    /// Attach a persistence handle, loading any prior signal cache for this
    /// project off disk first (so state accumulates across conversations).
    pub fn with_persistence(mut self, persistence: Persistence) -> Self {
        match persistence.load::<Signals>(&self.project_id) {
            Ok(Some(loaded)) => {
                eprintln!(
                    "think-and-ship: loaded {} signal(s) from disk",
                    loaded.signals.len()
                );
                self.signals = loaded;
            }
            Ok(None) => {}
            Err(e) => tracing::warn!("think-and-ship: signal load failed: {e}"),
        }
        self.persistence = Some(persistence);
        self
    }

    /// Locked merge-on-save (family-stores-merge-on-save): signals another
    /// live process acked are folded in, never clobbered. Merge policy:
    /// [`crate::signal::domain::merge_signal_stores`].
    fn persist(&self) {
        if let Some(p) = &self.persistence
            && let Err(e) = p.save_merging(
                &self.project_id,
                &self.signals,
                crate::signal::domain::merge_signal_stores,
            )
        {
            tracing::warn!("think-and-ship: signal persist failed: {e}");
        }
    }

    fn now() -> String {
        Utc::now().to_rfc3339()
    }

    /// Read-only access for the wire layer.
    pub fn signals(&self) -> &Signals {
        &self.signals
    }

    /// Merge a signal pulled from the cloud: insert when absent;
    /// replace an existing copy only when the incoming one has strictly
    /// progressed (`domain::signal_wins` — the same lifecycle-rank rule as the
    /// disk merge), so a stale cloud copy never rolls back a fresh local
    /// transition (reconcile-recency-guard). A SILENT merge — it
    /// does NOT emit a mutation frame or push back to the cloud; a reconcile
    /// that re-emitted would loop the pull straight back into a push. Persists.
    pub fn upsert_signal(&mut self, signal: Signal) {
        match self.index_of(&signal.id) {
            Ok(idx) => {
                if !crate::signal::domain::signal_wins(&signal, &self.signals.signals[idx]) {
                    return; // local copy is as far or further along — nothing to do
                }
                self.signals.signals[idx] = signal;
            }
            Err(_) => self.signals.signals.push(signal),
        }
        self.persist();
    }

    fn index_of(&self, id: &str) -> Result<usize, String> {
        self.signals
            .signals
            .iter()
            .position(|s| s.id == id)
            .ok_or_else(|| format!("signal '{id}' not found"))
    }

    /// Capture a new signal. Mints a UUIDv4 id and an RFC-3339 `created` stamp,
    /// starts it in `New`, and persists. (Cloud write-through happens via the
    /// attached cloud client, when one is wired.)
    pub fn capture(&mut self, kind: SignalKind, from: String, body: String) -> &Signal {
        self.capture_with_content(kind, from, body, None)
    }

    /// [`Self::capture`] carrying the optional structured body — the rich
    /// rendering sidecar the MCP seam steers writers toward. Validated by the
    /// caller; `None` is every legacy writer.
    pub fn capture_with_content(
        &mut self,
        kind: SignalKind,
        from: String,
        body: String,
        content: Option<crate::content::StructuredContent>,
    ) -> &Signal {
        let now = Self::now();
        self.signals.signals.push(Signal {
            id: Uuid::new_v4().to_string(),
            kind,
            from,
            body,
            content,
            created: now,
            status: SignalStatus::New,
            enrichment: Vec::new(),
            cross_refs: Vec::new(),
            surfaced_at: None,
            snooze_until: None,
            // Stamp origin at the record, so a later prune can prove ownership
            // rather than guess it (store-prune-think-signal).
            project_id: Some(self.project_id.clone()),
        });
        self.persist();
        let snapshot = self.signals.signals.last().unwrap().clone();
        self.record_event(SignalFrame::SignalCaptured { signal: snapshot });
        self.signals.signals.last().unwrap()
    }

    /// Fetch a signal by id.
    pub fn get(&self, id: &str) -> Result<&Signal, String> {
        let idx = self.index_of(id)?;
        Ok(&self.signals.signals[idx])
    }

    /// Move a signal to a new status, validated against the transition table.
    pub fn set_status(&mut self, id: &str, to: SignalStatus) -> Result<&Signal, String> {
        let idx = self.index_of(id)?;
        let from = self.signals.signals[idx].status;
        if !from.allows(to) {
            return Err(format!(
                "illegal transition for signal '{id}': {from:?} -> {to:?}"
            ));
        }
        self.signals.signals[idx].status = to;
        self.persist();
        Ok(&self.signals.signals[idx])
    }

    /// Append an enrichment record. Does not change status on its own — the
    /// status-advancing churn op is [`Self::research`].
    pub fn enrich(&mut self, id: &str, enrichment: Enrichment) -> Result<&Signal, String> {
        let idx = self.index_of(id)?;
        self.signals.signals[idx].enrichment.push(enrichment);
        self.persist();
        Ok(&self.signals.signals[idx])
    }

    /// Record one round of the agent "churning" on a signal: append
    /// a durable [`Enrichment`] (stamped `at = now`), advance the lifecycle
    /// toward `Researched`, and — when a motivating `think_step` is given —
    /// cross-ref it as `think:<N>` so the reasoning is auditable from the signal.
    ///
    /// Status rules (validated, never backward): `New`/`Triaged` advance to
    /// `Researched`; an already-`Researched` signal stays `Researched` (a signal
    /// can be re-researched with more enrichment); a `Surfaced` signal keeps its
    /// status (it's already past `Researched`) but still accrues enrichment;
    /// `Promoted`/`Dismissed` are terminal and rejected. `confidence` is clamped
    /// to `[0, 1]`.
    pub fn research(
        &mut self,
        id: &str,
        summary: String,
        confidence: f64,
        sources: Vec<String>,
        think_step: Option<u32>,
    ) -> Result<&Signal, String> {
        let idx = self.index_of(id)?;
        let from = self.signals.signals[idx].status;
        if matches!(from, SignalStatus::Promoted | SignalStatus::Dismissed) {
            return Err(format!(
                "signal '{id}' cannot be researched from {from:?} (terminal state)"
            ));
        }

        // Append the enrichment, time-stamped by the engine.
        self.signals.signals[idx].enrichment.push(Enrichment {
            think_step,
            sources,
            summary,
            confidence: confidence.clamp(0.0, 1.0),
            at: Self::now(),
        });

        // Advance toward Researched without ever moving backward.
        if matches!(from, SignalStatus::New | SignalStatus::Triaged) {
            self.signals.signals[idx].status = SignalStatus::Researched;
        }

        // Cross-ref the motivating think step (deduped, validated wire form).
        if let Some(n) = think_step {
            let wire = crate::infra::CrossRef::ThinkStep(n).to_wire();
            if !self.signals.signals[idx].cross_refs.contains(&wire) {
                self.signals.signals[idx].cross_refs.push(wire);
            }
        }

        self.persist();
        let snapshot = self.signals.signals[idx].clone();
        self.record_event(SignalFrame::SignalChanged { signal: snapshot });
        Ok(&self.signals.signals[idx])
    }

    /// Attach a validated cross-ref to a signal (deduped). Powers `signal_link`
    /// and the reciprocal `chunk:<id>` written by promotion. The ref is
    /// validated + normalized via [`crate::infra::CrossRef`], so only the forms
    /// that type accepts get through: `think:`, `task:`, `action:`, `check:`,
    /// `chunk:`, `signal:` and `ext:<provider>/<external_id>`.
    ///
    /// The `ext:` form arrived with the tracker seam and this list did not
    /// follow it for several phases — the behaviour was always right, because
    /// validation delegates, but the comment said otherwise. It is what a
    /// tracker divergence concern uses to name the ticket it is about.
    pub fn link(&mut self, id: &str, cross_ref: &str) -> Result<&Signal, String> {
        let normalized = crate::infra::CrossRef::from_wire(cross_ref)
            .map(|r| r.to_wire())
            .map_err(|e| format!("invalid cross-ref '{cross_ref}': {e}"))?;
        let idx = self.index_of(id)?;
        if !self.signals.signals[idx].cross_refs.contains(&normalized) {
            self.signals.signals[idx].cross_refs.push(normalized);
            self.persist();
            let snapshot = self.signals.signals[idx].clone();
            self.record_event(SignalFrame::SignalChanged { signal: snapshot });
        }
        Ok(&self.signals.signals[idx])
    }

    /// Mark a signal `Promoted`. Only a `Researched` or `Surfaced` signal can be
    /// promoted — a stakeholder opportunity that's been validated becomes a
    /// roadmap solution. This is a higher-level op than [`Self::set_status`]
    /// (it may skip `Surfaced`); the bidirectional cross-ref wiring lives at the
    /// service layer. Idempotency (don't re-promote) is enforced there via the
    /// existing `chunk:` cross-ref.
    pub fn promote(&mut self, id: &str) -> Result<&Signal, String> {
        let idx = self.index_of(id)?;
        let from = self.signals.signals[idx].status;
        if !matches!(from, SignalStatus::Researched | SignalStatus::Surfaced) {
            return Err(format!(
                "signal '{id}' cannot be promoted from {from:?} (must be researched or surfaced)"
            ));
        }
        self.signals.signals[idx].status = SignalStatus::Promoted;
        self.persist();
        let snapshot = self.signals.signals[idx].clone();
        self.record_event(SignalFrame::SignalChanged { signal: snapshot });
        Ok(&self.signals.signals[idx])
    }

    // ── Organic surfacing ───────────────────────────────────────────────────

    /// A signal's surfacing confidence — the MAX confidence over its enrichment
    /// trail. A signal with no enrichment scores 0.0, so an un-researched signal
    /// can never clear a positive threshold (a structural guard, not a check).
    fn surfacing_confidence(s: &Signal) -> f64 {
        s.enrichment
            .iter()
            .map(|e| e.confidence)
            .fold(0.0_f64, f64::max)
    }

    /// Whether `s` is currently snoozed (its `snooze_until` is in the future).
    fn is_snoozed(s: &Signal, now: chrono::DateTime<chrono::Utc>) -> bool {
        match &s.snooze_until {
            Some(t) => chrono::DateTime::parse_from_rfc3339(t)
                .map(|dt| dt.with_timezone(&Utc) > now)
                .unwrap_or(false),
            None => false,
        }
    }

    /// Signals ready to raise to the human under earned-interruption
    /// discipline: `status == Researched`, max-enrichment-confidence ≥
    /// `min_confidence`, not already surfaced, not snoozed, and — when `hints` is
    /// non-empty — relevant (the body or a cross-ref contains a hint substring,
    /// case-insensitive). Relevance is HINT-DRIVEN: the caller supplies the
    /// active files/keywords, since the engine has no editor-state oracle.
    /// Highest-confidence first, capped at `limit`.
    pub fn pending(&self, min_confidence: f64, hints: &[String], limit: usize) -> Vec<&Signal> {
        let now = Utc::now();
        let lc_hints: Vec<String> = hints
            .iter()
            .map(|h| h.to_lowercase())
            .filter(|h| !h.is_empty())
            .collect();
        let mut out: Vec<&Signal> = self
            .signals
            .signals
            .iter()
            .filter(|s| {
                s.status == SignalStatus::Researched
                    && s.surfaced_at.is_none()
                    && Self::surfacing_confidence(s) >= min_confidence
                    && !Self::is_snoozed(s, now)
                    && (lc_hints.is_empty() || {
                        let body = s.body.to_lowercase();
                        lc_hints.iter().any(|h| {
                            body.contains(h)
                                || s.cross_refs.iter().any(|r| r.to_lowercase().contains(h))
                        })
                    })
            })
            .collect();
        out.sort_by(|a, b| {
            Self::surfacing_confidence(b)
                .partial_cmp(&Self::surfacing_confidence(a))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        out.truncate(limit);
        out
    }

    /// How many signals are currently ready to raise — the count source for the
    /// `roadmap_status` pending-signal badge. Same gate as
    /// [`Self::pending`] with no relevance hints.
    pub fn pending_count(&self, min_confidence: f64) -> usize {
        self.pending(min_confidence, &[], usize::MAX).len()
    }

    /// Mark a signal as surfaced (raised to the human): `Researched → Surfaced`
    /// and stamp `surfaced_at`, so `pending` won't re-raise it.
    pub fn surface(&mut self, id: &str) -> Result<&Signal, String> {
        let idx = self.index_of(id)?;
        let from = self.signals.signals[idx].status;
        if from != SignalStatus::Researched {
            return Err(format!(
                "signal '{id}' cannot be surfaced from {from:?} (must be researched)"
            ));
        }
        self.signals.signals[idx].status = SignalStatus::Surfaced;
        self.signals.signals[idx].surfaced_at = Some(Self::now());
        self.persist();
        let snapshot = self.signals.signals[idx].clone();
        self.record_event(SignalFrame::SignalChanged { signal: snapshot });
        Ok(&self.signals.signals[idx])
    }

    /// Snooze a signal: suppress it from `pending` for `minutes`
    /// (`snooze_until = now + minutes`). Status is unchanged.
    pub fn snooze(&mut self, id: &str, minutes: i64) -> Result<&Signal, String> {
        let idx = self.index_of(id)?;
        let until = Utc::now() + chrono::Duration::minutes(minutes.max(0));
        self.signals.signals[idx].snooze_until = Some(until.to_rfc3339());
        self.persist();
        let snapshot = self.signals.signals[idx].clone();
        self.record_event(SignalFrame::SignalChanged { signal: snapshot });
        Ok(&self.signals.signals[idx])
    }

    /// Ignore a signal: dismiss it (validated terminal transition).
    pub fn ignore(&mut self, id: &str) -> Result<&Signal, String> {
        self.set_status(id, SignalStatus::Dismissed)?;
        let idx = self.index_of(id)?;
        let snapshot = self.signals.signals[idx].clone();
        self.record_event(SignalFrame::SignalDismissed { signal: snapshot });
        Ok(&self.signals.signals[idx])
    }

    /// A bounded JSON snapshot: counts by status + a capped, newest-first list
    /// of signal summaries. Mirrors `RoadmapEngine::status`'s "never blow the
    /// MCP output limit" discipline.
    pub fn status(&self) -> serde_json::Value {
        use SignalStatus::*;
        let count = |s: SignalStatus| {
            self.signals
                .signals
                .iter()
                .filter(|x| x.status == s)
                .count()
        };

        let mut recent: Vec<&Signal> = self.signals.signals.iter().collect();
        // Newest first by `created` (RFC-3339 sorts lexically).
        recent.sort_by(|a, b| b.created.cmp(&a.created));
        let listed: Vec<_> = recent
            .iter()
            .take(Self::STATUS_LIST_CAP)
            .map(|s| {
                serde_json::json!({
                    "id": s.id,
                    "kind": s.kind,
                    "status": s.status,
                    "from": s.from,
                    "body": Self::truncate(&s.body),
                    "created": s.created,
                })
            })
            .collect();
        let omitted = self
            .signals
            .signals
            .len()
            .saturating_sub(Self::STATUS_LIST_CAP);

        serde_json::json!({
            "project_id": self.project_id,
            "counts": {
                "new": count(New),
                "triaged": count(Triaged),
                "researched": count(Researched),
                "surfaced": count(Surfaced),
                "promoted": count(Promoted),
                "dismissed": count(Dismissed),
                "total": self.signals.signals.len(),
            },
            "signals": listed,
            "omitted": omitted,
        })
    }

    /// Max signals listed by `status()` before the rest are summarized as a
    /// count.
    const STATUS_LIST_CAP: usize = 60;
    /// Max body length echoed in a `status()` row.
    const STATUS_BODY_LEN: usize = 140;

    fn truncate(body: &str) -> String {
        if body.chars().count() <= Self::STATUS_BODY_LEN {
            return body.to_string();
        }
        let cut: String = body.chars().take(Self::STATUS_BODY_LEN).collect();
        format!("{}…", cut.trim_end())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infra::{Domain, Persistence, PersistenceConfig};
    use tempfile::TempDir;

    fn engine() -> SignalEngine {
        SignalEngine::new("proj".into())
    }

    #[test]
    fn capture_mints_id_and_starts_new() {
        let mut e = engine();
        let s = e.capture(SignalKind::Bug, "dana".into(), "it crashes".into());
        assert_eq!(s.status, SignalStatus::New);
        assert!(!s.id.is_empty());
        assert!(!s.created.is_empty());
        assert_eq!(s.kind, SignalKind::Bug);
    }

    #[test]
    fn get_finds_captured_and_errors_on_unknown() {
        let mut e = engine();
        let id = e
            .capture(SignalKind::Idea, "x".into(), "do the thing".into())
            .id
            .clone();
        assert_eq!(e.get(&id).unwrap().body, "do the thing");
        assert!(e.get("ghost").unwrap_err().contains("not found"));
    }

    #[test]
    fn set_status_enforces_transition_table() {
        let mut e = engine();
        let id = e
            .capture(SignalKind::Concern, "x".into(), "hmm".into())
            .id
            .clone();
        e.set_status(&id, SignalStatus::Triaged).unwrap();
        e.set_status(&id, SignalStatus::Researched).unwrap();
        // researched -> new is illegal.
        let err = e.set_status(&id, SignalStatus::New).unwrap_err();
        assert!(err.contains("illegal transition"));
    }

    #[test]
    fn status_counts_by_lifecycle_state() {
        let mut e = engine();
        e.capture(SignalKind::Bug, "a".into(), "one".into());
        let id = e
            .capture(SignalKind::Idea, "b".into(), "two".into())
            .id
            .clone();
        e.set_status(&id, SignalStatus::Triaged).unwrap();
        let s = e.status();
        assert_eq!(s["counts"]["total"], 2);
        assert_eq!(s["counts"]["new"], 1);
        assert_eq!(s["counts"]["triaged"], 1);
        assert_eq!(s["signals"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn research_appends_enrichment_advances_and_crossrefs_think() {
        let mut e = engine();
        let id = e
            .capture(SignalKind::Concern, "dana".into(), "perf is bad".into())
            .id
            .clone(); // New
        let s = e
            .research(
                &id,
                "profiled it; the export is O(n^2)".into(),
                0.8,
                vec!["sym-roadmap::export".into()],
                Some(69),
            )
            .unwrap();
        // Advanced New -> Researched.
        assert_eq!(s.status, SignalStatus::Researched);
        // Enrichment recorded with stamped fields.
        assert_eq!(s.enrichment.len(), 1);
        assert_eq!(s.enrichment[0].summary, "profiled it; the export is O(n^2)");
        assert_eq!(s.enrichment[0].confidence, 0.8);
        assert_eq!(s.enrichment[0].think_step, Some(69));
        assert!(!s.enrichment[0].at.is_empty());
        // The motivating think step is cross-ref'd onto the signal.
        assert!(s.cross_refs.contains(&"think:69".to_string()));
    }

    #[test]
    fn research_is_idempotent_on_status_and_accrues_enrichment() {
        let mut e = engine();
        let id = e
            .capture(SignalKind::Idea, "x".into(), "dark mode".into())
            .id
            .clone();
        e.research(&id, "round one".into(), 0.5, vec![], None)
            .unwrap();
        let s = e
            .research(&id, "round two".into(), 0.9, vec![], None)
            .unwrap();
        // Status stays Researched; both enrichments are kept.
        assert_eq!(s.status, SignalStatus::Researched);
        assert_eq!(s.enrichment.len(), 2);
    }

    #[test]
    fn research_clamps_confidence_and_rejects_terminal() {
        let mut e = engine();
        let id = e
            .capture(SignalKind::Bug, "x".into(), "boom".into())
            .id
            .clone();
        // Confidence above 1.0 is clamped.
        let s = e
            .research(&id, "found it".into(), 5.0, vec![], None)
            .unwrap();
        assert_eq!(s.enrichment[0].confidence, 1.0);
        // Dismiss it (terminal), then research must be rejected.
        e.set_status(&id, SignalStatus::Dismissed).unwrap();
        let err = e
            .research(&id, "too late".into(), 0.5, vec![], None)
            .unwrap_err();
        assert!(err.contains("terminal"));
    }

    #[test]
    fn research_enrichment_is_durable_across_restart() {
        let tmp = TempDir::new().unwrap();
        let cfg = PersistenceConfig::from_env()
            .with_data_dir(tmp.path().to_path_buf())
            .enabled(true);
        let id = {
            let mut e = SignalEngine::new("proj".into())
                .with_persistence(Persistence::new(&cfg, Domain::Signal));
            let id = e
                .capture(SignalKind::Feedback, "x".into(), "nice".into())
                .id
                .clone();
            e.research(&id, "grounded it".into(), 0.7, vec!["url".into()], Some(42))
                .unwrap();
            id
        };
        let e2 = SignalEngine::new("proj".into())
            .with_persistence(Persistence::new(&cfg, Domain::Signal));
        let s = e2.get(&id).unwrap();
        assert_eq!(s.status, SignalStatus::Researched);
        assert_eq!(s.enrichment.len(), 1);
        assert_eq!(s.enrichment[0].think_step, Some(42));
        assert!(s.cross_refs.contains(&"think:42".to_string()));
    }

    /// Capture a signal and research it to `confidence` so it's `Researched`
    /// and surfacing-eligible. Returns its id.
    fn researched(e: &mut SignalEngine, body: &str, confidence: f64) -> String {
        let id = e
            .capture(SignalKind::Idea, "x".into(), body.into())
            .id
            .clone();
        e.research(&id, "looked into it".into(), confidence, vec![], None)
            .unwrap();
        id
    }

    #[test]
    fn pending_returns_only_researched_above_threshold() {
        let mut e = engine();
        e.capture(SignalKind::Bug, "x".into(), "raw, unresearched".into()); // New
        researched(&mut e, "low confidence", 0.2); // below threshold
        let hi = researched(&mut e, "high confidence", 0.9);
        let pending = e.pending(0.6, &[], 20);
        assert_eq!(pending.len(), 1, "only the researched, above-threshold one");
        assert_eq!(pending[0].id, hi);
    }

    #[test]
    fn pending_filters_by_relevance_hints() {
        let mut e = engine();
        researched(&mut e, "the export is slow", 0.9);
        let auth = researched(&mut e, "auth token refresh bug", 0.9);
        let hits = e.pending(0.6, &["auth".to_string()], 20);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, auth);
    }

    #[test]
    fn surface_marks_and_excludes_from_pending() {
        let mut e = engine();
        let id = researched(&mut e, "surface me", 0.9);
        assert_eq!(e.pending(0.6, &[], 20).len(), 1);
        let s = e.surface(&id).unwrap();
        assert_eq!(s.status, SignalStatus::Surfaced);
        assert!(s.surfaced_at.is_some());
        assert!(
            e.pending(0.6, &[], 20).is_empty(),
            "a surfaced signal is not re-raised"
        );
        // A non-researched signal cannot be surfaced.
        let raw = e
            .capture(SignalKind::Bug, "x".into(), "raw".into())
            .id
            .clone();
        assert!(e.surface(&raw).is_err());
    }

    #[test]
    fn snooze_suppresses_until_expiry() {
        let mut e = engine();
        let id = researched(&mut e, "snooze me", 0.9);
        e.snooze(&id, 60).unwrap();
        assert!(
            e.pending(0.6, &[], 20).is_empty(),
            "a snoozed signal is hidden"
        );
        // A zero/expired snooze no longer suppresses.
        e.snooze(&id, 0).unwrap();
        assert_eq!(
            e.pending(0.6, &[], 20).len(),
            1,
            "an expired snooze re-surfaces the signal"
        );
    }

    #[test]
    fn ignore_dismisses_and_drops_from_pending() {
        let mut e = engine();
        let id = researched(&mut e, "ignore me", 0.9);
        let s = e.ignore(&id).unwrap();
        assert_eq!(s.status, SignalStatus::Dismissed);
        assert!(e.pending(0.6, &[], 20).is_empty());
    }

    #[test]
    fn pending_count_counts_only_ready_signals() {
        let mut e = engine();
        researched(&mut e, "ready one", 0.9);
        researched(&mut e, "ready two", 0.7);
        researched(&mut e, "too low", 0.2); // below threshold
        e.capture(SignalKind::Bug, "x".into(), "raw".into()); // un-researched
        assert_eq!(e.pending_count(0.6), 2);
    }

    #[test]
    fn persistence_round_trips_across_engine_instances() {
        let tmp = TempDir::new().unwrap();
        let cfg = PersistenceConfig::from_env()
            .with_data_dir(tmp.path().to_path_buf())
            .enabled(true);

        let id = {
            let mut e = SignalEngine::new("proj".into())
                .with_persistence(Persistence::new(&cfg, Domain::Signal));
            let id = e
                .capture(SignalKind::Feedback, "dana".into(), "nice".into())
                .id
                .clone();
            e.set_status(&id, SignalStatus::Triaged).unwrap();
            id
        };

        // A fresh engine on the same data dir loads the cache from disk.
        let e2 = SignalEngine::new("proj".into())
            .with_persistence(Persistence::new(&cfg, Domain::Signal));
        assert_eq!(e2.signals().signals.len(), 1);
        let s = e2.get(&id).unwrap();
        assert_eq!(s.status, SignalStatus::Triaged);
        assert_eq!(s.from, "dana");
    }
}
