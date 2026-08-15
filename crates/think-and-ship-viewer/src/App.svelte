<script lang="ts">
  import { onDestroy, onMount } from "svelte";
  import { traceStore } from "./lib/store/trace.svelte";
  import TitleBar from "./lib/titlebar/TitleBar.svelte";
  import BranchNav from "./lib/branches/BranchNav.svelte";
  import Timeline from "./lib/trace/Timeline.svelte";
  import DagView from "./lib/graph/DagView.svelte";
  import CheckpointView from "./lib/checkpoint/CheckpointView.svelte";
  import StepDetail from "./lib/detail/StepDetail.svelte";
  import StatusBar from "./lib/status/StatusBar.svelte";
  import EmptyState from "./lib/empty/EmptyState.svelte";

  let initError = $state<string | null>(null);

  onMount(async () => {
    try {
      await traceStore.init();
    } catch (e) {
      initError = String(e);
    }
  });

  onDestroy(() => traceStore.dispose());

  // Keyboard handling at the window level so j/k/t/r/c work regardless of
  // focus, except when an input is focused.
  function onKey(event: KeyboardEvent): void {
    const target = event.target as HTMLElement | null;
    const tag = target?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA") return;
    const steps = traceStore.current.history.steps;

    /// j/k walks by array index in the displayed `steps` list so a
    /// hidden/filtered row doesn't trap navigation. Step numbers are
    /// unique project-wide so the lookup needs no further qualifier.
    function currentIdx(): number {
      const ref = traceStore.selectedRef;
      if (!ref) return -1;
      return steps.findIndex((s) => s.step_number === ref.stepNumber);
    }

    switch (event.key) {
      case "j": {
        const cur = currentIdx();
        const idx = cur < 0 ? 0 : Math.min(steps.length - 1, cur + 1);
        traceStore.selectStepAt(idx);
        event.preventDefault();
        break;
      }
      case "k": {
        const cur = currentIdx();
        const idx = cur < 0 ? 0 : Math.max(0, cur - 1);
        traceStore.selectStepAt(idx);
        event.preventDefault();
        break;
      }
      case "G": {
        if (steps.length > 0) traceStore.selectStepAt(steps.length - 1);
        event.preventDefault();
        break;
      }
      case "g": {
        if (steps.length > 0) traceStore.selectStepAt(0);
        event.preventDefault();
        break;
      }
      case "t":
        traceStore.setView("trace");
        event.preventDefault();
        break;
      case "r":
        traceStore.setView("graph");
        event.preventDefault();
        break;
      case "c":
        traceStore.setView("checkpoint");
        event.preventDefault();
        break;
    }
  }
</script>

<svelte:window on:keydown={onKey} />

<div class="root">
  <TitleBar />
  {#if initError}
    <div class="error">orchestrator init failed: {initError}</div>
  {:else if traceStore.source.mode === "none" && traceStore.current.history.steps.length === 0}
    <EmptyState />
  {:else}
    <div class="body">
      <aside class="rail-left">
        <BranchNav />
      </aside>
      <main class="center">
        {#if traceStore.view === "trace"}
          <Timeline />
        {:else if traceStore.view === "graph"}
          <DagView />
        {:else}
          <CheckpointView />
        {/if}
      </main>
      <aside class="rail-right">
        <StepDetail />
      </aside>
    </div>
  {/if}
  <StatusBar />
</div>

<style>
  .root {
    display: grid;
    grid-template-rows: auto 1fr auto;
    height: 100vh;
    color: var(--ink);
    background: var(--bg);
  }

  .body {
    display: grid;
    grid-template-columns: 200px 1fr 380px;
    min-height: 0;
    border-bottom: 1px solid var(--rule);
  }

  .rail-left {
    border-right: 1px solid var(--rule);
    min-height: 0;
    overflow: hidden;
    background: var(--bg-elev);
  }

  .center {
    min-height: 0;
    min-width: 0;
    overflow: hidden;
    background: var(--bg);
  }

  .rail-right {
    border-left: 1px solid var(--rule);
    min-height: 0;
    overflow: hidden;
    background: var(--bg-elev);
  }

  .error {
    padding: 12px 16px;
    color: var(--alert);
    border-bottom: 1px solid var(--rule);
    font-size: var(--text-12);
  }
</style>
