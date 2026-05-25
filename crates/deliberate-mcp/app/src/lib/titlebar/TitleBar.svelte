<script lang="ts">
  import { traceStore, sourceLabel } from "../store/trace.svelte";

  let now = $state(Date.now());
  let timer: number | null = null;
  $effect(() => {
    timer = window.setInterval(() => (now = Date.now()), 1000);
    return () => {
      if (timer !== null) window.clearInterval(timer);
    };
  });

  const ageMs = $derived(
    traceStore.lastUpdateMs === 0 ? 0 : now - traceStore.lastUpdateMs
  );
  const ageLabel = $derived.by<string>(() => {
    if (traceStore.lastUpdateMs === 0) return "—";
    if (ageMs < 1000) return `${ageMs}ms`;
    const s = Math.floor(ageMs / 1000);
    if (s < 60) return `${s}s`;
    const m = Math.floor(s / 60);
    return `${m}m`;
  });

  const stepCount = $derived(traceStore.current.history.steps.length);
  const branchCount = $derived(traceStore.current.branches.size);

  // Project picker: the dropdown lists unique projects (count = number
  // of sessions under each). Selecting one calls setProject(), which
  // routes to that project's bare default session.
  const projects = $derived(traceStore.projects);
  const currentProject = $derived(traceStore.currentProject);
  const projectOptions = $derived(
    projects.map((p) => {
      const count = [...traceStore.sessions.entries()]
        .filter(([id, rec]) => {
          // Re-derive project from each session record. Mirrors
          // store.projectOf without re-importing here.
          const stamped = rec.history.metadata?.project_id;
          if (stamped) return stamped === p;
          const idx = id.indexOf("__");
          const prefix = idx > 0 ? id.slice(0, idx) : id;
          return prefix === p || id.startsWith(`${p}-`);
        }).length;
      return { id: p, label: p, count };
    })
  );

  const sourceTooltip = $derived.by<string>(() => {
    const s = traceStore.source;
    const lines = [
      `${sourceLabel(s.mode).toLowerCase()}`,
      `persist: ${s.persistence_enabled ? "on" : "off"}`,
    ];
    if (s.socket_path) lines.push(`socket: ${s.socket_path}`);
    if (s.data_dir) lines.push(`data_dir: ${s.data_dir}`);
    return lines.join("\n");
  });

  function onChange(e: Event): void {
    const v = (e.currentTarget as HTMLSelectElement).value;
    traceStore.setProject(v);
  }

  function modeGlyph(): string {
    switch (traceStore.source.mode) {
      case "none":
        return "○";
      case "file":
        return "◐";
      default:
        return "●";
    }
  }

  function modeShort(): string {
    switch (traceStore.source.mode) {
      case "socket":
      case "socket_and_file":
        return "live";
      case "file":
        return "file";
      case "none":
        return "idle";
    }
  }
</script>

<header class="bar">
  <span class="title">deliberate</span>
  <label class="session group-gap">
    <span class="select-wrap">
      <select value={currentProject} onchange={onChange} aria-label="project">
        {#each projectOptions as p (p.id)}
          <option value={p.id}>
            {p.label}{p.count > 1 ? ` · ${p.count} sess` : ""}
          </option>
        {/each}
      </select>
      <span class="chev" aria-hidden="true">v</span>
    </span>
  </label>
  <span
    class="mode group-gap"
    class:live={traceStore.source.mode === "socket" ||
      traceStore.source.mode === "socket_and_file"}
    class:dead={traceStore.source.mode === "none"}
    title={sourceTooltip}
  >
    <span class="mode-dot">{modeGlyph()}</span>
    {modeShort()}
  </span>
  <span class="counts group-gap" title="{stepCount} steps · {branchCount} branches">
    {stepCount}s {branchCount}b
  </span>

  <span class="spacer"></span>

  <nav class="tabs" aria-label="view">
    <button
      class:active={traceStore.view === "trace"}
      onclick={() => traceStore.setView("trace")}
      title="trace (t)"
    >trace</button>
    <button
      class:active={traceStore.view === "graph"}
      onclick={() => traceStore.setView("graph")}
      title="graph (r)"
    >graph</button>
    <button
      class:active={traceStore.view === "checkpoint"}
      onclick={() => traceStore.setView("checkpoint")}
      title="checkpoint (c)"
    >check</button>
  </nav>

  <span class="age" title="last update">{ageLabel}</span>
</header>

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 8px 0 10px;
    height: 28px;
    font-size: var(--text-12);
    color: var(--ink-soft);
    background: var(--bg-elev);
    border-bottom: 1px solid var(--rule);
    user-select: none;
  }
  .title {
    color: var(--ink);
    font-weight: 600;
    letter-spacing: 0.02em;
  }
  /* Each .group-gap element marks the start of a new visual cluster.
     Whitespace replaces the previous "·" bullets. */
  .group-gap {
    margin-left: 12px;
  }
  .session {
    color: var(--muted);
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .select-wrap {
    position: relative;
    display: inline-flex;
    align-items: center;
  }
  .session select {
    appearance: none;
    -webkit-appearance: none;
    background: transparent;
    border: 1px solid var(--rule);
    color: var(--ink);
    font: inherit;
    font-size: var(--text-12);
    padding: 0 16px 0 6px;
    line-height: 18px;
    height: 20px;
    border-radius: 0;
    cursor: pointer;
  }
  .session select:hover {
    border-color: var(--rule-strong);
  }
  .session select:focus-visible {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }
  .chev {
    position: absolute;
    right: 5px;
    color: var(--muted);
    pointer-events: none;
    font-family: var(--font-mono);
    font-size: var(--text-11);
    line-height: 1;
  }

  .mode {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    color: var(--muted);
    cursor: help;
  }
  .mode.live {
    color: var(--ok);
  }
  .mode.dead {
    color: var(--muted);
  }
  .mode-dot {
    font-size: var(--text-12);
    line-height: 1;
  }

  .counts {
    color: var(--ink-soft);
    font-variant-numeric: tabular-nums;
    cursor: help;
  }

  .spacer {
    flex: 1;
  }

  /* ── tabs as a right-aligned segmented control ── */
  .tabs {
    display: flex;
    border: 1px solid var(--rule);
    height: 20px;
  }
  .tabs button {
    border: none;
    border-left: 1px solid var(--rule);
    padding: 0 10px;
    height: 100%;
    font: inherit;
    font-size: var(--text-12);
    color: var(--muted);
    background: transparent;
    cursor: pointer;
    border-radius: 0;
    text-transform: lowercase;
  }
  .tabs button:first-child {
    border-left: none;
  }
  .tabs button:hover {
    color: var(--ink);
    background: var(--bg-hover);
  }
  .tabs button.active {
    color: var(--ink);
    background: var(--bg);
  }
  .tabs button:focus-visible {
    outline: 1px solid var(--accent);
    outline-offset: -2px;
  }

  .age {
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    min-width: 40px;
    text-align: right;
  }
</style>
