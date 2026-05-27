<script lang="ts">
  import { onMount } from "svelte";
  import dagre from "@dagrejs/dagre";
  import { traceStore } from "../store/trace.svelte";
  import { ALL_SESSIONS_SUFFIX, purposeColor, type DepEdge } from "../types";

  let g: dagre.graphlib.Graph<{}>;

  interface NodeBox {
    n: number;
    cx: number;
    cy: number;
    r: number;
    purpose: string;
  }
  interface EdgeLine {
    from: number;
    to: number;
    rel: string;
    points: { x: number; y: number }[];
  }

  let nodes = $state<NodeBox[]>([]);
  let edges = $state<EdgeLine[]>([]);
  let viewBox = $state("0 0 600 400");

  function relColorVar(rel: string): string {
    switch (rel) {
      case "supports":
        return "var(--rel-supports)";
      case "refutes":
        return "var(--rel-refutes)";
      case "depends_on":
        return "var(--rel-depends)";
      default:
        return "var(--rel-unlabeled)";
    }
  }

  function depRel(d: DepEdge): string {
    return typeof d === "number" ? "" : (d.relation ?? "");
  }
  function depN(d: DepEdge): number {
    return typeof d === "number" ? d : d.step;
  }

  $effect(() => {
    // Reads inside layout() drive reactivity.
    layout();
  });

  onMount(() => {
    layout();
  });

  const inAllMode = $derived(traceStore.active.endsWith(ALL_SESSIONS_SUFFIX));

  function layout(): void {
    const steps = traceStore.current.history.steps;
    // Stitched mode: dependencies are intra-session, so a cross-session
    // DAG would draw misleading edges (or no edges at all between
    // identically-numbered steps). Skip the layout entirely and let
    // the template render the hint.
    if (inAllMode || steps.length === 0) {
      nodes = [];
      edges = [];
      viewBox = "0 0 600 400";
      return;
    }
    g = new dagre.graphlib.Graph<{}>();
    g.setGraph({ rankdir: "TB", nodesep: 18, ranksep: 30, marginx: 16, marginy: 16 });
    g.setDefaultEdgeLabel(() => ({}));

    for (const s of steps) {
      g.setNode(String(s.step_number), { width: 36, height: 36 });
    }
    const rawEdges: { from: number; to: number; rel: string }[] = [];
    for (const s of steps) {
      if (!s.dependencies) continue;
      for (const d of s.dependencies) {
        const n = depN(d);
        if (!steps.find((x) => x.step_number === n)) continue;
        const rel = depRel(d);
        g.setEdge(String(n), String(s.step_number), { rel });
        rawEdges.push({ from: n, to: s.step_number, rel });
      }
      // Add revision edges as dashed (rendered separately).
      if (s.revises_step) {
        g.setEdge(String(s.step_number), String(s.revises_step), { rel: "revises" });
        rawEdges.push({ from: s.step_number, to: s.revises_step, rel: "revises" });
      }
    }

    dagre.layout(g);

    const newNodes: NodeBox[] = [];
    for (const s of steps) {
      const n = g.node(String(s.step_number));
      if (!n) continue;
      newNodes.push({
        n: s.step_number,
        cx: n.x,
        cy: n.y,
        r: 16,
        purpose: s.purpose,
      });
    }

    const newEdges: EdgeLine[] = [];
    for (const e of rawEdges) {
      const edge = g.edge({ v: String(e.from), w: String(e.to) });
      if (!edge || !edge.points) continue;
      newEdges.push({ from: e.from, to: e.to, rel: e.rel, points: edge.points });
    }

    const graphBox = g.graph();
    viewBox = `0 0 ${graphBox.width ?? 600} ${graphBox.height ?? 400}`;
    nodes = newNodes;
    edges = newEdges;
  }

  function pathFor(points: { x: number; y: number }[]): string {
    if (points.length === 0) return "";
    const [first, ...rest] = points;
    return (
      `M${first.x},${first.y} ` +
      rest.map((p) => `L${p.x},${p.y}`).join(" ")
    );
  }
</script>

<div class="wrap">
  {#if inAllMode}
    <div class="empty">select a single session to view its DAG.</div>
  {:else if nodes.length === 0}
    <div class="empty">(no steps to graph)</div>
  {:else}
    <svg viewBox={viewBox} preserveAspectRatio="xMidYMid meet">
      <defs>
        <marker id="arrow" viewBox="0 0 8 8" refX="7" refY="4"
                markerWidth="6" markerHeight="6" orient="auto-start-reverse">
          <path d="M0,0 L8,4 L0,8 z" fill="currentColor"></path>
        </marker>
      </defs>
      <g class="edges">
        {#each edges as e}
          <path
            d={pathFor(e.points)}
            class:revises={e.rel === "revises"}
            style:color={e.rel === "revises" ? "var(--accent)" : relColorVar(e.rel)}
            stroke="currentColor"
            fill="none"
            stroke-dasharray={e.rel === "revises" ? "3 3" : "none"}
            marker-end="url(#arrow)"
          />
        {/each}
      </g>
      <g class="nodes">
        {#each nodes as node}
          {@const selected = traceStore.selectedStep === node.n}
          <g
            class="node"
            class:selected
            transform={`translate(${node.cx},${node.cy})`}
            onclick={() => traceStore.selectStep(node.n)}
            onkeydown={(e) => {
              if (e.key === "Enter" || e.key === " ") traceStore.selectStep(node.n);
            }}
            role="button"
            tabindex="0"
          >
            <circle r={node.r} class="bg" />
            <circle r={node.r} class="ring" style:stroke={purposeColor(node.purpose)} />
            <text text-anchor="middle" dy="4">{node.n}</text>
          </g>
        {/each}
      </g>
    </svg>
  {/if}
</div>

<style>
  .wrap {
    height: 100%;
    overflow: auto;
    background: var(--bg);
  }
  svg {
    width: 100%;
    height: 100%;
    display: block;
  }
  .edges path {
    stroke-width: 1;
  }
  .node {
    cursor: pointer;
    color: var(--ink-soft);
  }
  .node .bg {
    fill: var(--bg);
  }
  .node .ring {
    fill: none;
    stroke-width: 1.5;
  }
  .node text {
    font-family: var(--font-mono);
    font-size: var(--text-11);
    fill: var(--ink);
    pointer-events: none;
    user-select: none;
  }
  .node.selected .bg {
    fill: var(--bg-active);
  }
  .node:focus-visible {
    outline: none;
  }
  .node:focus-visible .ring {
    stroke-width: 2.5;
  }
  .empty {
    padding: 16px;
    color: var(--muted);
    font-size: var(--text-12);
  }
</style>
