# Spec Delta

## ADDED Requirements

### Requirement: Pass flag

`crust render`, `check`, `diagnostic` and `light-paths` SHALL accept
`--pass <PRIM_PATH>`, an absolute path to the `RenderPass` prim to render, as the
`render-passes` capability defines. Without it, no pass is active. On the
procedural fallback scene (no `-i`) it SHALL warn that it has no effect, as
`--camera` does.

#### Scenario: Rendering one pass

- **WHEN** the user runs `crust render -i shot.usda --pass /Render/Passes/char`
- **THEN** the render holds out and hides what `/Render/Passes/char`'s collections
  name

#### Scenario: The same pass in a check

- **WHEN** the user runs `crust check -i shot.usda --pass /Render/Passes/char`
- **THEN** the check describes the render that the same `crust render` command
  line would run

#### Scenario: A pass without a scene

- **WHEN** the user runs `crust render --pass /Render/Passes/char` with no `-i`
- **THEN** a warning says `--pass` has no effect, and the procedural scene renders

## MODIFIED Requirements

### Requirement: Listing what a stage holds

`crust ls <kind> -i <scene>` SHALL print the stage's prims of `kind`, one
absolute path per line on stdout, in namespace order, found by the render's
own walk so that the list is what a render would use. `kind` SHALL be one of
`camera`, `light`, `material` or `pass`, each also accepted in the plural:

- `camera`: exactly the cameras the render can go through — none under an
  inactive, abstract or proxy- / guide-purpose ancestor, inside an instance's
  prototype or beneath a `PointInstancer`; those under an invisible ancestor
  included.
- `light`: the UsdLux lights the import reads (sphere, rect, disk, cylinder,
  distant, dome), pruned as geometry is, invisibility included. A light the
  import refuses for its evaluated values (a zero radius or size, a transform
  that collapses it) SHALL still be listed: membership does not depend on the
  time code, even when `-f` is given.
- Each path SHALL be listed once, however many streamed chunks contain it.
- `material`: every `UsdShadeMaterial` a binding can reach — all but those
  under an inactive ancestor or inside an instance's prototype.
- `pass`: every `RenderPass` prim `--pass` accepts — all but those under an
  inactive ancestor.

Its log SHALL go to stderr, so stdout holds only the list.

#### Scenario: Listing the cameras of a stage

- **WHEN** the user runs `crust ls camera -i samples/cornellbox.usda`
- **THEN** stdout is `/scene/camera1` and the exit status is 0

#### Scenario: Listing the passes of a stage

- **WHEN** a stage defines `/Render/Passes/bg` and `/Render/Passes/char`, and the
  user runs `crust ls pass -i shot.usda`
- **THEN** stdout is those two paths, one per line, in namespace order

#### Scenario: An unknown kind

- **WHEN** the user runs `crust ls mesh -i scene.usda`
- **THEN** the arguments are refused as a usage error naming the valid kinds

#### Scenario: A stage that cannot be opened

- **WHEN** the input does not exist
- **THEN** an error is logged and the exit status is non-zero

#### Scenario: The text listing is unchanged by metadata

- **WHEN** the user runs `crust ls light -i scene.usda` without `--json`
- **THEN** stdout holds only the lights' paths, one per line, with no metadata
