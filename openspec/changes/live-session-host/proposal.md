## Why

Mantle (https://github.com/doubleailes/Mantle), a USD node graph, drives
crust as an interactive renderer: each node edit re-cooks the graph, and the
render should follow as cheaply as a `crust mcp` edit does. Two things stand
in the way.

- **The session core follows one override layer.** Mantle writes its cooked
  output as a root layer and numbered sublayers (`mantle cook`'s merge). An
  edit changes one of the sublayers, which the session sees as an external
  source changing: every edit would be `full`, even a light's intensity.
- **The threads a host needs live in `crust-hydra`.** The session thread,
  the render thread, restarts and panic containment are the Hydra delegate's
  `Session`. A second host would copy them. Its `cancel` also queues behind
  the session thread, so a cancel issued during an import waits for it.

## What Changes

- **`crust_render::session::live::LiveSession`** (feature `session`): the
  Hydra delegate's session lifted out of `crust-hydra`, unchanged in
  behaviour, with host hooks for thread setup and import warnings.
  `crust-hydra::Session` wraps it.
- **An owned layer stack.** `LiveSession::load_owned(root, sublayers)` names
  layers the host writes beside the root. They must be in the root layer
  stack (reached through `subLayers` with no offset), so their paths are the
  stage's. `apply` diffs every owned layer whose bytes moved and classifies
  the union of the changes with the existing classifier. A layer the session
  does not own that changed on disk since the last import makes the change
  `full`.
- **`LiveSession::cancel_render`**: one atomic store on the running render's
  control, never queued behind an import.
- **Snapshots name their render** (`Snapshot::render`, `LiveSession::render`)
  and say whether they are a completed render's final image, so a host can
  tell which state an image shows.
- **No render or import change.** The classifier, the in-place rebuilds and
  the import are untouched.
