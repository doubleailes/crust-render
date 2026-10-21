//! The sampling stamp: what every EXR `crust render` writes records about
//! how its pixels were sampled and what they were rendered from (the
//! `image-output` spec's "EXRs record how they were sampled", "... what
//! they were rendered from" and "... the lens and the layer stack").
//!
//! Built once per render, after tracing, from the settings the render ran
//! with, its ray counters, and what the import recorded (the camera prim, the
//! time code, and the [`Source`]: the lens, the blurs and the layer stack).
//! The host writes [`SamplingStamp::attributes`] into each EXR header;
//! `crust diff` reads them back ([`crate::compare`]) to say whether two
//! images can be compared pixel for pixel.
//!
//! Names are `crust:` plus the USD render-setting attribute that sets the
//! value (`crust:indirectClamp`), so one vocabulary runs through USD, the CLI
//! and the EXR.

pub use crate::scene::{Lens, Source};
use crate::stats::RayStats;
use crate::tracer::{RenderOutcome, RenderSettings};
use std::path::Path;

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

/// One layer of the stamped layer stack: its file, and a hash of its bytes
/// ([`content_hash`]) when the host could read them.
#[derive(Debug, Clone, PartialEq)]
pub struct StampedLayer {
    pub path: String,
    pub hash: Option<u64>,
}

/// FNV-1a over `bytes`: a hash with no seed, the same across runs, builds
/// and platforms, so two files stamped years apart compare. Not a
/// cryptographic hash: it tells an edited layer from the one rendered, not a
/// forged one.
pub fn content_hash(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// The `crust:layerStack` text: one layer per line, strongest first, its
/// hash in hex (`-` when its bytes could not be read), a space, its path.
pub fn layer_stack_text(layers: &[StampedLayer]) -> String {
    layers
        .iter()
        .map(|l| match l.hash {
            Some(h) => format!("{h:016x} {}", l.path),
            None => format!("- {}", l.path),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The layers of a `crust:layerStack` text, read back: `(hash, path)` per
/// line, the hash as written (`-` when it was not read).
pub fn parse_layer_stack(text: &str) -> Vec<(&str, &str)> {
    text.lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.split_once(' ').unwrap_or(("-", l)))
        .collect()
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
    /// The render camera's linear exposure scale the radiance channels were
    /// multiplied by; `1` when none applied.
    pub exposure_scale: f32,
    pub max_depth: u32,
    pub light_samples: u32,
    pub light_samples_indirect: u32,
    pub sampling_strategy: String,
    pub light_selection: String,
    pub pixel_filter: String,
    pub pixel_filter_radius: f32,
    /// The QMC sampler every pass drew from (`crust:sampler`).
    pub sampler: String,
    /// The time code the stage was evaluated at; `None` without one.
    pub frame: Option<f64>,
    /// The camera prim rendered through; `None` for the procedural camera.
    pub camera: Option<String>,
    /// The camera's lens; `None` for the procedural camera.
    pub lens: Option<Lens>,
    pub depth_of_field: bool,
    pub motion_blur: bool,
    /// The root layer stack, strongest first; empty for a hand-assembled
    /// scene.
    pub layer_stack: Vec<StampedLayer>,
    pub invisible_overrides: Option<u32>,
    pub version: String,
}

impl SamplingStamp {
    /// The stamp of a render that ran with `settings` and counted `rays`,
    /// through `camera_path` at `time` (see [`crate::Scene::camera_path`]
    /// and [`crate::Scene::time`]).
    ///
    /// `crust:sppTaken` comes from the adaptive counters, which every final
    /// pass fills, adaptive sampling on or off; without them (counters no
    /// render produced), every pixel took `spp`. A cancelled render's stamp
    /// goes through [`SamplingStamp::for_outcome`] too.
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
            exposure_scale: settings.exposure_scale(),
            max_depth: settings.max_depth(),
            light_samples: settings.light_samples(),
            light_samples_indirect: settings.light_samples_indirect(),
            sampling_strategy: settings.sampling_strategy().to_string(),
            light_selection: settings.light_selection().to_string(),
            pixel_filter: filter.name().to_owned(),
            pixel_filter_radius: filter.radius(),
            sampler: settings.sampler().name().to_owned(),
            frame: time,
            camera: camera_path.map(str::to_owned),
            lens: None,
            depth_of_field: false,
            motion_blur: false,
            layer_stack: Vec::new(),
            invisible_overrides: None,
            version: crate::report::CRUST_VERSION.to_owned(),
        }
    }

    /// This stamp with what the import recorded of `source`; `read` returns
    /// a layer file's bytes, `None` when it cannot (the host reads them:
    /// crust-core opens no file of its own here). Read after the render, so
    /// a layer edited while it rendered is hashed as edited.
    pub fn with_source(mut self, source: &Source, read: impl Fn(&Path) -> Option<Vec<u8>>) -> Self {
        self.lens = source.lens.clone();
        self.depth_of_field = source.depth_of_field;
        self.motion_blur = source.motion_blur;
        self.layer_stack = source
            .layer_stack
            .iter()
            .map(|path| StampedLayer {
                path: path.to_string_lossy().into_owned(),
                hash: read(path).map(|bytes| content_hash(&bytes)),
            })
            .collect();
        self.invisible_overrides = source.invisible_overrides;
        self
    }

    /// The header attributes, by full name, in a fixed order. `crust:frame`
    /// and `crust:camera` are left out when there is no time code or no
    /// camera prim, the lens with the camera prim, `crust:layerStack` when
    /// no layer is known and `crust:invisibleOverrides` for a hand-assembled
    /// scene.
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
            ("crust:exposureScale", Float(self.exposure_scale)),
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
            ("crust:sampler", Text(self.sampler.clone())),
        ];
        if let Some(t) = self.frame {
            out.push(("crust:frame", Double(t)));
        }
        if let Some(c) = &self.camera {
            out.push(("crust:camera", Text(c.clone())));
        }
        // The lens under its USD attribute names; the policy is the render
        // settings' token.
        if let Some(lens) = &self.lens {
            out.push(("crust:focalLength", Float(lens.focal_length)));
            out.push(("crust:horizontalAperture", Float(lens.aperture[0])));
            out.push(("crust:verticalAperture", Float(lens.aperture[1])));
            out.push(("crust:aspectRatioConformPolicy", Text(lens.conform.clone())));
            out.push(("crust:fStop", Float(lens.f_stop)));
            out.push(("crust:focusDistance", Float(lens.focus_distance)));
        }
        let flag = |on: bool| Int(i32::from(on));
        out.push(("crust:depthOfField", flag(self.depth_of_field)));
        out.push(("crust:motionBlur", flag(self.motion_blur)));
        if !self.layer_stack.is_empty() {
            out.push((
                "crust:layerStack",
                Text(layer_stack_text(&self.layer_stack)),
            ));
        }
        if let Some(n) = self.invisible_overrides {
            out.push(("crust:invisibleOverrides", int(n)));
        }
        out.push(("crust:version", Text(self.version.clone())));
        out
    }
}

impl SamplingStamp {
    /// The stamp of a render that ended with `outcome`. A cancelled render
    /// without adaptive counters is a guided one whose image holds no final
    /// pass — stopped in training, or before the final pass gave every pixel
    /// the two samples it needs to join the blend: no pixel took a
    /// final-pass sample, so `crust:sppTaken` is `(0, 0)`, not the budget
    /// [`SamplingStamp::new`] falls back to, which would claim all of it for
    /// a partial frame. Anything else is left as `new` made it.
    pub fn for_outcome(mut self, outcome: RenderOutcome, rays: &RayStats) -> Self {
        if outcome == RenderOutcome::Cancelled && rays.adaptive_pixels == 0 {
            self.spp_taken = (0, 0);
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PixelFilter;
    use std::path::PathBuf;

    /// A cancelled render whose image holds no final pass took no final-pass
    /// sample anywhere; a completed or cancelled render with counters keeps
    /// them, and a completed one without keeps the budget.
    #[test]
    fn a_cancelled_render_without_a_final_pass_took_none() {
        let none = RayStats::default();
        let stamp = |outcome, rays: &RayStats| {
            SamplingStamp::new(&settings(), rays, None, None)
                .for_outcome(outcome, rays)
                .spp_taken
        };
        assert_eq!(stamp(RenderOutcome::Cancelled, &none), (0, 0));
        assert_eq!(stamp(RenderOutcome::Completed, &none), (16, 16));
        let partial = RayStats {
            adaptive_pixels: 4,
            spp_min: 4,
            spp_max: 8,
            ..RayStats::default()
        };
        assert_eq!(stamp(RenderOutcome::Cancelled, &partial), (4, 8));
        assert_eq!(stamp(RenderOutcome::Completed, &partial), (4, 8));
    }

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

    /// The camera's exposure scale is recorded right after the clamp: the
    /// default render's 1, an exposed one's own.
    #[test]
    fn the_exposure_scale_is_recorded() {
        let at = |scale: f32| {
            let s = SamplingStamp::new(
                &settings().with_exposure_scale(scale),
                &RayStats::default(),
                None,
                None,
            );
            s.attributes()
        };
        let attrs = at(1.0);
        let names: Vec<&str> = attrs.iter().map(|(n, _)| *n).collect();
        let clamp = names.iter().position(|n| *n == "crust:indirectClamp");
        assert_eq!(
            names.iter().position(|n| *n == "crust:exposureScale"),
            clamp.map(|i| i + 1)
        );
        assert_eq!(
            get(&attrs, "crust:exposureScale"),
            Some(&StampValue::Float(1.0))
        );
        assert_eq!(
            get(&at(4.0), "crust:exposureScale"),
            Some(&StampValue::Float(4.0))
        );
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

    /// The sampler is stamped by its token, `sobol` when nothing chose one.
    #[test]
    fn the_sampler_is_stamped_by_its_token() {
        let attrs = SamplingStamp::new(&settings(), &RayStats::default(), None, None).attributes();
        assert_eq!(
            get(&attrs, "crust:sampler"),
            Some(&StampValue::Text("sobol".into()))
        );
        let bn = settings().with_sampler(crate::SamplerKind::LatticeBn);
        let attrs = SamplingStamp::new(&bn, &RayStats::default(), None, None).attributes();
        assert_eq!(
            get(&attrs, "crust:sampler"),
            Some(&StampValue::Text("latticeBn".into()))
        );
    }

    fn source() -> Source {
        Source {
            lens: Some(Lens {
                focal_length: 50.0,
                aperture: [20.955, 15.2908],
                conform: "expandAperture".into(),
                f_stop: 0.02,
                focus_distance: 360.0,
            }),
            depth_of_field: true,
            motion_blur: false,
            layer_stack: vec![
                PathBuf::from("/s/session.usda"),
                PathBuf::from("/s/shot.usda"),
            ],
            invisible_overrides: Some(3),
        }
    }

    /// The lens, the blurs and the layer stack are stamped, each layer with
    /// the hash of the bytes the host read, `-` where it read none.
    #[test]
    fn the_lens_and_the_layer_stack_are_stamped() {
        let read = |p: &Path| (p == Path::new("/s/shot.usda")).then(|| b"#usda 1.0\n".to_vec());
        let s = SamplingStamp::new(&settings(), &RayStats::default(), Some("/cam"), None)
            .with_source(&source(), read);
        let attrs = s.attributes();
        assert_eq!(get(&attrs, "crust:fStop"), Some(&StampValue::Float(0.02)));
        assert_eq!(
            get(&attrs, "crust:focusDistance"),
            Some(&StampValue::Float(360.0))
        );
        assert_eq!(
            get(&attrs, "crust:focalLength"),
            Some(&StampValue::Float(50.0))
        );
        assert_eq!(
            get(&attrs, "crust:aspectRatioConformPolicy"),
            Some(&StampValue::Text("expandAperture".into()))
        );
        assert_eq!(get(&attrs, "crust:depthOfField"), Some(&StampValue::Int(1)));
        assert_eq!(get(&attrs, "crust:motionBlur"), Some(&StampValue::Int(0)));
        assert_eq!(
            get(&attrs, "crust:invisibleOverrides"),
            Some(&StampValue::Int(3))
        );
        let hash = content_hash(b"#usda 1.0\n");
        let text = format!("- /s/session.usda\n{hash:016x} /s/shot.usda");
        assert_eq!(
            get(&attrs, "crust:layerStack"),
            Some(&StampValue::Text(text.clone()))
        );
        assert_eq!(
            parse_layer_stack(&text),
            vec![
                ("-", "/s/session.usda"),
                (format!("{hash:016x}").as_str(), "/s/shot.usda")
            ]
        );
        // The version stays last.
        assert_eq!(attrs.last().map(|(n, _)| *n), Some("crust:version"));
    }

    /// FNV-1a's published values: the hash must not change between builds.
    #[test]
    fn the_content_hash_is_fnv1a() {
        assert_eq!(content_hash(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(content_hash(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(content_hash(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn frame_and_camera_are_omitted_when_absent() {
        let attrs = SamplingStamp::new(&settings(), &RayStats::default(), None, None).attributes();
        assert!(get(&attrs, "crust:frame").is_none());
        assert!(get(&attrs, "crust:camera").is_none());
        assert!(get(&attrs, "crust:version").is_some());
        // So are the lens, the layers and the overrides; the blurs are not.
        assert!(get(&attrs, "crust:fStop").is_none());
        assert!(get(&attrs, "crust:layerStack").is_none());
        assert!(get(&attrs, "crust:invisibleOverrides").is_none());
        assert_eq!(get(&attrs, "crust:depthOfField"), Some(&StampValue::Int(0)));
        let attrs = SamplingStamp::new(&settings(), &RayStats::default(), Some("/cam"), Some(10.5))
            .with_source(&source(), |_| None)
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
