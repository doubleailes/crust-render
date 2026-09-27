//! One spelling for every named setting.
//!
//! A setting that a stage (`crust:samplingStrategy`), the CLI (`--strategy`)
//! and the probes all name used to be parsed by each of them — the importer
//! by an inline `match`, the CLI through mirror `clap` enums with a `From`
//! into the engine's, and the probes by hand again. Each engine enum now
//! carries its own table ([`named!`]), which gives it `FromStr` and `Display`
//! and hands the CLI the list it builds `--help` from, so a name exists in
//! exactly one place.

/// A name that is no value of the setting being parsed — `what` names the
/// setting, `expected` lists what would have been accepted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnknownName {
    pub what: &'static str,
    pub got: String,
    pub expected: &'static [&'static str],
}

impl std::fmt::Display for UnknownName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "unknown {} \"{}\" (expected {})",
            self.what,
            self.got,
            self.expected.join(" | ")
        )
    }
}

impl std::error::Error for UnknownName {}

/// Gives a `Copy + PartialEq` enum its name table: a `CHOICES` constant of
/// `(value, name, help)`, `FromStr` over the names (plus any `aliases`, which
/// parse but are never printed), and `Display` printing the name.
macro_rules! named {
    (
        $ty:ty, $what:literal,
        [$(($value:expr, $name:literal, $help:literal)),+ $(,)?]
        $(, aliases [$(($alias:literal, $avalue:expr)),* $(,)?])?
    ) => {
        impl $ty {
            /// Every value with its name and a one-line description, in the
            /// order `--help` lists them.
            pub const CHOICES: &'static [($ty, &'static str, &'static str)] =
                &[$(($value, $name, $help)),+];

            /// The names [`Self::CHOICES`] accepts, for messages.
            pub const NAMES: &'static [&'static str] = &[$($name),+];
        }

        impl std::str::FromStr for $ty {
            type Err = $crate::names::UnknownName;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                $($(if s == $alias {
                    return Ok($avalue);
                })*)?
                Self::CHOICES
                    .iter()
                    .find(|(_, name, _)| *name == s)
                    .map(|(value, _, _)| *value)
                    .ok_or_else(|| $crate::names::UnknownName {
                        what: $what,
                        got: s.to_string(),
                        expected: Self::NAMES,
                    })
            }
        }

        impl std::fmt::Display for $ty {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                let name = Self::CHOICES
                    .iter()
                    .find(|(value, _, _)| $crate::names::same_choice(value, self))
                    .map_or("?", |(_, name, _)| *name);
                f.write_str(name)
            }
        }
    };
}

pub(crate) use named;

/// Whether two values are the same choice: the same variant, whatever data
/// it carries (a pixel filter's radius is not part of its name).
pub(crate) fn same_choice<T>(a: &T, b: &T) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}

#[cfg(test)]
mod tests {
    use crate::material::preview_surface::TexOutput;
    use crate::{LightSelection, PixelFilter, SamplingStrategy};

    /// Every name parses to its value and prints back as itself.
    fn round_trips<T>(table: &[(T, &str, &str)])
    where
        T: std::str::FromStr<Err = super::UnknownName> + std::fmt::Display + PartialEq + Copy,
        T: std::fmt::Debug,
    {
        for &(value, name, _) in table {
            assert_eq!(name.parse::<T>(), Ok(value));
            assert_eq!(value.to_string(), name);
        }
        let err = "nope".parse::<T>().unwrap_err();
        assert!(err.to_string().contains("\"nope\""), "{err}");
    }

    #[test]
    fn every_name_round_trips() {
        round_trips(SamplingStrategy::CHOICES);
        round_trips(LightSelection::CHOICES);
        round_trips(PixelFilter::CHOICES);
        round_trips(TexOutput::CHOICES);
    }

    #[test]
    fn an_alias_parses_but_prints_as_its_name() {
        let mis: SamplingStrategy = "mis".parse().unwrap();
        assert_eq!(mis, SamplingStrategy::PowerMis);
        assert_eq!(mis.to_string(), "power");
    }

    #[test]
    fn a_filter_prints_its_name_whatever_its_radius() {
        let wide = PixelFilter::Gaussian { radius: 1.5 }.with_radius(4.0);
        assert_eq!(wide.to_string(), "gaussian");
        assert_eq!(wide.name(), "gaussian");
        assert_eq!(
            "lanczos".parse::<PixelFilter>().unwrap_err().to_string(),
            "unknown pixel filter \"lanczos\" (expected box | triangle | gaussian | blackman | mitchell)"
        );
    }
}
