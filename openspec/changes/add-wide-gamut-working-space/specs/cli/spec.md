## ADDED Requirements

### Requirement: Colour-management flags

The CLI SHALL accept `--ocio-config <CONFIG>` (an OCIO config file, archive or
`ocio://` URI, installed before any colour is converted; an error when it
cannot be loaded or lacks the spaces crust needs; without the flag, the
non-empty `OCIO` environment variable names the config, else the builtin ACES
CG config is used), `--working-space <SPACE>`
(overriding `renderingColorSpace`), and `--display` / `--view` for the PNG
preview.

#### Scenario: An unknown view

- **WHEN** `--view "no such view"` is given
- **THEN** the tool exits with an error before rendering

#### Scenario: The OCIO variable as the fallback

- **WHEN** `OCIO` names a config and `--ocio-config` is not given
- **THEN** that config is installed, and one that cannot be loaded is an error
  naming `$OCIO`
- **WHEN** both are given
- **THEN** `--ocio-config` is used and `OCIO` is ignored
