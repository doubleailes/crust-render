## Context

`crust-hydra/src/session.rs` already owns what any host needs: a session thread
holding the `!Send` authoring stage, a render thread per render, restarts on a
fresh `RenderControl`, polls that never wait behind an import, and panics
caught at every boundary. The classification (`session::delta`) and the
in-place update (`session::scene`) are shared with `crust mcp` already.

## Decisions

### D1. Move, don't rewrite

The session moves to `crust_render::session::live` as it was. What was Hydra's
becomes a hook:

- `Hooks::on_thread` runs first on the session thread and every render
  thread (Hydra routes that thread's log to its host there);
- `Hooks::on_warnings` receives each import's warnings (Hydra forwards them
  to `TF_WARN`).

`crust-hydra::Session` keeps its API, its directory and its log id, and
delegates every call. Its tests pass unchanged.

### D2. Owned layers are diffed; other layers are sources

A host that writes several layers names them at `load_owned`. Each must be
reached from the root through `subLayers` alone, with an identity offset. A
sublayer of the root layer stack composes in the stage's own namespace, and
with no offset its time samples are the stage's, so a diff of it is read by
`classify` exactly as an override layer's is. Anything else (a referenced
layer, an offset sublayer) would need its paths or times mapped, so it is
refused.

`apply` reads every owned layer, diffs the ones whose bytes moved
(`classify_layers`), and classifies the union. A change of which layers the
root stack holds is a `subLayers` field change on the root, which the
classifier already sends to `full`.

The layers the import composed that the session does not own are stamped as
`crust mcp` stamps its sources (`Sources`). One that moved makes the change
`full`.

*Alternative rejected:* one override layer, written by the host as the
difference between its outputs. An override cannot express an erased field
or a removed spec of a weaker layer, and the image would no longer be the one
`crust render` gives of the host's own output.

### D3. Cancel without the queue

`cancel` stops and joins on the session thread, so it waits behind a running
import. `cancel_render` stores the flag on the shared running render
directly. A host that cancels the moment an edit arrives (before it even
writes the layers) gets the render loop's bound: one per-pixel advance.

## Risks

- **A host could name a layer it does not own.** The session would classify a
  third party's edit in place. Ownership is the host's statement; the check
  only verifies the namespace argument (D2).
