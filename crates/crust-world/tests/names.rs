//! The `named!` tables (`crust_base::names`) of the enums that carry one,
//! which live in the layers above the macro: every name round-trips, an alias
//! parses but prints as its name.

use crust_world::material::preview_surface::TexOutput;
use crust_world::names::UnknownName;
use crust_world::{LightSelection, PixelFilter, SamplingStrategy};

/// Every name parses to its value and prints back as itself.
fn round_trips<T>(table: &[(T, &str, &str)])
where
    T: std::str::FromStr<Err = UnknownName> + std::fmt::Display + PartialEq + Copy,
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
