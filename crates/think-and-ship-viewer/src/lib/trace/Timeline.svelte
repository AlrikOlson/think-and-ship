<script lang="ts">
  import { onMount } from "svelte";
  import { traceStore } from "../store/trace.svelte";
  import {
    ALL_SESSIONS_SUFFIX,
    purposeColor,
    sessionLabel,
    type Branch,
    type DeliberateStep,
  } from "../types";

  // ---------------------------------------------------------------------------
  // Geometry. All pixel values are explicit so the layout is auditable. If you
  // change one, every column re-flows because the layout is computed from
  // these constants on every draw — no magic offsets in draw().
  // ---------------------------------------------------------------------------
  const ROW_H = 22;
  const LANE_W = 14;
  const PADDING_TOP = 8;
  const HEADER_H = 18; // lane-label strip above the rows
  const SEL_STRIPE_W = 3; // left-edge accent on the selected row
  const PINNED_STRIPE_W = 2; // left-edge stripe on pinned rows
  const LANE_GUTTER_L = SEL_STRIPE_W + PINNED_STRIPE_W + 3; // room for both stripes
  const LANE_GUTTER_R = 10; // between lanes and the text columns
  const GLYPH_R = 4;
  const COL_PAD = 6;
  const COL_NUM_W = 36; // right-aligned step number
  const COL_SESS_W = 100; // session badge column — only present in __ALL__ mode
  const COL_PURPOSE_W = 110;
  const COL_DUR_W = 44; // ms / s text, right-aligned
  const COL_CONF_LABEL_W = 32; // "92%"
  const COL_CONF_BAR_W = 48; // filled bar
  const COL_CONF_BAR_H = 4;
  const EDGE_R = 10;
  const FONT_BODY = '12px "Berkeley Mono", "JetBrains Mono", "SF Mono", ui-monospace, monospace';
  const FONT_SMALL = '11px "Berkeley Mono", "JetBrains Mono", "SF Mono", ui-monospace, monospace';
  // Cap visible lanes so a runaway-branch trace can't squeeze the text columns
  // to zero. Anything beyond is collapsed into the LANE_MAX-1 column and an
  // overflow badge is drawn over its glyph.
  const LANE_MAX = 6;

  // ---------------------------------------------------------------------------
  // Component state.
  // ---------------------------------------------------------------------------
  let canvas: HTMLCanvasElement;
  let wrapper: HTMLDivElement;
  let dpr = 1;
  let showOverlays = $state(false);

  function onKey(e: KeyboardEvent): void {
    const tag = (e.target as HTMLElement | null)?.tagName;
    if (tag === "INPUT" || tag === "TEXTAREA") return;
    if (e.key === "d") {
      showOverlays = !showOverlays;
      e.preventDefault();
    }
  }

  $effect(() => {
    if (canvas) draw();
  });

  // Auto-scroll the selected row into view whenever the selection
  // changes. j/k keyboard nav in App.svelte just flips the selected
  // ref; this effect makes sure the canvas catches up so the active
  // row is always on screen.
  $effect(() => {
    const ref = traceStore.selectedRef;
    if (ref != null) {
      scrollRowIntoView(ref.stepNumber);
    }
  });

  onMount(() => {
    dpr = window.devicePixelRatio || 1;
    const ro = new ResizeObserver(() => {
      sizeCanvas();
      draw();
    });
    ro.observe(wrapper);
    sizeCanvas();
    draw();
    return () => ro.disconnect();
  });

  // Build the filtered step list once per consult — kept in sync between
  // sizeCanvas() and draw() so the canvas tracks the visible row count.
  function visibleSteps(): DeliberateStep[] {
    const all = traceStore.current.history.steps;
    const f = traceStore.filters;
    let out = all;
    if (f.hideRevised) out = out.filter((s) => s.revised_by == null);
    if (f.onlyHypothesis) out = out.filter((s) => s.purpose === "hypothesis");
    if (f.onlyRefuted) {
      const set = new Set<number>();
      for (const s of all) {
        if (!s.dependencies) continue;
        for (const d of s.dependencies) {
          if (typeof d === "number") continue;
          if (d.relation === "refutes") {
            set.add(s.step_number);
            set.add(d.step);
          }
        }
      }
      out = out.filter((s) => set.has(s.step_number));
    }
    return out;
  }

  function sizeCanvas(): void {
    if (!canvas || !wrapper) return;
    const w = wrapper.clientWidth;
    const count = visibleSteps().length;
    const h = Math.max(
      wrapper.clientHeight,
      count * ROW_H + PADDING_TOP * 2 + HEADER_H,
    );
    canvas.width = Math.floor(w * dpr);
    canvas.height = Math.floor(h * dpr);
    canvas.style.width = w + "px";
    canvas.style.height = h + "px";
  }

  function rowYForIndex(i: number): number {
    return PADDING_TOP + HEADER_H + i * ROW_H + ROW_H / 2;
  }
  function rowTopForIndex(i: number): number {
    return PADDING_TOP + HEADER_H + i * ROW_H;
  }

  /// Scroll the row matching `stepNumber` into view if it's currently
  /// outside the visible viewport. Step numbers are unique project-wide
  /// so the lookup needs no session qualifier.
  export function scrollRowIntoView(stepNumber: number): void {
    if (!wrapper) return;
    const steps = visibleSteps();
    const i = steps.findIndex((s) => s.step_number === stepNumber);
    if (i < 0) return;
    const top = rowTopForIndex(i);
    const bottom = top + ROW_H;
    const viewTop = wrapper.scrollTop;
    const viewBottom = viewTop + wrapper.clientHeight;
    const margin = ROW_H * 2;
    if (top < viewTop + margin) {
      wrapper.scrollTop = Math.max(0, top - margin);
    } else if (bottom > viewBottom - margin) {
      wrapper.scrollTop = bottom - wrapper.clientHeight + margin;
    }
  }

  // ---------------------------------------------------------------------------
  // Lane assignment. Each branch gets a column index, capped at LANE_MAX-1
  // (the last slot is the collapse-overflow bucket).
  // ---------------------------------------------------------------------------
  function laneAssignment(
    steps: DeliberateStep[],
    branches: Map<string, Branch>,
  ): { lanes: Map<string, number>; visible: number; overflow: boolean } {
    const lanes = new Map<string, number>();
    let next = 1;
    for (const s of steps) {
      if (s.branch_id && !lanes.has(s.branch_id)) {
        lanes.set(s.branch_id, next++);
      }
    }
    for (const id of branches.keys()) {
      if (!lanes.has(id)) lanes.set(id, next++);
    }
    let overflow = false;
    if (next - 1 > LANE_MAX - 1) {
      // Collapse all overflow lanes into the last visible slot.
      const collapsed = LANE_MAX - 1;
      for (const [id, lane] of lanes) {
        if (lane > collapsed) lanes.set(id, collapsed);
      }
      overflow = true;
    }
    const visible = Math.min(next, LANE_MAX);
    return { lanes, visible, overflow };
  }

  // ---------------------------------------------------------------------------
  // Column layout. Recomputed every draw from canvas width and lane count so
  // resize and adding branches both reflow correctly.
  // ---------------------------------------------------------------------------
  interface Col {
    x: number;
    w: number;
    end: number;
    alignRight?: boolean;
  }
  interface Layout {
    laneOrigin: number;
    laneCount: number;
    num: Col;
    /// Session-badge column, only populated when stitched-all mode is
    /// active (the canvas is rendering steps from multiple sessions
    /// at once). Undefined in single-session mode.
    sess?: Col;
    purpose: Col;
    thought: Col;
    dur: Col;
    confLabel: Col;
    confBar: Col;
  }

  function computeLayout(
    canvasW: number,
    laneCount: number,
    inAllMode: boolean,
  ): Layout {
    const col = (x: number, w: number, alignRight = false): Col => ({
      x,
      w,
      end: x + w,
      alignRight,
    });
    const laneOrigin = LANE_GUTTER_L;
    const lanesEnd = laneOrigin + laneCount * LANE_W + LANE_GUTTER_R;
    const numCol = col(lanesEnd, COL_NUM_W, true);
    const sessCol = inAllMode
      ? col(numCol.end + COL_PAD, COL_SESS_W)
      : undefined;
    const purposeStart = sessCol ? sessCol.end + COL_PAD : numCol.end + COL_PAD;
    const purposeCol = col(purposeStart, COL_PURPOSE_W);
    const rightEnd = canvasW - EDGE_R;
    const confBarCol = col(rightEnd - COL_CONF_BAR_W, COL_CONF_BAR_W);
    const confLabelCol = col(
      confBarCol.x - COL_PAD - COL_CONF_LABEL_W,
      COL_CONF_LABEL_W,
      true,
    );
    const durCol = col(
      confLabelCol.x - COL_PAD - COL_DUR_W,
      COL_DUR_W,
      true,
    );
    const thoughtStart = purposeCol.end + COL_PAD;
    const thoughtEnd = durCol.x - COL_PAD;
    const thoughtCol = col(thoughtStart, Math.max(0, thoughtEnd - thoughtStart));
    return {
      laneOrigin,
      laneCount,
      num: numCol,
      sess: sessCol,
      purpose: purposeCol,
      thought: thoughtCol,
      dur: durCol,
      confLabel: confLabelCol,
      confBar: confBarCol,
    };
  }

  function laneX(layout: Layout, lane: number): number {
    return layout.laneOrigin + lane * LANE_W + LANE_W / 2;
  }

  // ---------------------------------------------------------------------------
  // Drawing primitives. Every text draw goes through drawCell — which clips
  // to the column rect, truncates with binary-search measureText, and aligns.
  // No raw fillText elsewhere.
  // ---------------------------------------------------------------------------
  function fitText(ctx: CanvasRenderingContext2D, text: string, maxW: number): string {
    if (maxW <= 0) return "";
    if (ctx.measureText(text).width <= maxW) return text;
    const ELL = "…";
    if (ctx.measureText(ELL).width > maxW) return "";
    let lo = 0;
    let hi = text.length;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      const candidate = text.slice(0, mid) + ELL;
      if (ctx.measureText(candidate).width <= maxW) lo = mid;
      else hi = mid - 1;
    }
    return lo > 0 ? text.slice(0, lo) + ELL : ELL;
  }

  function drawCell(
    ctx: CanvasRenderingContext2D,
    text: string,
    col: Col,
    y: number,
    color: string,
    font: string = FONT_BODY,
  ): void {
    ctx.save();
    ctx.beginPath();
    ctx.rect(col.x, y - ROW_H / 2, col.w, ROW_H);
    ctx.clip();
    ctx.font = font;
    const fitted = fitText(ctx, flat(text), col.w);
    ctx.fillStyle = color;
    ctx.textBaseline = "middle";
    if (col.alignRight) {
      ctx.textAlign = "right";
      ctx.fillText(fitted, col.end, y);
    } else {
      ctx.textAlign = "left";
      ctx.fillText(fitted, col.x, y);
    }
    ctx.restore();
  }

  function flat(s: string): string {
    return s.replace(/\s+/g, " ").trim();
  }

  function formatDuration(ms: number | null | undefined): string {
    if (ms == null) return "";
    if (ms < 1) return "<1ms";
    if (ms < 1000) return `${Math.round(ms)}ms`;
    if (ms < 60_000) return `${(ms / 1000).toFixed(1)}s`;
    return `${Math.round(ms / 1000)}s`;
  }

  // ---------------------------------------------------------------------------
  // CSS variable resolution. Cached per draw() to avoid hitting
  // getComputedStyle in the inner loop.
  // ---------------------------------------------------------------------------
  function getCss(name: string): string {
    return getComputedStyle(canvas).getPropertyValue(name).trim() || "#999";
  }

  function purposeColorVar(p: string): string {
    return purposeColor(p).match(/--purpose-[a-z]+/)?.[0] ?? "--purpose-unknown";
  }

  // ---------------------------------------------------------------------------
  // Main draw.
  // ---------------------------------------------------------------------------
  function draw(): void {
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    sizeCanvas();
    ctx.save();
    ctx.scale(dpr, dpr);
    const cw = canvas.clientWidth;
    const ch = canvas.clientHeight;
    ctx.clearRect(0, 0, cw, ch);

    const allSteps = traceStore.current.history.steps;
    const branches = traceStore.current.branches;
    const steps = visibleSteps();

    if (steps.length === 0) {
      ctx.fillStyle = getCss("--muted");
      ctx.font = FONT_BODY;
      ctx.textBaseline = "middle";
      ctx.fillText(
        allSteps.length === 0
          ? "(no steps in this session)"
          : "(no steps match the current filter)",
        12,
        24
      );
      ctx.restore();
      return;
    }

    const { lanes, visible: visibleLanes, overflow: laneOverflow } = laneAssignment(steps, branches);
    const inAllMode = traceStore.active.endsWith(ALL_SESSIONS_SUFFIX);
    const projectId = traceStore.currentProject;
    const layout = computeLayout(cw, visibleLanes, inAllMode);

    const rowByStep = new Map<number, number>();
    steps.forEach((s, i) => rowByStep.set(s.step_number, i));

    // Resolve CSS once per draw.
    const C = {
      rule: getCss("--rule"),
      ruleStrong: getCss("--rule-strong"),
      ink: getCss("--ink"),
      inkSoft: getCss("--ink-soft"),
      muted: getCss("--muted"),
      bg: getCss("--bg"),
      bgElev: getCss("--bg-elev"),
      bgActive: getCss("--bg-active"),
      accent: getCss("--accent"),
      warn: getCss("--warn"),
      alert: getCss("--alert"),
      ok: getCss("--ok"),
      relSupports: getCss("--rel-supports"),
      relRefutes: getCss("--rel-refutes"),
      relDepends: getCss("--rel-depends"),
      relUnlabeled: getCss("--rel-unlabeled"),
    };

    // ---- header strip with lane labels + column headers
    const headerY = PADDING_TOP + HEADER_H - 6;
    ctx.font = FONT_SMALL;
    ctx.textBaseline = "middle";
    // In stitched (__ALL__) mode lane 0's `main` would lie — every
    // session has its own `main` and they share lane 0 in the canvas.
    // Skip the text labels; the session badge column carries the
    // disambiguation per row. Vertical lane lines still draw below.
    if (!inAllMode) {
      ctx.fillStyle = C.muted;
      ctx.textAlign = "center";
      ctx.fillText("main", laneX(layout, 0), headerY);
      const laneToBranchId = new Map<number, string>();
      for (const [bid, lane] of lanes) {
        if (!laneToBranchId.has(lane)) laneToBranchId.set(lane, bid);
      }
      for (let l = 1; l < visibleLanes; l++) {
        const bid = laneToBranchId.get(l);
        if (!bid) continue;
        const b = branches.get(bid);
        const label = b?.name && b.name !== bid ? b.name : bid;
        ctx.save();
        ctx.beginPath();
        ctx.rect(laneX(layout, l) - 14, headerY - 7, 28, 14);
        ctx.clip();
        ctx.fillStyle = laneOverflow && l === visibleLanes - 1 ? C.alert : C.muted;
        ctx.textAlign = "center";
        ctx.fillText(fitText(ctx, label, 26), laneX(layout, l), headerY);
        ctx.restore();
      }
    }
    // Column header labels — use the column rects we already computed.
    const headerCols: Array<[Col, string]> = [
      [layout.num, "#"],
      [layout.purpose, "purpose"],
      [layout.thought, "thought"],
      [layout.dur, "dur"],
      [layout.confLabel, "conf"],
    ];
    if (layout.sess) headerCols.push([layout.sess, "session"]);
    for (const [col, label] of headerCols) {
      drawCell(ctx, label, col, headerY, C.muted, FONT_SMALL);
    }

    // Hairline divider below the header.
    ctx.strokeStyle = C.rule;
    ctx.lineWidth = 1;
    ctx.beginPath();
    const hl = Math.floor(PADDING_TOP + HEADER_H) + 0.5;
    ctx.moveTo(0, hl);
    ctx.lineTo(cw, hl);
    ctx.stroke();

    // ---- vertical lane lines
    ctx.strokeStyle = C.rule;
    ctx.lineWidth = 1;
    ctx.beginPath();
    const lanesTop = PADDING_TOP + HEADER_H;
    for (let l = 0; l < visibleLanes; l++) {
      const x = Math.floor(laneX(layout, l)) + 0.5;
      ctx.moveTo(x, lanesTop);
      ctx.lineTo(x, ch - PADDING_TOP);
    }
    ctx.stroke();

    // Faint dividers between text columns (purpose | thought | right-cluster).
    ctx.strokeStyle = C.rule;
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (const x of [layout.purpose.end + COL_PAD / 2, layout.thought.end + COL_PAD / 2]) {
      const px = Math.floor(x) + 0.5;
      ctx.moveTo(px, lanesTop);
      ctx.lineTo(px, ch - PADDING_TOP);
    }
    ctx.stroke();

    // ---- rows
    for (const [i, step] of steps.entries()) {
      const y = rowYForIndex(i);
      const rawLane = step.branch_id ? (lanes.get(step.branch_id) ?? 0) : 0;
      const lane = Math.min(rawLane, visibleLanes - 1);
      const x = laneX(layout, lane);
      const ref = traceStore.selectedRef;
      const isSelected = ref !== null && ref.stepNumber === step.step_number;
      const isRevised = step.revised_by != null;

      // Per-branch row tint — subtle background so branch rows are
      // visually grouped even when the lane gutter is narrow.
      if (step.branch_id) {
        ctx.fillStyle = C.bgElev;
        ctx.fillRect(LANE_GUTTER_L, y - ROW_H / 2, cw - LANE_GUTTER_L, ROW_H);
      }

      // Selection band: a subtle full-row tint plus a punchier
      // left-edge accent so the active row reads from across the room.
      if (isSelected) {
        ctx.fillStyle = C.bgActive;
        ctx.fillRect(LANE_GUTTER_L, y - ROW_H / 2, cw - LANE_GUTTER_L, ROW_H);
        ctx.fillStyle = C.accent;
        ctx.fillRect(0, y - ROW_H / 2, SEL_STRIPE_W, ROW_H);
      }

      // Pinned stripe — full-height left edge in warn color, just
      // inside the selection stripe so they don't fight.
      if (step.pinned) {
        ctx.fillStyle = C.warn;
        ctx.fillRect(SEL_STRIPE_W, y - ROW_H / 2, PINNED_STRIPE_W, ROW_H);
      }

      // Step glyph: filled circle if normal, hollow if revised. If lane was
      // collapsed (overflow), draw a tiny "+" badge on top.
      ctx.beginPath();
      ctx.arc(x, y, GLYPH_R, 0, Math.PI * 2);
      if (isRevised) {
        ctx.fillStyle = getCss("--bg");
        ctx.fill();
        ctx.lineWidth = 1.4;
        ctx.strokeStyle = C.muted;
        ctx.stroke();
      } else {
        ctx.fillStyle = C.inkSoft;
        ctx.fill();
      }
      if (laneOverflow && rawLane >= visibleLanes - 1 && rawLane > visibleLanes - 1) {
        ctx.fillStyle = C.alert;
        ctx.font = FONT_SMALL;
        ctx.textAlign = "center";
        ctx.textBaseline = "middle";
        ctx.fillText("+", x, y);
      }

      // # column (right-aligned).
      drawCell(
        ctx,
        String(step.step_number),
        layout.num,
        y,
        isRevised ? C.muted : isSelected ? C.ink : C.inkSoft,
      );

      // Session badge (stitched-all mode only). Shows the suffix part
      // of `<project>__<suffix>` so the column stays readable on a
      // narrow rail. The label resolves to `(default)` for steps that
      // belong to the project's bare default session.
      if (layout.sess && step.session_id) {
        drawCell(
          ctx,
          sessionLabel(step.session_id, projectId),
          layout.sess,
          y,
          C.muted,
          FONT_SMALL,
        );
      }

      // purpose column (clipped + truncated).
      drawCell(
        ctx,
        step.purpose,
        layout.purpose,
        y,
        getCss(purposeColorVar(step.purpose)),
      );

      // thought column (clipped + truncated).
      drawCell(
        ctx,
        step.thought,
        layout.thought,
        y,
        isSelected ? C.ink : isRevised ? C.muted : C.inkSoft,
      );

      // duration column.
      drawCell(
        ctx,
        formatDuration(step.duration_ms ?? null),
        layout.dur,
        y,
        C.muted,
        FONT_SMALL,
      );

      // confidence label + bar.
      if (step.confidence != null && Number.isFinite(step.confidence)) {
        const c = Math.max(0, Math.min(1, step.confidence));
        drawCell(
          ctx,
          `${Math.round(c * 100)}%`,
          layout.confLabel,
          y,
          c < 0.6 ? C.warn : C.muted,
          FONT_SMALL,
        );
        const bar = layout.confBar;
        const trackY = y - COL_CONF_BAR_H / 2;
        ctx.fillStyle = C.rule;
        ctx.fillRect(bar.x, trackY, bar.w, COL_CONF_BAR_H);
        const fillColor = c >= 0.8 ? C.ok : c >= 0.5 ? C.accent : C.warn;
        ctx.fillStyle = fillColor;
        ctx.fillRect(bar.x, trackY, bar.w * c, COL_CONF_BAR_H);
      }

      // Branch fork connector. Only drawn on the FIRST row of a branch and
      // only when from_step is visible above this row.
      if (step.branch_id && step.branch_from !== undefined) {
        const firstInBranch = steps.findIndex((s) => s.branch_id === step.branch_id);
        const parentRow = rowByStep.get(step.branch_from);
        if (firstInBranch === i && parentRow !== undefined && parentRow < i) {
          ctx.strokeStyle = C.muted;
          ctx.lineWidth = 1;
          ctx.beginPath();
          const px = laneX(layout, 0);
          const py = rowYForIndex(parentRow);
          ctx.moveTo(px, py);
          ctx.lineTo(px, y);
          ctx.lineTo(x - GLYPH_R - 2, y);
          ctx.stroke();
        }
      }
    }

    // ---- branch fold-back (merged) arrows
    for (const branch of branches.values()) {
      if (branch.status !== "merged" || branch.merged_into === undefined) continue;
      const rawLane = lanes.get(branch.id);
      if (rawLane === undefined) continue;
      const lane = Math.min(rawLane, visibleLanes - 1);
      const branchSteps = steps.filter((s) => s.branch_id === branch.id);
      const last = branchSteps[branchSteps.length - 1];
      if (!last) continue;
      const fromRow = rowByStep.get(last.step_number);
      const toRow = rowByStep.get(branch.merged_into);
      if (fromRow === undefined || toRow === undefined) continue;
      ctx.strokeStyle = C.relSupports;
      ctx.setLineDash([4, 3]);
      ctx.lineWidth = 1;
      ctx.beginPath();
      const fx = laneX(layout, lane);
      const fy = rowYForIndex(fromRow);
      const tx = laneX(layout, 0);
      const ty = rowYForIndex(toRow);
      ctx.moveTo(fx, fy);
      ctx.lineTo(fx, ty);
      ctx.lineTo(tx + GLYPH_R + 2, ty);
      ctx.stroke();
      ctx.setLineDash([]);
    }

    // ---- overlays (toggled by `d`): revision back-arrows + dep edges.
    //      Defaulted off so the lane gutter stays clean on dense traces;
    //      the hollow-glyph + revised-row tinting still flag revised
    //      steps at a glance without the curves.
    if (showOverlays) {
      for (const step of steps) {
        if (!step.revises_step) continue;
        const fromRow = rowByStep.get(step.step_number);
        const toRow = rowByStep.get(step.revises_step);
        if (fromRow === undefined || toRow === undefined) continue;
        const rawLane = step.branch_id ? (lanes.get(step.branch_id) ?? 0) : 0;
        const lane = Math.min(rawLane, visibleLanes - 1);
        const x = laneX(layout, lane);
        const yFrom = rowYForIndex(fromRow);
        const yTo = rowYForIndex(toRow);
        ctx.strokeStyle = C.accent;
        ctx.lineWidth = 1;
        ctx.beginPath();
        const cpx = Math.max(layout.laneOrigin, x - 14);
        ctx.moveTo(x - GLYPH_R, yFrom);
        ctx.bezierCurveTo(cpx, yFrom, cpx, yTo, x - GLYPH_R, yTo);
        ctx.stroke();
        ctx.fillStyle = C.accent;
        ctx.beginPath();
        ctx.moveTo(x - GLYPH_R, yTo);
        ctx.lineTo(x - GLYPH_R - 4, yTo - 3);
        ctx.lineTo(x - GLYPH_R - 4, yTo + 3);
        ctx.closePath();
        ctx.fill();
      }
    }

    // ---- dependency edges (same `d` toggle as revisions).
    if (showOverlays) {
      const relColor = (rel?: string): string =>
        rel === "supports"
          ? C.relSupports
          : rel === "refutes"
            ? C.relRefutes
            : rel === "depends_on"
              ? C.relDepends
              : C.relUnlabeled;
      for (const step of steps) {
        if (!step.dependencies) continue;
        const fromRow = rowByStep.get(step.step_number);
        if (fromRow === undefined) continue;
        const fromRawLane = step.branch_id ? (lanes.get(step.branch_id) ?? 0) : 0;
        const fromLane = Math.min(fromRawLane, visibleLanes - 1);
        const fx = laneX(layout, fromLane);
        const fy = rowYForIndex(fromRow);
        for (const d of step.dependencies) {
          const depN = typeof d === "number" ? d : d.step;
          const rel = typeof d === "number" ? undefined : d.relation;
          const toRow = rowByStep.get(depN);
          if (toRow === undefined) continue;
          const toStep = steps[toRow];
          const toRawLane = toStep?.branch_id
            ? (lanes.get(toStep.branch_id) ?? 0)
            : 0;
          const toLane = Math.min(toRawLane, visibleLanes - 1);
          const tx = laneX(layout, toLane);
          const ty = rowYForIndex(toRow);
          ctx.strokeStyle = relColor(rel);
          ctx.lineWidth = 1;
          ctx.globalAlpha = 0.65;
          ctx.beginPath();
          const cpx = Math.max(fx, tx) + 30;
          ctx.moveTo(fx + GLYPH_R, fy);
          ctx.bezierCurveTo(cpx, fy, cpx, ty, tx + GLYPH_R, ty);
          ctx.stroke();
          ctx.globalAlpha = 1;
        }
      }
    }

    ctx.restore();
  }

  function onMouseDown(e: MouseEvent): void {
    const rect = canvas.getBoundingClientRect();
    const yRel = e.clientY - rect.top - PADDING_TOP - HEADER_H;
    if (yRel < 0) return; // clicked the header strip
    const idx = Math.floor(yRel / ROW_H);
    const step = visibleSteps()[idx];
    if (step) traceStore.selectStep(step.step_number);
  }
</script>

<svelte:window on:keydown={onKey} />

<div class="wrap" bind:this={wrapper}>
  <canvas bind:this={canvas} onmousedown={onMouseDown}></canvas>
</div>

<style>
  .wrap {
    position: relative;
    height: 100%;
    overflow: auto;
    background: var(--bg);
  }
  canvas {
    display: block;
  }
</style>
