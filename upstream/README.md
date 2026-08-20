# Upstream PR drafts for linebender/kurbo

Reading kurbo's internals turned up three small quality improvements worth
upstreaming. None block kurbo-se — local copies ship in 0.1.0 and can
be deleted if/when these land. Filing requires a GitHub account; each file
below is a ready-to-submit PR description with the concrete change sketched
against kurbo v0.13.1.

1. [`01-expose-tangents.md`](01-expose-tangents.md) — make
   `PathSeg::tangents()` public.
2. [`02-join-cap-tolerance.md`](02-join-cap-tolerance.md) — plumb the user
   tolerance into join/cap arc emission (resolves an existing `TODO: scale`).
3. [`03-dash-robustness.md`](03-dash-robustness.md) — guard `dash()` against
   empty and non-positive-sum patterns (panic / infinite-loop today).
4. [`04-quad-arclen-nan.md`](04-quad-arclen-nan.md) — `QuadBez::arclen`
   returns `NaN` for coincident control points (and at the under/overflow
   extremes), which silently turns dashed output solid downstream.

Suggested sequencing: file 1, 3 and 4 immediately (tiny, uncontroversial);
float 2 on Zulip `#kurbo` first since it changes output density for existing
users. Mention kurbo-se as the motivating consumer, mirroring how #475
exposed `StrokeCtx` for external reuse.
