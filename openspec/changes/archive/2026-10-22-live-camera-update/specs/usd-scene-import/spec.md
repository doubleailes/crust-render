## ADDED Requirements

### Requirement: The dicing camera

Tessellation that reads a camera SHALL read the dicing camera: adaptive subdivision levels, per-face edge rates and their view test, and the radial count of implicit shapes. Where those requirements say "the render camera", they mean the dicing camera. The dicing camera SHALL be the target of `rel crust:dicingCamera` on the `RenderSettings` prim when that target is a camera, and the render camera otherwise.

#### Scenario: Unauthored, the render camera dices

- **WHEN** a stage authors no `crust:dicingCamera`
- **THEN** every subdivision level, edge rate and radial count is the one the
  import gave before this requirement, and a 16 spp render is bit-identical to it

#### Scenario: Geometry is diced for the dicing camera

- **WHEN** a stage authors `crust:subdivisionEdgeLength`, renders through camera
  `/A`, and sets `crust:dicingCamera` to camera `/B`, placed elsewhere
- **THEN** every mesh is tessellated exactly as when the same stage renders
  through `/B` with no `crust:dicingCamera`, while the image is seen through `/A`

#### Scenario: A dicing camera alone enables adaptive tessellation

- **WHEN** a stage authors `crust:subdivisionEdgeLength` and `crust:dicingCamera`
  but names no render camera (no `--camera`, no `RenderSettings.camera`)
- **THEN** adaptive subdivision and on-screen radial counts are computed through
  the dicing camera, and no `subdiv.adaptive_needs_camera` warning is emitted

#### Scenario: A target that is not a camera

- **WHEN** `crust:dicingCamera` targets a prim that is not a camera, or no prim
- **THEN** one warning naming the target is emitted, and the render camera dices

#### Scenario: Products do not override it

- **WHEN** the first render product authors its own `camera`
- **THEN** the dicing camera is still `crust:dicingCamera` when authored, and the
  product's camera only when it is not
