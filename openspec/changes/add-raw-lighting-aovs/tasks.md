## 1. Diffuse filter (`material/`)

- [x] 1.1 Add `ShadingPoint::diffuse_filter()`: OpenPBR
      `ρ · (1 − F̄) · base_atten · dark` (D3); a closure's `Σ color × weight`
      over its `Diffuse` leaves; 0 for a material queried directly.
- [x] 1.2 Tests: an OpenPBR diffuse material's filter is its base colour
      times `(1 − F̄)`; a coat dims it by its darkening and not by its
      directional passage; two half-weight diffuse leaves sum to one; a
      translucent or subsurface leaf adds nothing; clear glass is 0.

## 2. Expressions that start with a diffuse reflection (`lpe/`)

- [x] 2.1 Add `Lpe::starts_with_diffuse_reflection(i)` (D5): from the state
      after `C`, every symbol that is not `R` + `D` leads to a state where
      expression `i` is not live.
- [x] 2.2 Tests: accepted `C<RD>[LO]`, `C<RD>.*<L.'key'>`,
      `C<RD'diffuse'>.*L`, `C(<RD>|<RD'diffuse'>)L`; refused `C.*[LO]`,
      `C<RG>L`, `C[LO]`, `C<.D>L` (it also accepts `TD`), `C'diffuse'.*L`
      (a bare label matches any event type).

## 3. Sources and import (`aov.rs`, `usd_import/products.rs`)

- [x] 3.1 Add the `raw` flag to `AovVar` and to the film's slot key for LPE
      slots (D6); `rawLight` / `rawGI` / `rawTotalLight` and their V-Ray
      aliases resolve to their fixed expressions with `raw` set.
- [x] 3.2 Add `AovSource::DiffuseFilter` (`diffuse_albedo`, `DiffuseFilter`,
      `diffuseFilter`); take `diffuse_albedo` off the `albedo` aliases.
- [x] 3.3 Read `bool crust:aov:raw` on `lpe` vars; refuse a raw expression
      that fails 2.1 with one `WARN`; refuse a non-colour type for a raw var.
- [x] 3.4 Tests: each name and alias; the refusal; `crust:aov:raw` on a
      `raw`-type source is ignored with a warning.

## 4. Film and integrator (`tracer/`, `aov.rs`)

- [x] 4.1 Record the vertex-0 diffuse filter in the AOV instantiation when a
      raw or `diffuse_albedo` AOV is requested (D4), beside `FirstHit`;
      pass it to the film in `SampleExtras`.
- [x] 4.2 At accumulation, divide a raw slot's routed value by the sample's
      filter per channel, 0 below 1e-4 (D6). Accumulate `diffuse_albedo`
      filtered, 0 where the camera ray escapes.
- [x] 4.3 Tests:
      - `rawLight × diffuse_albedo == C<RD>[LO]` per pixel to rounding on an
        untextured diffuse surface, also with the indirect clamp;
      - per sample (a 1-spp box-filter render), the identity on a textured
        surface;
      - `rawLight` shows no texture where `C<RD>[LO]` does;
      - raw channels are 0 where the camera sees a mirror, glass or the sky;
      - the beauty and every other AOV are unchanged by adding raw AOVs.
- [x] 4.4 Zero-request gate: callgrind instruction count on cornellbox at
      `-s 2` unchanged; `check_images.sh check` passes.

## 5. Docs and sample

- [x] 5.1 `site/content/docs/usd/aovs.md`: the raw AOVs, `diffuse_albedo`,
      `crust:aov:raw`, the per-sample identity and its per-pixel caveat, the
      1e-4 floor. Note the change of `diffuse_albedo`.
- [x] 5.2 Add `rawLight`, `rawGI` and `diffuse_albedo` to
      `samples/aovs_lpe.usda`.
- [x] 5.3 Record the design and its trade-offs in the `aovs` design record;
      `zola build` passes.
