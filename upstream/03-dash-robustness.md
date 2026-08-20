# `dash()`: don't panic on empty patterns / hang on non-positive sums

## Summary

The public `dash()` iterator has two sharp edges at v0.13.1
(`kurbo/src/stroke.rs`):

1. **Empty pattern panics**: `dashes[dash_ix]` with `dash_ix = 0` on an
   empty slice — `stroke.rs:744`. (`stroke_with` guards with `is_empty`
   before calling; raw `dash()` does not.)

   ```rust
   let _ = kurbo::dash(path.iter(), 0.0, &[]).count(); // index out of bounds
   ```

2. **Negative-sum patterns hang**: the initial phase scan

   ```rust
   while dash_remaining < 0.0 {
       dash_ix = (dash_ix + 1) % dashes.len();
       dash_remaining += dashes[dash_ix];
       ...
   }
   ```

   (`stroke.rs:747-751`) never terminates when a full cycle decreases
   `dash_remaining` (e.g. pattern `[1.0, -4.0]` with a positive offset).
   All-zero patterns take a third path: `rem_euclid(0.0)` yields NaN and the
   input passes through as solid — accidental but reasonable.

## Change

At the top of `dash_iter`: if the pattern is empty, or any entry is
non-finite or negative, or the sum is not `> 0.0`, yield the input elements
unchanged (solid). Document the contract on `dash()`: "patterns must consist
of finite, non-negative lengths with a positive sum; anything else renders
solid." This matches the SVG error-handling spirit (invalid `stroke-dasharray`
→ as if `none`) and the crate's direction of validating stroke inputs
(#545 added `Stroke::is_finite`/`is_nan`).

Implementation sketch: an enum-wrapped iterator
(`enum DashOrSolid<T> { Dash(DashIterator<T>), Solid(T) }`) keeps the
signature `impl Iterator<Item = PathEl>` without boxing.

## Tests

- empty pattern → identity
- `[0.0, 0.0]` → identity
- `[1.0, -4.0]`, offset 10 → identity (regression for the hang)
- `[4.0, f64::NAN]` → identity
