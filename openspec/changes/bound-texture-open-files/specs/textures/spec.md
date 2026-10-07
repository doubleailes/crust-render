## ADDED Requirements

### Requirement: Streaming holds a bounded number of open files

Streamed UV textures SHALL keep at most `CRUST_TEX_MAX_OPEN_FILES` idle open files
(default 256) across all files. At any moment the open count SHALL NOT exceed that
cap plus the number of render threads. A render thread SHALL never wait on the cap.
`0` SHALL keep every reader open, as before. Images SHALL NOT change with the cap.
Every streamed file SHALL be closed before any output image is written.

#### Scenario: More streamed files than the cap

- **WHEN** a render streams tiles from more `.tx` files than `CRUST_TEX_MAX_OPEN_FILES`
- **THEN** the process's open texture files stay within the cap plus the thread count,
  and `--stats` reports the peak open count, the cap and the number of reopens

#### Scenario: The cap does not change the image

- **WHEN** the same scene is rendered at `-s 16` with `CRUST_TEX_MAX_OPEN_FILES=0` and
  with `CRUST_TEX_MAX_OPEN_FILES=1`
- **THEN** the two EXRs are bit-identical

#### Scenario: The output write after a long textured render

- **WHEN** a render that streamed thousands of `.tx` files finishes
- **THEN** its texture files are closed before the EXR and PNG are written, and the
  write does not fail for lack of file descriptors

### Requirement: A failed tile read is reported

When a streamed tile cannot be read, the lookup SHALL use the texture's fallback, and
the failure SHALL NOT be silent. If an open fails because the process or system is out
of file descriptors, idle streamed files SHALL be closed and the open retried once
before the read counts as failed. Each failing file SHALL be named once at WARN. If any
tile read failed, the end of the render SHALL log one WARN with the count.

#### Scenario: Descriptors run out during the render

- **WHEN** opening a `.tx` fails with "too many open files" while idle streamed files
  are open
- **THEN** those idle files are closed, the open is retried, and the tile is read with
  no warning

#### Scenario: A file that cannot be read

- **WHEN** a `.tx` file becomes unreadable after the texture was bound
- **THEN** its lookups use the fallback, the file is named in one WARN line however many
  of its tiles fail, and the render ends with one WARN counting the failed tile reads
