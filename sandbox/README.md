# kurbo-se sandbox

Interactive playground for [kurbo-se](../). SVG path data goes into the
wasm build of the crate, a fill outline comes back, and plain SVG renders
it. There are no renderer dependencies; the point is fast geometry
iteration.

## Run

```sh
cd sandbox
npm install
npm run dev
```

`npm run dev` builds the wasm module and starts Vite. It needs `wasm-pack`
on your PATH (`cargo install wasm-pack`). Edits to any `.rs` file in
`kurbo-se/src` or `sandbox/wasm/src` rebuild the wasm and reload the page.
Dev builds compile with `opt-level = 2`, because unoptimized geometry is
about 20× slower and feels laggy. `npm run build` uses `wasm-pack
--release`.

## What's on screen

- Panel: Figma-style stroke settings. A Fill group (show/hide and colour),
  stroke colour, Alignment (Inside/Center/Outside), raw Side override
  (Left/Center/Right), Weight, Join, Miter angle in degrees, per-end caps,
  Dashes (a comma-separated pattern like Figma's `2, 4, 6, 8`; even
  entries are dash lengths, odd entries gaps, and an odd-length list reads
  as doubled), Dash offset, Dash cap, and a tolerance selector.
- Stroke result: the expanded outline, filled with the nonzero rule (the
  output's contract) in the chosen colour, over the shape's own fill.
  Toggle "Even-odd rule" to x-ray the self-overlap structure; artifacts
  that nonzero cancels become visible.
- Result wireframe + nodes: the outline's raw segments.
- Source path + control points: the input hairline, in green.
- Direction, orientation, side: a travel arrow at each subpath start plus
  a badge: `CW+`/`CCW−` (signed area, Y-down positive is CW), `open`, the
  resolved side letter, or `·zero` for degenerate subpaths.
- Miter-fallback markers: a ring on every corner when the join is Miter.
  Orange means the interior angle is at or below the miter angle and the
  corner bevels; green means it stays sharp.
- Native SVG stroke reference: the browser's own stroking of the source
  path, in red. For Center + solid the red and blue regions must coincide,
  which makes it a free correctness oracle. Known intentional differences
  elsewhere: SVG has no per-end caps (the start cap is used for both), no
  alignment, and browsers dash across subpaths while kurbo-se restarts the
  pattern per subpath.
- Stats (bottom bar): input/output segment counts, expansion time, and
  per-subpath orientation/side/area chips.

Two-finger scroll pans. Pinch, or ⌘/ctrl + scroll, zooms toward the
cursor. Dragging pans too. The corner toolbar zooms in steps; clicking the
percentage or double-clicking the canvas fits the shape.

## Gallery

The shapes cover the edge cases the crate had to get right: an open
polyline; an open curve with a loop and cusp; a circle; a rectangle; a
concave star; a donut (hole with opposite winding); a self-intersecting
bowtie; a figure-eight; zero-length segments; a smooth spiral; a
multi-subpath mix (which also demonstrates dash phase); and a sharp wedge
that exercises the inner-side clip.
