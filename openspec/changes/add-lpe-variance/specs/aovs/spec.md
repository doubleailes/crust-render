## ADDED Requirements

### Requirement: Variance modifier on light path expressions

A RenderVar with `sourceType = "lpe"` that authors
`bool crust:aov:variance = true` SHALL write one scalar channel: the
per-pixel variance of the expression's luminance mean. It SHALL use the
same estimator, luminance and sample count as the `variance` raw source,
with each sample's contribution to the expression in place of the beauty.
Samples that contribute nothing to the expression SHALL count as zero
samples.

Combinations and refusals:

- With `crust:aov:raw = true`, the channel SHALL be the variance of the raw
  value.
- A var with the modifier and `closest` accumulation SHALL be refused with
  one warning naming the var.
- The modifier on a var whose `sourceType` is not `lpe` SHALL be refused
  with one warning naming the var.
- A refused var SHALL write no channel.

The same expression MAY be requested once with the modifier and once
without, in one product. The value channel SHALL be bitwise identical to the
channel the expression writes without a variance var in the product.

Variances of expressions that partition the paths SHALL NOT be presented
as adding up to the beauty's variance; the documentation SHALL state that
they are correlated.

Requesting no variance var SHALL leave every other channel, and the
zero-AOV render, unchanged.

#### Scenario: The full path's variance equals the beauty's

- **WHEN** a product requests `C.*[LO]` with `crust:aov:variance = true`
  and the raw source `variance`
- **THEN** the two channels are bitwise equal

#### Scenario: Value and variance of one expression

- **WHEN** a product requests `C<RD>.+[LO]` twice, once with the modifier
- **THEN** it writes the colour channels and a scalar variance channel, and
  the colour channels are bitwise equal to a product requesting
  `C<RD>.+[LO]` alone

#### Scenario: Variance falls with samples

- **WHEN** `C<RD>.+[LO]` with the modifier is rendered at 16, 64 and 256 spp
- **THEN** the mean of the channel falls in proportion to 1/spp, within
  statistical tolerance

#### Scenario: Closest accumulation is refused

- **WHEN** a var authors the modifier and
  `driver:parameters:aov:multiSampled = false`
- **THEN** one warning names the var, and the product has no channel for it

#### Scenario: No variance var, nothing changes

- **WHEN** a scene with LPE vars and no variance modifier is rendered
- **THEN** every channel is bitwise equal to the output before this change
