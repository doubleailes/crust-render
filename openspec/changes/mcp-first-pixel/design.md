# Design

## Context

Where the time to the first pixel goes in a session (release build, 4 cores,
2026-10-09):

```
 open_session ──────────────▶ render(budget_s = 10) ──────────────────────────▶ PNG
 │ import: 2–75 ms on every   │ reconfigure       │ render thread:    │ handler waits
 │ checked-in sample          │ (learned: trains  │ 1 spp at 0.2 s,   │ for done or the
 │                            │  a second time)   │ 4 spp at 0.5 s    │ WHOLE budget
```

| measurement | value |
|---|---|
| `crust render cornellbox.usda`, whole process, `-s 1 / 4 / 16 / 128` | 0.20 / 0.47 / 1.7 / 11.0 s |
| session `render` on cornellbox, first answer (`mcp-session` design) | 9.9 s |
| import (`Parse USD stage`), lion / usdpreview_textured / ptex_quads / displacement | 2 / 11 / 7 / 75 ms |
| texture loading in those imports | ≤ 4 ms |

The render already exposes what an early answer needs. `RenderControl::samples_reached`
is the last first-sweep stage (1, 2, 4, … spp) that every pixel has completed.
`snapshot` is the beauty published as each unit finishes a stage.

## Goals / Non-Goals

**Goals:**

- The agent sees a readable image as soon as one exists, with no change to the image
  the render converges to.
- No polling: an agent that wants the converged image waits for it in one call.
- No duplicate light-selection training between the import and the first render.

**Non-Goals:**

- Faster imports. Nothing measured here justifies it yet (see Follow-ups).
- A preview or low-quality mode. Every image a session shows is a stage of the real
  render.

## Decisions

### D1. Answer at 4 spp, the first stage that reads as an image

`render`'s default wait (`wait = "image"`) ends at the first of three events:

- `samples_reached >= 4`;
- the render returned (done or cancelled);
- `budget_s` passed.

The threshold is 4 spp. At 1 spp the cornellbox frame is mostly noise, which an agent
judging a light or a material would misread. 4 spp costs 0.3 s more and is the first
stage where the spheres' shading reads. The threshold is a constant, not an argument:
`budget_s` already bounds a scene where 4 spp is slow, and `wait = "done"` covers the
other direction.

A render whose `spp` is 4 or fewer waits for completion under `"image"` too. Its
first sweep reaches `spp` in its last stage, a few microseconds before the render
returns. Answering on that stage would report `done = false` for a render that is in
fact complete, depending on a race. Waiting costs nothing, since that stage *is* the
whole render.

A guided render restarts `samples_reached` with each training pass, so its first image
is a training pass at 4 spp. That is still an image of the scene. The docs already say
a guided render's snapshot shows the pass in progress.

**Alternatives considered:**

- Answer at the first snapshot (1 spp). Rejected: too noisy to judge (above).
- A numeric `answer_at_spp`. Rejected: it means nothing above the first check point
  (`min_samples_per_pixel`), where pixels stop sharing a count, and an agent cannot
  know where that is.

### D2. The wait is polled on the protocol side

`samples_reached` is an atomic store with no notification, and adding one to
crust-core's `RenderControl` would put a wake-up on the render's hot path. The
handler polls it every 10 ms alongside the render's existing `returned` signal.
That is at most 100 cheap loads a second, on the one tokio thread, while a tool call
is waiting anyway. The render does no extra work.

### D3. `snapshot(budget_s)` waits for the render to return

With D1, the first answer is usually `done = false`. Without a wait, an agent that
wants the converged image could only call `snapshot` in a loop, or call `render`
again with `wait = "done"`, which restarts the render and throws its samples away.
`snapshot` therefore takes an optional `budget_s` and waits for the render to return
for at most that long. The default 0 keeps today's no-wait behaviour.

### D4. `Renderer::retune` keeps a light selection whose inputs did not change

`reconfigure` always rebuilds the light selection, and under `learned` that includes
the training pre-pass. The pre-pass is a function of the world, the camera, the
lights, the resolution and the frame, reduced in receiver order, so it is
deterministic (`light_cache.rs`). Within one import, only `light_selection`, `width`,
`height` and `frame` can change its result. `retune(settings)` sets the settings and
rebuilds the selection only when one of those four changed. Otherwise the result
equals a rebuild, bit for bit, and a test pins `new(s0).retune(s)` against `new(s)`.

`reconfigure` keeps rebuilding unconditionally. `crust diagnostic` calls it per
trial and reports the light selection's setup time as part of each trial's cost.
Skipping there would make a `learned` trial look free after the first one. The session
calls `retune`.

## Risks / Trade-offs

- [An agent judges a 4-spp image as final] → every answer carries `done`,
  `spp_reached` and `spp`. The server's instructions say that renders answer at the
  first image, and that `snapshot` with a budget waits for more.
- [Existing clients that relied on `render` waiting for completion] → they pass
  `wait = "done"`. The only clients today are this repository's tests and Claude
  Desktop, which reads the tool schema per session.
- [`retune`'s key misses an input] → the bitwise test renders `learned` with a changed
  spp and region, and adding a training input to `train` without adding it to the key
  breaks that test.

## Follow-ups

- **Parallel or deferred texture decoding at import.** Textures decode serially on the
  importing thread (`timed_asset`), and a preloaded texture decodes every source pixel
  before its cap. On the samples this is ≤ 4 ms. The change that addresses it should
  start from `--stats` on a production-sized stage (Kitchen_set, ALab), where the
  `Load assets` phase can be measured.
- **Keep the asset loader across re-imports.** Each edit builds a new `FileAssets`, so
  every texture is decoded again after an edit that touches none. This costs edit
  latency, not the first pixel, and needs the same measurement.
