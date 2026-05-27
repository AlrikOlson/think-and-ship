// Trace store. Single source of truth for the rendered session. Built on
// Svelte 5 runes so consumers read `state.sessions` directly and get
// fine-grained reactivity without subscribe boilerplate.

import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  Branch,
  DeliberateHistory,
  DeliberateStep,
  FrontendEvent,
  SnapshotResponse,
  SourceInfo,
  SourceMode,
} from "../types";
import {
  ALL_SESSIONS_SUFFIX,
  allSessionsId,
  projectOf,
} from "../types";

interface SessionRecord {
  history: DeliberateHistory;
  branches: Map<string, Branch>;
  // Cached step lookup; rebuilt on snapshot/append.
  index: Map<number, DeliberateStep>;
}

function emptyHistory(): DeliberateHistory {
  return { steps: [], branches: [], completed: false };
}

function emptySession(): SessionRecord {
  return { history: emptyHistory(), branches: new Map(), index: new Map() };
}

function indexFor(history: DeliberateHistory): Map<number, DeliberateStep> {
  const m = new Map<number, DeliberateStep>();
  for (const s of history.steps) m.set(s.step_number, s);
  return m;
}

function branchesFromList(list: Branch[]): Map<string, Branch> {
  const m = new Map<string, Branch>();
  for (const b of list) m.set(b.id, b);
  return m;
}

export interface TraceFilters {
  hideRevised: boolean;
  onlyHypothesis: boolean;
  onlyRefuted: boolean;
}

/// Step selection. Step numbers are unique project-wide (the engine
/// rejects duplicates and renumbers legacy data on load), so the bare
/// `step_number` identifies a step in both single-session and stitched
/// (`__ALL__`) views.
export interface StepRef {
  stepNumber: number;
}

export class TraceStore {
  // Runes — accessed directly by components.
  sessions = $state<Map<string, SessionRecord>>(new Map());
  active = $state<string>("");
  source = $state<SourceInfo>({ mode: "none", persistence_enabled: false });
  selectedRef = $state<StepRef | null>(null);
  view = $state<"trace" | "graph" | "checkpoint">("trace");
  lastUpdateMs = $state<number>(0);
  filters = $state<TraceFilters>({
    hideRevised: false,
    onlyHypothesis: false,
    onlyRefuted: false,
  });

  // Derived shortcuts.
  current = $derived<SessionRecord>(this.resolveCurrent());

  /// Display-only view of the selected step's number. Components that
  /// only need to render `#N` (StatusBar, Timeline highlight) use this;
  /// components that need to actually look up the step's data use
  /// `selectedStepData`.
  selectedStep = $derived<number | null>(this.selectedRef?.stepNumber ?? null);

  /// All distinct project ids across loaded sessions, in alpha order.
  /// Sessions with no metadata.project_id and no parseable id fall
  /// under the bucket `_legacy`.
  projects = $derived<string[]>(this.computeProjects());

  /// The project id of the currently-active session (resolves the
  /// virtual `__ALL__` suffix back to the project).
  currentProject = $derived<string>(this.computeCurrentProject());

  /// Sessions belonging to the current project, sorted: bare project
  /// id first (the default), then alphabetical by suffix.
  sessionsInProject = $derived<string[]>(this.computeSessionsInProject());

  selectedStepData = $derived<DeliberateStep | null>(this.resolveSelectedStep());

  private unlisteners: UnlistenFn[] = [];

  async init(): Promise<void> {
    const snap = (await invoke("get_snapshot")) as SnapshotResponse;
    const next = new Map<string, SessionRecord>();
    for (const s of snap.sessions) {
      next.set(s.session_id, {
        history: s.history,
        branches: branchesFromList(s.branches),
        index: indexFor(s.history),
      });
    }
    if (!next.has(snap.active_session)) {
      next.set(snap.active_session, emptySession());
    }
    this.sessions = next;
    this.active = snap.active_session;
    this.source = snap.source;
    this.lastUpdateMs = Date.now();

    const unlisten = await listen<FrontendEvent>("trace://event", (e) =>
      this.apply(e.payload)
    );
    this.unlisteners.push(unlisten);
  }

  dispose(): void {
    for (const u of this.unlisteners) u();
    this.unlisteners = [];
  }

  /// Select a step by its project-wide number. Passing `null` clears
  /// the selection.
  selectStep(n: number | null): void {
    this.selectedRef = n === null ? null : { stepNumber: n };
  }

  /// Select by index into `current.history.steps` (the displayed order).
  /// Used by j/k nav.
  selectStepAt(idx: number): void {
    const s = this.current.history.steps[idx];
    if (!s) return;
    this.selectedRef = { stepNumber: s.step_number };
  }

  setView(v: "trace" | "graph" | "checkpoint"): void {
    this.view = v;
  }

  setFilter<K extends keyof TraceFilters>(key: K, value: TraceFilters[K]): void {
    this.filters = { ...this.filters, [key]: value };
  }

  /// Switch the active session. Clears step selection because the
  /// previously-selected step may not exist in the new session.
  setActive(id: string): void {
    if (this.active === id) return;
    this.active = id;
    this.selectedRef = null;
  }

  /// Switch to a project: select the project's bare default session
  /// when it exists, else the first session under that project. Drops
  /// any current step selection.
  setProject(project: string): void {
    const sessions = this.computeSessionsForProject(project);
    if (sessions.length === 0) return;
    // Prefer the bare project id (the project's default session).
    const preferred = sessions.find((id) => id === project) ?? sessions[0];
    this.setActive(preferred);
  }

  /// Stitched view: address `<project>__ALL__` to render every session
  /// under `project` concatenated by step `timestamp` (falling back to
  /// session-then-step order).
  setProjectAll(project: string): void {
    if (this.computeSessionsForProject(project).length === 0) return;
    this.active = allSessionsId(project);
    this.selectedRef = null;
  }

  /// Sessions sorted for display: the default (empty-string key) first,
  /// then the rest alphabetically. The result is shown as-is by the
  /// title bar's switcher.
  listSessions(): { id: string; label: string; steps: number }[] {
    const out: { id: string; label: string; steps: number }[] = [];
    for (const [id, rec] of this.sessions) {
      out.push({
        id,
        label: id === "" ? "_default" : id,
        steps: rec.history.steps.length,
      });
    }
    out.sort((a, b) => {
      if (a.id === "" && b.id !== "") return -1;
      if (b.id === "" && a.id !== "") return 1;
      return a.id.localeCompare(b.id);
    });
    return out;
  }

  // Default to the empty-key session if explicit session isn't yet known.
  private touch(sessionId: string): SessionRecord {
    let rec = this.sessions.get(sessionId);
    if (!rec) {
      rec = emptySession();
      this.sessions.set(sessionId, rec);
      // Ensure reactivity picks up the new key. Runes-tracked maps emit
      // change notifications on .set; the `=` reassignment keeps a path
      // for older runtimes that don't fully track Map mutation.
      this.sessions = new Map(this.sessions);
    }
    return rec;
  }

  private apply(evt: FrontendEvent): void {
    this.lastUpdateMs = Date.now();
    switch (evt.kind) {
      case "snapshot": {
        const rec: SessionRecord = {
          history: evt.history,
          branches: branchesFromList(evt.branches),
          index: indexFor(evt.history),
        };
        this.sessions.set(evt.session_id, rec);
        this.sessions = new Map(this.sessions);
        break;
      }
      case "step_appended": {
        const rec = this.touch(evt.session_id);
        if (!rec.index.has(evt.step.step_number)) {
          rec.history.steps = [...rec.history.steps, evt.step];
          rec.index.set(evt.step.step_number, evt.step);
        }
        // Reassign the wrapper to trigger derived recompute.
        this.sessions = new Map(this.sessions);
        break;
      }
      case "step_revised": {
        const rec = this.touch(evt.session_id);
        const target = rec.index.get(evt.revised_step);
        if (target) {
          target.revised_by = evt.by_step;
        }
        this.sessions = new Map(this.sessions);
        break;
      }
      case "pin_changed": {
        const rec = this.touch(evt.session_id);
        const target = rec.index.get(evt.step_number);
        if (target) {
          target.pinned = evt.pinned ? true : undefined;
        }
        this.sessions = new Map(this.sessions);
        break;
      }
      case "estimate_revised": {
        const rec = this.touch(evt.session_id);
        const last = rec.history.steps[rec.history.steps.length - 1];
        if (last) {
          last.estimated_total = evt.new;
        }
        this.sessions = new Map(this.sessions);
        break;
      }
      case "branch_status_changed": {
        const rec = this.touch(evt.session_id);
        const b = rec.branches.get(evt.branch_id);
        if (b) {
          b.status = evt.status;
          b.merged_into = evt.status === "merged" ? evt.merged_into : undefined;
        }
        this.sessions = new Map(this.sessions);
        break;
      }
      case "cleared": {
        const rec = this.touch(evt.session_id);
        rec.history = emptyHistory();
        rec.branches.clear();
        rec.index.clear();
        this.sessions = new Map(this.sessions);
        if (this.selectedRef !== null) this.selectedRef = null;
        break;
      }
      case "source_changed": {
        this.source = { ...this.source, mode: evt.mode };
        break;
      }
    }
  }

  // ── Selection resolution ───────────────────────────────────────────

  /// Look up the selected step's data. Step numbers are unique
  /// project-wide, so `index` covers stitched and single views alike.
  private resolveSelectedStep(): DeliberateStep | null {
    const ref = this.selectedRef;
    if (!ref) return null;
    return this.current.index.get(ref.stepNumber) ?? null;
  }

  // ── Project / stitched-view derivation ─────────────────────────────

  /// `current` resolves the active id to a SessionRecord. The virtual
  /// `<project>__ALL__` id is materialized on demand: we build a
  /// stitched history from every session whose project matches.
  private resolveCurrent(): SessionRecord {
    const id = this.active;
    if (id.endsWith(ALL_SESSIONS_SUFFIX)) {
      const project = id.slice(0, -ALL_SESSIONS_SUFFIX.length);
      return this.buildStitched(project);
    }
    return this.sessions.get(id) ?? emptySession();
  }

  private buildStitched(project: string): SessionRecord {
    const sessionIds = this.computeSessionsForProject(project);
    if (sessionIds.length === 0) return emptySession();
    // Concatenate steps from every session in order. Steps within a
    // session keep their server-assigned ordering; across sessions we
    // sort by first-step timestamp so the natural reading-order is
    // earlier-session-first.
    const annotated: { sessionId: string; ts: string; steps: DeliberateStep[] }[] = [];
    for (const sid of sessionIds) {
      const rec = this.sessions.get(sid);
      if (!rec || rec.history.steps.length === 0) continue;
      const ts = rec.history.steps[0]?.timestamp ?? rec.history.created_at ?? "";
      annotated.push({ sessionId: sid, ts, steps: rec.history.steps });
    }
    annotated.sort((a, b) => a.ts.localeCompare(b.ts));
    const merged: DeliberateStep[] = [];
    for (const a of annotated) {
      for (const s of a.steps) {
        // Stamp session_id on each step so the Timeline can render
        // the per-row session badge in stitched mode without an extra
        // lookup. Mutation-free; we clone before stamping.
        merged.push({ ...s, session_id: s.session_id ?? a.sessionId });
      }
    }
    // Branches: union across sessions. Keys may collide (same branch
    // id used in two sessions) — that's fine, the stitched view treats
    // them as separate visual lanes via the session badge.
    const branches = new Map<string, Branch>();
    for (const sid of sessionIds) {
      const rec = this.sessions.get(sid);
      if (!rec) continue;
      for (const [bid, b] of rec.branches) {
        if (!branches.has(`${sid}::${bid}`)) branches.set(bid, b);
      }
    }
    const history: DeliberateHistory = {
      steps: merged,
      branches: [],
      completed: annotated.every((a) => {
        const rec = this.sessions.get(a.sessionId);
        return rec?.history.completed ?? false;
      }),
      metadata: { project_id: project },
    };
    return { history, branches, index: indexFor(history) };
  }

  private computeProjects(): string[] {
    const set = new Set<string>();
    for (const [id, rec] of this.sessions) {
      set.add(projectOf(id, rec.history));
    }
    return [...set].sort((a, b) => a.localeCompare(b));
  }

  private computeCurrentProject(): string {
    const id = this.active;
    if (id.endsWith(ALL_SESSIONS_SUFFIX)) {
      return id.slice(0, -ALL_SESSIONS_SUFFIX.length);
    }
    const rec = this.sessions.get(id);
    return projectOf(id, rec?.history);
  }

  private computeSessionsInProject(): string[] {
    return this.computeSessionsForProject(this.computeCurrentProject());
  }

  private computeSessionsForProject(project: string): string[] {
    const out: string[] = [];
    for (const [id, rec] of this.sessions) {
      if (projectOf(id, rec.history) === project) out.push(id);
    }
    out.sort((a, b) => {
      // Bare project id first (the "default" session for the project).
      if (a === project) return -1;
      if (b === project) return 1;
      return a.localeCompare(b);
    });
    return out;
  }
}

export type ViewMode = "trace" | "graph" | "checkpoint";

export function sourceLabel(mode: SourceMode): string {
  switch (mode) {
    case "socket":
      return "SOCKET";
    case "file":
      return "FILE";
    case "socket_and_file":
      return "SOCKET+FILE";
    case "none":
      return "NO SOURCE";
  }
}

export const traceStore = new TraceStore();
