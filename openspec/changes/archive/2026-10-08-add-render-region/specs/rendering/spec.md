## ADDED Requirements

### Requirement: Rendering a region of the frame

When the render has a region smaller than the frame, the renderer SHALL
trace only the pixels inside it. It SHALL keep the full-frame camera,
resolution and per-pixel sampling. Everything derived from the resolution
SHALL be computed for the full frame:

- ray-cone texture filtering;
- adaptive subdivision's screen rate;
- frustum culling.

A pixel's value SHALL be bit-identical to the same pixel in a full-frame
render of the same scene and settings whenever its sample count does not
depend on its neighbours: a fixed sample count, or adaptive sampling with
no neighbour tolerance. Under the adaptive neighbour hold, a neighbour
outside the region SHALL count as absent.

Path guiding SHALL train on the region's paths only. A guided region is
therefore not bit-identical to the same pixels of a guided full render.

#### Scenario: A crop matches the full render

- **WHEN** `samples/cornellbox.usda` is rendered at `-s 16` once full-frame
  and once with `--region 37,21,101,77`
- **THEN** every pixel, every AOV channel and `sampleCount` of the crop are
  bitwise equal to the full render's at the same coordinates

#### Scenario: Tiles and scanlines agree on a region

- **WHEN** the same region is rendered once with tiles and once with
  `--scanline`
- **THEN** the two images are bitwise equal

#### Scenario: The full frame is unchanged

- **WHEN** no region is authored and `--region` is not given
- **THEN** the image is bitwise equal to the image rendered before this
  change
