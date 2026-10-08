## ADDED Requirements

### Requirement: Render region flag

`crust render` SHALL accept `--region X0,Y0,X1,Y1`: a rectangle in pixels,
with a top-left origin, half-open (`X1` and `Y1` exclusive). Only the
pixels inside it SHALL be rendered and written. It SHALL take precedence
over any `dataWindowNDC` the stage authors.

- The value SHALL be four non-negative integers with `X1 > X0` and
  `Y1 > Y0`; anything else SHALL be a usage error, raised before the stage
  is loaded.
- The rectangle SHALL be clipped to the render's resolution. A rectangle
  that is empty after clipping SHALL be an error naming the resolution, and
  nothing SHALL be rendered.
- With `--stats`, the report SHALL state the region and the share of the
  frame it covers.

#### Scenario: Rendering a crop

- **WHEN** the user runs
  `crust render -i samples/cornellbox.usda --region 100,50,164,114`
- **THEN** a 64×64-pixel region is rendered, and the EXR's data window is
  `(100, 50)–(163, 113)` inside the full-resolution display window

#### Scenario: A malformed region

- **WHEN** the user runs `crust render -i scene.usda --region 10,10,5,20`
- **THEN** the arguments are refused as a usage error and nothing is loaded

#### Scenario: A region outside the image

- **WHEN** the resolution is 640×360 and the user passes
  `--region 700,0,800,100`
- **THEN** an error names the 640×360 resolution, the exit status is
  non-zero, and no image is written
