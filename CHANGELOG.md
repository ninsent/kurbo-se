# Changelog

This changelog follows <https://keepachangelog.com/en/>.

## Unreleased

### Added

- Package metadata: author, repository and homepage
  (<https://github.com/ninsent/kurbo-se>), plus `LICENSE-APACHE`,
  `LICENSE-MIT` and `AUTHORS` files.

### Changed

- **Aligned strokes on closed contours are now defined set-theoretically**,
  matching Figma: `Inside(w) = { p ∈ F : dist(p, path) ≤ w }` and
  `Outside(w) = { p ∉ F : dist(p, path) ≤ w }`. An inside stroke never
  leaves the fill and an outside stroke never enters it at any width, and a
  width beyond the local thickness **saturates** — the shape fills solid
  instead of the band folding over itself and punching hollow pockets.
  Implemented by pruning the raw offset (cut at every intersection, discard
  pieces nearer than `w` or on the wrong side of the fill, restitch), which
  replaces the earlier effective-width clamp.
- Self-intersecting contours are split at their crossings, so each lobe of a
  bowtie or figure-eight resolves its side against the fill independently
  and overlapping bands add instead of cancelling.
- Faster predicates: the source is flattened once into a `y`-bucketed edge
  index, so distance and winding queries no longer run a per-curve solve;
  line-involving intersections use kurbo's analytic `PathSeg::intersect_line`.

## 0.1.0 (unpublished)

Initial release, targeting kurbo 0.13.

### Added

- `StrokeStyle` / `StrokeAlignment` / `StrokeSide` / `DashStyle`: Figma's
  stroke panel as a style type — alignment (inside/center/outside), raw side
  override, joins with a **miter angle in degrees**, independent start/end
  caps, dash pattern/offset/**dash cap**.
- `stroke_aligned` / `stroke_aligned_with` (+ `AlignedStrokeCtx` for
  allocation reuse): expand an aligned stroke into a nonzero-rule fill
  outline. One-sided bands keep the source path as their exact shared
  boundary; sharp corners are clipped so inside strokes never leak outside
  the fill (and vice versa).
- Hole-aware per-subpath side resolution (winding probe): holes stroke into
  the ring; reversing winding direction never changes the result.
- Dashing on the centerline with seam-merge on closed contours (smooth
  `dash_offset` animation), pattern sanitization (empty/negative/zero-sum
  patterns render solid instead of panicking or hanging), and zero-length
  dashes rendered as dots (round + oriented square).
- `miter_angle_to_limit` / `miter_limit_to_angle` conversions.
- `From<&StrokeStyle> for kurbo::Stroke` escape hatch (centered
  interpretation, documented lossiness).
- `analyze_subpaths` introspection for editors/debug tooling.
- `no_std` support (`libm` feature), `serde` feature, zero `unsafe`.
- Interactive wasm sandbox (`sandbox/`), headless vello example
  (`examples/vello-demo`), criterion benchmarks.
