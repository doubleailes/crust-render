//! Which OpenQMC sampler a render draws every number from.
//!
//! The integrator is generic over `openqmc::SamplerImpl` and materials take
//! numbers rather than a sampler (`BsdfDraw`), so the sequence is a run-time
//! choice made once per pass (`Renderer::render_pass`), not a compile-time
//! alias. `sobol` is the default and renders bit-identically to the renderer
//! before the choice existed; the others are what `crust:sampler` /
//! `--sampler` can switch to without a rebuild.
//!
//! No sampler wins everywhere (the `rendering` design record has the
//! measurements): Lattice's error is scene-dependent (−9% relMSE on
//! cornellbox, +20% on materialx_basic), and the blue-noise variants cost
//! 3–10% wall time for a small low-spp gain in the error's spectrum, which
//! is what a preview or a denoiser sees.

/// The QMC sampler a render draws from. Token spelling is camelCase
/// (`sobolBn`), as USD tokens name compound words.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SamplerKind {
    /// Owen-scrambled Sobol: the default, cache-free, and the sequence the
    /// renderer always drew from.
    #[default]
    Sobol,
    /// Sobol with a blue-noise error distribution across pixels.
    SobolBn,
    /// Rank-1 lattice.
    Lattice,
    /// Rank-1 lattice, blue-noise across pixels.
    LatticeBn,
    /// Progressive multi-jittered (0, 2) sequence, from a lazily built table.
    Pmj,
    /// PMJ, blue-noise across pixels.
    PmjBn,
}

crate::names::named!(
    SamplerKind,
    "sampler",
    [
        (
            SamplerKind::Sobol,
            "sobol",
            "Owen-scrambled Sobol (default; the renderer before the setting, bit for bit)"
        ),
        (
            SamplerKind::SobolBn,
            "sobolBn",
            "Sobol, blue-noise across pixels (low-spp previews and denoised output)"
        ),
        (
            SamplerKind::Lattice,
            "lattice",
            "Rank-1 lattice (scene-dependent)"
        ),
        (
            SamplerKind::LatticeBn,
            "latticeBn",
            "Rank-1 lattice, blue-noise across pixels"
        ),
        (
            SamplerKind::Pmj,
            "pmj",
            "Progressive multi-jittered (0, 2) sequence"
        ),
        (SamplerKind::PmjBn, "pmjBn", "PMJ, blue-noise across pixels"),
    ]
);

impl SamplerKind {
    /// The token the setting is spelled with (its `Display`).
    pub fn name(&self) -> &'static str {
        Self::CHOICES
            .iter()
            .find(|(value, _, _)| value == self)
            .map_or("?", |(_, name, _)| *name)
    }

    /// Builds any table the sampler draws from — the PMJ sequence, the
    /// blue-noise ranking tables — so that a pass never pays for it inside
    /// its timer. A no-op once built, and for the cache-free samplers.
    pub fn warm_cache(self) {
        match self {
            SamplerKind::Sobol => openqmc::SobolSampler::warm_cache(),
            SamplerKind::SobolBn => openqmc::SobolBnSampler::warm_cache(),
            SamplerKind::Lattice => openqmc::LatticeSampler::warm_cache(),
            SamplerKind::LatticeBn => openqmc::LatticeBnSampler::warm_cache(),
            SamplerKind::Pmj => openqmc::PmjSampler::warm_cache(),
            SamplerKind::PmjBn => openqmc::PmjBnSampler::warm_cache(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_token_round_trips() {
        assert_eq!(SamplerKind::CHOICES.len(), 6);
        for &(value, name, _) in SamplerKind::CHOICES {
            assert_eq!(name.parse::<SamplerKind>(), Ok(value), "{name}");
            assert_eq!(value.name(), name);
            assert_eq!(value.to_string(), name);
        }
        assert_eq!(SamplerKind::default(), SamplerKind::Sobol);
        assert_eq!(SamplerKind::default().name(), "sobol");
    }

    #[test]
    fn an_unknown_token_is_an_error_naming_the_six() {
        let err = "halton".parse::<SamplerKind>().unwrap_err();
        assert_eq!(err.what, "sampler");
        assert_eq!(err.got, "halton");
        assert_eq!(
            err.expected,
            ["sobol", "sobolBn", "lattice", "latticeBn", "pmj", "pmjBn"]
        );
        // Case matters: the tokens are camelCase, as USD spells them.
        assert!("SobolBN".parse::<SamplerKind>().is_err());
    }

    #[test]
    fn warming_every_cache_is_harmless() {
        for &(value, _, _) in SamplerKind::CHOICES {
            value.warm_cache();
            value.warm_cache();
        }
    }
}
