# Spec Delta

## MODIFIED Requirements

### Requirement: The render described

The report SHALL give:

- the stage path and the frame;
- the camera path, the resolution and the region (`null` for the whole frame);
- every product, with the file a render would write and the channels in it;
  the file is the path with its frame variables expanded for the checked frame.

A stage with no products SHALL report the single beauty image a render would write.

#### Scenario: A stage with AOVs

- **WHEN** a stage's product orders `color`, `albedo` and `normal` RenderVars
- **THEN** the product lists its file and the channels of all three vars

#### Scenario: No products

- **WHEN** the stage authors no `RenderProduct`
- **THEN** one product is reported: the beauty image a render would write by default

#### Scenario: A product with a frame variable

- **WHEN** a stage's product authors `productName = "renders/beauty.$F4.exr"` and the
  user runs `crust check -i scene.usda -f 5`
- **THEN** the product's file is reported as `renders/beauty.0005.exr`
