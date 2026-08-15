<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { traceStore } from "../store/trace.svelte";
  import { ALL_SESSIONS_SUFFIX } from "../types";

  // Shape mirrors src/server.rs `checkpoint_snapshot` output. Loose
  // typing: this is read-only and we render whatever's present.
  interface Snapshot {
    open_hypotheses?: { step: number; thought_excerpt?: string }[];
    stale_branches?: {
      branch_id: string;
      name?: string;
      last_step?: number;
      head_step?: number;
      gap?: number;
    }[];
    confidence_trend?: {
      trend: "rising" | "falling" | "stable" | "insufficient";
      slope?: number;
      window?: number;
      values?: number[];
    };
    revised_but_undefended?: {
      step: number;
      revised_by?: number;
      dependents?: number[];
    }[];
    refuted_chain_alerts?: {
      step: number;
      refuter?: number;
      via?: string;
    }[];
  }

  let snap = $state<Snapshot | null>(null);
  let loadError = $state<string | null>(null);
  let loading = $state(false);
  let lastFetched = $state(0);

  // Auto-refetch whenever the trace changes — but throttle hard so a
  // burst of step appends doesn't hammer the engine. Read the dep
  // signals explicitly so Svelte 5 tracks them, then guard.
  // Stitched (__ALL__) is a viewer-only synthetic id; the server has
  // no such session to compute a checkpoint for. Short-circuit before
  // touching the network.
  $effect(() => {
    // Touch tracked reads — return values are intentionally discarded.
    traceStore.current.history.steps.length;
    void traceStore.active;
    if (traceStore.active.endsWith(ALL_SESSIONS_SUFFIX)) {
      snap = null;
      loadError = null;
      loading = false;
      return;
    }
    const now = Date.now();
    if (now - lastFetched < 500) return;
    fetch();
  });

  const inAllMode = $derived(traceStore.active.endsWith(ALL_SESSIONS_SUFFIX));

  async function fetch(): Promise<void> {
    loading = true;
    loadError = null;
    try {
      const result = await invoke("get_checkpoint", {
        sessionId: traceStore.active,
      });
      snap = result as Snapshot;
      lastFetched = Date.now();
    } catch (e) {
      loadError = String(e);
    } finally {
      loading = false;
    }
  }

  function trendGlyph(t?: string): string {
    switch (t) {
      case "rising":
        return "↗";
      case "falling":
        return "↘";
      case "stable":
        return "→";
      default:
        return "—";
    }
  }

  // Treat the four "alert" panels as warnings; the confidence-trend
  // panel is informational and excluded from this check.
  function hasAnyWarnings(s: Snapshot | null): boolean {
    if (!s) return false;
    return Boolean(
      (s.open_hypotheses?.length ?? 0) > 0 ||
        (s.stale_branches?.length ?? 0) > 0 ||
        (s.revised_but_undefended?.length ?? 0) > 0 ||
        (s.refuted_chain_alerts?.length ?? 0) > 0
    );
  }

  function hasTrend(s: Snapshot | null): boolean {
    return Boolean(
      s?.confidence_trend?.values && s.confidence_trend.values.length > 1
    );
  }

  function sparkPath(values: number[], w: number, h: number): string {
    if (values.length === 0) return "";
    const min = Math.min(...values);
    const max = Math.max(...values);
    const range = max - min || 1;
    const step = w / Math.max(1, values.length - 1);
    return values
      .map((v, i) => {
        const x = i * step;
        const y = h - ((v - min) / range) * h;
        return `${i === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`;
      })
      .join(" ");
  }
</script>

<section class="wrap">
  {#if loading || loadError}
    <header>
      {#if loading}
        <span class="muted">refreshing…</span>
      {/if}
      {#if loadError}
        <span class="err">{loadError}</span>
      {/if}
    </header>
  {/if}

  {#if inAllMode}
    <p class="muted">select a single session to view its checkpoint.</p>
  {:else if !snap}
    <p class="muted">no checkpoint computed yet.</p>
  {:else if !hasAnyWarnings(snap) && !hasTrend(snap)}
    <p class="all-clear">✓ no warnings</p>
  {:else}
    {#if snap.open_hypotheses && snap.open_hypotheses.length > 0}
      <section class="panel">
        <h3>open hypotheses</h3>
        <ul>
          {#each snap.open_hypotheses as h}
            <li>
              <button class="step-link" onclick={() => traceStore.selectStep(h.step)}
                >#{h.step}</button
              >
              {#if h.thought_excerpt}<span class="excerpt">{h.thought_excerpt}</span>{/if}
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if snap.stale_branches && snap.stale_branches.length > 0}
      <section class="panel">
        <h3>stale branches</h3>
        <ul>
          {#each snap.stale_branches as b}
            <li>
              <span class="bid">{b.branch_id}</span>
              {#if b.name && b.name !== b.branch_id}<span class="excerpt">{b.name}</span>{/if}
              {#if b.gap !== undefined}<span class="muted">gap: {b.gap}</span>{/if}
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if hasTrend(snap)}
      <section class="panel">
        <h3>
          confidence trend
          <span class="trend">{trendGlyph(snap.confidence_trend?.trend)} {snap.confidence_trend?.trend ?? ""}</span>
        </h3>
        <svg class="spark" viewBox="0 0 200 36" preserveAspectRatio="none">
          <path d={sparkPath(snap.confidence_trend!.values!, 200, 30)} fill="none" stroke="var(--accent)" stroke-width="1" />
        </svg>
        <p class="muted">
          last {snap.confidence_trend!.values!.length} steps
          {#if snap.confidence_trend?.slope !== undefined}
            · slope {(snap.confidence_trend.slope * 100).toFixed(1)}%
          {/if}
        </p>
      </section>
    {/if}

    {#if snap.revised_but_undefended && snap.revised_but_undefended.length > 0}
      <section class="panel">
        <h3>revised but undefended</h3>
        <ul>
          {#each snap.revised_but_undefended as r}
            <li>
              <button class="step-link" onclick={() => traceStore.selectStep(r.step)}
                >#{r.step}</button
              >
              {#if r.revised_by !== undefined && r.revised_by !== null}<span class="muted">revised by #{r.revised_by}</span>{/if}
              {#if r.dependents && r.dependents.length > 0}
                <span class="excerpt">dependents: {r.dependents.join(", ")}</span>
              {/if}
            </li>
          {/each}
        </ul>
      </section>
    {/if}

    {#if snap.refuted_chain_alerts && snap.refuted_chain_alerts.length > 0}
      <section class="panel">
        <h3>refuted-chain alerts</h3>
        <ul>
          {#each snap.refuted_chain_alerts as a}
            <li>
              <button class="step-link" onclick={() => traceStore.selectStep(a.step)}
                >#{a.step}</button
              >
              {#if a.refuter !== undefined && a.refuter !== null}<span class="muted">refuted by #{a.refuter}</span>{/if}
              {#if a.via}<span class="excerpt">via {a.via}</span>{/if}
            </li>
          {/each}
        </ul>
      </section>
    {/if}
  {/if}
</section>

<style>
  .wrap {
    height: 100%;
    overflow-y: auto;
    padding: 12px 16px;
    font-size: var(--text-12);
  }
  header {
    display: flex;
    align-items: baseline;
    gap: 10px;
    margin-bottom: 8px;
    font-size: var(--text-11);
  }
  .all-clear {
    color: var(--ok);
    font-size: var(--text-12);
    padding: 4px 0;
  }
  .muted {
    color: var(--muted);
  }
  .err {
    color: var(--alert);
  }
  .panel {
    border-top: 1px solid var(--rule);
    padding: 8px 0;
  }
  .panel h3 {
    color: var(--muted);
    font-size: var(--text-11);
    text-transform: lowercase;
    margin-bottom: 4px;
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .panel .trend {
    color: var(--ink-soft);
  }
  ul {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  li {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .step-link {
    border: none;
    background: transparent;
    color: var(--accent);
    padding: 0;
    font: inherit;
    cursor: pointer;
  }
  .step-link:hover {
    text-decoration: underline;
  }
  .bid {
    color: var(--ink-soft);
  }
  .excerpt {
    color: var(--ink-soft);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .spark {
    width: 200px;
    height: 36px;
    border: 1px solid var(--rule);
    background: var(--bg-elev);
  }
</style>
