# Tasks

## 1. One estimator (design D2)

- [ ] 1.1 Extract `PixelState::var_of_mean` into a free function
      `var_of_mean(sum, sq, n)` in `tracer/mod.rs` (or `utils`), and use it
      from `PixelState`.
      Verify: goldens (`scripts/check_images.sh check`) unchanged, and the
      `variance` AOV is bitwise equal to the parent commit's.

## 2. Slot and planes (design D1, D4)

- [ ] 2.1 Add `variance: bool` to `SlotKey`. Add
      `Option<VarPlanes { sum, sq, n }>` to `SlotPlanes`, allocated only for
      variance slots. Give a variance slot `ChannelKind::Scalar` with one
      component. Make the variance and value slots of one expression share
      its DFA bit.
- [ ] 2.2 Accumulate in `FilmPlanes::add`: for a variance slot,
      `x = luma(w·v)` (raw-divided first if raw), with `n` counting every
      sample. Gather and merge the planes like the existing ones (tile →
      frame).
- [ ] 2.3 Emit `var_of_mean` per pixel in `AovFilm::channels`.

## 3. Import and refusals (design D3)

- [ ] 3.1 Read `crust:aov:variance` in `usd_import/products.rs`. Refuse it
      with one warning per var on a non-`lpe` var and with `closest`
      accumulation. Expose the modifier on `AovVar` so engine code can build
      such a request.
      Verify with `.usda` unit tests for each refusal.

## 4. Tests

- [ ] 4.1 Add a bitwise test: `C.*[LO]` + modifier == the `variance` raw
      source (Cornell box, 16 spp).
- [ ] 4.2 Add a bitwise test: an expression's colour channels are
      unchanged by adding its variance var to the product.
- [ ] 4.3 Add a statistical test: the mean of the variance channel falls
      as 1/spp across 16 / 64 / 256 spp for `C<RD>.+[LO]` (ratio within a
      tolerance, not plateauing).
- [ ] 4.4 Check that renders without a variance var are bitwise unchanged
      (existing AOV goldens). Confirm the zero-AOV callgrind instruction
      count on the Cornell box at `-s 2` against the parent.

## 5. Documentation

- [ ] 5.1 Update `site/content/docs/usd/aovs.md`: the modifier, its
      estimator, the refusals, the memory cost, and the correlation caveat,
      with an example product holding a partition's values and variances.
      Update the `aovs` design record.
      Verify with `zola build` in `site/` (Zola 0.21).
- [ ] 5.2 Run the CI set locally: fmt, clippy `-D warnings`, `cargo test
      --workspace`.
