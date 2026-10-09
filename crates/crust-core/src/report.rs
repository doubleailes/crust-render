//! The conventions every machine-readable report shares (the `cli` spec's
//! "Machine-readable reports share one shape"): one JSON object opening with
//! `format` and `crust_version`, snake_case keys with the unit in the name,
//! and `null` for a value that is not finite or not available.
//!
//! [`Report`] is the envelope; the `serialize_with` helpers below map the
//! two values serde would otherwise get wrong — a non-finite float, which
//! serde_json refuses, and a `Duration`, which it would write as a
//! `{secs, nanos}` object — onto the rule.

use serde::{Serialize, Serializer};
use std::time::Duration;

/// The crate version every report carries as `crust_version`.
pub const CRUST_VERSION: &str = env!("CARGO_PKG_VERSION");

/// A report: `format` first, `crust_version` second, then the body's own
/// keys. serde writes a struct's fields in declaration order, so the
/// envelope leads without `preserve_order`.
#[derive(Debug, Clone, Serialize)]
pub struct Report<T> {
    pub format: &'static str,
    pub crust_version: &'static str,
    #[serde(flatten)]
    pub body: T,
}

impl<T: Serialize> Report<T> {
    /// `body` under `format` (`crust-stats/1`, `crust-ls/1`, ...).
    pub fn new(format: &'static str, body: T) -> Self {
        Report {
            format,
            crust_version: CRUST_VERSION,
            body,
        }
    }

    /// The report as pretty-printed JSON, with a final newline.
    pub fn to_json(&self) -> String {
        let mut s = serde_json::to_string_pretty(self).expect("a report always serializes");
        s.push('\n');
        s
    }
}

/// A float as itself when finite, else `null` (JSON has no NaN or infinity).
/// An `f32` is written at `f32` precision (`12.7`, not `12.700016975402832`).
pub fn finite_or_null<S: Serializer, F: Float>(x: &F, s: S) -> Result<S::Ok, S::Error> {
    if x.finite() {
        x.write(s)
    } else {
        s.serialize_none()
    }
}

/// The two float widths [`finite_or_null`] takes.
pub trait Float: Copy {
    fn finite(self) -> bool;
    fn write<S: Serializer>(self, s: S) -> Result<S::Ok, S::Error>;
}

impl Float for f32 {
    fn finite(self) -> bool {
        self.is_finite()
    }
    fn write<S: Serializer>(self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f32(self)
    }
}

impl Float for f64 {
    fn finite(self) -> bool {
        self.is_finite()
    }
    fn write<S: Serializer>(self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_f64(self)
    }
}

/// A `Duration` as its seconds, for a `*_s` key.
pub fn seconds<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_f64(d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Serialize)]
    struct Body {
        #[serde(serialize_with = "finite_or_null")]
        ratio: f64,
        #[serde(serialize_with = "finite_or_null")]
        single: f32,
        #[serde(serialize_with = "seconds")]
        time_s: Duration,
    }

    fn body(ratio: f64) -> Report<Body> {
        Report::new(
            "crust-test/1",
            Body {
                ratio,
                single: f32::NAN,
                time_s: Duration::from_millis(1500),
            },
        )
    }

    #[test]
    fn the_envelope_leads_in_order() {
        let json = body(0.5).to_json();
        let keys: Vec<&str> = json
            .lines()
            .filter_map(|l| l.trim().strip_prefix('"')?.split('"').next())
            .collect();
        assert_eq!(
            keys,
            ["format", "crust_version", "ratio", "single", "time_s"]
        );
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["format"], "crust-test/1");
        assert_eq!(v["crust_version"], CRUST_VERSION);
    }

    #[test]
    fn non_finite_floats_are_null_and_durations_are_seconds() {
        for x in [f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            let v: serde_json::Value = serde_json::from_str(&body(x).to_json()).unwrap();
            assert!(v["ratio"].is_null(), "{x}");
            assert!(v["single"].is_null());
            assert_eq!(v["time_s"], 1.5);
        }
        let v: serde_json::Value = serde_json::from_str(&body(0.25).to_json()).unwrap();
        assert_eq!(v["ratio"], 0.25);
        // At its own precision: the f32 nearest 12.7, not its f64 widening.
        let json = Report::new(
            "crust-test/1",
            Body {
                ratio: 0.0,
                single: 12.7,
                time_s: Duration::ZERO,
            },
        )
        .to_json();
        assert!(json.contains("\"single\": 12.7,"), "{json}");
    }
}
