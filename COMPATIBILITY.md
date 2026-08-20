# kurbo version compatibility

kurbo is pre-1.0: every `0.x` minor bump is a breaking release. kurbo-se
therefore pins one kurbo minor per kurbo-se minor and re-exports it
(`kurbo_se::kurbo`) so downstream code can rely on type identity.

| kurbo-se | kurbo | vello (via peniko) | notes |
|---|---|---|---|
| 0.1.x | 0.13.x (≥ 0.13.1) | 0.10.x | initial release |

## Update policy

- When kurbo publishes a new minor (e.g. 0.14), kurbo-se follows with its own
  minor (e.g. 0.2) that bumps the dependency, re-verifies the private-machinery
  assumptions the crate relies on (join emission structure, `offset_cubic`
  contract, dash seam semantics), and re-runs the full suite (unit, degenerate
  matrix, winding properties, regression ports, sandbox gallery).
- Patch releases of kurbo (0.13.x) are picked up automatically by the caret
  requirement; CI has jobs pinned to both the minimum supported patch and the
  newest one.
- Older kurbo-se lines receive fixes only for soundness/correctness bugs.
