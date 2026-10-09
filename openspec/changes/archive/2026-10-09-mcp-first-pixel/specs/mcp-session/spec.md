## MODIFIED Requirements

### Requirement: Time-bounded progressive renders

`render` SHALL start a progressive render of the current scene and return within
`budget_s` seconds. It SHALL return:

- a PNG of the latest snapshot, downscaled to at most 1024 pixels on its long side;
- the samples per pixel reached;
- whether the render is done;
- a render id.

By default (`wait = "image"`), `render` SHALL return as soon as every pixel has taken 4
samples, the render has returned, or `budget_s` has passed, whichever comes first. A
render of 4 samples per pixel or fewer SHALL return when the render does, or at
`budget_s`. With `wait = "done"`, `render` SHALL return when the render does, or at
`budget_s`.

The render SHALL keep refining after the call returns, until it completes or is
cancelled.

#### Scenario: The first image

- **WHEN** the agent calls `render` with `spp = 128` and `budget_s = 60` on a frame
  whose 4-spp stage completes in under a second
- **THEN** the call returns within a few seconds with an image, `done = false` and
  `spp_reached` of at least 4, and a later `snapshot` returns a newer image

#### Scenario: Waiting for completion

- **WHEN** the agent calls `render` with `spp = 16`, `wait = "done"` and a budget the
  render fits in
- **THEN** the call returns with `done = true` and status `done`

#### Scenario: A long render

- **WHEN** the agent calls `render` with `wait = "done"` and `budget_s = 20` on a
  frame that needs minutes
- **THEN** the call returns after at most about 20 seconds with an image and
  `done = false`, and a later `snapshot` returns a newer image

#### Scenario: A render of a region

- **WHEN** `render` is given a region
- **THEN** only that region is rendered, and the image covers only it

#### Scenario: Changing only the samples keeps the light selection

- **WHEN** the stage selects lights by `learned`, and a render names `spp` or a region
  but nothing else differs from the imported settings
- **THEN** the light selection is not trained again, and the render is bit-identical
  to one of a freshly built renderer with the same settings

### Requirement: Snapshot and cancel

`snapshot(render_id, budget_s?)` SHALL return the latest image and progress. With a
`budget_s`, it SHALL first wait for the render to return, for at most that long.
Without one, it SHALL not wait. `cancel(render_id)` SHALL stop the render and keep what
it has done. An edit, or a new `render`, SHALL cancel any render still running.

#### Scenario: Waiting for the converged image

- **WHEN** `render` answered with `done = false`, and the agent calls `snapshot` with
  the render's id and a budget the render fits in
- **THEN** the call returns with `done = true` and the render's final image

#### Scenario: Editing during a render

- **WHEN** a render is refining and the agent calls `set_attribute`
- **THEN** that render is cancelled before the scene is re-imported, and its id
  reports `cancelled`
