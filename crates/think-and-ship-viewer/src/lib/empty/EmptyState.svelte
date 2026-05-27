<script lang="ts">
  import { traceStore } from "../store/trace.svelte";

  const sock = $derived(traceStore.source.socket_path ?? "(unset)");
  const dir = $derived(traceStore.source.data_dir ?? "(unset)");
</script>

<section class="wrap">
  <p class="primary">no trace yet — run an agent with deliberate-mcp connected.</p>
  <p class="hint">
    set <code>DELIBERATE_BROADCAST_PATH</code> to receive live frames, or
    <code>DELIBERATE_PERSIST=true</code> + <code>DELIBERATE_DATA_DIR</code> to tail the
    sessions directory.
  </p>
  <p class="paths">
    socket: <code>{sock}</code> · data_dir: <code>{dir}</code>
  </p>
</section>

<style>
  .wrap {
    padding: 24px 24px;
    max-width: 720px;
    color: var(--ink-soft);
  }
  .primary {
    font-size: var(--text-13);
    color: var(--ink);
    margin-bottom: 6px;
  }
  .hint {
    font-size: var(--text-12);
    color: var(--muted);
    margin-bottom: 10px;
  }
  .paths {
    font-size: var(--text-11);
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  code {
    font: inherit;
    color: var(--ink);
  }
</style>
