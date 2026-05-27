// Wire types mirror crates/app-tauri/src/source/mod.rs `FrontendEvent`
// and the deliberate-mcp Rust schema. Kept narrow on purpose — anything
// the UI doesn't render is left as `unknown` so we don't accumulate
// shadow definitions that drift.

export type DepEdge =
  | number
  | { step: number; relation?: "supports" | "refutes" | "depends_on" | string };

export type NextAction =
  | string
  | {
      tool?: string;
      action: string;
      parameters?: Record<string, unknown>;
      expectedOutput?: string;
    };

export interface DeliberateStep {
  step_number: number;
  estimated_total: number;
  purpose: string;
  context: string;
  thought: string;
  outcome: string;
  next_action: NextAction;
  rationale: string;

  confidence?: number;
  uncertainty_notes?: string;

  revises_step?: number;
  revision_reason?: string;
  revised_by?: number;

  is_final_step?: boolean;

  branch_from?: number;
  branch_id?: string;
  branch_name?: string;

  tools_used?: string[];
  dependencies?: DepEdge[];

  timestamp?: string;
  duration_ms?: number;
  session_id?: string;
  pinned?: boolean;
}

export type BranchStatus = "active" | "merged" | "abandoned";

export interface Branch {
  id: string;
  name: string;
  from_step: number;
  steps: DeliberateStep[];
  status: BranchStatus;
  created_at: string;
  depth: number;
  merged_into?: number;
}

export interface DeliberateHistory {
  steps: DeliberateStep[];
  branches?: Branch[];
  completed: boolean;
  session_id?: string;
  created_at?: string;
  updated_at?: string;
  metadata?: {
    total_duration_ms?: number;
    revisions_count?: number;
    branches_created?: number;
    tools_used?: string[];
    /** Project id stamped at first-step time by the server. Lets the
     * viewer group sessions by project without parsing the session id.
     * Optional for back-compat with sessions written before the round-3
     * fix. */
    project_id?: string;
  };
}

/** Separator used by the server to namespace caller-supplied session
 * ids under the project — `<project>__<rest>`. Mirrors the Rust
 * constant `PROJECT_SEP` in src/config.rs. */
export const PROJECT_SEP = "__";

/** Synthetic session id selected when the user picks `(all)` in the
 * BranchNav's session list. The store materializes a stitched history
 * by concatenating every session under the current project. */
export const ALL_SESSIONS_SUFFIX = "__ALL__";

/** Extract the project id from a session id + optional history. Prefers
 * the explicit `metadata.project_id` stamp; falls back to splitting on
 * `__` or matching the legacy `<base>-<6hex>(-rotation)?` shape. */
export function projectOf(sessionId: string, history?: DeliberateHistory): string {
  const stamped = history?.metadata?.project_id;
  if (stamped) return stamped;
  const sepIdx = sessionId.indexOf(PROJECT_SEP);
  if (sepIdx > 0) return sessionId.slice(0, sepIdx);
  // Legacy rotation: `<project>-YYYYMMDD-HHMMSS-XXXX`.
  const m = sessionId.match(
    /^([A-Za-z0-9_.-]+?-[0-9a-f]{6})(?:-\d{8}-\d{6}-[A-Za-z0-9]{4})?$/
  );
  return m ? m[1] : "_legacy";
}

/** Return the user-facing short name for a session inside its project.
 * Strips the `<project>__` prefix when present; otherwise returns the
 * full id (legacy rotations etc). */
export function sessionLabel(sessionId: string, projectId: string): string {
  const prefix = `${projectId}${PROJECT_SEP}`;
  if (sessionId.startsWith(prefix)) {
    return sessionId.slice(prefix.length) || "(default)";
  }
  if (sessionId === projectId) return "(default)";
  return sessionId;
}

/** The virtual id used to address the stitched-all-sessions view for a
 * given project. */
export function allSessionsId(projectId: string): string {
  return `${projectId}${ALL_SESSIONS_SUFFIX}`;
}

export type SourceMode = "none" | "file" | "socket" | "socket_and_file";

export interface SourceInfo {
  mode: SourceMode;
  socket_path?: string | null;
  data_dir?: string | null;
  persistence_enabled: boolean;
}

export interface Snapshot {
  session_id: string;
  history: DeliberateHistory;
  branches: Branch[];
}

export interface SnapshotResponse {
  source: SourceInfo;
  active_session: string;
  sessions: Snapshot[];
}

// Discriminated union mirroring FrontendEvent in mod.rs.
export type FrontendEvent =
  | {
      kind: "snapshot";
      session_id: string;
      history: DeliberateHistory;
      branches: Branch[];
    }
  | { kind: "step_appended"; session_id: string; step: DeliberateStep }
  | {
      kind: "step_revised";
      session_id: string;
      revised_step: number;
      by_step: number;
    }
  | {
      kind: "pin_changed";
      session_id: string;
      step_number: number;
      pinned: boolean;
    }
  | { kind: "estimate_revised"; session_id: string; old: number; new: number }
  | {
      kind: "branch_status_changed";
      session_id: string;
      branch_id: string;
      status: BranchStatus;
      merged_into?: number;
    }
  | { kind: "cleared"; session_id: string }
  | { kind: "source_changed"; mode: SourceMode };

export const PURPOSE_VARS: Record<string, string> = {
  analysis: "var(--purpose-analysis)",
  decision: "var(--purpose-decision)",
  action: "var(--purpose-action)",
  hypothesis: "var(--purpose-hypothesis)",
  reflection: "var(--purpose-reflection)",
  validation: "var(--purpose-validation)",
  correction: "var(--purpose-correction)",
  summary: "var(--purpose-summary)",
  exploration: "var(--purpose-exploration)",
  planning: "var(--purpose-planning)",
};

export function purposeColor(purpose: string): string {
  return PURPOSE_VARS[purpose] ?? "var(--purpose-unknown)";
}
