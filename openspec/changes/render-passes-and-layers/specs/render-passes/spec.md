# Spec Delta

## Purpose

Renders one USD RenderPass in one invocation and splits its image into render layers, each layer
holding out the others, then writes the outputs into files grouped by a file
pattern, so a compositor receives the split it expects.

## ADDED Requirements

### Requirement: Rendering a render pass

`crust render --pass <path>` SHALL render the RenderPass prim at `path`. The render SHALL use the
`RenderSettings` prim targeted by the pass's `renderSource` relationship. When the pass authors no
`renderSource`, it SHALL use the stage's own render settings. Without `--pass`, a render SHALL
behave exactly as before this change.

#### Scenario: A pass picks its settings

- **WHEN** `/Render/Passes/hero` targets `renderSource = </Render/hero_settings>`, and those
  settings author `resolution = (640, 360)` while the stage's default settings author 1920×1080
- **THEN** `crust render --pass /Render/Passes/hero` renders at 640×360

#### Scenario: No pass given

- **WHEN** a stage authors RenderPass prims and the render is run without `--pass`
- **THEN** the files written and the image are identical to a render made before this change

#### Scenario: Not a render pass

- **WHEN** `--pass` names a prim that does not exist or is not a `RenderPass`
- **THEN** the command fails with a non-zero exit status before rendering, naming the path

#### Scenario: A source that is not render settings

- **WHEN** the pass's `renderSource` targets a prim that is not a `RenderSettings`
- **THEN** the command fails with a non-zero exit status before rendering, naming both prims

### Requirement: Render layers are collections on the pass

A pass SHALL declare each render layer as a multiple-apply collection `collection:layer:<name>` on
the RenderPass prim. Membership SHALL follow `UsdCollectionAPI`, as light-link collections do.
Layers SHALL be ordered as the pass's `apiSchemas` list them. A prim no layer claims SHALL belong
to an implicit layer named `default`.

#### Scenario: Two layers

- **WHEN** a pass applies `CollectionAPI:layer:set` including `/World/Set` and
  `CollectionAPI:layer:hero` including `/World/Hero`
- **THEN** the pass has the layers `set`, `hero` and `default`, in that order

#### Scenario: A pass with no layers

- **WHEN** a pass applies no `layer:` collection
- **THEN** the pass has one layer, `default`, which holds the whole image

#### Scenario: Membership is inherited

- **WHEN** `collection:layer:hero` includes `/World/Hero` and the camera sees the mesh
  `/World/Hero/Body/Mesh`
- **THEN** that mesh belongs to `hero`

### Requirement: A prim claimed by two layers

When two layers of a pass both claim a prim, the prim SHALL belong to the earlier layer, and the
import SHALL raise one coded warning naming the prim and both layers.

#### Scenario: Overlapping layers

- **WHEN** `layer:set` includes `/World` and `layer:hero` includes `/World/Hero`
- **THEN** `/World/Hero` belongs to `set`, and a warning names `/World/Hero`, `set` and `hero`

### Requirement: Each sample belongs to one layer

Every camera sample SHALL be owned by the layer of the first thing its camera ray meets: a surface
after cutouts are passed through (a thin wall counts as met), or a volume region where the ray
scatters. A ray that escapes the scene, including one that sees a dome light, SHALL be owned by
`default`.

#### Scenario: A cutout leaf in front of the hero

- **WHEN** a camera ray passes through the transparent part of a cutout leaf in layer `set` and
  then hits the hero
- **THEN** the sample belongs to `hero`

#### Scenario: Seeing the sky

- **WHEN** a camera ray escapes and sees the dome light
- **THEN** the sample belongs to `default`

#### Scenario: Glass owns what is seen through it

- **WHEN** a camera ray hits a glass pane in layer `set`, refracts and reaches the hero
- **THEN** the sample belongs to `set`

### Requirement: A layer receives only the samples it owns

Each layer SHALL have its own copy of every RenderVar the pass's settings request. A sample SHALL
add its colour, alpha and AOV values only to the layer that owns it. In every other layer it SHALL
add nothing, so a pixel those layers do not own there holds zero colour and zero alpha (a
holdout). Geometric AOVs SHALL hold their clear value where a layer owns no sample.

#### Scenario: A hero behind a wall is held out

- **WHEN** a wall in `set` hides the hero over some pixels
- **THEN** in `hero` those pixels have zero colour and zero alpha, and in `set` they hold the wall

#### Scenario: Depth where a layer owns nothing

- **WHEN** a pass requests a `depth` var and a pixel's samples all belong to `set`
- **THEN** `hero`'s depth at that pixel is depth's clear value

### Requirement: Layers do not change the light

Layering SHALL NOT change any path's light transport. Every object SHALL shadow, reflect and
bounce light in every layer, as in the render without layers. The colours of a pass's layers SHALL
sum to the beauty of the same render, up to floating-point rounding. A pass with one layer SHALL
give that layer exactly the beauty, bit for bit.

#### Scenario: The hero's shadow on the set

- **WHEN** the hero, in `hero`, casts a shadow on the floor, in `set`
- **THEN** the shadow is in `set`'s colour, and `hero` holds none of the floor

#### Scenario: The layers add up to the beauty

- **WHEN** a pass with layers `set`, `hero` and `default` is rendered at `-s 16`
- **THEN** the sum of the three layers' colour equals the beauty to within rounding

#### Scenario: One layer is the beauty

- **WHEN** a pass with no layer collections is rendered
- **THEN** the `default` layer's colour is bit-identical to the render without `--pass`

### Requirement: The pass's file pattern

A pass's output paths SHALL come from its `crust:filePattern` string attribute. With `--pass`,
`-o` SHALL replace the pattern. When neither is given, the pattern SHALL be `$l_$n_$o.$04f.$x`.
The pattern SHALL be expanded once for every (layer, RenderVar) of the pass. A relative result
SHALL resolve against the working directory, and parent directories SHALL be created.

#### Scenario: The authored pattern

- **WHEN** a pass authors `crust:filePattern = "renders/$l_$n_$o.$04f.$x"`, is named `render`, and
  has layers `hero` and `default` and vars `Beauty` and `Diffuse`, rendered at frame 3
- **THEN** the pass writes `renders/render_hero_Beauty.0003.exr`,
  `renders/render_hero_Diffuse.0003.exr`, `renders/render_default_Beauty.0003.exr` and
  `renders/render_default_Diffuse.0003.exr`

#### Scenario: `-o` replaces the pattern

- **WHEN** the same pass is rendered with `--pass` and `-o 'out/$n.$x'`
- **THEN** the pass writes `out/hero.exr` and `out/default.exr`

### Requirement: File pattern specifiers

A pattern SHALL expand the following specifiers:

- `$l` and `$L`: the pass prim's name.
- `$n`: the layer's name.
- `$o`: the RenderVar's name.
- `$f` and `$0Nf`: the frame (see "Frame specifiers in a pattern").
- `$c` and `$C`: the render camera prim's name.
- `$e` and `$E`: the eye, `1` and `left` for crust's single view.
- `$x`: `exr`.
- `$(NAME)`: the environment variable `NAME`.

`$_` followed by a specifier SHALL lower-case its value. `\$` SHALL write a literal `$`.

#### Scenario: Lower-cased AOV name

- **WHEN** the pattern is `$l_$_o.$x` and the var is named `Diffuse`
- **THEN** the file is `render_diffuse.exr`

#### Scenario: Environment variable

- **WHEN** the pattern is `$(IMAGES)/$l.$x` and `IMAGES=/tmp/images`
- **THEN** the file is `/tmp/images/render.exr`

### Requirement: Frame specifiers in a pattern

`$f` SHALL expand to the integer frame and `$0Nf`, with N from 1 to 9, to that frame zero-padded to
N characters, sign included. Without `-f`, the frame SHALL be 1, as for a product path.

#### Scenario: Padded frame

- **WHEN** the pattern is `$l.$05f.$x` for pass `render` at frame 12
- **THEN** the file is `render.00012.exr`

#### Scenario: No frame given

- **WHEN** the pattern is `$l.$04f.$x` and the render passes no `-f`
- **THEN** the file is `render.0001.exr`

#### Scenario: Negative frame

- **WHEN** the pattern is `$l.$04f.$x` and the render uses `-f -3`
- **THEN** the file is `render.-003.exr`

### Requirement: A subframe in a pattern

When the render's frame is not an integer, `$f` and `$0Nf` SHALL expand to the nearest frame,
halves rounded away from zero, and SHALL raise one `product.subframe_frame` warning (kind
`approximated`) per pattern, naming the frame and the pattern, as a product path does.

#### Scenario: Frame 12.5

- **WHEN** the pattern is `$l.$04f.$x` and the render uses `-f 12.5`
- **THEN** the file is `render.0013.exr` and a `[product.subframe_frame]` warning is logged

### Requirement: Unknown specifiers are left as written

A `$` followed by anything that is not a specifier (`$q`, `$SHOT`, a lone `$`) SHALL be left in the
path as written, as a product path leaves an unknown variable. A pattern holding one SHALL raise
one `product.unknown_variable` warning (kind `refused`) naming it and the pass; a lone `$` raises
nothing.

#### Scenario: Unknown specifier

- **WHEN** the pattern is `$l_$q.$x`
- **THEN** the file is `render_$q.exr` and a `[product.unknown_variable]` warning names `$q`

#### Scenario: A lone dollar

- **WHEN** the pattern is `$l_price$.$x`
- **THEN** the file is `render_price$.exr` and no warning is logged

### Requirement: Patterns that would write elsewhere are refused

The command SHALL fail with a non-zero exit status, before rendering, and naming the pattern, when
the pattern holds a `$(NAME)` whose variable is unset, or when its expansion's extension is not
`.exr`, ignoring case.

#### Scenario: Unset variable

- **WHEN** the pattern is `$(IMAGES)/$l.$x` and `IMAGES` is unset
- **THEN** the command fails before rendering, naming `IMAGES`

#### Scenario: Not an EXR

- **WHEN** the pattern is `$l_$n_$o.png`
- **THEN** the command fails before rendering, naming the extension

### Requirement: Outputs sharing a path share a file

Every (layer, RenderVar) output whose pattern expands to the same path SHALL be written to that one
single-part, multichannel EXR. Outputs SHALL be ordered by layer, then by RenderVar. The file
SHALL carry the header attributes a product's EXR carries.

#### Scenario: No `$o`

- **WHEN** the pattern is `$l_$n.$x` with layers `hero` and `default` and vars `Beauty` and
  `Diffuse`
- **THEN** two files are written, `render_hero.exr` and `render_default.exr`, each holding both
  vars of its layer

#### Scenario: Neither `$n` nor `$o`

- **WHEN** the pattern is `$l.$x`
- **THEN** one file, `render.exr`, holds every var of every layer

### Requirement: Channel names inside a pattern file

A file holding one output SHALL name its channels as a RenderProduct holding that one RenderVar
does (the beauty unprefixed). In a file holding several outputs, each output's channels SHALL be
prefixed with its **layer name**, the pass's `crust:layerPattern` string attribute expanded for
that (layer, RenderVar) with the file pattern's specifiers, and a dot: `<layer name>.R`. When the
pass authors no layer pattern it SHALL be `$n_$o`.

#### Scenario: One output per file

- **WHEN** the pattern is `$l_$n_$o.$x` and a file holds only `hero`'s `Beauty` (color4f)
- **THEN** its channels are `R`, `G`, `B` and `A`

#### Scenario: One layer per file

- **WHEN** a file holds `hero`'s `Beauty` (color4f) and `Diffuse` (color3f), with no layer pattern
  authored
- **THEN** its channels are `hero_Beauty.R`, `hero_Beauty.G`, `hero_Beauty.B`, `hero_Beauty.A`,
  `hero_Diffuse.R`, `hero_Diffuse.G` and `hero_Diffuse.B`

#### Scenario: Several layers per file

- **WHEN** a file holds `hero` and `default`, each with `Beauty` (color4f) and `Diffuse` (color3f)
- **THEN** its channels include `hero_Beauty.R`, `hero_Beauty.A`, `hero_Diffuse.R`,
  `default_Beauty.R` and `default_Diffuse.B`

#### Scenario: An authored layer pattern

- **WHEN** the pass authors `crust:layerPattern = "$_n.$_o"` and a file holds `hero`'s `Diffuse`
  and `Specular`
- **THEN** its channels include `hero.diffuse.R` and `hero.specular.B`

### Requirement: Layer names must be unique within a file

Two outputs of one file whose layer names are equal SHALL fail the command with a non-zero exit
status before rendering, naming the file and the layer name.

#### Scenario: Two outputs with one layer name

- **WHEN** the file pattern is `$l_$n.$x`, the layer pattern is `$n`, and the vars are `Beauty`
  and `Diffuse`
- **THEN** the command fails before rendering, naming `render_hero.exr` and the layer name `hero`

### Requirement: Preview PNGs of a pass

A file that holds exactly one layer and that layer's beauty SHALL get a tone-mapped PNG beside it,
as a product's EXR does. A file holding several layers SHALL get no PNG.

#### Scenario: Per-layer previews

- **WHEN** the pattern is `$l_$n.$x` and the vars include the beauty
- **THEN** `render_hero.png` and `render_default.png` are written next to their EXRs

### Requirement: Pass collections that are not read

A pass that authors a `renderVisibility` or `cameraVisibility` collection that restricts membership
SHALL be rendered as if it did not. The import SHALL raise one coded warning naming the pass and
the collection.

#### Scenario: A camera-visibility collection

- **WHEN** a pass authors `collection:cameraVisibility:includeRoot = 0`
- **THEN** the render sees every object, and a warning names the pass and `cameraVisibility`
