# Tasks

## 1. Measure the walk

- [x] 1.1 Profile the fixture (`--stats`, `--profile`, callgrind at `-s 2`) and split the walk's cost between ray casts, libm and arithmetic; verified by the tables in `docs/subsurface_walk.md` § 1
- [x] 1.2 Build a walk-level harness with a copy of `random_walk` behind knobs, verified bit-identical to the real walk over 4 000 walks before any variant is measured
- [x] 1.3 Run the variant experiments (albedo mapping × entry, cap, stratified steps, reduced-albedo Dwivedi length, in-walk roulette); verified by `docs/subsurface_walk.md` § 2

## 2. The walk

- [x] 2.1 In-walk roulette below a peak throughput of 0.05 with `1/p` reweighting, drawn from the step's `new_domain(2)`; verified by `subsurface::tests` passing and the fixture's relmse against a 2048-spp reference matching the previous build to three digits at 16 and 32 spp
- [x] 2.2 Backward-stretched transmittance as `tr² / tr_fwd` with the 1e-18 guard; verified by callgrind (`expf` 419 M → 292 M, walk −7.1 %, render −3.6 %) and `dwivedi_sampling_matches_its_pdf` / slab tests passing
- [x] 2.3 Document both in `openspec/specs/materials/design.md` (the walk paragraph) and in `openspec/specs/rendering/design.md` (the `K_SSS` draw note); verified by the records naming the roulette threshold and the identity

## 3. The profile section

- [x] 3.1 `Section::Subsurface` in `profile.rs` (`ALL`, name, `Integrator` category) and the scope around `walk_subsurface` in `trace_path`; verified by `tests/profile.rs` passing and `--profile` on the fixture listing `Subsurface` with a call count equal to the walk count
- [x] 3.2 Cornellbox instruction count unchanged; verified by callgrind (2 663 960 177 → 2 663 964 522)
- [x] 3.3 Add the section to the list in `openspec/specs/cli/design.md`; verified by the record naming `Subsurface`

## 4. Records and integration

- [x] 4.1 `docs/subsurface_walk.md` with the cost breakdown, the experiments, the ranked recommendations and the references, linked from `docs/architecture.md` § Further reading; verified by the page existing and the link resolving
- [x] 4.2 `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings` on crust-core, and `cargo test -p crust-core` all clean with the pinned toolchain; verified by their exit codes
- [ ] 4.3 Re-record goldens for scenes with a `subsurface_bsdf` (`scripts/check_images.sh record`) once the branch merges; verified by `check_images.sh check` passing on the new goldens
