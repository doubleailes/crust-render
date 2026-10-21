# Spec Delta

## ADDED Requirements

### Requirement: All-passes flag

`crust render` and `crust check` SHALL accept `--all-passes`, which selects every
`RenderPass` of the stage as the `render-passes` capability defines.
`--all-passes` SHALL be a usage error with `--pass`, with `render`'s `-o`, or
without `-i`. Every other flag SHALL apply to every pass, as it would to that
pass rendered alone.

#### Scenario: All passes and one pass

- **WHEN** the user runs `crust render -i shot.usda --all-passes --pass /Render/Passes/char`
- **THEN** the arguments are refused as a usage error and nothing is loaded

#### Scenario: All passes and an output path

- **WHEN** the user runs `crust render -i shot.usda --all-passes -o out.exr`
- **THEN** the arguments are refused as a usage error naming `-o`

#### Scenario: A flag shared by every pass

- **WHEN** the user runs `crust render -i shot.usda --all-passes -s 16 -f 12`
- **THEN** every pass renders frame 12 at 16 samples per pixel

### Requirement: Passes that would write the same file

Before it renders any pixel, `crust render --all-passes` SHALL compute the file
of every product of every pass for the frame being rendered. When two passes
would write the same file, it SHALL stop with an error naming both passes and the
file, write nothing, and exit non-zero.

#### Scenario: Two passes with no products

- **WHEN** two passes read settings that author no `RenderProduct`, so both would
  write the default beauty image
- **THEN** the render stops before rendering, naming both passes and the default
  file

### Requirement: Interrupting all passes

An interrupt during `--all-passes` SHALL stop at the pass it reaches. That pass
SHALL keep its work as an interrupted single render does. Passes not yet started
SHALL not be rendered. Passes already finished SHALL keep their files. The exit
status SHALL be that of an interrupted render.

#### Scenario: Interrupted in the second pass

- **WHEN** the user interrupts `crust render --all-passes` during the second of
  three passes
- **THEN** the first pass's files are complete, the second's hold the samples
  taken so far, and the third pass's files are not written
