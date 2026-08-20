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
  width: 12,
  alignment: "inside" as "center" | "inside" | "outside",
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
      "Stroke",
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
            ["fill", "Result fill"],
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

  // Result fill.
  const fill = layer("layer-fill");
  fill.replaceChildren();
  if (state.layers.fill && debug.outputD) {
    fill.append(
      svgEl("path", {
        d: debug.outputD,
        fill: "#6ea8fe",
        "fill-opacity": 0.45,
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
      stroke: "#ff5c5c",
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
        stroke: "#d7dae0",
        "stroke-width": px(1),
      }),
    );
    const parsed = parseD(debug.outputD);
    for (const sub of parsed.anchors)
      for (const [x, y] of sub)
        wire.append(svgEl("circle", { cx: x, cy: y, r: px(1.6), fill: "#d7dae0" }));
  }

  // Source path + control points.
  const src = layer("layer-src");
  src.replaceChildren();
  if (state.layers.src) {
    src.append(
      svgEl("path", {
        d: shape.d,
        fill: "none",
        stroke: "#3ddc84",
        "stroke-width": px(1),
        "stroke-dasharray": `${px(4)} ${px(3)}`,
      }),
    );
    const parsed = parseD(shape.d);
    for (const [cx, cy, ax, ay] of parsed.controls) {
      src.append(
        svgEl("line", { x1: cx, y1: cy, x2: ax, y2: ay, stroke: "#3ddc84", "stroke-opacity": 0.4, "stroke-width": px(0.7) }),
        svgEl("circle", { cx, cy, r: px(1.8), fill: "none", stroke: "#3ddc84", "stroke-width": px(0.8) }),
      );
    }
    for (const sub of parsed.anchors)
      for (const [x, y] of sub)
        src.append(svgEl("rect", { x: x - px(1.6), y: y - px(1.6), width: px(3.2), height: px(3.2), fill: "#3ddc84" }));
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
            fill: "#ffc857",
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
        fill: "#ffc857",
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
          stroke: j.fellBack ? "#ff8c42" : "#3ddc84",
          "stroke-width": px(1),
          "stroke-opacity": 0.9,
        }),
      );
    }
  }

  // Stats.
  const stats = document.getElementById("stats")!;
  const sub = debug.subpaths
    .map(
      (s, i) =>
        `  ${i}: ${s.closed ? "closed" : "open"}${s.zeroLength ? " zero" : ""} area ${s.area.toFixed(1)} side ${s.side}`,
    )
    .join("\n");
  stats.innerHTML = debug.error
    ? `<span class="err">${debug.error}</span>`
    : `<b>${shapes[state.shapeIx].name}</b>\n` +
      `segs in ${debug.inputSegs} → out <b>${debug.outputSegs}</b>\n` +
      `expand <b>${lastExpandMs.toFixed(2)} ms</b>  tol ${state.tolerance}\n` +
      `miter limit ${debug.miterLimit.toFixed(3)}\n` +
      sub;
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
  render();
}

function setupViewControls() {
  const canvas = document.getElementById("canvas")!;
  canvas.addEventListener("wheel", (e) => {
    e.preventDefault();
    const k = e.deltaY > 0 ? 1.15 : 1 / 1.15;
    const rect = canvas.getBoundingClientRect();
    const fx = (e.clientX - rect.left) / rect.width;
    const fy = (e.clientY - rect.top) / rect.height;
    const { x, y, w, h } = state.view;
    state.view = {
      x: x + fx * w * (1 - k),
      y: y + fy * h * (1 - k),
      w: w * k,
      h: h * k,
    };
    render();
  });
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
