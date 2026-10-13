## 1. Baseline

- [x] 1.1 Keep the "before" binary (`target/release/crust` built from `bffa7a3`, the
      parent commit) aside for the A/B.
- [x] 1.2 Reduced scene: `an_excluded_glass_pane_does_not_count_a_linked_light_twice`
      in `crates/crust-core/tests/light_linking.rs`. A glossy floor under a 5° sun,
      behind a thick glass slab (transmission only) the sun's link excludes and the
      camera does not see, compared with the floor with no slab, under `light`,
      `bsdf` and `power`. Record that it fails today (bsdf 1.103 ± 0.016 against
      0.586).
- [x] 1.3 ALab "before", frame 1004, through an overlay in the scratchpad:
      `varianceThreshold = 0`, `indirectClamp = 0`, `crust:light:lpeTag` on the four
      exterior lights, and one product of `C<RG><L.'tag'>` / `C<RD><L.'tag'>` per
      light plus the beauty. Two seeds (layer offset), `--light-samples 4
      --light-selection learned`. Record the means, the two-seed variance and the
      top-0.1% share, as in `docs/alab_profile.md`.

## 2. Mask table (D4)

- [x] 2.1 `WorldBuilder::keep_masks()` and `World::mask(geom_id)` (`MASK_ALL` when not
      kept). `encode_shadows` calls `keep_masks()`.

## 3. The chain and the drop (D1, D2, D5)

- [x] 3.1 `PrevBounce::through` and `PrevVertex::through()`, with the chain rules of
      D1: a continuous reflection is an owner; a transmission extends the chain
      (`None` stays `None`); a delta reflection ends it; a phase vertex is an owner.
      Computed only in `trace_path`'s `LINKED` instantiation (D5). Remove the first delta-only patch
      (its `through()` defaulting to `Some(NONE)` for any non-delta vertex).
- [x] 3.2 `chain_here` at each surface hit, from `prev` before it is replaced, and a
      `LightList`-side test `drops_through(index, chain)`: restricted and
      `!shadow_mask(index).sees(chain)`.
- [x] 3.3 NEE: pass the chain into `surface_light_sample` /
      `extra_surface_light_samples`, and skip a sample of a dropped light whose
      direction transmits, before its shadow ray.
- [x] 3.4 Link twin: skip a dropped light when the sampled bounce transmits.
- [x] 3.5 Bounce side after a delta vertex: `bounce_emission_weight_at`,
      `escaped_emission` and `escaped_split` (one rule, in the pair) skip light `i`
      when `prev` is delta, its `through` passes for `i`, and `bounce_skips(i)`.

## 4. Verification

- [x] 4.1 The reduced-scene test passes under all three strategies. All of
      `light_linking.rs`, `lpe.rs`, `aovs.rs` and `light_paths.rs` pass, and so does
      `cargo test --workspace`.
- [x] 4.2 Golden images: `scripts/check_images.sh check` against goldens recorded with
      the before binary, at 16 spp. No sample authors links, so there must be no
      difference.
- [x] 4.3 Instruction count, unlinked: callgrind on `cornellbox.usda -s 2`, single
      thread, before against after. It must be flat (±0.1%), or the chain gets folded
      into existing state (design, Risks).
- [x] 4.4 ALab "after", with the same overlay, seeds and flags as 1.3: means per light
      and lobe, two-seed variance and top-0.1% share, and the
      `crust light-paths` pair at the oscilloscope region, which no longer shows
      `C<RG><TS><TS>L` beside `C<RG>L`. Done: the pair at (1114, 530) sample 31 is
      now `C<RG>L 243` alone, and the clamp's share of the region falls from 33% to 0.7%.
- [x] 4.5 CI checks: `cargo fmt --all -- --check`, `cargo clippy --workspace
      --all-targets -- -D warnings`.

## 5. Documentation

- [x] 5.1 `openspec/specs/lighting/design.md` § Shadow linking: the rule, why the
      thick glass is continuous, D1–D3, and the trap. Known gaps: camera-started
      chains, mixed chains, unrestricted lights through non-casters.
- [x] 5.2 `docs/alab_profile.md`: a dated section with the before/after numbers and
      the commands.
- [x] 5.3 The user documentation for light linking in `site/` (shadow-link
      semantics), if it describes what an excluded surface does. Check the Zola build.
