# Tasks

Prerequisite: `mcp-session` is archived before this change is (its spec is the one modified).

## 1. Keep the light selection

- [x] 1.1 Add `Renderer::retune(settings)`: set the settings, and rebuild the light selection only when `light_selection`, `width`, `height` or `frame` changed (D4). Verify with a crust-core test that `new(s0).retune(s)` renders bit-identically to `new(s)` under `learned`, for `s` differing from `s0` in spp and region, and that `retune` to the same selection inputs does not train (the returned setup time is the rebuild's absence).
- [x] 1.2 Switch the session's render start from `reconfigure` to `retune`.

## 2. Answer at the first image

- [x] 2.1 Add `wait` (`"image"` default, `"done"`) to `render`. Under `"image"`, answer once `samples_reached >= 4`, the render returned, or the budget passed; a render of 4 spp or fewer waits for its return (D1, D2). Verify with an integration test that a 100 000-spp render with `budget_s = 60` answers within a few seconds with `done = false` and `spp_reached >= 4`.
- [x] 2.2 Add `budget_s` to `snapshot`: wait for the render to return for at most that long, no wait by default (D3). Verify that `snapshot` with a budget after an early `render` answer reports `done = true`.
- [x] 2.3 Pass `wait = "done"` in the existing tests that assert completion above 4 spp, and keep every other assertion unchanged.

## 3. Documentation

- [x] 3.1 Update the server instructions, the tool docs, and `site/content/docs/help/claude-desktop.md` (tool table, Rendering notes, the worked session). Verify with `zola build`.

## 4. Integration

- [ ] 4.1 Run `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, and the crust-core and crust-render tests, and verify all are clean.
