## ADDED Requirements

### Requirement: Cropped renders record their place in the frame

When the render has a region smaller than the frame, every EXR it writes
(the single beauty EXR, and each product) SHALL have:

- a display window covering the full resolution;
- a data window covering the region;
- pixel data only for the region.

The tone-mapped PNG SHALL contain only the region's pixels, at the region's
size. When the region is the full frame, both files SHALL be byte-identical
to the output before this change.

#### Scenario: Compositor placement

- **WHEN** a 640×360 render with `--region 320,0,640,360` is read into Nuke
- **THEN** the image has format 640×360, and its bounding box covers the
  right half

#### Scenario: PNG of a crop

- **WHEN** a render with `--region 0,0,64,32` completes
- **THEN** the PNG next to the EXR is 64×32 pixels

#### Scenario: No region, unchanged files

- **WHEN** a render without a region completes
- **THEN** its EXR and PNG are byte-identical to those written before this
  change
