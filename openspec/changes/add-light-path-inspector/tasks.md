# Tasks

Step 1 is groups 1–6. Step 2 (group 7) waits on `add-identity-aovs-openexrid`.

## 1. Inspection alphabet and light indices in `Route`

- [ ] 1.1 Add an inspection constructor to `RouteCtx` whose alphabet names every `LobeLabel`, scatter and event type, with a decoder from symbol back to `<type scatter 'label'>`; verify with a unit test that every `LobeEvent` round-trips to the OSL text the LPE parser accepts.
- [ ] 1.2 Record the light-list index beside each `L` entry (NEE, `next_emit`, crossings, terminal lights) when the inspection alphabet is active; verify the zero-AOV render's callgrind instruction count on `samples/cornellbox.usda -s 2` is unchanged and the existing `tests/lpe.rs` pass bitwise.

## 2. The record fold

- [ ] 2.1 Implement the forward fold over `Route` that emits one record per emission-ending term (D1 table), with prefix product, MIS weight, dominant lobe per vertex, `lobe_split` and `label_ambiguous`; verify with a test that each sample's records sum to its radiance within 1e-5 relative on Cornell box, `veach_mis`, a volume sample and a thin-walled glass sample.
- [ ] 2.2 Apply the beauty's indirect-clamp factor to records beyond vertex 0 (`contribution_clamped`); verify with a test that clamped records sum to the clamped beauty within 1e-5.
- [ ] 2.3 Pass each sample's index, film coordinates `(i + 0.5 + fx, j + 0.5 + fy)` and filter weight from `advance_pixel` to the fold; verify with a test that under `--filter box` every record's coordinates lie inside its pixel.
- [ ] 2.4 Add the inspection-vs-beauty pair to `docs/architecture.md` § Invariants and verify the docs build (`zola build` in `site/` is unaffected; the file renders in the repo).

## 3. Record store, grouping and metrics

- [ ] 3.1 Per-unit top-N heap with the D5 total order and per-unit group maps on interned keys (`none`, `light`, `expr`, `expr+light`), merged in unit order; verify with a test that the merged result is identical with `RAYON_NUM_THREADS=1` and `=8`.
- [ ] 3.2 Compute `luma_share`, `second_moment_share`, `hit_rate`, `max_luma`, clamped shares, `clamp_removed_share`, `ambiguous_luma_share` and `consistency` (D6); verify with a test that `luma_share` and `second_moment_share` each sum to 1 over the groups within 1e-6 on Cornell box.
- [ ] 3.3 Add a test scene (or reuse a caustic sample) with a rare specular path, and verify its group ranks first by `second_moment_share` with a hit rate below 1e-2.

## 4. Engine entry point and report

- [ ] 4.1 Add `crust_core::inspect::run(scene, settings, region, &Options) -> Report`, refusing regions over 100 pixels or outside the frame, rendering through the AOV instantiation without the user's AOVs; verify with a test that the region's beauty equals the full render's pixels bitwise on Cornell box and `veach_mis` at 16 spp, and that a guided scene reports `reproduces_frame: false`.
- [ ] 4.2 `Report::to_json()` as `crust-light-paths/1` (D7 fields and order, `prims: null` + `prims_note`, hints as data with their thresholds in one table) and `Report::to_markdown()`; verify with a test that two runs produce byte-identical JSON and that each record's `expression` compiles with the LPE parser.
- [ ] 4.3 Resolve light names and tags once at report time (prim path in `light`, tag in the expression's `L`, plain `L` when untagged); verify with a test on a scene with one tagged and one untagged light.
- [ ] 4.4 Write the `light-path-inspection` design record (`openspec/specs/light-path-inspection/design.md` content prepared in this change's design) and add the capability's row to the table in `CLAUDE.md`; verify the table links resolve.

## 5. CLI subcommand

- [ ] 5.1 Add `crust light-paths` with the shared scene/settings flags, `-s`, required `--region`, `--top`, `--group-by`, `--json PATH|-`; refuse `-o`, oversized regions and `expr+prims+light` as usage errors before loading; verify with CLI tests for each refusal and for `--json -` keeping stdout pure JSON.
- [ ] 5.2 Document the subcommand in `site/content/docs/reference/command-line.md` and add a help page on finding the path behind a firefly (probe → `light-paths` → author the expression as a RenderVar); verify with `zola build` (0.21) in `site/`.

## 6. MCP tool

- [ ] 6.1 Keep a hash of the layer bytes on each `Job`; add `light_paths(render_id, region, group_by, top)` returning the report JSON, refusing incomplete renders, a changed layer and bad regions; verify with tests in `crates/crust-render/src/mcp/tests.rs` for the probe → light_paths flow, an edit, and an edit undone.
- [ ] 6.2 Add `light_paths` to the tool list test and document it in `site/content/docs/help/claude-desktop.md`; verify the tool-listing test and `zola build`.

## 7. Step 2: prims (after `add-identity-aovs-openexrid` lands the hit → prim table)

- [ ] 7.1 Fill each record's `prims` with every vertex's instance prim path from `(geom_id, prim_id, placement)`, and drop `prims_note` when the table is present; verify with a test on an instanced sample that no path names `/__Prototype_`.
- [ ] 7.2 Enable `--group-by expr+prims+light` (CLI and MCP) and update the docs pages; verify with a test that the grouping splits a path by the instance it bounced on, and that the format string is still `crust-light-paths/1`.

## Workflow follow-up

- Archive the change once step 1 ships if step 2 is split into its own change; otherwise after step 2.
