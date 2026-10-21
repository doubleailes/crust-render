## MODIFIED Requirements

### Requirement: Opening a session creates an override layer

`open_session` SHALL take an input stage and an output path. When `output` does not
exist, it SHALL write a USD layer there whose only composition arc is a sublayer on
the input, open the stage on that layer, and import it. The layer SHALL author the
input root layer's `metersPerUnit` and `upAxis` when the input authors them, so the
session's stage has the input's units and up axis. It SHALL refuse an `output`
that is one of the input's own layers, and one that is not a `.usda` text layer.

#### Scenario: A new session

- **WHEN** `open_session` is called with `input = shot.usda` and
  `output = shot_lookdev.usda`, and `shot.usda` authors no `metersPerUnit` and no
  `upAxis`
- **THEN** `shot_lookdev.usda` exists, holds `subLayers = [@./shot.usda@]` and no
  prim opinion, and the result reports the camera, resolution and warning count

#### Scenario: The input's units and up axis carry over

- **WHEN** `open_session` is called on an input that authors `metersPerUnit = 1`
  and `upAxis = "Z"`
- **THEN** the new override layer authors `metersPerUnit = 1` and `upAxis = "Z"`
  beside its `subLayers`, and a camera with `fStop = 2` renders as
  `crust render` of the input does

#### Scenario: Output is a source layer

- **WHEN** `output` names the input itself or a layer it composes
- **THEN** the call fails and no file is written

#### Scenario: Output is not a text layer

- **WHEN** `output` is `shot_lookdev.usdc`
- **THEN** the call fails, saying the layer must be a `.usda`, and no file is
  written

### Requirement: Resuming a session

When `output` already exists and is an override layer that sublayers `input`,
`open_session` SHALL open it and resume, keeping its opinions. If the layer does not
author `metersPerUnit` or `upAxis` and the input's root layer does, the session SHALL
add the input's value to the layer before importing it. A value the layer authors is
kept. When `output` exists but does not sublayer `input`, the call SHALL fail without
writing.

#### Scenario: Resuming yesterday's work

- **WHEN** `open_session` is called again with the same `input` and `output` after
  edits were made in an earlier session
- **THEN** the stage composes those edits, and the first `render` shows them

#### Scenario: Resuming a layer written without the input's units

- **WHEN** `output` is an override layer that authors no `metersPerUnit`, over an
  input that authors `metersPerUnit = 1`
- **THEN** after `open_session` the layer authors `metersPerUnit = 1`, and its
  earlier opinions are unchanged
