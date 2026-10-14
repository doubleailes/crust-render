# Notes: in-place-edits-on-production-shots

Measurements for tasks 4.1 and 4.2. The long-form record is
`openspec/specs/mcp-session/design.md` § Edits applied in place.

## 4.1 ALab edit latency

`import_s` of each edit, min / mean of 3 rounds. Release build, 72-vCPU VM, never two
sessions at once. Driver: an MCP client over stdio (`initialize`, `open_session`,
`author_usda` round-robin over the classes, alternating between two values). Stage:
`renders/alab/alab.usda` (the shot's camera, `crust:subdivisionEdgeLength = 2`,
`learned` light selection). Edits: the render camera's `exposure` (settings),
`/root/lightrig01/lights/set/interior/lgt_fill_top`'s `inputs:intensity` (lights), the
`/root/stoat/outfit_M_hrc/MATERIAL/sweater_full/sweater_full` shader's
`inputs:roughness` (materials, delivered by `baked_procedurals/main.usda`'s payload),
and a translate on `/root/stoat` (full).

| binary | `CRUST_SCENE_DELTAS` | open | settings | lights | materials | full |
|---|---|---|---|---|---|---|
| before (`live-stage-scene-deltas`) | 1 | 114 s | 103 / 107 s | 104 / 105 s | 104 / 105 s | 104 / 106 s |
| before | 0 | 113 s | 108 / 109 s | 106 / 109 s | 107 / 111 s | 107 / 112 s |
| this change | 1 | 117 s | **42 / 60 ms** | **18 / 20 ms** | **48 / 54 ms** | 112 / 114 s |
| this change | 0 | 114 s | 102 / 110 s | 103 / 110 s | 105 / 108 s | 104 / 108 s |

The deltas-on session ran with `-l debug`, which logs the traversal, so its `full`
column (112 / 114 s) is not a clean comparison with the others. Its only fallback reason, three times, is
`applied as full: /root/stoat.xformOp:translate:latency`, the edit meant to be `full`.
Each confined class now reports its own `applied`.

The in-place classes cost tens of milliseconds on ALab, against ~1 ms on the teapot.
Where that goes was not profiled. The likely owners are the parts of an update that grow
with the scene: a masked stage per rebuild, a new `Renderer`, and its light selection
(here `learned`, over ALab's lights). At three orders of magnitude under the import it
replaces, it was left there.

## 4.2 Render cost

`RAYON_NUM_THREADS=1 valgrind --tool=callgrind --cache-sim=no --branch-sim=no
crust render -s 2`, the pre-change HEAD binary against this change:

| scene | before | after | difference |
|---|---|---|---|
| `cornellbox.usda` | 4 245 088 454 | 4 244 728 535 | −359 919 (−0.008%) |
| `light_linking.usda` | 196 036 570 | 196 150 926 | +114 356 (+0.06%) |

No render function moved. The differences are import-time: openusd's
`sdf::ordering::element_cmp` (−197 928 on the cornell box, +118 494 on light_linking),
then `memcpy` and `malloc`. They go in opposite directions on the two scenes, which is
composition-order noise, not added work.
