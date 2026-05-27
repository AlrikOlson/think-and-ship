<script lang="ts">
  import { traceStore } from "../store/trace.svelte";

  const ref = $derived(traceStore.selectedRef);
  const selLabel = $derived<string | null>(ref ? `#${ref.stepNumber}` : null);
</script>

<footer class="bar">
  {#if selLabel !== null}
    <span class="cell">sel {selLabel}</span>
    <span class="sep">·</span>
  {/if}
  <span class="cell muted">j/k · t/r/c · / · d</span>
</footer>

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 2px 10px;
    height: 20px;
    font-size: var(--text-11);
    color: var(--muted);
    background: var(--bg-elev);
    border-top: 1px solid var(--rule);
    user-select: none;
  }
  .cell {
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .cell.muted {
    color: var(--muted);
    opacity: 0.85;
  }
  .sep {
    color: var(--rule-strong);
  }
</style>
