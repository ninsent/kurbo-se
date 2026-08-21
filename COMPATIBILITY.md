# kurbo version compatibility

kurbo is pre-1.0, so every `0.x` minor bump is a breaking release. kurbo-se
pins one kurbo minor per kurbo-se minor and re-exports it
(`kurbo_se::kurbo`), so downstream code can rely on type identity.

| kurbo-se | kurbo | vello (via peniko) | notes |
|---|---|---|---|
| 0.1.x | 0.13.x (≥ 0.13.1) | 0.10.x | initial release |

## Update policy

- When kurbo publishes a new minor (0.14, say), kurbo-se follows with its
  own minor (0.2) that bumps the dependency, re-verifies the assumptions
  this crate makes about kurbo's internals (join emission structure, the
  `offset_cubic` contract, dash seam semantics), and re-runs the full
  suite: unit tests, the degenerate matrix, the winding properties, the
  regression ports, and the sandbox gallery.
- kurbo patch releases (0.13.x) are picked up automatically by the caret
  requirement. CI pins jobs to both the minimum supported patch and the
  newest one.
- Older kurbo-se lines receive fixes only for soundness and correctness
  bugs.
