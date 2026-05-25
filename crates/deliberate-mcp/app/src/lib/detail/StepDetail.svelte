<script lang="ts">
  import { traceStore } from "../store/trace.svelte";
  import {
    ALL_SESSIONS_SUFFIX,
    purposeColor,
    sessionLabel,
    type DepEdge,
  } from "../types";

  const step = $derived(traceStore.selectedStepData);
  const inAllMode = $derived(traceStore.active.endsWith(ALL_SESSIONS_SUFFIX));
  const currentProject = $derived(traceStore.currentProject);

  function depRelation(d: DepEdge): string {
    if (typeof d === "number") return "";
    return d.relation ?? "";
  }
  function depStep(d: DepEdge): number {
    return typeof d === "number" ? d : d.step;
  }
  function nextActionText(s: typeof step): string {
    if (!s) return "";
    const na = s.next_action;
    if (typeof na === "string") return na;
    return na.action + (na.tool ? `  [tool: ${na.tool}]` : "");
  }

  function timeShort(ts?: string): string | null {
    if (!ts) return null;
    // ISO 8601 — slice the HH:MM:SS portion.
    return ts.length >= 19 ? `${ts.slice(11, 19)}Z` : ts;
  }
</script>

{#if !step}
  <div class="empty">no step selected</div>
{:else}
  <article class="detail">
    <header>
      <span class="num">#{step.step_number}</span>
      <span class="purpose" style:color={purposeColor(step.purpose)}>{step.purpose}</span>
      {#if inAllMode && step.session_id}
        <span class="sess" title={step.session_id}>
          {sessionLabel(step.session_id, currentProject)}
        </span>
      {/if}
    </header>

    {#if step.branch_id}
      <div class="meta-row">
        <span class="meta-label">branch</span>
        <span class="meta-value">
          {step.branch_id}{#if step.branch_name && step.branch_name !== step.branch_id} · {step.branch_name}{/if}{#if step.branch_from} (from #{step.branch_from}){/if}
        </span>
      </div>
    {/if}

    <dl class="fields">
      {#if step.context}
        <dt>ctx</dt><dd>{step.context}</dd>
      {/if}
      {#if step.thought}
        <dt>thou</dt><dd>{step.thought}</dd>
      {/if}
      {#if step.outcome}
        <dt>out</dt><dd>{step.outcome}</dd>
      {/if}
      {#if nextActionText(step)}
        <dt>next</dt><dd>{nextActionText(step)}</dd>
      {/if}
      {#if step.rationale}
        <dt>rat</dt><dd>{step.rationale}</dd>
      {/if}
      {#if step.uncertainty_notes}
        <dt>unc</dt><dd>{step.uncertainty_notes}</dd>
      {/if}
      {#if step.revises_step}
        <dt>rev</dt><dd>#{step.revises_step}{#if step.revision_reason} — {step.revision_reason}{/if}</dd>
      {/if}
    </dl>

    {#if step.dependencies && step.dependencies.length > 0}
      <div class="block">
        <div class="block-label">deps</div>
        <div class="dep-pills">
          {#each step.dependencies as d}
            {@const rel = depRelation(d)}
            {@const n = depStep(d)}
            <button
              class="dep-pill"
              class:rel-supports={rel === "supports"}
              class:rel-refutes={rel === "refutes"}
              class:rel-depends={rel === "depends_on"}
              onclick={() => traceStore.selectStep(n)}
              title={rel ? `${rel} step #${n}` : `depends on #${n}`}
            >
              <span class="dep-rel">{rel || "dep"}</span>
              <span class="dep-sep">·</span>
              <span class="dep-n">#{n}</span>
            </button>
          {/each}
        </div>
      </div>
    {/if}

    {#if step.tools_used && step.tools_used.length > 0}
      <div class="meta-row">
        <span class="meta-label">tools</span>
        <span class="meta-value">{step.tools_used.join(", ")}</span>
      </div>
    {/if}

    <footer>
      {#if timeShort(step.timestamp)}<span title={step.timestamp}>{timeShort(step.timestamp)}</span>{/if}
      {#if step.duration_ms !== undefined && step.duration_ms !== null}<span>{step.duration_ms}ms</span>{/if}
      <span>est {step.estimated_total}</span>
      {#if step.session_id}<span title="session" class="ellide">sess {step.session_id}</span>{/if}
    </footer>
  </article>
{/if}

<style>
  .empty {
    padding: 12px;
    color: var(--muted);
    font-size: var(--text-12);
  }
  .detail {
    height: 100%;
    overflow-y: auto;
    padding: 6px 10px 12px 10px;
    font-size: var(--text-12);
    color: var(--ink);
  }
  header {
    display: flex;
    align-items: baseline;
    gap: 8px;
    padding-bottom: 6px;
    border-bottom: 1px solid var(--rule);
    margin-bottom: 6px;
    position: sticky;
    top: 0;
    background: var(--bg-elev);
    padding-top: 2px;
    z-index: 1;
  }
  .num {
    font-variant-numeric: tabular-nums;
    color: var(--ink);
    font-weight: 600;
  }
  .purpose {
    text-transform: lowercase;
    flex: 1 1 auto;
    min-width: 0;
    overflow-wrap: anywhere;
    word-break: break-word;
  }
  /* Session badge in stitched-all mode — small muted chip, helps
     disambiguate when step #s repeat across sessions. */
  .sess {
    flex: 0 0 auto;
    color: var(--muted);
    font-size: var(--text-11);
    padding: 0 4px;
    border: 1px solid var(--rule);
  }

  /* ── label/value rows for sparse meta (branch, tools) ── */
  .meta-row {
    display: flex;
    gap: 8px;
    padding: 1px 0;
    font-size: var(--text-11);
  }
  .meta-label {
    flex: 0 0 36px;
    color: var(--muted);
    text-transform: lowercase;
  }
  .meta-value {
    color: var(--ink-soft);
    min-width: 0;
    overflow-wrap: anywhere;
    word-break: break-word;
  }

  /* ── label/prose grid for the main content blocks ── */
  .fields {
    display: grid;
    grid-template-columns: 36px 1fr;
    column-gap: 8px;
    row-gap: 6px;
    margin: 4px 0 6px 0;
  }
  .fields dt {
    color: var(--muted);
    font-size: var(--text-11);
    text-transform: lowercase;
    padding-top: 1px;
  }
  .fields dd {
    margin: 0;
    color: var(--ink);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    word-break: break-word;
  }

  /* ── deps ── */
  .block {
    margin: 6px 0;
  }
  .block-label {
    color: var(--muted);
    font-size: var(--text-11);
    text-transform: lowercase;
    margin-bottom: 3px;
  }
  .dep-pills {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .dep-pill {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 0 6px;
    height: 18px;
    border: 1px solid var(--rule);
    background: transparent;
    color: var(--muted);
    font: inherit;
    font-size: var(--text-11);
    cursor: pointer;
    border-radius: 0;
  }
  .dep-pill:hover {
    border-color: var(--rule-strong);
    color: var(--ink);
  }
  .dep-pill.rel-supports {
    color: var(--rel-supports);
    border-color: var(--rel-supports);
  }
  .dep-pill.rel-refutes {
    color: var(--rel-refutes);
    border-color: var(--rel-refutes);
  }
  .dep-pill.rel-depends {
    color: var(--rel-depends);
    border-color: var(--rel-depends);
  }
  .dep-pill .dep-sep {
    color: var(--rule-strong);
  }
  .dep-pill .dep-n {
    font-variant-numeric: tabular-nums;
  }

  /* ── footer: single-line meta strip ── */
  footer {
    display: flex;
    flex-wrap: wrap;
    gap: 10px;
    margin-top: 8px;
    padding-top: 5px;
    border-top: 1px solid var(--rule);
    font-size: var(--text-11);
    color: var(--muted);
  }
  footer span {
    font-variant-numeric: tabular-nums;
  }
  footer .ellide {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
    max-width: 200px;
  }
</style>
