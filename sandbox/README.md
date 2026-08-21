# kurbo-se sandbox

Interactive playground for [kurbo-se](../): SVG path data goes into the wasm
build of the crate, a fill outline comes back, plain SVG renders it. No
renderer dependencies — the point is fast geometry iteration.

## Run

```sh
cd sandbox
npm install
npm run dev
```

`npm run dev` builds the wasm module (via `wasm-pack`, which must be on your
PATH: `cargo install wasm-pack`) and starts Vite. Edits to any `.rs` file in
`kurbo-se/src` or `sandbox/wasm/src` rebuild the wasm and reload the page.
Dev builds compile with `opt-level = 2` (unoptimized geometry is ~20×
slower and feels laggy); `npm run build` uses `wasm-pack --release`.

## What's on screen

- **Panel** mirrors Figma's stroke settings — a Fill group (show/hide +
  colour), stroke colour, Alignment (Inside/Center/Outside), raw Side
  override (Left/Center/Right), Weight, Join + Miter angle (degrees),
  per-end caps, Dashes (a comma-separated pattern like Figma's: `2, 4, 6, 8`
  — even entries are dash lengths, odd entries gaps, and an odd-length list
  reads as doubled)/Dash offset/Dash cap — plus a tolerance selector.
- **Stroke result** renders the expanded outline with the **nonzero** rule
  (the output's contract) in the chosen stroke colour, over the shape's own
  fill. Toggle **Even-odd rule** to x-ray the self-overlap structure —
  artifacts that nonzero cancels become visible.
- **Result wireframe + nodes** shows the outline's raw segments.
- **Source path + control points** is the input hairline (dashes = green).
- **Direction, orientation, side** draws a travel arrow at each subpath
  start plus a badge: `CW+`/`CCW−` (signed area, Y-down positive=CW),
  `open`, the resolved side letter, or `·zero` for degenerate subpaths.
- **Miter-fallback markers** ring every corner when the join is Miter:
  orange = interior angle ≤ miter angle (bevels), green = stays sharp.
- **Native SVG stroke reference** overlays the browser's own stroking of
  the source path (red). For **Center + solid** the red and blue regions
  must coincide — a free correctness oracle. Known intentional differences
  elsewhere: SVG has no per-end caps (the start cap is used for both), no
  alignment, and browsers dash across subpaths while kurbo-se restarts the
  pattern per subpath.
- **Stats** (bottom bar): input/output segment counts, expansion time, and
  per-subpath orientation/side/area chips.

Two-finger scroll pans; pinch (or ⌘/ctrl + scroll) zooms toward the cursor;
drag pans too. The toolbar in the corner zooms in steps, and clicking the
percentage — or double-clicking the canvas — fits the shape.

## Gallery

The shapes cover the mission's edge cases: open polyline; open curve with a
loop/cusp; circle; rectangle; concave star; donut (hole with opposite
winding); self-intersecting bowtie; figure-eight; zero-length segments; a
smooth spiral; a multi-subpath mix (dash-phase demo); and a sharp wedge
that exercises the inner-side clip.
