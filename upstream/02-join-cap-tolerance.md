# Plumb the user tolerance into join/cap arc emission

## Summary

`stroke_undashed` honors the caller's `tolerance` for parallel curves, but
round joins and caps are generated with a hardcoded `tolerance = 1e-3`,
marked `// TODO: scale`:

- `StrokeCtx::finish` — `kurbo/src/stroke.rs:402` @ v0.13.1
- `StrokeCtx::do_join` — `kurbo/src/stroke.rs:445`

At coarse tolerances the joins are over-tessellated relative to the rest of
the outline; at fine tolerances (output that will be scaled up — the use
case the `stroke` docs call out) the joins are visibly *under*-tessellated
while the offsets are smooth.

## Change

Thread the `tolerance` argument of `stroke_undashed` into `finish`,
`finish_closed`, and `do_join` (the three sites already receive `style`;
adding the parameter is mechanical), replacing both hardcoded constants.
Resolves the two `TODO: scale` comments.

## Compatibility note

Output geometry changes for callers using non-default tolerances (segment
counts in round joins/caps). Worth a changelog entry under `### Changed`;
no API change. Happy to gate it behind `StrokeOpts` instead if preferred —
maintainer call, hence this PR description leads with the question.

## Tests

Extend `expand_rounding_tolerance`-style assertions (cf.
`kurbo/src/expand.rs:500-514`, which already verifies that *expand* join
accuracy follows tolerance) to `stroke` round joins: area error of a
round-joined right angle must shrink as tolerance does.
