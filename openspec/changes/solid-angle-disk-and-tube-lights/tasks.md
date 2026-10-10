## 1. Groundwork: the switches and a refusing sampler (D6)

- [ ] 1.1 Add `Config::tube_sampling` (`CRUST_TUBE_SAMPLING`: `area` | `arc` |
      `equiangular`) and `Config::disk_sampling` (`CRUST_DISK_SAMPLING`: `area` |
      `ellipse`) to `config.rs`, both defaulting to `area` until task 5 decides. Add
      one warning per bad value. Done when the config tests parse every value and
      warn on a bad one.
- [ ] 1.2 Make `SolidAngleSampler::sample` return `Option<(Vec3A, PdfSolidAngle)>`
      and `pdf` return `Option<PdfSolidAngle>`. Wrap Cone, AffineCone and Rect in
      `Some`. In `AreaLight`, a sampler that exists from `from` is final on both MIS
      sides: `sample_li` returns `None` on a refused sample, and `solid_angle_pdf`
      returns the sampler's `None` instead of falling through to the area density.
      Done when the existing light tests pass unchanged.
- [ ] 1.3 Callgrind cornellbox and `veach_mis` at 2 spp, before and after 1.2
      (`RAYON_NUM_THREADS=1`). Done when `render_pixel`, `sample_li` and
      `Bvh::hit` have the same instruction counts, or the hot path is restructured
      until they do. Record the numbers in `notes.md`.
- [ ] 1.4 `scripts/check_images.sh record` on the pre-change binary, then `check`.
      Done when every sample is `identical`.

## 2. Disk: the spherical ellipse (D5)

- [ ] 2.1 Read Guillén et al. 2017 and transcribe into `design.md` D5:
      - the cone frame;
      - `α` and `β`;
      - the solid angle;
      - both maps' CDFs and inversions;
      - the paper's cost and stratification figures.
      Pick the map and record why. Settles Open Question 1.
- [ ] 2.2 `light/ellipse.rs`: Carlson's `R_F` and `R_J` in f64. Add a script
      generating reference values offline (mpmath or Boost, in the style of
      `scripts/osl_oracle.py`) and a test pinning them to a relative 1e-12 across
      the parameter range the map uses, including `α ≈ β` and `β → 0`.
- [ ] 2.3 `SphericalEllipse::new(f)` for the unit disk seen from a local point. It
      returns `None` unless `f.z < 0` and the solid angle is in `[1e-4, 6.22]` sr.
      Done when its solid angle matches a brute-force integral to 1e-4 relative,
      over a grid of positions including near edge-on and on axis.
- [ ] 2.4 Sampling and density. Done when:
      - a histogram test (sample counts against the pdf over a grid of solid-angle
        bins) passes for a centred, an off-axis and a near-edge-on view;
      - every sample lies on the unit disk.
- [ ] 2.5 `Strategy::Ellipse` in `shape.rs`: sample in local space, return the point
      through `light_to_world`, and map the density with `world_solid_angle_pdf`.
      Gate it on `Config::disk_sampling`. Done when a sheared, non-uniformly scaled
      disk passes the same histogram test in world space.

## 3. Tube: visible arc and equiangular axis (D1, D2, D4)

- [ ] 3.1 `Emissive::is_one_sided()`. Add `AffineShape::front_only`, set by
      `AreaLight::new`. The cylinder's `solid_angle_sampler` returns `None` unless
      the flag is set, `ρ² > 1`, and `Config::tube_sampling` is not `area`.
- [ ] 3.2 `Strategy::TubeArc`: `φ` over the visible arc, `x` uniform (`arc`) or
      equiangular along the world wall line (`equiangular`). Compute the density as
      in D2. `pdf(p)` recomputes it from `world_to_light(p)` and refuses a point
      outside the arc or where `cos θ_l = 0`. Done when:
      - every sample from 1 000 outside points faces its point;
      - `pdf(sample(u, v).0) == sample(u, v).1` bit for bit;
      - a histogram test passes for a thin tube (`(1, 0.01, 0.01)`), a thick tube
        up close, and a sheared elliptical tube.
- [ ] 3.3 A two-sided tube seen through its open end takes area sampling. Done when
      its light-only and BSDF-only estimates agree within noise.

## 4. Integration tests (spec scenarios)

- [ ] 4.1 The "Disk and cylinder sampling is unbiased" scenario, in
      `crust-core/tests/`, under all switch values: light-only, BSDF-only and power
      MIS agree within noise, and the difference falls as 1/√N between 64 and
      1024 spp.
- [ ] 4.2 "A disk seen from behind", "Area sampling restored" (a bitwise test
      against a render with both switches at `area`) and "Scenes without round
      lights are unchanged" (task 1.4's goldens).

## 5. Measure and decide defaults (D7)

- [ ] 5.1 Build the scratch sweep in D7, uncommitted, under the scratchpad, and
      1024 spp references per scene.
- [ ] 5.2 Measure each switch value at equal time:
      - relMSE at 16 spp with `--indirect-clamp 0` (`crust diff`), and seconds per
        sample from `scripts/bench_ab.sh`;
      - on the sweep, `samples/usdlux.usda`, the DPEL MaterialX samples with round
        lights, and ALab frame 1004 if present.
      Record min and mean in `notes.md`.
- [ ] 5.3 Choose each shape's default, and the disk's lower band bound, by the D7
      rule. If the disk's strategy loses on small disks, raise the bound. If it
      loses generally, try D5's circumscribed-square alternative before giving up.
      Answer Open Questions 2 and 3.
- [ ] 5.4 Re-record the goldens that move with the new defaults. Check each moved
      sample against its reference: the relMSE falls, and there is no plateau
      across spp.

## 6. Documentation

- [ ] 6.1 `openspec/specs/lighting/design.md`:
      - a section on the disk and tube strategies, with the numbers from task 5;
      - "Known gaps: light sampling" updated, keeping the cosine-blind tube density
        and thick-tube Gamito pointer as gaps.
- [ ] 6.2 `docs/light_sampling.md`:
      - §3.2's `DiskLight, CylinderLight` row;
      - §5.2's "Disks and cylinders" paragraph;
      - the line "Disks and tubes remain area-sampled";
      - the comparison table's crust row, if it lists the strategies.
- [ ] 6.3 `docs/architecture.md` § Environment switches: two rows. Add both switches
      to `site/content/docs/reference/environment-variables.md`. Run `zola build` in
      `site/` (Zola 0.21) and make sure it passes.
- [ ] 6.4 CI set: `cargo fmt --all -- --check`,
      `cargo clippy --workspace --all-targets -- -D warnings`,
      `cargo test --workspace`.
