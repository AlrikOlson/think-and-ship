<script lang="ts">
  import { traceStore } from "../store/trace.svelte";
  import type { Branch, DeliberateStep } from "../types";
  import { allSessionsId, sessionLabel } from "../types";

  // Synthetic row for the main lane — every trace has main rows even if
  // no Branch entries exist. We build a unified list so the rendering
  // pass doesn't have to special-case main everywhere.
  interface NavRow {
    id: string; // "" for main; otherwise branch.id
    label: string;
    status: "active" | "merged" | "abandoned";
    steps: number;
    firstStep: number | null;
    statusHint: "" | "m" | "a";
  }

  const steps = $derived(traceStore.current.history.steps);
  const branchMap = $derived(traceStore.current.branches);

  const rows = $derived.by<NavRow[]>(() => {
    const out: NavRow[] = [];
    // The synthetic `main` row only matters when at least one real
    // branch exists — otherwise the whole timeline IS main and the row
    // would just be redundant chrome.
    if (branchMap.size === 0) return out;
    const mainSteps = steps.filter((s: DeliberateStep) => !s.branch_id);
    out.push({
      id: "",
      label: "main",
      status: "active",
      steps: mainSteps.length,
      firstStep: mainSteps[0]?.step_number ?? null,
      statusHint: "",
    });
    // Sort branches by id for stable order. Same ordering the Timeline
    // uses (insertion order via laneAssignment), but here id-sorted is
    // fine because BranchNav isn't trying to match lane indices.
    const branches: Branch[] = [...branchMap.values()].sort((a, b) =>
      a.id.localeCompare(b.id)
    );
    for (const b of branches) {
      const bSteps = steps.filter((s: DeliberateStep) => s.branch_id === b.id);
      out.push({
        id: b.id,
        label: b.name && b.name !== b.id ? b.name : b.id,
        status: b.status,
        steps: bSteps.length,
        firstStep: bSteps[0]?.step_number ?? null,
        statusHint:
          b.status === "merged" ? "m" : b.status === "abandoned" ? "a" : "",
      });
    }
    return out;
  });

  // Filters add no value on tiny traces — hide the strip until the
  // trace is big enough that filtering becomes useful.
  const showFilters = $derived(steps.length >= 6);

  const filters = $derived(traceStore.filters);

  // Highlight the row whose branch contains the current selection — so
  // clicking around the timeline visually surfaces which branch you're on.
  const activeId = $derived.by<string>(() => {
    const n = traceStore.selectedStep;
    if (n == null) return "";
    const s = steps.find((x) => x.step_number === n);
    return s?.branch_id ?? "";
  });

  function pick(row: NavRow): void {
    if (row.firstStep == null) return;
    traceStore.selectStep(row.firstStep);
  }

  function glyph(status: NavRow["status"]): string {
    switch (status) {
      case "active":
        return "●";
      case "merged":
        return "✓";
      case "abandoned":
        return "✕";
    }
  }

  // ── Sessions section (above branches) ──────────────────────────────
  //
  // Lists every session belonging to the currently-selected project,
  // plus a virtual `(all)` row that stitches them into one trace.

  const project = $derived(traceStore.currentProject);
  const sessionsInProject = $derived(traceStore.sessionsInProject);
  const activeSession = $derived(traceStore.active);
  const allId = $derived(allSessionsId(project));

  interface SessRow {
    id: string;
    label: string;
    stepCount: number;
    isAll: boolean;
  }

  const sessRows = $derived.by<SessRow[]>(() => {
    const out: SessRow[] = [];
    // `(all)` only makes sense when there's more than one session.
    if (sessionsInProject.length > 1) {
      const total = sessionsInProject.reduce((sum, id) => {
        return sum + (traceStore.sessions.get(id)?.history.steps.length ?? 0);
      }, 0);
      out.push({ id: allId, label: "(all)", stepCount: total, isAll: true });
    }
    for (const id of sessionsInProject) {
      const rec = traceStore.sessions.get(id);
      out.push({
        id,
        label: sessionLabel(id, project),
        stepCount: rec?.history.steps.length ?? 0,
        isAll: false,
      });
    }
    return out;
  });

  function pickSession(row: SessRow): void {
    if (row.isAll) {
      traceStore.setProjectAll(project);
    } else {
      traceStore.setActive(row.id);
    }
  }
</script>

<nav class="wrap">
  {#if sessRows.length > 1}
    <div class="header">sessions</div>
    <ul class="sessions">
      {#each sessRows as row (row.id)}
        <li>
          <button
            class="row"
            class:selected={activeSession === row.id}
            class:is-all={row.isAll}
            onclick={() => pickSession(row)}
          >
            <span class="glyph">{row.isAll ? "≡" : "○"}</span>
            <span class="label" title={row.label}>{row.label}</span>
            <span class="count">{row.stepCount}</span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}

  {#if rows.length > 0}
    <div class="header">branches</div>
    <ul>
      {#each rows as row (row.id)}
        <li>
          <button
            class="row"
            class:selected={activeId === row.id}
            class:dim={row.status === "abandoned"}
            onclick={() => pick(row)}
            disabled={row.firstStep === null}
          >
            <span class="glyph" class:active={row.status === "active"} class:merged={row.status === "merged"} class:abandoned={row.status === "abandoned"}>{glyph(row.status)}</span>
            <span class="label" title={row.label}>{row.label}</span>
            <span class="count">{row.steps}</span>
            <span class="status-hint">{row.statusHint}</span>
          </button>
        </li>
      {/each}
    </ul>
  {/if}

  {#if showFilters}
  <div class="filters">
    <label>
      <input
        type="checkbox"
        checked={filters.hideRevised}
        onchange={(e) =>
          traceStore.setFilter("hideRevised", (e.currentTarget as HTMLInputElement).checked)}
      />
      <span>hide revised</span>
    </label>
    <label>
      <input
        type="checkbox"
        checked={filters.onlyHypothesis}
        onchange={(e) =>
          traceStore.setFilter("onlyHypothesis", (e.currentTarget as HTMLInputElement).checked)}
      />
      <span>only hypothesis</span>
    </label>
    <label>
      <input
        type="checkbox"
        checked={filters.onlyRefuted}
        onchange={(e) =>
          traceStore.setFilter("onlyRefuted", (e.currentTarget as HTMLInputElement).checked)}
      />
      <span>only refuted</span>
    </label>
  </div>
  {/if}
</nav>

<style>
  .wrap {
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    font-size: var(--text-12);
    color: var(--ink-soft);
  }

  .header {
    color: var(--muted);
    font-size: var(--text-11);
    text-transform: lowercase;
    padding: 4px 9px 3px 9px;
    border-bottom: 1px solid var(--rule);
    background: var(--bg-elev);
  }

  ul {
    flex: 1 1 auto;
    overflow-y: auto;
    min-height: 0;
  }

  /* When both lists exist they share vertical space; the sessions
     list never gets so tall it pushes branches off-screen. */
  ul.sessions {
    flex: 0 1 auto;
    max-height: 40%;
    border-bottom: 1px solid var(--rule);
  }

  .row.is-all {
    color: var(--ink);
  }
  .row.is-all .glyph {
    color: var(--accent);
  }

  li {
    border-bottom: 1px solid var(--rule);
  }

  .row {
    display: flex;
    align-items: center;
    width: 100%;
    height: 22px;
    padding: 0 6px 0 9px;
    border: none;
    background: transparent;
    color: var(--ink-soft);
    font: inherit;
    text-align: left;
    cursor: pointer;
    gap: 6px;
    box-sizing: border-box;
  }
  .row:hover {
    background: var(--bg-hover);
  }
  .row.selected {
    background: var(--bg-active);
    color: var(--ink);
    position: relative;
  }
  .row.selected::before {
    content: "";
    position: absolute;
    left: 0;
    top: 0;
    bottom: 0;
    width: 3px;
    background: var(--accent);
  }
  .row.dim .label,
  .row.dim .count {
    color: var(--muted);
    text-decoration: line-through;
    text-decoration-color: var(--rule-strong);
  }
  .row:disabled {
    cursor: default;
    opacity: 0.6;
  }

  .glyph {
    flex: 0 0 10px;
    width: 10px;
    text-align: center;
    line-height: 1;
    font-size: 10px;
  }
  .glyph.active {
    color: var(--ok);
  }
  .glyph.merged {
    color: var(--rel-supports);
  }
  .glyph.abandoned {
    color: var(--muted);
  }

  .label {
    flex: 1 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .count {
    flex: 0 0 auto;
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .row.selected .count {
    color: var(--ink-soft);
  }

  .status-hint {
    flex: 0 0 10px;
    width: 10px;
    text-align: right;
    color: var(--muted);
    font-size: var(--text-11);
  }

  .filters {
    border-top: 1px solid var(--rule);
    background: var(--bg-elev);
    padding: 4px 0 6px 0;
    flex: 0 0 auto;
  }
  .filters label {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 9px;
    font-size: var(--text-11);
    color: var(--muted);
    cursor: pointer;
    user-select: none;
  }
  .filters label:hover {
    color: var(--ink-soft);
  }
  .filters input[type="checkbox"] {
    appearance: none;
    -webkit-appearance: none;
    width: 10px;
    height: 10px;
    border: 1px solid var(--rule-strong);
    background: var(--bg);
    border-radius: 0;
    cursor: pointer;
    position: relative;
    padding: 0;
    margin: 0;
  }
  .filters input[type="checkbox"]:checked {
    background: var(--accent);
    border-color: var(--accent);
  }
  .filters input[type="checkbox"]:checked::after {
    content: "";
    position: absolute;
    left: 2px;
    top: 0;
    width: 3px;
    height: 6px;
    border: solid var(--bg);
    border-width: 0 1.5px 1.5px 0;
    transform: rotate(45deg);
  }
</style>
