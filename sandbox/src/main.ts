import init, {
  expand_stroke_debug,
  gallery,
} from "../wasm/pkg/kurbo_se_sandbox.js";

type GalleryEntry = {
  name: string;
  d: string;
  bbox: [number, number, number, number];
};
type SubpathInfo = {
  closed: boolean;
  zeroLength: boolean;
  area: number;
  side: string;
  start: [number, number];
};
type JoinInfo = { x: number; y: number; angle: number; fellBack: boolean };
type DebugOut = {
  outputD: string;
  subpaths: SubpathInfo[];
  joins: JoinInfo[];
  inputSegs: number;
  outputSegs: number;
  miterLimit: number;
  error: string | null;
};

const state = {
  shapeIx: 4,
  width: 10,
  alignment: "center" as "center" | "inside" | "outside",
  side: "auto" as "auto" | "left" | "center" | "right",
  join: "miter" as "miter" | "bevel" | "round",
  miterAngle: 28.96,
  startCap: "none" as "none" | "round" | "square",
  endCap: "none" as "none" | "round" | "square",
  dashed: false,
  dashLen: 18,
  gap: 10,
  dashOffset: 0,
  dashCap: "none" as "none" | "round" | "square",
  animate: false,
  tolerance: 0.01,
  shapeFill: true,
  fillColor: "#ffffff",
  strokeColor: "#0056d6",
  layers: {
    fill: true,
    evenodd: false,
    wire: true,
    src: true,
    dir: true,
    joins: true,
    ref: false,
  },
  view: { x: 0, y: 0, w: 300, h: 300 },
  fitW: 300,
};

let shapes: GalleryEntry[] = [];
let lastExpandMs = 0;

// ---------- tiny absolute-path d parser (M/L/Q/C/Z, kurbo's to_svg output) ----------
type Parsed = {
  anchors: [number, number][][];
  controls: [number, number, number, number][]; // control point + its anchor
  firstDir: ([number, number, number, number] | null)[]; // start pt + dir per subpath
};
function parseD(d: string): Parsed {
  const anchors: [number, number][][] = [];
  const controls: [number, number, number, number][] = [];
  const firstDir: ([number, number, number, number] | null)[] = [];
  const tokens = d.match(/[MLQCZz]|-?(?:\d+\.?\d*|\.\d+)(?:e-?\d+)?/gi) ?? [];
  let i = 0;
  let cur: [number, number] = [0, 0];
  let sub: [number, number][] | null = null;
  const num = () => parseFloat(tokens[i++]);
  while (i < tokens.length) {
    const cmd = tokens[i++];
    switch (cmd) {
      case "M": {
        cur = [num(), num()];
        sub = [cur];
        anchors.push(sub);
        firstDir.push(null);
        break;
      }
      case "L": {
        const p: [number, number] = [num(), num()];
        if (firstDir[firstDir.length - 1] === null && sub!.length === 1)
          firstDir[firstDir.length - 1] = [cur[0], cur[1], p[0], p[1]];
        cur = p;
        sub!.push(p);
        break;
      }
      case "Q": {
        const c1: [number, number] = [num(), num()];
        const p: [number, number] = [num(), num()];
        controls.push([c1[0], c1[1], cur[0], cur[1]], [c1[0], c1[1], p[0], p[1]]);
        if (firstDir[firstDir.length - 1] === null && sub!.length === 1)
          firstDir[firstDir.length - 1] = [cur[0], cur[1], c1[0], c1[1]];
        cur = p;
        sub!.push(p);
        break;
      }
      case "C": {
        const c1: [number, number] = [num(), num()];
        const c2: [number, number] = [num(), num()];
        const p: [number, number] = [num(), num()];
        controls.push([c1[0], c1[1], cur[0], cur[1]], [c2[0], c2[1], p[0], p[1]]);
        if (firstDir[firstDir.length - 1] === null && sub!.length === 1)
          firstDir[firstDir.length - 1] = [cur[0], cur[1], c1[0], c1[1]];
        cur = p;
        sub!.push(p);
        break;
      }
      case "Z":
      case "z":
        break;
      default:
        // Unexpected token: bail out to avoid an infinite loop.
        i = tokens.length;
    }
  }
  return { anchors, controls, firstDir };
}

/// The `d` restricted to closed subpaths. SVG fills open subpaths as if
/// implicitly closed, which would paint phantom areas under open polylines —
/// the crate's fill semantics give open subpaths no fill at all. kurbo's
/// `to_svg` emits absolute commands, so subpaths start at each `M` and a
/// closed one contains a `Z`.
function closedSubpathsD(d: string): string {
  return d
    .split(/(?=M)/)
    .filter((sub) => /Z/i.test(sub))
    .join("");
}

// ---------- DOM helpers ----------
const SVGNS = "http://www.w3.org/2000/svg";
function svgEl(tag: string, attrs: Record<string, string | number>): SVGElement {
  const el = document.createElementNS(SVGNS, tag);
  for (const [k, v] of Object.entries(attrs)) el.setAttribute(k, String(v));
  return el;
}
function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  props: Partial<HTMLElementTagNameMap[K]> & { class?: string } = {},
  ...children: (HTMLElement | string)[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  const { class: cls, ...rest } = props;
  if (cls) el.className = cls;
  Object.assign(el, rest);
  el.append(...children);
  return el;
}

// ---------- panel ----------
function segmented<T extends string>(
  options: [T, string][],
  get: () => T,
  set: (v: T) => void,
): HTMLElement {
  const wrap = h("div", { class: "seg" });
  const update = () => {
    for (const b of Array.from(wrap.children) as HTMLButtonElement[])
      b.classList.toggle("on", b.dataset.v === get());
  };
  for (const [v, label] of options) {
    const b = h("button", { textContent: label });
    b.dataset.v = v;
    b.onclick = () => {
      set(v);
      update();
      render();
    };
    wrap.append(b);
  }
  update();
  return wrap;
}

function row(label: string, ...ctrl: HTMLElement[]): HTMLElement {
  return h("div", { class: "row" }, h("label", { textContent: label }), ...ctrl);
}

function slider(
  min: number,
  max: number,
  step: number,
  get: () => number,
  set: (v: number) => void,
): HTMLElement[] {
  const r = h("input", { type: "range" });
  r.min = String(min);
  r.max = String(max);
  r.step = String(step);
  const n = h("input", { type: "number" });
  n.min = String(min);
  n.step = String(step);
  const sync = () => {
    r.value = String(get());
    n.value = String(get());
  };
  r.oninput = () => {
    set(parseFloat(r.value));
    sync();
    render();
  };
  n.oninput = () => {
    const v = parseFloat(n.value);
    if (Number.isFinite(v)) {
      set(v);
      r.value = n.value;
      render();
    }
  };
  sync();
  return [r, n];
}

function select<T extends string>(
  options: [T, string][],
  get: () => T,
  set: (v: T) => void,
): HTMLSelectElement {
  const s = h("select");
  for (const [v, label] of options) s.append(h("option", { value: v, textContent: label }));
  s.value = get();
  s.onchange = () => {
    set(s.value as T);
    render();
  };
  return s;
}

function group(title: string, ...children: HTMLElement[]): HTMLElement {
  return h("div", { class: "group" }, h("div", { class: "title", textContent: title }), ...children);
}

const CAPS: ["none" | "round" | "square", string][] = [
  ["none", "None"],
  ["round", "Round"],
  ["square", "Square"],
];

function colorRow(label: string, get: () => string, set: (v: string) => void): HTMLElement {
  const c = h("input", { type: "color" });
  c.value = get();
  const hex = h("span", { class: "hex", textContent: get() });
  c.oninput = () => {
    set(c.value);
    hex.textContent = c.value;
    render();
  };
  return row(label, c, hex);
}

function checkbox(get: () => boolean, set: (v: boolean) => void): HTMLElement {
  const c = h("input", { type: "checkbox", checked: get() });
  c.onchange = () => {
    set(c.checked);
    render();
  };
  return c;
}

function buildPanel() {
  const panel = document.getElementById("panel")!;
  panel.replaceChildren(
    h("h1", { innerHTML: "kurbo-se <span>stroke sandbox</span>" }),
    group(
      "Shape",
      row(
        "Gallery",
        select(
          shapes.map((s, i) => [String(i), s.name]) as [string, string][],
          () => String(state.shapeIx),
          (v) => {
            state.shapeIx = parseInt(v);
            resetView();
          },
        ),
      ),
    ),
    group(
      "Fill",
      row("Show fill", checkbox(() => state.shapeFill, (v) => (state.shapeFill = v))),
      colorRow("Color", () => state.fillColor, (v) => (state.fillColor = v)),
    ),
    group(
      "Stroke",
      colorRow("Color", () => state.strokeColor, (v) => (state.strokeColor = v)),
      row("Alignment", segmented([["inside", "Inside"], ["center", "Center"], ["outside", "Outside"]], () => state.alignment, (v) => (state.alignment = v))),
      row("Side (raw)", segmented([["auto", "Auto"], ["left", "L"], ["center", "C"], ["right", "R"]], () => state.side, (v) => (state.side = v))),
      row("Weight", ...slider(0, 60, 0.5, () => state.width, (v) => (state.width = v))),
      row("Join", select([["miter", "Miter"], ["bevel", "Bevel"], ["round", "Round"]], () => state.join, (v) => (state.join = v))),
      row("Miter angle", ...slider(0, 180, 0.01, () => state.miterAngle, (v) => (state.miterAngle = v))),
      row("Start cap", select(CAPS, () => state.startCap, (v) => (state.startCap = v))),
      row("End cap", select(CAPS, () => state.endCap, (v) => (state.endCap = v))),
    ),
    group(
      "Dash",
      row("Style", segmented([["solid", "Solid"], ["dash", "Dash"]], () => (state.dashed ? "dash" : "solid"), (v) => (state.dashed = v === "dash"))),
      row("Dash", ...slider(0, 60, 1, () => state.dashLen, (v) => (state.dashLen = v))),
      row("Gap", ...slider(0, 60, 1, () => state.gap, (v) => (state.gap = v))),
      row("Dash offset", ...slider(-120, 120, 0.5, () => state.dashOffset, (v) => (state.dashOffset = v))),
      row("Dash cap", select(CAPS, () => state.dashCap, (v) => (state.dashCap = v))),
      row(
        "Animate",
        (() => {
          const c = h("input", { type: "checkbox", checked: state.animate });
          c.onchange = () => {
            state.animate = c.checked;
            if (state.animate) tick();
          };
          return c;
        })(),
      ),
    ),
    group(
      "Quality",
      row("Tolerance", select([["0.25", "0.25 (display)"], ["0.01", "0.01"], ["0.001", "0.001"], ["0.0001", "0.0001"]], () => String(state.tolerance), (v) => (state.tolerance = parseFloat(v)))),
    ),
    group(
      "Layers",
      h(
        "div",
        { class: "checks" },
        ...(
          [
            ["fill", "Stroke result"],
            ["evenodd", "Even-odd rule (artifact x-ray)"],
            ["wire", "Result wireframe + nodes"],
            ["src", "Source path + control points"],
            ["dir", "Direction, orientation, side"],
            ["joins", "Miter-fallback markers"],
            ["ref", "Native SVG stroke reference"],
          ] as [keyof typeof state.layers, string][]
        ).map(([key, label]) => {
          const c = h("input", { type: "checkbox", checked: state.layers[key] });
          c.onchange = () => {
            state.layers[key] = c.checked;
            render();
          };
          return h("label", {}, c, label);
        }),
      ),
    ),
  );
}

// ---------- rendering ----------
function styleJson(): string {
  return JSON.stringify({
    width: state.width,
    alignment: state.alignment,
    side: state.side === "auto" ? null : state.side,
    join: state.join,
    miterAngle: state.miterAngle,
    startCap: state.startCap,
    endCap: state.endCap,
    dash: state.dashed
      ? { pattern: [state.dashLen, state.gap], offset: state.dashOffset, cap: state.dashCap }
      : null,
  });
}

function layer(id: string): SVGGElement {
  return document.getElementById(id) as unknown as SVGGElement;
}

function px(v: number): number {
  // Scale-invariant hairline sizes: view width / canvas width.
  const canvas = document.getElementById("canvas")!;
  return (state.view.w / canvas.clientWidth) * v;
}

function render() {
  const shape = shapes[state.shapeIx];
  if (!shape) return;
  const t0 = performance.now();
  const debug: DebugOut = JSON.parse(
    expand_stroke_debug(shape.d, styleJson(), state.tolerance),
  );
  lastExpandMs = performance.now() - t0;

  const canvas = document.getElementById("canvas")!;
  canvas.setAttribute(
    "viewBox",
    `${state.view.x} ${state.view.y} ${state.view.w} ${state.view.h}`,
  );

  // The source shape's own fill, under everything. Open subpaths have no
  // fill (SVG would implicitly close them and paint phantom areas).
  const shapeFill = layer("layer-shapefill");
  shapeFill.replaceChildren();
  const fillD = closedSubpathsD(shape.d);
  if (state.shapeFill && fillD) {
    shapeFill.append(
      svgEl("path", { d: fillD, fill: state.fillColor, "fill-rule": "nonzero" }),
    );
  }

  // Stroke result.
  const fill = layer("layer-fill");
  fill.replaceChildren();
  if (state.layers.fill && debug.outputD) {
    fill.append(
      svgEl("path", {
        d: debug.outputD,
        fill: state.strokeColor,
        "fill-opacity": 0.92,
        "fill-rule": state.layers.evenodd ? "evenodd" : "nonzero",
      }),
    );
  }

  // Native SVG stroke reference (center + solid comparison oracle).
  const ref = layer("layer-ref");
  ref.replaceChildren();
  if (state.layers.ref) {
    const capMap = { none: "butt", round: "round", square: "square" } as const;
    const attrs: Record<string, string | number> = {
      d: shape.d,
      fill: "none",
      stroke: "#ff6b6b",
      "stroke-opacity": 0.6,
      "stroke-width": state.width,
      "stroke-linecap": capMap[state.startCap],
      "stroke-linejoin": state.join === "miter" ? "miter" : state.join,
      "stroke-miterlimit": debug.miterLimit,
    };
    if (state.dashed) {
      attrs["stroke-dasharray"] = `${state.dashLen} ${state.gap}`;
      attrs["stroke-dashoffset"] = -state.dashOffset;
    }
    ref.append(svgEl("path", attrs));
  }

  // Result wireframe + nodes.
  const wire = layer("layer-wire");
  wire.replaceChildren();
  if (state.layers.wire && debug.outputD) {
    wire.append(
      svgEl("path", {
        d: debug.outputD,
        fill: "none",
        stroke: "#8f99ad",
        "stroke-width": px(1),
      }),
    );
    const parsed = parseD(debug.outputD);
    for (const sub of parsed.anchors)
      for (const [x, y] of sub)
        wire.append(svgEl("circle", { cx: x, cy: y, r: px(1.6), fill: "#8f99ad" }));
  }

  // Source path + control points.
  const src = layer("layer-src");
  src.replaceChildren();
  if (state.layers.src) {
    src.append(
      svgEl("path", {
        d: shape.d,
        fill: "none",
        stroke: "#4cd6a0",
        "stroke-width": px(1),
        "stroke-dasharray": `${px(4)} ${px(3)}`,
      }),
    );
    const parsed = parseD(shape.d);
    for (const [cx, cy, ax, ay] of parsed.controls) {
      src.append(
        svgEl("line", { x1: cx, y1: cy, x2: ax, y2: ay, stroke: "#4cd6a0", "stroke-opacity": 0.4, "stroke-width": px(0.7) }),
        svgEl("circle", { cx, cy, r: px(1.8), fill: "none", stroke: "#4cd6a0", "stroke-width": px(0.8) }),
      );
    }
    for (const sub of parsed.anchors)
      for (const [x, y] of sub)
        src.append(svgEl("rect", { x: x - px(1.6), y: y - px(1.6), width: px(3.2), height: px(3.2), fill: "#4cd6a0" }));
  }

  // Direction arrows + orientation/side badges.
  const dir = layer("layer-dir");
  dir.replaceChildren();
  if (state.layers.dir) {
    const parsed = parseD(shape.d);
    debug.subpaths.forEach((info, ix) => {
      const fd = parsed.firstDir[ix];
      const [sx, sy] = info.start;
      if (fd) {
        const [x0, y0, x1, y1] = fd;
        const len = Math.hypot(x1 - x0, y1 - y0) || 1;
        const ux = (x1 - x0) / len;
        const uy = (y1 - y0) / len;
        const s = px(10);
        const tipX = sx + ux * s;
        const tipY = sy + uy * s;
        dir.append(
          svgEl("path", {
            d: `M ${tipX} ${tipY} L ${tipX - ux * s * 0.55 - uy * s * 0.3} ${tipY - uy * s * 0.55 + ux * s * 0.3} L ${tipX - ux * s * 0.55 + uy * s * 0.3} ${tipY - uy * s * 0.55 - ux * s * 0.3} Z`,
            fill: "#f2c14e",
          }),
        );
      }
      const label = info.zeroLength
        ? "·zero"
        : `${info.closed ? (info.area >= 0 ? "CW+" : "CCW−") : "open"} ${info.side[0].toUpperCase()}`;
      const t = svgEl("text", {
        x: sx + px(6),
        y: sy - px(6),
        "font-size": px(9),
        fill: "#f2c14e",
      });
      t.textContent = label;
      dir.append(t);
    });
  }

  // Join markers (only meaningful with miter joins).
  const joins = layer("layer-joins");
  joins.replaceChildren();
  if (state.layers.joins && state.join === "miter") {
    for (const j of debug.joins) {
      joins.append(
        svgEl("circle", {
          cx: j.x,
          cy: j.y,
          r: px(3),
          fill: "none",
          stroke: j.fellBack ? "#ff9a4d" : "#4cd6a0",
          "stroke-width": px(1),
          "stroke-opacity": 0.9,
        }),
      );
    }
  }

  // Stats bar.
  const stats = document.getElementById("stats")!;
  stats.replaceChildren();
  if (debug.error) {
    stats.append(h("span", { class: "err", textContent: debug.error }));
  } else {
    const item = (html: string) => {
      const el = h("span");
      el.innerHTML = html;
      return el;
    };
    stats.append(
      item(`<b>${shapes[state.shapeIx].name}</b>`),
      item(`segs ${debug.inputSegs} → <b>${debug.outputSegs}</b>`),
      item(`expand <b>${lastExpandMs.toFixed(2)} ms</b>`),
      item(`tol ${state.tolerance}`),
      item(`miter limit ${debug.miterLimit.toFixed(3)}`),
      ...debug.subpaths.map((sp, i) => {
        const kind = sp.zeroLength
          ? "zero"
          : sp.closed
            ? `closed ${sp.area >= 0 ? "CW+" : "CCW−"}`
            : "open";
        const el = h("span", { class: "chip" });
        el.innerHTML = `<b>#${i}</b> ${kind} · side ${sp.side[0].toUpperCase()} · area ${sp.area.toFixed(1)}`;
        return el;
      }),
    );
  }
  updateZoomPct();
}

function updateZoomPct() {
  const el = document.getElementById("zoom-pct");
  if (el) el.textContent = `${Math.round((state.fitW / state.view.w) * 100)}%`;
}

// ---------- view control ----------
function resetView() {
  const [x0, y0, x1, y1] = shapes[state.shapeIx].bbox;
  const pad = Math.max(60, state.width * 3);
  state.view = {
    x: x0 - pad,
    y: y0 - pad,
    w: x1 - x0 + 2 * pad,
    h: y1 - y0 + 2 * pad,
  };
  state.fitW = state.view.w;
  render();
}

/// Zoom range relative to fit: 3%..6400%.
function clampW(w: number): number {
  return Math.min(Math.max(w, state.fitW / 64), state.fitW / 0.03);
}

/// Zoom by factor `k` (>1 zooms out) around the viewport fraction (fx, fy).
function zoomAt(fx: number, fy: number, k: number) {
  const { x, y, w, h } = state.view;
  const kk = clampW(w * k) / w;
  state.view = {
    x: x + fx * w * (1 - kk),
    y: y + fy * h * (1 - kk),
    w: w * kk,
    h: h * kk,
  };
  render();
}

function setupViewControls() {
  const canvas = document.getElementById("canvas")!;
  const viewport = document.getElementById("viewport")!;

  // All wheel/pinch handling is captured at the window and gated by cursor
  // COORDINATES, never by event target: Safari hit-tests SVG children and
  // the SVG root differently, so target-based listeners work over the shape
  // but not the empty canvas (or the other way round).
  const overViewport = (x: number, y: number) => {
    const r = viewport.getBoundingClientRect();
    return x >= r.left && x <= r.right && y >= r.top && y <= r.bottom;
  };

  // ---- pinch (Safari WebKit-only gesture events) ----
  // Fields are not in the TS DOM lib and not guaranteed at runtime; guard
  // every number — a single NaN in the view blanks the whole canvas.
  let pinch: { view: typeof state.view; fx: number; fy: number } | null = null;
  const gesturePoint = (e: Event) => {
    const rect = canvas.getBoundingClientRect();
    const g = e as unknown as { clientX?: number; clientY?: number };
    const cx =
      typeof g.clientX === "number" && isFinite(g.clientX)
        ? g.clientX
        : rect.left + rect.width / 2;
    const cy =
      typeof g.clientY === "number" && isFinite(g.clientY)
        ? g.clientY
        : rect.top + rect.height / 2;
    // Clamp into the viewport so a pinch straying over the panel still
    // anchors inside the view.
    return {
      fx: Math.min(1, Math.max(0, (cx - rect.left) / rect.width)),
      fy: Math.min(1, Math.max(0, (cy - rect.top) / rect.height)),
    };
  };
  const onGestureStart = (e: Event) => {
    e.preventDefault(); // never let Safari pinch-zoom the page itself
    // Anchor the whole pinch at its start point: `scale` is cumulative
    // since gesturestart, so a per-event anchor would drift.
    pinch = { view: { ...state.view }, ...gesturePoint(e) };
  };
  const onGestureChange = (e: Event) => {
    e.preventDefault();
    if (!pinch) return;
    const scale = (e as unknown as { scale?: number }).scale;
    if (typeof scale !== "number" || !isFinite(scale) || scale <= 0) return;
    const w = clampW(pinch.view.w / scale);
    const h = pinch.view.h * (w / pinch.view.w);
    // Keep the world point under the fingers fixed.
    state.view = {
      x: pinch.view.x + pinch.fx * (pinch.view.w - w),
      y: pinch.view.y + pinch.fy * (pinch.view.h - h),
      w,
      h,
    };
    render();
  };
  const onGestureEnd = (e: Event) => {
    e.preventDefault();
    pinch = null;
  };
  window.addEventListener("gesturestart", onGestureStart as EventListener, {
    passive: false,
    capture: true,
  });
  window.addEventListener("gesturechange", onGestureChange as EventListener, {
    passive: false,
    capture: true,
  });
  window.addEventListener("gestureend", onGestureEnd as EventListener, {
    passive: false,
    capture: true,
  });

  // ---- wheel ----
  // Figma-style: two-finger scroll pans; pinch (or ctrl/⌘ + scroll) zooms
  // toward the cursor. Chrome/Firefox deliver trackpad pinches as ctrl+wheel
  // with small deltas; a real mouse wheel notch is ±100+, so clamp the
  // per-event factor to keep both usable.
  window.addEventListener(
    "wheel",
    (e) => {
      if (!overViewport(e.clientX, e.clientY)) return; // let the panel scroll
      e.preventDefault();
      if (pinch) return; // a gesture owns the view right now
      const rect = canvas.getBoundingClientRect();
      if (e.ctrlKey || e.metaKey) {
        const fx = (e.clientX - rect.left) / rect.width;
        const fy = (e.clientY - rect.top) / rect.height;
        const k = Math.min(1.5, Math.max(1 / 1.5, Math.exp(e.deltaY * 0.01)));
        zoomAt(fx, fy, k);
      } else {
        const s = state.view.w / rect.width;
        state.view.x += e.deltaX * s;
        state.view.y += e.deltaY * s;
        render();
      }
    },
    { passive: false, capture: true },
  );

  let drag: { x: number; y: number } | null = null;
  canvas.addEventListener("pointerdown", (e) => {
    drag = { x: e.clientX, y: e.clientY };
    canvas.setPointerCapture(e.pointerId);
  });
  canvas.addEventListener("pointermove", (e) => {
    if (!drag) return;
    const rect = canvas.getBoundingClientRect();
    const sx = state.view.w / rect.width;
    state.view.x -= (e.clientX - drag.x) * sx;
    state.view.y -= (e.clientY - drag.y) * sx;
    drag = { x: e.clientX, y: e.clientY };
    render();
  });
  canvas.addEventListener("pointerup", () => (drag = null));
  canvas.addEventListener("dblclick", () => resetView());

  document.getElementById("zoom-in")!.onclick = () => zoomAt(0.5, 0.5, 1 / 1.3);
  document.getElementById("zoom-out")!.onclick = () => zoomAt(0.5, 0.5, 1.3);
  document.getElementById("zoom-pct")!.onclick = () => resetView();
}

// ---------- dash animation ----------
let lastT = 0;
function tick() {
  if (!state.animate) return;
  requestAnimationFrame((t) => {
    if (lastT) {
      state.dashOffset += ((t - lastT) / 1000) * 24;
      if (state.dashOffset > 1e4) state.dashOffset = 0;
    }
    lastT = t;
    render();
    tick();
  });
}

// ---------- URL state (shareable repros) ----------
function applyQueryParams() {
  const q = new URLSearchParams(location.search);
  const num = (k: string, set: (v: number) => void) => {
    const v = q.get(k);
    if (v !== null && Number.isFinite(parseFloat(v))) set(parseFloat(v));
  };
  const str = <T extends string>(k: string, allowed: T[], set: (v: T) => void) => {
    const v = q.get(k);
    if (v !== null && (allowed as string[]).includes(v)) set(v as T);
  };
  const flag = (k: string, set: (v: boolean) => void) => {
    const v = q.get(k);
    if (v !== null) set(v === "1" || v === "true");
  };
  num("shape", (v) => (state.shapeIx = Math.min(shapes.length - 1, Math.max(0, v | 0))));
  num("width", (v) => (state.width = v));
  str("alignment", ["center", "inside", "outside"], (v) => (state.alignment = v));
  str("side", ["auto", "left", "center", "right"], (v) => (state.side = v));
  str("join", ["miter", "bevel", "round"], (v) => (state.join = v));
  num("miterAngle", (v) => (state.miterAngle = v));
  str("startCap", ["none", "round", "square"], (v) => (state.startCap = v));
  str("endCap", ["none", "round", "square"], (v) => (state.endCap = v));
  flag("dashed", (v) => (state.dashed = v));
  num("dash", (v) => (state.dashLen = v));
  num("gap", (v) => (state.gap = v));
  num("dashOffset", (v) => (state.dashOffset = v));
  str("dashCap", ["none", "round", "square"], (v) => (state.dashCap = v));
  num("tol", (v) => (state.tolerance = v));
  flag("shapeFill", (v) => (state.shapeFill = v));
  const color = (k: string, set: (v: string) => void) => {
    const v = q.get(k);
    if (v !== null && /^[0-9a-fA-F]{6}$/.test(v)) set(`#${v}`);
  };
  color("fillColor", (v) => (state.fillColor = v));
  color("strokeColor", (v) => (state.strokeColor = v));
  for (const k of Object.keys(state.layers) as (keyof typeof state.layers)[]) {
    flag(k, (v) => (state.layers[k] = v));
  }
}

// ---------- boot ----------
await init();
shapes = JSON.parse(gallery());
applyQueryParams();
buildPanel();
setupViewControls();
resetView();
