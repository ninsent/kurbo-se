# Make `PathSeg::tangents()` public

## Summary

`PathSeg::tangents()` (`kurbo/src/bezpath.rs:1323-1358` @ v0.13.1) computes
robust endpoint tangents with fallbacks across coincident control points. It
is currently `pub(crate)`, used by the stroker. Any external stroking,
offsetting, or marker-placement code needs exactly this routine — degenerate
control points (retracted handles) are common in real path data, and the
naive `p1 - p0` tangent silently produces zero vectors for them.

## Motivation

kurbo-se (stroke alignment as an extension crate on kurbo's public API) had
to copy these ~35 lines verbatim. Precedent: #475 exposed `StrokeCtx` so
external callers could reuse allocations; this is the same story for
geometry. Arrowhead/marker placement code has the same need.

## Change

```diff
-    pub(crate) fn tangents(&self) -> (Vec2, Vec2) {
+    /// Compute endpoint tangents of a path segment.
+    ///
+    /// The results are robust to degenerate control points: a tangent
+    /// handle of zero length falls back to the next control point, so the
+    /// returned vectors are non-zero whenever the segment has any extent.
+    /// The vectors are not normalized.
+    pub fn tangents(&self) -> (Vec2, Vec2) {
```

Plus a `### Added` changelog entry. No behavior change; semver-additive.

## Tests

Doc-example with a retracted-handle cubic
(`CubicBez::new(p, p, c, q)`) asserting a non-zero start tangent.
