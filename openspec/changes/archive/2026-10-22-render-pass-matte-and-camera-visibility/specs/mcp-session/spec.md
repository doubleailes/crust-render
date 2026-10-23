# Spec Delta

## ADDED Requirements

### Requirement: A session renders one pass

`open_session` SHALL accept an optional `pass`, the path of a `RenderPass` prim,
with the meaning `--pass` has for `crust render`. Every render, probe and final
render of the session SHALL use that pass. Without it, the session has no active
pass. An edit to the active pass's collections SHALL take effect on the next
render, as an edit to any other prim does.

#### Scenario: A lookdev session on the character pass

- **WHEN** `open_session` is called with `pass = /Render/Passes/char`
- **THEN** the session's renders hold out what that pass's `matte` names, and
  `render_final` writes what `crust render --pass /Render/Passes/char` of the
  session layer writes

#### Scenario: Editing the matte collection

- **WHEN** a session on `/Render/Passes/char` authors a new include on its
  `collection:matte`
- **THEN** the next render holds out the newly included geometry too

#### Scenario: A pass that does not exist

- **WHEN** `open_session` names a `pass` that is not a `RenderPass` on the stage
- **THEN** the call fails, naming the path
