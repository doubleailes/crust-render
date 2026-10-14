# Spec Delta

## ADDED Requirements

### Requirement: The drawn purposes come from the render settings

The importer SHALL draw a prim only when its computed purpose is `default` or is listed in
the `RenderSettings` prim's `includedPurposes` (fallback `["default", "render"]`). A host
override (the CLI's `--purposes`) SHALL replace the authored list. Purpose is inherited:
a non-default purpose on an ancestor decides for its whole subtree. The same purposes
SHALL apply to lights, instances' prototypes and `PointInstancer` prototypes.

#### Scenario: A stage that authors no includedPurposes

- **WHEN** a model holds a `GEO` scope with `purpose = "render"` and a `GEO_PROXY`
  scope with `purpose = "proxy"`, and the settings author no `includedPurposes`
- **THEN** `GEO` renders and `GEO_PROXY` does not, as before this change

#### Scenario: Rendering the proxies

- **WHEN** the same stage is rendered with `--purposes proxy`
- **THEN** `GEO_PROXY` renders and `GEO` does not

#### Scenario: Default purpose is always drawn

- **WHEN** a mesh authors no purpose under ancestors that author none, and the render
  draws `--purposes proxy`
- **THEN** the mesh renders

#### Scenario: A purpose on an ancestor wins

- **WHEN** a mesh that authors no purpose sits under a scope with `purpose = "render"`,
  and the render draws `--purposes proxy`
- **THEN** the mesh does not render

#### Scenario: An unknown purpose token

- **WHEN** `includedPurposes` lists `"preview"`, which is not a purpose
- **THEN** the token is ignored with the `settings.unknown_purpose` warning, and the
  other listed purposes are drawn

### Requirement: A proxy stands in only for an unloaded payload

When both `render` and `proxy` are drawn, the importer SHALL skip a proxy-purpose subtree
unless its nearest ancestor carrying a payload arc is unloaded. A prim under no payload
counts as loaded. So a loaded asset is drawn by its render geometry alone, an unloaded
one by its proxy, and an asset without payloads as in the final render. When `proxy` is
drawn without `render`, every proxy SHALL be drawn.

#### Scenario: One asset loaded among unloaded ones

- **WHEN** `/World/a` and `/World/b` each author a proxy beside their payload arc, and
  the stage is rendered with `--unloaded --load /World/a --purposes render,proxy`
- **THEN** `/World/a` is drawn by its render geometry only, and `/World/b` by its proxy

#### Scenario: An asset without a payload

- **WHEN** a referenced asset authors both a render-purpose and a proxy-purpose scope
  and no payload, and the stage is rendered with `--purposes render,proxy`
- **THEN** only its render-purpose scope is drawn

#### Scenario: Proxies alone

- **WHEN** the same stage is rendered with `--unloaded --load /World/a --purposes proxy`
- **THEN** both `/World/a`'s and `/World/b`'s proxies are drawn, and no render-purpose
  geometry is

### Requirement: The material binding purposes come from the render settings

The importer SHALL resolve each prim's material by trying, in order, the binding purposes
in the `RenderSettings` prim's `materialBindingPurposes` (fallback `["full", ""]`), with
`""` the all-purpose binding and `allPurpose` its synonym. Each binding is inherited from
ancestors as `ComputeBoundMaterial` computes it. A host override (the CLI's
`--binding-purposes`) SHALL replace the authored list. A prim that no listed purpose binds
SHALL render with the default grey material.

#### Scenario: The fallback order

- **WHEN** a mesh has a `full` binding, a `preview` binding and an all-purpose binding,
  and the settings author no `materialBindingPurposes`
- **THEN** the mesh renders with its `full` material, as before this change

#### Scenario: Preview bindings

- **WHEN** the same mesh is rendered with `--binding-purposes preview,all`
- **THEN** it renders with its `preview` material

#### Scenario: Falling back to the all-purpose binding

- **WHEN** a mesh has only an all-purpose binding and the render uses
  `--binding-purposes preview,all`
- **THEN** it renders with the all-purpose material

#### Scenario: No listed purpose binds the prim

- **WHEN** a mesh has only a `full` binding and the render uses
  `--binding-purposes preview,all`
- **THEN** it renders with the default grey material

#### Scenario: Houdini's allPurpose token

- **WHEN** the settings author `materialBindingPurposes = ["full", "allPurpose"]`
- **THEN** a mesh with only an all-purpose binding renders with it, and no warning is
  raised

### Requirement: The import loads the payloads it is asked to

The importer SHALL take a load set: every payload loaded (the default) or none (the
CLI's `--unloaded`), then per-prim rules that load (`--load`) or unload (`--unload`) the
payloads at and below a prim, with USD load-rule semantics. A payload left unloaded
SHALL contribute nothing to the render: no geometry, light, material, volume or camera.

#### Scenario: Every payload by default

- **WHEN** a stage whose assets bring their geometry through payloads is rendered with
  none of the load flags
- **THEN** the image is bit-identical to the same render before this change

#### Scenario: Nothing loaded

- **WHEN** an asset authors its render geometry inside a payload and its proxy beside the
  payload arc, and the stage is rendered with `--unloaded --purposes proxy`
- **THEN** the proxy renders and the render geometry does not exist

#### Scenario: Loading one asset

- **WHEN** a stage holds two payloaded assets `/World/a` and `/World/b`, and is rendered
  with `--unloaded --load /World/a`
- **THEN** `/World/a`'s payload contents render and `/World/b`'s do not exist

#### Scenario: A nested payload

- **WHEN** `--unloaded --load /World/a` is given and `/World/a`'s payload itself holds a
  payloaded prim
- **THEN** that nested payload is loaded too

#### Scenario: Everything but one asset

- **WHEN** the stage is rendered with `--unload /World/b`
- **THEN** every payload is loaded except `/World/b`'s and those below it

#### Scenario: Loading inside an unloaded asset

- **WHEN** the stage is rendered with `--unload /World/b --load /World/b/hero`
- **THEN** `/World/b/hero`'s payload, and the payloads on its ancestry it needs, are
  loaded, and no other payload under `/World/b` is

#### Scenario: The order of the rules does not matter

- **WHEN** the same `--load` and `--unload` flags are given in another order
- **THEN** the same payloads are loaded and the image is bit-identical

#### Scenario: A light inside an unloaded payload

- **WHEN** a light is authored inside an unloaded payload
- **THEN** the render has no such light

#### Scenario: A prim that is not on the stage

- **WHEN** `--load /World/nothing` or `--unload /World/nothing` names a prim the stage
  does not compose even with the payloads on its ancestry loaded
- **THEN** the import fails, naming the prim, and nothing is rendered

### Requirement: The render camera is always composed

When the render camera (`--camera` or `RenderSettings.camera`) lies under an unloaded
payload, the importer SHALL load the payloads on the camera's own ancestry, and no other,
so that the render goes through it. Without a named camera, the render SHALL go through
the first camera the traversal meets on the stage as loaded.

#### Scenario: A shot camera inside a payload

- **WHEN** the stage is rendered with `--unloaded --camera /root/cam01/GEO/renderCam`,
  and `/root/cam01` brings the camera through a payload
- **THEN** the render goes through that camera, and no payload outside `/root/cam01`'s
  ancestry is loaded
