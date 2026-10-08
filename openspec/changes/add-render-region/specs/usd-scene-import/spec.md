## ADDED Requirements

### Requirement: Data window from render settings

The importer SHALL read `float4 dataWindowNDC` (`xmin, ymin, xmax, ymax`,
`(0, 0)` at the bottom-left) from the first `RenderProduct`, else from the
`RenderSettings` prim, as it resolves `resolution`. The render's region SHALL
be the pixels whose centres lie inside the window. The default
`(0, 0, 1, 1)` is the full frame.

- `dataWindowNDC` SHALL no longer appear in the "not honoured" warning.
- A window extending outside [0, 1] SHALL be clipped to the frame with one
  warning.
- A window that selects no pixel SHALL be refused with one warning, and the
  full frame SHALL render.
- The CLI's `--region` SHALL take precedence over any authored window.

#### Scenario: A window selecting the right half

- **WHEN** the settings author `resolution = (640, 360)` and
  `dataWindowNDC = (0.5, 0, 1, 1)`
- **THEN** pixels with `x` in 320..640 and every `y` are rendered, and no
  warning about `dataWindowNDC` is logged

#### Scenario: NDC y is bottom-up

- **WHEN** the settings author `resolution = (100, 100)` and
  `dataWindowNDC = (0, 0, 1, 0.25)`
- **THEN** the rendered rows are `y` in 75..100, the bottom quarter of the
  image

#### Scenario: Product overrides settings

- **WHEN** the settings author `dataWindowNDC = (0, 0, 0.5, 0.5)` and the
  first product authors `dataWindowNDC = (0.5, 0.5, 1, 1)`
- **THEN** the product's window is used

#### Scenario: Overscan is clipped

- **WHEN** the window is `(-0.1, 0, 1.1, 1)`
- **THEN** one warning is logged, and the full width renders

#### Scenario: An empty window

- **WHEN** the window is `(0.5, 0.5, 0.5, 0.6)`
- **THEN** one warning is logged, and the full frame renders
