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
///
/// Exported (and hidden) because the enums it names live in the crates
/// above; reach it as `crate::names::named!`.
#[doc(hidden)]
#[macro_export]
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

#[doc(hidden)]
pub use crate::named;

/// Whether two values are the same choice: the same variant, whatever data
/// it carries (a pixel filter's radius is not part of its name).
pub fn same_choice<T>(a: &T, b: &T) -> bool {
    std::mem::discriminant(a) == std::mem::discriminant(b)
}
