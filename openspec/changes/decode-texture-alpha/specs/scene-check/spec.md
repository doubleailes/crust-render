## MODIFIED Requirements

### Requirement: JSON report

`--json PATH|-` SHALL write one JSON object of format `crust-check/2`, following the
shared report shape, with keys in this fixed order:

- `format`, `crust_version`;
- `scene`, `products`, `effective_settings`;
- `import`, `counts`;
- `findings`, `warnings`;
- `denied`: the codes of the warnings that a `--deny` matched, or an empty array.

With `-`, the JSON SHALL replace the text report on stdout. With a path, the text
report SHALL still go to stdout.

#### Scenario: JSON on stdout

- **WHEN** the user runs `crust check -i scene.usda --json -`
- **THEN** stdout parses as one JSON object with `format` `crust-check/2`, and the
  log appears on stderr

#### Scenario: JSON to a file

- **WHEN** the user runs `crust check -i scene.usda --json check.json`
- **THEN** `check.json` holds the report and the text report is on stdout
