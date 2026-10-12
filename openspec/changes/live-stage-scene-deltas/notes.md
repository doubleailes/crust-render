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

`--stats` peak memory on ALab and Moana: **not measured** — neither stage is in this
checkout.

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

The cornell box, the DPEL teapot and ALab, named by the task, were not measured as
such: the cornell box has no material inputs or rect light to edit (its edits are
covered by the in-repo lookdev stage of the session tests, all under 4 ms either
way), and the teapot (whose wrapper references an asset outside the repository) and
ALab are not in this checkout. The synthetic stage stands in for "textured": its
`full` edit is ~70× faster with the switch on, from the kept decodes alone.
