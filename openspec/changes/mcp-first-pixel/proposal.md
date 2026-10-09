## Why

In an MCP session, the first image reaches the agent at the end of the render's
budget, not when it exists. `render` waits until the render is done or until
`budget_s` (default 10 s) runs out. On `cornellbox.usda` the first answer took 9.9 s
(`mcp-session` design, Risks). Yet `crust render` writes the same frame at 1 spp in
0.20 s and at 4 spp in 0.47 s, process start included (release build, 4 cores,
2026-10-09). Importing is not the cost either: on every checked-in sample it takes
2–75 ms (`--stats`). About 95% of the time to the first pixel is the tool waiting.

A second, smaller cost precedes every render that names `spp` or a region. The session
switches its renderer to those settings with `Renderer::reconfigure`, which rebuilds the
light selection. Under `crust:lightSelection = "learned"` that rebuild includes the
training pre-pass, which the import has already run with the same inputs (camera,
resolution, frame). Only the sample count or the region changed.

## What Changes

- **`render` answers at the first image.** By default it returns as soon as every pixel
  has `4` samples (the first-sweep stage the render already records as
  `samples_reached`), when the render is done, or when `budget_s` passes, whichever
  comes first. The render keeps refining after the answer, as it does today. A render
  of `4` spp or fewer waits for completion, because its first image is the whole
  render.
- **`render(wait = "done")`** keeps today's behaviour: answer when the render is done
  or the budget passes.
- **`snapshot` can wait.** `snapshot(render_id, budget_s?)` waits for the render to
  return for at most `budget_s` (default 0, which means no wait, as today). An agent
  that took the early image waits for the converged one without polling, and without
  restarting the render.
- **A render that changes only the sample count or the region keeps the light
  selection.** A new `Renderer::retune` switches settings like `reconfigure`, but
  rebuilds the light selection only when what it is built from changes (the selection
  strategy, the resolution, the frame). It is pinned bitwise against a fresh
  `Renderer::new`. `reconfigure` is unchanged, because `crust diagnostic` measures
  setup cost through it.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `mcp-session`: `Time-bounded progressive renders` (answer at the first image, the
  `wait` argument) and `Snapshot and cancel` (`snapshot` may wait).

## Impact

- `crates/crust-render/src/mcp/` (`mod.rs` tool arguments, `render.rs` waiting and
  `retune`), `crates/crust-core/src/tracer/mod.rs` (`Renderer::retune`).
- `tests/mcp.rs`: renders that assert completion at more than 4 spp pass
  `wait = "done"`.
- `site/content/docs/help/claude-desktop.md`: the tool table and the Rendering notes.
- Sequenced after `mcp-session`, whose spec this modifies: archive that change first.
- Not in scope: parallel or deferred texture decoding at import. It is unmeasurable
  on the checked-in samples (1–4 ms of texture loading) and needs a production-sized
  stage to justify it. It is recorded as a follow-up in the design.
