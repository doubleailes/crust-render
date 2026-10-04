## ADDED Requirements

### Requirement: Colour-management flags

The CLI SHALL accept `--ocio-config <CONFIG>` (an OCIO config file, archive or
`ocio://` URI, installed before any colour is converted; an error when it
cannot be loaded or lacks the spaces crust needs), `--working-space <SPACE>`
(overriding `renderingColorSpace`), and `--display` / `--view` for the PNG
preview.

#### Scenario: An unknown view

- **WHEN** `--view "no such view"` is given
- **THEN** the tool exits with an error before rendering
