# Notes: live-stage-scene-deltas

Measurements for tasks 2.2 and 5.2. The long-form record is
`openspec/specs/mcp-session/design.md` § Edits applied in place.

## 2.2 Provenance cost

`RAYON_NUM_THREADS=1 valgrind --tool=callgrind --cache-sim=no --branch-sim=no
crust render -i samples/cornellbox.usda -s 2`:

| binary | instructions |
|---|---|
| before | 4 222 741 990 |
| after | 4 222 911 859 (+169 869, 0.004%) |

The difference is all import-time: `openusd::sdf::ordering::element_cmp` +111 482 (the
`RenderSettings` path the provenance records), `_int_malloc` +23 302 (the maps); no
render function moved.

`--stats` peak memory, `7f981a7^` (before) against `353fd0f` (after), `-s 4`, one run
each, before and after side by side on the 72-vCPU VM. Both stages live outside the
checkout, in `~/Workspace/samples/` (ALab through `renders/alab/alab.usda -f 1004`, the
island as `usd/island.usda --camera /island/cam/shotCam`):

| stage | before | after | difference |
|---|---|---|---|
| ALab, frame 1004 | 13.22 GiB | 13.18 GiB | −0.04 GiB |
| Moana island | 23.84 GiB | 24.07 GiB | +0.23 GiB (+1.0%) |

Both peaks are reached at the BVH commit, as before. Neither difference is resolved by
one run: the island's peak has a recorded run-to-run spread of 23.92–24.14 GiB
(`docs/moana_profile.md`). If the island's +0.23 GiB is real, the likeliest owner is
the kept `LightLinks` runs, the one provenance map that grows with the prim count
rather than the material and light counts (the island has 3.15 M geometries).

## 5.2 Edit latency

`import_s` of each edit, min / mean of 10 alternating edits, release build, 4 cores.
Driver: an MCP client over stdio (`initialize`, `open_session`, `author_usda` ×10 per
kind).

| stage | `CRUST_SCENE_DELTAS` | open | settings | lights | materials | full |
|---|---|---|---|---|---|---|
| synthetic, 16 × 2048² PNG | 1 | 0.811 s | 1.7 / 1.9 ms | 0.9 / 1.0 ms | 1.2 / 1.3 ms | 8.9 / 9.1 ms |
| synthetic, 16 × 2048² PNG | 0 | 0.764 s | 608 / 632 ms | 633 / 662 ms | 634 / 659 ms | 624 / 644 ms |
| `usdpreview_textured` | 1 | 18 ms | 1.0 / 1.2 ms | 0.6 / 0.7 ms | 0.9 / 1.3 ms | 4.3 / 5.1 ms |
| `usdpreview_textured` | 0 | 15 ms | 5.7 / 6.5 ms | 5.8 / 6.3 ms | 5.5 / 6.5 ms | 5.6 / 6.1 ms |
| `PointInstancedMedCity` | 1 | 6 ms | 0.8 / 0.9 ms | — | — | 1.6 / 1.8 ms |
| `PointInstancedMedCity` | 0 | 6 ms | 1.7 / 1.8 ms | — | — | 1.6 / 1.9 ms |

The synthetic stage stands in for "textured": its `full` edit is ~70× faster with the
switch on, from the kept decodes alone.

### The stages the task names

Same driver (`initialize`, `open_session`, then `author_usda` round-robin over the
classes, alternating between two values), `CRUST_SCENE_DELTAS` on then off, never two
sessions at once. 10 rounds on the cornell box and the teapot, 3 on ALab.

| stage | `CRUST_SCENE_DELTAS` | open | settings | lights | materials | full |
|---|---|---|---|---|---|---|
| cornell box | 1 | 26 ms | 4.4 / 4.5 ms | 2.8 / 2.8 ms | — | 24.6 / 24.8 ms |
| cornell box | 0 | 26 ms | 24.9 / 25.5 ms | 24.7 / 25.5 ms | — | 24.8 / 25.3 ms |
| DPEL teapot | 1 | 24.4 s | 1.5 / 1.8 ms | 0.7 / 0.9 ms | 0.9 / 1.1 ms | 337 / 381 ms |
| DPEL teapot | 0 | 23.2 s | 21.7 / 22.7 s | 22.3 / 22.6 s | 21.7 / 22.7 s | 22.1 / 22.6 s |
| ALab, 1004 | 1 | 114 s | **103 / 107 s** | **104 / 105 s** | **104 / 105 s** | 104 / 106 s |
| ALab, 1004 | 0 | 113 s | 108 / 109 s | 106 / 109 s | 107 / 111 s | 107 / 112 s |

The edits: the render camera's `exposure` (settings); the dome's `inputs:intensity` on
the cornell box, `Key`'s on the teapot, `lgt_fill_top`'s on ALab (lights); the
teapot's `GroundMaterial` `roughness`, the ALab stoat's `sweater_full` `roughness`
(materials); a translate (full). The cornell box has no `materials` column: its one
material is empty, so no shader input exists to edit.

- **The teapot** is the case the change was for: 22.6 s per edit becomes ~1 ms in place,
  and a `full` edit drops to 0.38 s — the import without its texture decodes.
- **ALab applies nothing in place.** Every edit was `full`, with the switch on. A
  `-l debug` session names one refusal per class, each a deliberate one:
  - settings: `adaptive subdivision reads the camera` — the shot sets
    `crust:subdivisionEdgeLength`, and the check refuses any camera change, even an
    exposure, which does not move the tessellation;
  - lights: `the scene's lights author links` — the light rig uses light linking;
  - materials: the sweater's shader input does not resolve to a material. The stoat's
    looks arrive through a payload (`baked_procedurals/main.usda`), and the session's
    authoring stage keeps payloads unloaded, so the classifier cannot read that the
    edited prim is a `Shader`.
  The kept assets save ALab almost nothing either (~2–5 s of a 110 s import): its
  import is composition and traversal, not decoding.
