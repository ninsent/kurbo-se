# kurbo-se geometric regression suite

The suite strokes a corpus of about 5,200 cases twice: once with a pinned
reference revision of kurbo-se, and once with the working tree, the
candidate. Each candidate outline must paint the same region as the
reference outline, within the case's own tolerance. This is the contract
for refactoring and performance work. A change that moves the output by
more than the tolerance is a bug, whatever it gains.

It is a separate package, not a test of the crate, for two reasons. It
links two versions of kurbo-se into one binary. And it needs a font and a
reference checkout that the published crate does not ship.

## Running

```bash
tools/regress/setup-ref.sh <git-rev>
cd tools/regress && cargo run --release
```

`setup-ref.sh` extracts the revision into `target/regress-ref/` and renames
its package to `kurbo-se-ref`. Point it at the commit whose output is the
reference: usually the parent of the work being checked, or the baseline of
a series. The summary prints which revision that was.

A full run on four worker threads takes a few seconds and about 200 MB. The
exit status is non-zero if any case fails.

| Option | Effect |
|---|---|
| `--filter SUBSTR` | only cases whose name contains `SUBSTR` (repeatable) |
| `--list` | print the selected case names and exit |
| `--report FILE.tsv` | one row per case: verdict, `H/tol`, areas, element counts, timings |
| `--threads N` | worker threads, default 4 |
| `--delta-scale X` | allowed boundary distance `δ = X · tol`, default 1 |
| `--dump DIR` | SVG overlays of reference and candidate for every failing case |
| `--trace` | print each case before comparing it, to find one that hangs or blows up |
| `--perturb KIND` | compare the reference against an edited copy of itself (below) |
| `--geometric-all` | run the geometric check on identical outputs too |
| `--top N` | how many of the worst and slowest cases to list, default 10 |

The glyph cases use the sandbox's Inter Regular. Set
`KURBO_SE_REGRESS_FONT` to use another font. Without a font, the glyph
cases are skipped with a warning.

## Verdicts

- `IDENTICAL`: the outlines are equal element for element, bit for bit. A
  change meant to be exact should keep every case here.
- `EQUIVALENT`: the outlines differ but paint the same region within the
  tolerance. The report gives the boundary distance `H` measured for each.
- `FAIL`: the regions differ by more than the tolerance, or the candidate
  panicked or produced non-finite output.

## The contract

An outline stands for the region kurbo-se defines: `{p : winding(p) ≠ 0}`
under the nonzero rule, with open subpaths closed the way filling closes
them. Its edges say little on their own, since stroke outlines overlap
themselves by design. Two outlines are equivalent when

1. every point on the boundary of either region lies within `δ = tol` of
   the other region's boundary, and
2. every point painted by exactly one of them lies within `δ` of both
   boundaries.

The second condition catches what the first misses. A disc and a thin ring
with the same outer edge have boundaries close together, but they differ by
the whole hole.

Stroke outlines are full of features far thinner than the tolerance:
slivers that rounding leaves open or closed. So both conditions excuse a
point that lies within the resolution `ρ = tol / 2` of an edge of both
outlines. Shifting the whole outline by `0.4 · tol` stays within `ρ`, so it
passes unmeasured. A shift of `0.8 · tol` passes and is measured as such,
and `1.25 · tol` fails. `compare::tests::translation_sensitivity` pins
these numbers.

The comparator flattens curves itself, at `tol / 16`, in equal parameter
steps sized by the classic interpolation error bound. It does not use
`kurbo::flatten`. In kurbo 0.13.1 that can stray far past its tolerance on
an S-shaped cubic, by an amount that depends on which way the curve runs.

## Corpus

Shapes: lines, polylines and zigzags, single cubics and quads (loops, cusps,
near-collinear, collinear backtracks), arcs, circles and ellipses, rects and
rounded rects, stars and gears up to 750 teeth, bulging-edge stars, donuts,
plates with holes, overlapping and nested contours, bowties, figure-eights,
pretzels, pentagrams, self-crossing polylines, zero-length segments and
subpaths, copies scaled by 1e-3 and 1e5, thin wedges and bars, the shapes
of `tests/golden.rs`, degenerate cubics from kurbo and vello issues, the
Inter glyphs `&@$%#?48gBQRSaek`, and 100 seeded random paths.

Styles: every alignment and side override, all three joins at miter angles
from 10° to 120°, all caps with different start and end caps, widths from
0.05 to 200, and dashes with odd-length patterns, zero-length dashes, zero
gaps, large and negative offsets, very short periods and dashes longer than
the path. Most cases run at tolerance 0.25, and subsets at 0.05 and 1e-3.

The corpus also holds the cases behind the fixes listed under 0.2.0 in
CHANGELOG.md: dash phase across self-intersections, dots and discs where
the pattern restarts, the end cap of a dash running to the end of a line,
bevels and miters at widths near the corner spacing, spiky gears near the
collapse of their erosion, and the fill probes of bulging stars and looping
curves.

Case names spell out their parameters, for example
`star5/inside/miter/w12/tol0.25` or
`bowtie/side-right/miter/w2/dash4-2-1-2-off5/tol0.001`.

## Validating the comparator

`--perturb KIND` compares each reference output with an edited copy of
itself, in place of the candidate. These edits must pass on every case:

- `reorder` and `reverse`: subpaths reordered, or all reversed;
- `split`: every curve split in two at its midpoint;
- `degenerate`: zero-area loops added;
- `jitter`: every coordinate moved by up to `tol / 10`;
- `reflatten`: curves replaced by lines within `tol / 20`.

These must fail on every case that paints anything:

- `translate`: the outline moved by `3 · tol`;
- `scale`: scaled so its farthest point moves by `3 · tol`.

`cargo test` in this directory runs the comparator's own unit tests: thin
rings, dropped loops and dashes, reversed holes, overlaps, cracks, and the
translation sensitivity above.
