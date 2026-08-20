# `QuadBez::arclen` returns `NaN` for degenerate and extreme quads

## Summary

`QuadBez::arclen` (`kurbo/src/quadbez.rs` @ v0.13.1) returns `NaN` instead of
a length whenever the closed-form's denominator is not a usable number:

```rust
use kurbo::{ParamCurveArclen, Point, QuadBez};

let p = Point::new(1.0, 2.0);
assert!(QuadBez::new(p, p, p).arclen(1e-6).is_nan());          // coincident: want 0

let q = QuadBez::new(Point::ZERO, Point::new(1e-300, 0.0), Point::new(2e-300, 0.0));
assert!(q.arclen(1e-6).is_nan());                               // underflow: want ~2e-300

let q = QuadBez::new(Point::ZERO, Point::new(1e160, 0.0), Point::new(2e160, 1e160));
assert!(q.arclen(1e-6).is_nan());                               // overflow
```

`CubicBez::arclen` returns `0.0` for the coincident case (it integrates
numerically), so the inconsistency is specific to the quad closed form.

## Why it matters

Coincident control points are ordinary in real path data — exported SVGs,
retracted handles, and editors that emit `Q p p` for a "corner" node. The
`NaN` is silent and contaminating: it propagates through any arc-length
accumulator, and in particular through `dash()`, whose phase becomes `NaN`.
Once that happens every comparison against the remaining dash length is
false, so the pattern stops advancing and **the rest of the path is emitted
as one uninterrupted dash** — a plausible-looking result rather than an
obvious failure.

kurbo-se hit this through `dash()` on a path with a `Q p p` element: dashes
rendered correctly up to that element and solid after it. Both the direct
call and the `dash()` interaction were reported by a downstream user as a
rendering bug.

## Change

Guard the closed form: when the denominator is not finite and positive, fall
back to the chord length (exact for the degenerate case, correct to rounding
for the underflow case) or to the numerical path already used by
`CubicBez::arclen`. Sketch:

```rust
fn arclen(&self, accuracy: f64) -> f64 {
    let d2 = /* existing squared-derivative term */;
    if !(d2 > 0.0) || !d2.is_finite() {
        // Degenerate or unrepresentable: the chord is the best available
        // answer and is exact when the control points coincide.
        return (self.p2 - self.p0).hypot();
    }
    /* existing closed form */
}
```

Documenting the guarantee — "`arclen` returns a finite, non-negative value
for any finite curve" — is arguably the more valuable half of the change,
since callers currently have no way to know they must check.

## Tests

- `QuadBez::new(p, p, p).arclen(..) == 0.0`
- sub-normal and overflow-scale control deltas produce finite results
- `dash()` over a path containing `Q p p` yields the same dash count as the
  same path with that element removed (the regression that motivated this)
