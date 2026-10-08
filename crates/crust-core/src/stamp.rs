//! The sampling stamp: what every EXR `crust render` writes records about
//! how its pixels were sampled and what they were rendered from (the
//! `image-output` spec's "EXRs record how they were sampled" and "... what
//! they were rendered from").
//!
//! Built once per render, after tracing, from the settings the render ran
//! with, its ray counters, and what the import recorded (the camera prim and
//! the time code). The host writes [`SamplingStamp::attributes`] into each
//! EXR header; `crust diff` reads them back ([`crate::compare`]) to say
//! whether two images can be compared pixel for pixel.
//!
//! Names are `crust:` plus the USD render-setting attribute that sets the
//! value (`crust:indirectClamp`), so one vocabulary runs through USD, the CLI
//! and the EXR.

use crate::stats::RayStats;
use crate::tracer::RenderSettings;

/// The prefix every stamped attribute carries, and that a RenderProduct may
/// not author.
pub const STAMP_PREFIX: &str = "crust:";

/// One attribute's value, typed as the EXR header stores it.
#[derive(Debug, Clone, PartialEq)]
pub enum StampValue {
    Int(i32),
    /// Two ints (`v2i`): `crust:sppTaken`'s fewest and most.
    Int2(i32, i32),
    Float(f32),
    Double(f64),
    Text(String),
}

impl std::fmt::Display for StampValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StampValue::Int(v) => write!(f, "{v}"),
            StampValue::Int2(a, b) => write!(f, "({a}, {b})"),
            StampValue::Float(v) => write!(f, "{v}"),
            StampValue::Double(v) => write!(f, "{v}"),
            StampValue::Text(v) => f.write_str(v),
        }
    }
}

/// How a render was sampled, and what it was rendered from.
#[derive(Debug, Clone, PartialEq)]
pub struct SamplingStamp {
    pub spp: u32,
    pub min_spp: u32,
    /// The fewest and most samples any pixel took.
    pub spp_taken: (u32, u32),
    pub variance_threshold: f32,
    /// `0` when the clamp is off.
    pub indirect_clamp: f32,
    pub max_depth: u32,
    pub light_samples: u32,
    pub light_samples_indirect: u32,
    pub sampling_strategy: String,
    pub light_selection: String,
    pub pixel_filter: String,
    pub pixel_filter_radius: f32,
    /// The time code the stage was evaluated at; `None` without one.
    pub frame: Option<f64>,
    /// The camera prim rendered through; `None` for the procedural camera.
    pub camera: Option<String>,
    pub version: String,
}

impl SamplingStamp {
    /// The stamp of a render that ran with `settings` and counted `rays`,
    /// through `camera_path` at `time` (see [`crate::Scene::camera_path`]
    /// and [`crate::Scene::time`]).
    ///
    /// `crust:sppTaken` comes from the adaptive counters, which only
    /// adaptive passes fill; without one, every pixel took `spp`.
    pub fn new(
        settings: &RenderSettings,
        rays: &RayStats,
        camera_path: Option<&str>,
        time: Option<f64>,
    ) -> Self {
        let spp = settings.samples_per_pixel();
        let filter = settings.pixel_filter();
        SamplingStamp {
            spp,
            min_spp: settings.min_samples_per_pixel(),
            spp_taken: if rays.adaptive_pixels > 0 {
                (rays.spp_min, rays.spp_max)
            } else {
                (spp, spp)
            },
            variance_threshold: settings.variance_threshold(),
            indirect_clamp: settings.indirect_clamp().unwrap_or(0.0),
            max_depth: settings.max_depth(),
            light_samples: settings.light_samples(),
            light_samples_indirect: settings.light_samples_indirect(),
            sampling_strategy: settings.sampling_strategy().to_string(),
            light_selection: settings.light_selection().to_string(),
            pixel_filter: filter.name().to_owned(),
            pixel_filter_radius: filter.radius(),
            frame: time,
            camera: camera_path.map(str::to_owned),
            version: crate::report::CRUST_VERSION.to_owned(),
        }
    }

    /// The header attributes, by full name, in a fixed order. `crust:frame`
    /// and `crust:camera` are left out when there is no time code or no
    /// camera prim.
    pub fn attributes(&self) -> Vec<(&'static str, StampValue)> {
        use StampValue::*;
        // Counts and ints fit an EXR `int`: every one is clamped far below
        // `i32::MAX` by the settings (`MAX_LIGHT_SAMPLES`, the depth cap).
        let int = |v: u32| Int(i32::try_from(v).unwrap_or(i32::MAX));
        let mut out = vec![
            ("crust:spp", int(self.spp)),
            ("crust:minSpp", int(self.min_spp)),
            (
                "crust:sppTaken",
                Int2(
                    i32::try_from(self.spp_taken.0).unwrap_or(i32::MAX),
                    i32::try_from(self.spp_taken.1).unwrap_or(i32::MAX),
                ),
            ),
            ("crust:varianceThreshold", Float(self.variance_threshold)),
            ("crust:indirectClamp", Float(self.indirect_clamp)),
            ("crust:maxDepth", int(self.max_depth)),
            ("crust:lightSamples", int(self.light_samples)),
            (
                "crust:lightSamplesIndirect",
                int(self.light_samples_indirect),
            ),
            (
                "crust:samplingStrategy",
                Text(self.sampling_strategy.clone()),
            ),
            ("crust:lightSelection", Text(self.light_selection.clone())),
            ("crust:pixelFilter", Text(self.pixel_filter.clone())),
            ("crust:pixelFilterRadius", Float(self.pixel_filter_radius)),
        ];
        if let Some(t) = self.frame {
            out.push(("crust:frame", Double(t)));
        }
        if let Some(c) = &self.camera {
            out.push(("crust:camera", Text(c.clone())));
        }
        out.push(("crust:version", Text(self.version.clone())));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PixelFilter;

    fn settings() -> RenderSettings {
        crate::get_settings().1.with_samples_per_pixel(16)
    }

    fn get<'a>(attrs: &'a [(&str, StampValue)], name: &str) -> Option<&'a StampValue> {
        attrs.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }

    #[test]
    fn a_fixed_budget_took_its_budget_everywhere() {
        let s = SamplingStamp::new(&settings(), &RayStats::default(), None, None);
        assert_eq!(s.spp_taken, (16, 16));
        let attrs = s.attributes();
        assert_eq!(get(&attrs, "crust:spp"), Some(&StampValue::Int(16)));
        assert_eq!(
            get(&attrs, "crust:sppTaken"),
            Some(&StampValue::Int2(16, 16))
        );
        // Adaptive passes report their own spread.
        let rays = RayStats {
            adaptive_pixels: 4,
            spp_min: 32,
            spp_max: 256,
            ..RayStats::default()
        };
        let s = SamplingStamp::new(&settings(), &rays, None, None);
        assert_eq!(s.spp_taken, (32, 256));
    }

    #[test]
    fn the_clamp_off_is_zero() {
        let off = settings().with_indirect_clamp(0.0);
        let s = SamplingStamp::new(&off, &RayStats::default(), None, None);
        assert_eq!(
            get(&s.attributes(), "crust:indirectClamp"),
            Some(&StampValue::Float(0.0))
        );
        let on = settings().with_indirect_clamp(10.0);
        let s = SamplingStamp::new(&on, &RayStats::default(), None, None);
        assert_eq!(s.indirect_clamp, 10.0);
    }

    #[test]
    fn the_filter_is_named_with_its_radius() {
        let g = settings().with_pixel_filter(PixelFilter::Gaussian { radius: 1.5 });
        let attrs = SamplingStamp::new(&g, &RayStats::default(), None, None).attributes();
        assert_eq!(
            get(&attrs, "crust:pixelFilter"),
            Some(&StampValue::Text("gaussian".into()))
        );
        assert_eq!(
            get(&attrs, "crust:pixelFilterRadius"),
            Some(&StampValue::Float(1.5))
        );
    }

    #[test]
    fn frame_and_camera_are_omitted_when_absent() {
        let attrs = SamplingStamp::new(&settings(), &RayStats::default(), None, None).attributes();
        assert!(get(&attrs, "crust:frame").is_none());
        assert!(get(&attrs, "crust:camera").is_none());
        assert!(get(&attrs, "crust:version").is_some());
        let attrs = SamplingStamp::new(&settings(), &RayStats::default(), Some("/cam"), Some(10.5))
            .attributes();
        assert_eq!(get(&attrs, "crust:frame"), Some(&StampValue::Double(10.5)));
        assert_eq!(
            get(&attrs, "crust:camera"),
            Some(&StampValue::Text("/cam".into()))
        );
        // Every name carries the prefix and appears once.
        let mut names: Vec<&str> = attrs.iter().map(|(n, _)| *n).collect();
        assert!(names.iter().all(|n| n.starts_with(STAMP_PREFIX)));
        let n = names.len();
        names.sort();
        names.dedup();
        assert_eq!(names.len(), n);
    }
}
