# Changelog

This changelog follows <https://keepachangelog.com/en/>.

## 0.2.0 (2026-10-22)

### Fixed

- Strokes with bevel or miter joins no longer tear apart when the width
  gets close to the spacing of the corners. The stroker drops the parts of
  an offset outline that fall inside the stroke, and it measured that as
  if every corner were round. A bevel's straight edge sits closer to the
  corner than the width, so it got dropped, while parts inside a miter's
  tip were kept. A star stroked centered at width 32 came out with holes
  in its arms, and a 24-tooth gear with bevels filled solid when stroked
  inside from width 7.5 on. The check now uses the real shape of each
  join: a triangle for a bevel, a kite for a miter, a disc for a round
  join.
- Dashed strokes on one side of a path that crosses itself now put their
  dashes where kurbo does. Such a stroke cuts the path at its crossings
  and dashes each piece on its own, so every piece has to pick up the
  pattern where the whole path has it. That offset went the wrong way, it
  was guessed rather than measured inside curves, and on closed paths it
  carried on past the start point, where kurbo starts the pattern again.
  On a bowtie, a figure-eight or a looping curve, the dashes after the
  first crossing ended up in the wrong place. Centered strokes, and paths
  that don't cross themselves, were not affected.
- Dashed strokes no longer draw stray dots. A zero-length dash that
  landed exactly on a crossing was drawn by both pieces that meet there.
  Rounding added more where pattern boundaries fall on corners or on the
  start point, which is common on shapes with whole-number edge lengths:
  a dash about `1e-14` long still got round caps and drew a disc. The
  stroker now allows for rounding at those points and drops dashes too
  short to matter.
- Dashed strokes no longer produce `NaN` coordinates. kurbo's `dash`
  returns `NaN` when a zero-length entry of the pattern lands exactly on
  the end of a segment, and the corrected dash positions made that
  common enough to hit 31 of 48,000 random dashed strokes. Those elements
  are now dropped.
- A dash that runs to the end of an open path keeps the path's end cap
  again. kurbo can end such a dash a hair short, at `x = 7.299999999999999`
  on a line ending at 7.3, and the stroker then gave it the dash cap
  instead. Endpoints now match within the distance the stroker resolves.
- One-sided strokes now go on the right side of the loop in a curve that
  crosses itself. The stroker decides which side is filled by testing a
  point just inside the loop, and kurbo's winding count missed both
  crossings there. On the sandbox's looping cubic the loop was taken for
  empty, so outside strokes went into it. The stroker now counts the
  crossings itself, along a line that cuts across the curve, and makes
  sure the point it tests is not sitting on the curve.
- Dashed inside strokes now line the loop of a self-crossing curve instead
  of filling it. After a dash is trimmed to the fill, outlines made of a
  single curve were thrown away as slivers, but a loop cut off at a
  crossing is often just one curve. Outlines are now judged by their area.
- Dashed inside and outside strokes no longer leave unpainted gaps where
  they are wider than the shape. There the inner edge of a dash folds back
  across the dash, and the part inside the fill was kept although it
  bounds nothing. That left an unpainted line across the sandbox's loop at
  width 10. A dash is now also cut where it crosses itself, and an edge is
  kept only where the dash covers just one side of it.
- Inside and outside strokes no longer switch sides when the fill test
  lands on a curve. The stroker tests a point just below the highest
  points of a contour, and it counted the points where a curve reaches
  furthest left or right among them. Below such a point the test lands on
  the curve itself. A 12-point star with inward-curved edges was stroked
  on the wrong side at every width. Only a curve's highest and lowest
  points count now.
- Paths are now cut where they actually cross. The crossing search ends
  by crossing straight approximations, which can land well off the real
  crossing when a curve has a control point bunched at one end: 11 units
  off on a notch of Impact's `w`. Cut there, a shape could be stroked on
  the wrong side, or dashed from the wrong place. A few Newton steps now
  move each crossing onto both curves. On 1,344 curved stars the strokes
  with wrong points went from 89 to 5, and on spiky polygons from 8 to 2.
- Inside and centered strokes on very spiky shapes no longer fill solid or
  paint the middle at large widths. Near the width where the inside of
  such a shape disappears, what's left is a small ring of short arcs. The
  stroker closed a loop as soon as it came back near its start, so each
  arc became its own loop and was dropped as a sliver. Each arc also ran a
  little past its neighbours, by less than the trimming margin, and those
  overshoots made extra loops in the middle. A loop now closes only if it
  bounds more than a sliver, and at a crossing the piece that heads into
  the other stroke is dropped, with a few exceptions glyphs needed.

Gears show the last fix well. A 24-tooth gear stroked inside at widths 60
to 80 used to fill solid, and so did a 750-tooth gear at widths 8 to 30.
Both now keep their middle. On text the changes are neutral: letters,
digits and `&@$%#?` from Inter and 15 macOS fonts, stroked at widths 2 and
12 with every alignment and compared point by point against what the
stroke should cover, went from 7,117 wrong points to 7,064 over 15,504
cases.

### Changed

- Inside strokes with bevel joins now fill a shape solid at a larger width
  than round joins. A bevel cuts across a concave corner, so it reaches
  less far. The test star now fills solid from about width 48.7 with
  bevels, against 40 with round joins. Before, it filled solid from
  about 25.
- Large and detailed paths stroke much faster. Finding crossings no
  longer tests every pair of segments, distance checks read only nearby
  edges, and stitching, fill tests and the dash mask skip work whose
  answer is already known. Against the fixed code before these speedups,
  a circle drawn as 4,000 tiny lines strokes in 0.7–0.9 ms instead of
  12–16 ms, a braid that crosses itself 161 times in 11 ms instead of 87,
  a plate with 64 holes 20–26% faster, and the 750-tooth gear in 5.9 ms
  instead of 9.4. A rounded rectangle stroked centered as wide as its
  corners, where the inner offset collapses, takes 9.5 ms instead of 16.
  Most small shapes got faster too, but a few are still slower: a rounded
  rectangle takes 5.9 µs instead of 5.5, a 25-tooth gear 278 µs instead
  of 254, a five-point star stroked inside at width 500 10.1 µs instead
  of 9.3, and a five-point polyline 0.59 µs instead of 0.49. The output is
  the same, bit for bit, on about 5,200 test cases.
- A reused `AlignedStrokeCtx` now also keeps the working storage of the
  region construction and the dash mask, so animated and batched strokes
  allocate far less. 200 small shapes stroke 19% faster through one
  context than before, and an animated dashed star 14% faster.
- Shapes with many corners got slower because of the new join check and
  the refined crossings: the 10-point star's inside stroke went from 7.4
  to 9.3 µs, and with the speedups above it is at 8.8 µs. Curved shapes
  and dashed strokes are within noise of 0.1.2 or faster. The 0.1.1
  figure for a 1500-segment gear, 254 ms down to 28 ms, may have been
  measured on a wrong result: that gear's inside stroke used to fill
  solid.
- `tests/golden.txt` is regenerated. 21 of its 82 cases change: the dashed
  one-sided cases on the bowtie, figure-eight and loop curve, the solid
  one-sided cases on the loop curve, and eleven where the refined
  crossings move the outline a little.

### Added

- Tests for each fix: bevel and miter joins on stars and a gear, dash
  positions and dots on self-crossing paths checked against
  `kurbo::dash`, the end cap, the loop's fill side, the spiky gears,
  Inter's `4`, a thin `#` and the curved stars. The `check_at` helper
  gained `check_side_at`, which checks one alignment.
- Benchmarks for a 750-tooth gear stroked inside at width 8, and
  `benches/workloads.rs`: 70 more, from UI shapes and long dashed paths to
  tiny segments and extreme widths.
- `tools/regress`, which strokes about 5,200 shapes and styles with a
  reference revision and with the working tree and checks that they paint
  the same, and `tests/context_reuse.rs`, which checks that a reused
  context gives the same output as a fresh one.

### Sandbox

- The native SVG reference now draws its dashes where the crate does. It
  passed the dash offset negated, so the red reference dashes sat away
  from the crate's. kurbo reads the offset exactly like SVG's
  `stroke-dashoffset`, so it now goes through unchanged.
- A new gallery entry, "Text (type your own)", strokes up to 32 characters
  of your own text in the bundled Inter Regular. Overlapping parts are
  merged first, so touching letters stroke as one outline. The text, its
  size and the merge switch go in the URL like the other settings.

## 0.1.2 (2026-10-11)

### Fixed

- Tiny segments no longer split a contour in two. Boolean operations often
  leave segments around `1e-13` long where two outlines met, and the
  self-intersection check saw their two ends as the same point, so it
  treated a simple contour as one that crosses itself. In practice a
  merged glyph like `$` lost the centered stroke around one of its holes,
  and inside strokes showed thin slivers. These segments are now dropped
  when the path is first read, using the same distance the
  self-intersection check uses.
- Holes with a slanted top are recognized as holes again. To decide which
  side of a contour is filled, the stroker probes a point just below its
  highest corners. For a hole whose top corner leans to one side, with a
  straight vertical edge under the next corner, every probe missed and the
  stroker assumed the hole was filled. The counter of the `4` in Lato and
  Poppins got painted solid by an inside stroke, and a centered stroke
  skipped its edge. When the usual probe misses, it now tries a point just
  beside the middle of an edge.
- Inside and outside strokes no longer draw long straight lines across a
  shape. After trimming, the stroker joins the remaining pieces back into
  loops, connecting a piece to whichever one starts within a small radius.
  That radius grew with the tolerance, so at coarse settings it was wider
  than the stroke itself and pieces got connected to the wrong neighbours.
  At fine settings it was too small to bridge a real gap, such as where
  the `$` bar crosses the S, and the loop was closed with a straight line
  down the bar. The radius is now capped at a quarter of the stroke width,
  and when nothing is close enough, the loop continues to the nearest
  remaining piece. That lookup goes through a grid, so it stays fast.

To check these on real text, every Latin letter, digit and some symbols
from 56 font files were set in lines, merged and stroked at width 2 with
round joins, then compared point by point against what the stroke should
cover. At tolerances 0.25, 0.0625 and 0.0156, the wrong points dropped
from 2.4M, 0.8M and 0.7M to 91k, 14k and 1.8k. Before, every font had at
least one line with noticeable errors; now 28 do. A few cases got worse:
64 out of 1887, and only 15 by a visible amount, mostly at the coarsest
tolerance. In those the loop still continues to the wrong nearby piece,
for example `æ` in Lato SemiBold Italic, `À` and `!` in Instrument Sans,
and `cdg` in Poppins Bold Italic.

### Changed

- Performance is within 2% of 0.1.1. The one exception is the 10-point
  star, which went from 7.19 to 7.41 µs because of the extra pass over the
  input. For comparison, kurbo's own stroke benchmark moved by −0.9% in the
  same run.

### Added

- Tests for each of the three fixes: a square with a tiny segment on its
  closing edge, a hole with a slanted top, and Inter's `$` with its S and
  bar merged by `linesweeper`, checked at three tolerances. The `check`
  and `check_center` helpers gained `check_at` and `check_center_at`,
  which take a tolerance.

## 0.1.1 (2026-08-22)

### Fixed

- Overflow-scale coordinates could hang the call outright. When every
  bounding box overlaps every other and no subdivision ever looks flat —
  what differences of `1e300` produce — the intersection search ran to its
  full depth down every branch, and 2^28 nodes never finishes. A
  four-segment path was enough. The search now carries a node budget, and
  paths whose coordinates do not survive squaring (beyond roughly `1e154`)
  are gated at the input like non-finite ones, since every predicate in the
  pipeline is meaningless there and kurbo's own arc-length solver panics on
  them.
- The flattening tolerance is now floored at a fraction of the geometry's
  own extent. `kurbo::flatten` emits segments in proportion to size over
  tolerance and nothing bounds that: a path spanning `1e300` flattened at
  `1e-3` asked for more segments than a `Vec` can hold and aborted the
  process on allocation.
- A bridge chord in the region stitcher is refused past a multiple of the
  stroke width. It exists to replace dropped join geometry, which is bounded
  by the join, but nothing stopped it spanning an arbitrary distance when
  pruning removed a long run of pieces — drawing a confident straight line
  through geometry that was never part of the boundary. The loop now ends
  there instead, which is visible rather than plausible.
- Region pieces are classified from three samples rather than one.
  A single midpoint sample assumes validity is constant along a piece, which
  holds only while the cut set is complete, and it deliberately is not:
  tangential touches are ignored, near-coincident hits are merged, and the
  subdivision search stops at a depth limit.
- The saturation test is now per-component. It compared a *net* signed area
  summed across every loop, so one component emitted with reversed winding
  could cancel a legitimate one and discard the whole region — a wide stroke
  silently filling solid, indistinguishable from intended saturation. The
  net test is kept for genuine cancel pairs (a donut at exactly half its
  ring thickness) but applies only once the loops are confirmed to run
  within the resolution of one another.
- The vertex-coincidence radius scales with the subpath's extent instead of
  being fixed at `1e-6` user units, so self-intersection splitting no longer
  depends on whether the caller works in pixels or in a normalized system.
- Hairline widths keep a working fold-over guard. Below roughly twice the
  tolerance, the approximation slack consumed the whole distance threshold
  and the guard switched itself off.

### Changed

- Region stitching uses a spatial grid for continuations and an ordered
  per-contour set for bridges, replacing two linear scans that made the walk
  quadratic in surviving pieces. A detailed contour stroked wider than its
  own feature spacing cuts into tens of thousands of pieces, and that
  dominated everything else: a 1500-segment gear at width 8 went from 254 ms
  to 28 ms. Output is unchanged, including the preference for the
  lowest-indexed candidate.
- The extra sampling costs the 10-point star, which goes 4.7 → 7.0 µs: a
  polyline of sharp reflex corners sends nearly every piece through the
  cut-and-prune path, which is where the samples are taken. Measured
  against the previous code on the same machine back to back, every other
  benchmark is unchanged or slightly faster, the 40-cubic wavy path by 4%,
  since the stitcher's grid replaces linear scans there.

### Added

- `hostile_coordinates_stay_finite`: 200 deterministic cases built from
  coordinates a fuzzer would reach for — exact zeros, values that underflow
  or overflow when squared, ULP-apart neighbours — at every alignment,
  dashed and solid, each under a watchdog. It found the hang and the
  allocation abort above.
- Tests for hairline widths at and below the tolerance, and for
  scale-invariant self-intersection splitting.

## 0.1.0 (2026-08-21)

Initial release, targeting kurbo 0.13.

### Added

- `StrokeStyle` / `StrokeAlignment` / `StrokeSide` / `DashStyle`: Figma's
  stroke panel as a style type. Alignment (inside/center/outside), raw side
  override, joins with a miter angle in degrees, independent start/end
  caps, dash pattern/offset/cap.
- `stroke_aligned` / `stroke_aligned_with`, plus `AlignedStrokeCtx` for
  allocation reuse: expand an aligned stroke into a nonzero-rule fill
  outline. One-sided bands keep the source path as their exact shared
  boundary. Sharp corners are clipped so inside strokes never leak outside
  the fill, and outside strokes never leak in.
- Hole-aware per-subpath side resolution via a winding probe. Holes stroke
  into the ring; reversing winding direction never changes the result.
- Dashing on the centerline, with seam-merge on closed contours (smooth
  `dash_offset` animation), pattern sanitization (empty, negative, or
  zero-sum patterns render solid instead of panicking or hanging), and
  zero-length dashes rendered as dots (round, or oriented square).
- `miter_angle_to_limit` / `miter_limit_to_angle` conversions.
- `From<&StrokeStyle> for kurbo::Stroke` escape hatch. Centered
  interpretation; the lossiness is documented on the impl.
- `analyze_subpaths` introspection for editors and debug tooling.
- `no_std` support (`libm` feature), a `serde` feature, no `unsafe`.
- Interactive wasm sandbox (`sandbox/`), headless vello example
  (`examples/vello-demo`), criterion benchmarks.
- Golden characterization tests (`tests/golden.rs` plus a repo-only
  fixture) that pin the exact outline of a 10-shape × 8-style matrix, as a
  net for behavior-preserving refactors.
- Package metadata: author, repository and homepage
  (<https://github.com/ninsent/kurbo-se>), plus `LICENSE-APACHE`,
  `LICENSE-MIT` and `AUTHORS` files.

The sections below record the pre-release development history of the
above. None of it was ever published.

### Changed (during development)

- Aligned strokes on closed contours became set-defined, matching Figma:
  `Inside(w) = { p ∈ F : dist(p, path) ≤ w }` and
  `Outside(w) = { p ∉ F : dist(p, path) ≤ w }`. An inside stroke never
  leaves the fill and an outside stroke never enters it, at any width. A
  width beyond the local thickness saturates: the shape fills solid
  instead of the band folding over itself and punching hollow pockets.
  Implemented by pruning the raw offset — cut at every intersection,
  discard pieces nearer than `w` or on the wrong side of the fill,
  restitch — replacing the earlier effective-width clamp.
- Self-intersecting contours are split at their crossings. Each lobe of a
  bowtie or figure-eight resolves its side against the fill independently,
  and overlapping bands add instead of cancelling.
- Faster predicates: the source is flattened once into a `y`-bucketed edge
  index, so distance and winding queries no longer run a per-curve solve.
  Line-involving intersections use kurbo's analytic
  `PathSeg::intersect_line`.
- One-sided expansion fast path: when the raw offsets have no mutual or
  self intersections — every smooth contour at moderate width — each
  offset loop is classified whole, and the cut/prune/stitch machinery is
  skipped. Adjacent segments are domain-trimmed before intersection, so
  smooth joints reject at the bounding-box level instead of recursing to
  flatness. Inside/outside strokes on circles and donuts got 2–5× faster,
  and one-sided cost no longer grows as tolerance shrinks.

### Fixed (during development)

- Centered strokes on closed contours grew holes at extreme weights. Past
  `w/2 >` the local thickness, the direct band's inner boundary inverts
  and its winding cancels interior coverage: a circle stroked wider than
  its diameter had a hole in the middle, and a donut's hole contour did
  the same from `w/2 >` the hole radius. Solid centered bands on closed
  simple contours are now built set-theoretically like the one-sided ones,
  `Center(w) = D_{w/2} \ E_{w/2}`, so they saturate instead. Centered
  strokes on open subpaths, dashed centered strokes, and self-intersecting
  contours (kurbo winding-additive parity, documented) keep the direct
  band. Solid centered closed contours now cost the region construction:
  about 50 µs for the 62-segment circle against 14 µs before.
- Dashing stopped partway along paths containing degenerate segments. A
  fully coincident `QuadTo` has a `NaN` arc length upstream
  (`QuadBez::arclen`), which poisoned the dash phase; everything past that
  element rendered as one uninterrupted band. Segments whose squared
  length vanishes are now dropped before dashing and before
  self-intersection splitting. That includes control deltas too small to
  square without underflowing, which are not point-coincident and used to
  make the whole output non-finite; runs of them also read as the path
  revisiting a vertex and split dashes for no reason. Arc-length reads are
  additionally guarded, so an unmeasurable overflow-scale segment cannot
  stall the phase. Reported upstream as
  [`upstream/04-quad-arclen-nan.md`](upstream/04-quad-arclen-nan.md).
- Solid strokes on paths with underflow-scale segments went non-finite. A
  delta of about 1e-300 passes an exact point-inequality check, but its
  squared length underflows to zero, so normalizing the tangent divided by
  zero. Only the dash and split stages had the guard. Degenerate segments
  are now dropped once at input canonicalization, which protects every
  construction. The synthesized closing chord, created after
  canonicalization, gets its own underflow-aware gate, and an
  all-subnormal subpath now renders like a coincident-point one: a dot at
  most. Measured cost: none.
- Square dash caps on curved contours shattered each dash into a wedge
  plus a sliver. The cap overshoots the fill on a convex curve, so the
  mask runs; a fill-boundary piece that straddled the end of the band's
  shared edge was discarded whole as coincident, stranding that edge as
  its own loop. The fill boundary is now also cut at the ends of the
  shared edge, so every piece is either wholly coincident with it or
  wholly free.
- Extreme inside weights on a star punched a hole in the middle of an
  otherwise saturated shape: weights around 79–95 failed while 60 and 100
  were fine. The distance prune exempted outer-side join geometry
  outright, so at large widths the join fans at the star's reflex corners
  survived inside the fill and stitched into a bogus erosion loop. The
  exemption is now bounded — a bevel chord may come within `cos(φ/2)·w` of
  its corner, round joins and miters not at all — and applies only to the
  dilation, where corner-cutting is the requested style and nothing folds
  inward. For the erosion the distance test is the saturation guard and
  stays strict. Dropping a bevel chord there costs nothing, because the
  stitcher bridges the gap with the same chord.
- Dashed inside/outside strokes are now masked by the fill. A dash is
  banded directly, since the region construction cannot be used once the
  contour is cut into dashes, so a band that did not fit the local
  geometry escaped: a dash starting at a star's tip sat mostly outside the
  shape, and dashes beside the bowtie's crossing reached into the
  neighbouring lobe's fill. Each dash and synthesized dot is now
  intersected with the fill, or subtracted from it for outside alignment.
  The mask is skipped where the excursion is below `tolerance`, so
  display-tolerance dashing keeps its speed. A dashed circle costs about
  55 µs against 31 µs before.
- Inside/outside strokes on paths mixing open and closed subpaths: the
  open subpaths' geometry took part in the region predicates (distance
  pruning and implicit-closure winding), so at larger widths a nearby open
  polyline carved chunks out of a closed contour's ring, and the stitcher
  bridged the holes with chords across the fill (sandbox "Multi-subpath
  mixed", outside weights above about 24). The region construction now
  sees only the closed contours that participate in it. Open subpaths are
  banded independently and add by winding where they overlap.
- The Boolean between overlapping one-sided bands of different contours is
  now join-aware. Cross-contour validity of a boundary piece used to be a
  distance test against the other sources, which is only correct for round
  joins: it under-pruned inside miter wedges (another contour's arc stubs
  survived inside a corner and stitched into strips across the fill) and
  over-pruned inside bevel notches (the union boundary lost its transition
  pieces and split into disjoint loops, exposing the fill). It is now a
  winding test against the actual raw offset loops, with a coincidence
  guard for loops closer than the resolution. The distance test remains as
  the self-fold guard, restricted to the piece's own contour. Outer-side
  join geometry — bevel chords, miter legs, round fans — is tagged at
  emission and exempt from it.
- Degenerate one-sided widths no longer blow up:
  - A collapsed offset (a circle stroked inside at exactly `w = r`) was
    shattered by cusp handling into thousands of segments and cut
    pairwise, over 120 ms per call. A resolution guard now drops loops
    smaller than the pruning fuzz before cutting: about 1 ms, correct
    saturation.
  - Two coincident offsets (a donut at exactly half its ring thickness)
    stitched about 1.4k fuzz segments into the output. A net-area gate now
    collapses region components thinner than the boundary band everywhere
    into clean saturation.
- Sandbox redesign: refreshed palette and panel styling; stats moved to a
  bottom bar so they no longer overlap the drawing; Figma-style navigation
  (two-finger scroll pans, pinch or ⌘/ctrl+scroll zooms toward the cursor,
  a zoom toolbar with fit, double-click to fit, a clamped zoom range); a
  shape Fill toggle with colour pickers for both the fill and the stroke
  result, all URL-shareable; and a pruned gallery. The kurbo #344 cubic
  and the collinear-cubics entries are gone, and the polyline spiral is
  now a smooth Hermite-fitted Archimedean spiral.
- Sandbox: the dev server's wasm now builds with `opt-level = 2`, and
  `npm run build` uses `wasm-pack --release`. Both were unoptimized
  `--dev` builds before, the dominant cause of sandbox lag.
- `no_std` (`libm`) build: the `y`-bucket lookups in the source index
  called the std-only `f64::floor`/`f64::ceil` inherent methods. They now
  go through the crate's float shims. Caught by the CI `no-std` job.

### Sandbox

- The dash controls take a full Figma-style pattern (`2, 4, 6, 8`) instead
  of a single dash/gap pair. Even entries are dash lengths, odd entries
  gaps, and odd-length lists read doubled — the semantics the crate always
  had in `DashStyle::from_pattern`, now reachable from the panel, from the
  shareable URL (`dashes=2,4,6,8`; the old `dash`/`gap` params still
  parse), and from the native-SVG reference overlay, whose
  `stroke-dasharray` shares the same semantics and so remains a valid
  oracle. Pinned crate-side by `tests/dash_pattern.rs`, including the
  byte-exact odd-doubling equivalence.

### Internal (during development)

- The two flattened edge indexes (source path and raw offset loops) now
  share one `y`-bucketed `EdgeIndex` with filter-parameterized winding and
  proximity predicates. The region-construction call sites share a common
  setup helper. Small geometry helpers (`append_seg`, `overlaps`,
  `start_point`) are single-sourced. The vestigial per-piece effective
  width, a leftover of the pre-region width clamp, is gone. No output or
  measured performance change (golden suite and benches).
- Documented limitation: a dash pattern whose single dash swallows an
  entire closed contour (dash ≥ perimeter, zero gap) expands as an
  unmasked ring and can cross the fill boundary at extreme widths. Use a
  solid stroke instead.
- The golden fixture records the OS it was generated on, and the test
  skips elsewhere: a different libm can flip a subdivision decision and
  change the segment count, which the per-coordinate slack cannot absorb.
  The CI matrix still enforces the fixture on a matching runner.
