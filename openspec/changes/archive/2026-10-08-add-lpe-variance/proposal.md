## Why

Crust measures noise for one thing only: the beauty. The `variance` AOV is
the per-pixel variance of the beauty's luminance mean, kept by the adaptive
sampler as it goes (`PixelState::lum_sq`, `var_of_mean`). Light path
expression AOVs accumulate a mean and nothing else. So crust can say *where
in the image* the noise is, but not *which light transport* makes it:
direct versus indirect diffuse, glossy, caustics, volumes, or one light group
versus another.

That second question decides which setting helps. Examples:

- light selection and light-sample counts help noisy direct light;
- path guiding helps indirect and caustic paths;
- neither helps noise from a material that emits but is not a light.

`add-diagnostic-command` ranks settings by efficiency. It needs this
breakdown to know which trials to run first and to explain to its reader
why a setting helps. A compositor or look-dev artist asking "why is my key
light's pass so noisy" wants the same channel.

## What Changes

- **`bool crust:aov:variance = true` on an `lpe` RenderVar**:
  - the var's channel becomes the per-pixel variance of the expression's
    luminance mean, a scalar, instead of its colour;
  - it uses the same estimator as the beauty's `variance` (the unbiased
    sample variance of the mean, over the samples the pixel took);
  - the luminance is the working space's.
- **Combining with other modifiers:**
  - with `crust:aov:raw = true`, it is the variance of the raw value;
  - with `closest` accumulation, it is refused with a warning, because a
    variance needs every sample.
- **An expression can be requested twice**, once for its value and once for
  its variance. The two vars share the expression's DFA bit. The variance
  var adds one second-moment plane.
- **The same capability is available in code**: an `AovRequest` built by
  the engine (for the diagnostic) can ask for an expression's variance
  without authoring USD.
- **Documented caveat:** variances of a partition of the paths do **not**
  add up to the beauty's variance, because the components of one sample are
  correlated. They are reported as each component's own noise, not as
  shares of the total.
- **Unchanged:**
  - the zero-AOV render, pinned by instruction count;
  - `C.*[LO]`, still bitwise equal to the beauty;
  - every existing LPE channel, bitwise.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `aovs`: a variance modifier on light path expressions.

## Impact

- `crust-core/src/aov.rs`:
  - `SlotKey` gains a `variance` flag;
  - `SlotPlanes` gains per-pixel `lum_sum` / `lum_sq` / `n` planes, allocated
    only for variance slots;
  - `AovFilm::channels` emits `var_of_mean`.
- `crust-core/src/scene/usd_import/products.rs` reads the attribute.
- Cost appears only when requested: one luminance, two adds and a square per
  sample per variance slot. Renders without a variance var run the code they
  run today.
- Docs:
  - `site/content/docs/usd/aovs.md` (the modifier, with the correlation
    caveat);
  - the `aovs` design record.
