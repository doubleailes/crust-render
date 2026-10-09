## ADDED Requirements

### Requirement: Interrupting a render keeps its work

Once `crust render` is rendering, the first Ctrl-C (SIGINT) SHALL cancel the render and write
its outputs (EXR or products, and the PNG) from what was traced. It SHALL log a
warning that names the interruption and the samples reached, and exit with
status 130. A Ctrl-C that arrives after the render's last sample but before
`crust render` starts writing its outputs stops nothing: the outputs SHALL be
written complete and unmarked, and the exit status SHALL still be 130. A second
Ctrl-C, one before rendering starts, or a first one once the outputs are being
written, SHALL exit at once with status 130, writing nothing further.

#### Scenario: Ctrl-C during a long render

- **WHEN** the user presses Ctrl-C while `crust render -i scene.usda -o out.exr`
  renders
- **THEN** `out.exr` and `out.png` are written from the partial render, a warning
  says the render was interrupted, and the exit status is 130

#### Scenario: Ctrl-C as the render finishes

- **WHEN** the user presses Ctrl-C after the render traced its last sample but
  before `crust render` starts writing its outputs
- **THEN** the outputs are written complete, without `crust:renderStatus`, and the
  exit status is 130

#### Scenario: A second Ctrl-C

- **WHEN** the user presses Ctrl-C again while the partial outputs are written
- **THEN** the process exits at once with status 130

#### Scenario: Ctrl-C while loading the scene

- **WHEN** the user presses Ctrl-C before rendering has started
- **THEN** the process exits at once with status 130 and writes no output

### Requirement: Checkpoint previews

`crust render` SHALL accept `--checkpoint <SECONDS>`, a positive number. While the
render runs, every interval in which the snapshot changed, the tone-mapped PNG
preview SHALL be rewritten at the path the final PNG will use, from the latest
snapshot. The final outputs SHALL be unchanged by the flag. Without the flag, no
file SHALL be written before the render ends.

#### Scenario: Watching a render

- **WHEN** the user runs `crust render -i scene.usda -o out.exr --checkpoint 10`
- **THEN** `out.png` appears within about ten seconds of the render starting and is
  rewritten every ten seconds, and when the render ends `out.exr` and `out.png`
  are the same files a render without the flag would write

#### Scenario: A non-positive interval

- **WHEN** the user passes `--checkpoint 0`
- **THEN** the arguments are refused as a usage error

#### Scenario: No beauty to preview

- **WHEN** the stage's first product has no beauty var and `--checkpoint` is passed
- **THEN** a warning says no preview will be written, and the render proceeds
