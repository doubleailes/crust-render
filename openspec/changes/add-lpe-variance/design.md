## Context

See `proposal.md` for the motivation. This section describes the code the
design builds on.

**How the beauty's variance is estimated** (`tracer/mod.rs`):

- Per sample, the FIS-weighted colour `color · (wx·wy)` is reduced to
  luminance with the working space's luma (`self.lights.luma()`).
- `PixelState` accumulates `lum_sum` and `lum_sq`, in f64.
- `var_of_mean()` is the unbiased variance of the mean:
  `(Σx² − (Σx)²/n) / (n−1) / n`, clamped at 0.
- This is what adaptive sampling stops on and what the `variance` AOV
  writes.
- For box and triangle filters every weight is 1, so it is exactly the
  variance of the pixel estimate. For Gaussian/Mitchell it is the same
  approximation the beauty already uses.

**How an LPE slot accumulates** (`aov.rs`):

- `trace_path::<AOV = true>` routes each sample's contribution per DFA
  bit into `scratch.route.out`.
- `FilmPlanes::add(first, extras, fx, fy, w)` folds every slot.
- A filtered slot keeps `values += w · v` per component, and the final
  channel is `values / weight`.
- `SlotKey` decides sharing: two vars with the same source, expression,
  mode and clear value share a slot.
- The raw modifier (`crust:aov:raw`) is a `SlotKey` field that divides by
  the diffuse filter before accumulation, and shares the expression's DFA
  bit.

## Goals / Non-Goals

**Goals:**

- A per-pixel, per-expression variance whose estimator is the beauty's,
  so the two are directly comparable (`C.*[LO]`'s variance equals the
  beauty's `variance`).
- Zero cost when not requested.
- Usable both from USD and from an engine-built `AovRequest`.

**Non-Goals:**

- Covariances, or any "share of total variance" decomposition.
- Colour (per-channel) variance; luminance only, like the beauty's.
- Variance for non-LPE data sources (depth, normal, …).

## Decisions

### D1. A `variance` modifier on the slot, not a new source

- `SlotKey { …, variance: bool }`.
- A variance var and a value var of the same expression are two slots that
  share the DFA bit, exactly as raw and plain do.
- Channel kind: scalar, one component.

*Alternatives considered:*

- A raw source `lpevariance:<expr>`. Rejected: it duplicates the LPE
  sourceType's parsing and refusal rules.
- An automatic companion channel `<name>.variance` on every LPE var.
  Rejected: it costs on every LPE render, and it changes existing products'
  channel lists.

### D2. Same estimator as the beauty, on the slot's own sample values

Per sample, for a variance slot:

- `x = luma(w · v)`, where `v` is the expression's (raw-divided, if raw)
  colour and `w` the FIS weight;
- `lum_sum += x`, `lum_sq += x²` (f64);
- `n` counts **every** sample the pixel took, including those that
  contributed zero. A zero is a sample of the estimator; skipping it would
  understate variance for rare paths, exactly the paths that matter.

The channel is `var_of_mean` over `n`. `PixelState::var_of_mean` and the
slot's computation call one shared function, so `C.*[LO]` with the modifier
is bitwise equal to the `variance` AOV. A test pins this.

### D3. Variance requires filtered accumulation

- With `closest` accumulation there is one value per pixel, so there is no
  sample variance.
- An authored `closest` with the modifier is refused with one warning
  naming the var; it writes no channel.
- The modifier on a non-`lpe` var is refused with one warning.

### D4. Planes only for variance slots

- `SlotPlanes` gains `Option<VarPlanes { sum: Vec<f64>, sq: Vec<f64>, n: Vec<u32> }>`,
  allocated only when `SlotKey::variance`.
- `FilmPlanes::add` handles a variance slot in its own arm.
- The zero-AOV path (`AOV = false`) is untouched, and so is its
  instruction count.
- Renders with AOVs but no variance var run the existing arms, so every
  existing channel stays bitwise identical.

### D5. The correlation caveat is part of the contract, not a footnote

For a partition `C<RD>[LO]`, `C<RD>.+[LO]`, … the component estimators of
one sample are correlated (one path feeds several, and zeros are shared).
So `Σ Var(componentᵢ) ≠ Var(beauty)` in general.

The user documentation says so. The diagnostic reports each component's
variance on its own: as relative error against the component's own mean,
and against the beauty's mean. It never reports them as percentages of the
total.

## Risks / Trade-offs

- **[f64 planes are memory]** 20 bytes per pixel per variance slot: about
  166 MB per slot at 4K. That is acceptable for an opt-in diagnostic
  channel. The diagnostic requests them on crops or low-resolution full
  frames only.
- **[Mitchell's negative weights]** `luma(w·v)` can be negative, so the
  estimator behaves as the beauty's does, an approximation. This is
  documented, not fixed: matching the beauty is the requirement.
- **[A test that pins two estimators equal can be satisfied by sharing
  them]** It can, and that is the intent of D2's shared function.

## Migration Plan

None. The attribute is new, and absent means today's behaviour.

## Open Questions

None.
