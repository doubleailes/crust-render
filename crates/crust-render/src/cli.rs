//! The command line: every flag, and the parsers that refuse a value the
//! engine would otherwise accept silently.

use clap::Parser;
use crust_core::{LightSelection, PixelFilter, SamplingStrategy};

#[derive(clap::ValueEnum, Clone, Debug, Copy)]
pub(super) enum LoggerLevel {
    Debug,
    Info,
    Warn,
    Error,
    Trace,
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
pub(super) struct Cli {
    /// Input scene path — .usda / .usdc / .usdz.
    /// When absent, falls back to a hard-coded procedural scene.
    #[arg(short, long)]
    pub(super) input: Option<String>,
    /// Output image path. The linear EXR is written here and a tone-mapped
    /// sRGB PNG next to it (same path with a .png extension).
    #[arg(short, long, default_value = "output.exr")]
    pub(super) output: String,
    /// Verbose level
    #[arg(short, long, default_value = "info")]
    pub(super) level: LoggerLevel,
    /// Also write the log to a file named for the time the run started
    /// (`crust-render-<UTC timestamp>.log`). Bare, it writes into the
    /// current directory; given a directory, it writes there and creates it
    /// if needed. The file receives the same events as the terminal, so
    /// `-l debug --log-file` is how a full record of a render is kept.
    #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = ".")]
    pub(super) log_file: Option<std::path::PathBuf>,
    /// Render by scanlines — a row is the work unit, rows in parallel, each
    /// written into the image in place — instead of the default 16x16 tiles.
    /// The image is bit-identical; kept as the A/B and for a progress bar in
    /// rows.
    #[arg(long, default_value_t = false)]
    pub(super) scanline: bool,
    /// Tiles ("bucket" order) are the default now; accepted so existing
    /// command lines keep working, and ignored.
    #[arg(short, long, default_value_t = false, hide = true)]
    pub(super) bucket: bool,
    /// Samples per pixel. Overrides the scene / default value when set.
    #[arg(short, long)]
    pub(super) samples: Option<u32>,
    /// USD time code (frame) to render. Every animated attribute resolves
    /// its time samples here; unanimated ones read their default. Fractional
    /// values render a subframe. Also sets the sampler's frame seed,
    /// overriding the scene's `crust:frame`. When absent, attributes read
    /// their default (non-time-sampled) value.
    #[arg(short, long, allow_negative_numbers = true, value_parser = parse_frame)]
    pub(super) frame: Option<f64>,
    /// Camera to render through, as an absolute USD prim path (e.g.
    /// `/root/camera01/renderCam`). Without it the stage's
    /// `RenderSettings.camera` is used, else the first camera found. A path
    /// that is not a camera on the stage stops the render.
    #[arg(long, value_name = "PRIM_PATH")]
    pub(super) camera: Option<String>,
    /// Subdivision refinement level for every mesh whose `subdivisionScheme`
    /// is not `none` (unauthored means USD's fallback, `catmullClark`).
    /// Overrides the scene's `crust:subdivisionLevel` render setting
    /// (default 0: each cage shaded with smooth normals, unrefined). Clamped to 6.
    #[arg(long, value_name = "N")]
    pub(super) subdiv_level: Option<u32>,
    /// Adaptive subdivision: refine each subdivision mesh only until its mean
    /// cage edge, at its nearest distance to the render camera, is at most
    /// this many pixels long. Overrides the scene's
    /// `crust:subdivisionEdgeLength`. `--subdiv-level` then caps the level
    /// (default 3). Needs `--camera` or the stage's `RenderSettings.camera`.
    #[arg(long, value_name = "PX", allow_negative_numbers = true, value_parser = parse_edge_length)]
    pub(super) subdiv_edge_length: Option<f32>,
    /// How light sampling and BSDF sampling combine. Overrides the scene's
    /// `crust:samplingStrategy` when set; `light` and `bsdf` render one
    /// strategy alone to visualize what MIS balances between.
    #[arg(long, value_parser = choices(SamplingStrategy::CHOICES))]
    pub(super) strategy: Option<SamplingStrategy>,
    /// How NEE picks the light it samples at each vertex. Overrides the
    /// scene's `crust:lightSelection` when set.
    #[arg(long, value_parser = choices(LightSelection::CHOICES))]
    pub(super) light_selection: Option<LightSelection>,
    /// Pixel reconstruction filter. Overrides the scene's
    /// `crust:pixelFilter` when set.
    #[arg(long, value_parser = choices(PixelFilter::CHOICES))]
    pub(super) filter: Option<PixelFilter>,
    /// Pixel filter radius in pixels, measured from the pixel center
    /// (each filter has its own default: box 0.5, triangle 1, gaussian /
    /// blackman 1.5, mitchell 2). Overrides `crust:pixelFilterRadius`.
    #[arg(long, value_parser = parse_radius)]
    pub(super) filter_radius: Option<f32>,
    /// Firefly clamp: cap each sample's indirect light at this value in its
    /// largest channel (linear, hue kept). Biased, and 0 turns it off.
    /// Overrides the scene's `crust:indirectClamp`.
    #[arg(long, value_parser = parse_clamp)]
    pub(super) indirect_clamp: Option<f32>,
    /// Print render statistics and a per-phase profile (parse, build,
    /// render, output) when the render finishes.
    #[arg(long, default_value_t = false)]
    pub(super) stats: bool,
    /// Also time the render section by section (Trace, EvalBsdfs, Texture,
    /// SurfaceLighting, ...) and add Guerilla-style profiles of it to the
    /// `--stats` report, which it implies. Costs render time (the report
    /// prints its own estimate), so it is separate from `--stats`, whose
    /// Render phase must stay comparable between runs.
    #[arg(long, default_value_t = false)]
    pub(super) profile: bool,
    /// Convert UV textures to a tiled, mip-mapped `.tx` beside the original
    /// (same path, extension `.tx`) on first use, when the `.tx` is missing or
    /// older than its source. A `.tx` beside a texture is always streamed when
    /// present; this only creates the missing ones.
    #[arg(long, default_value_t = false)]
    pub(super) auto_tx: bool,
}

/// `--frame`'s parser: an `f64` that is also finite. `f64::from_str` accepts
/// `nan`, `inf` and `infinity`, none of which is a time code; crust-core
/// refuses them too, but rejecting them here reports it as a usage error
/// before any scene is opened.
fn parse_frame(s: &str) -> std::result::Result<f64, String> {
    let frame: f64 = s.parse().map_err(|e| format!("{e}"))?;
    if frame.is_finite() {
        Ok(frame)
    } else {
        Err(format!("{s} is not a finite time code"))
    }
}

/// `--filter-radius`'s parser: a finite, positive radius in pixels. The
/// engine would clamp anything else to its minimum radius without a word;
/// refusing it here reports the typo as a usage error instead.
fn parse_radius(s: &str) -> std::result::Result<f32, String> {
    let r: f32 = s.parse().map_err(|e| format!("{e}"))?;
    if r.is_finite() && r > 0.0 {
        Ok(r)
    } else {
        Err(format!("{s} is not a positive, finite radius"))
    }
}

/// `--subdiv-edge-length`'s parser: a finite, positive length in pixels.
fn parse_edge_length(s: &str) -> std::result::Result<f32, String> {
    let l: f32 = s.parse().map_err(|e| format!("{e}"))?;
    if l.is_finite() && l > 0.0 {
        Ok(l)
    } else {
        Err(format!("{s} is not a positive, finite length in pixels"))
    }
}

/// `--indirect-clamp`'s parser: a finite, non-negative limit, `0` turning
/// the clamp off. The engine reads anything else as off too, silently;
/// refusing it here says so.
fn parse_clamp(s: &str) -> std::result::Result<f32, String> {
    let c: f32 = s.parse().map_err(|e| format!("{e}"))?;
    if c.is_finite() && c >= 0.0 {
        Ok(c)
    } else {
        Err(format!(
            "{s} is not a finite, non-negative limit (0 turns the clamp off)"
        ))
    }
}

/// A clap parser for one of the engine's named settings: the possible values
/// and their `--help` lines come from the enum's own table (`CHOICES`), and
/// the value from its `FromStr`, so the CLI holds no second spelling.
fn choices<T>(
    table: &'static [(T, &'static str, &'static str)],
) -> impl clap::builder::TypedValueParser<Value = T>
where
    T: std::str::FromStr<Err = crust_core::names::UnknownName> + Clone + Send + Sync + 'static,
{
    use clap::builder::{PossibleValue, PossibleValuesParser, TypedValueParser};
    PossibleValuesParser::new(
        table
            .iter()
            .map(|&(_, name, help)| PossibleValue::new(name).help(help)),
    )
    .try_map(|s| s.parse::<T>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_file_flag_is_optional_and_takes_an_optional_directory() {
        // Absent: no file.
        let c = Cli::try_parse_from(["crust-render"]).unwrap();
        assert_eq!(c.log_file, None);
        // Bare: the current directory.
        let c = Cli::try_parse_from(["crust-render", "--log-file"]).unwrap();
        assert_eq!(c.log_file.as_deref(), Some(std::path::Path::new(".")));
        // With a directory.
        let c = Cli::try_parse_from(["crust-render", "--log-file", "renders/logs"]).unwrap();
        assert_eq!(
            c.log_file.as_deref(),
            Some(std::path::Path::new("renders/logs"))
        );
        // Bare, followed by another flag: the flag must not be eaten as the
        // directory, which is what `num_args = 0..=1` is there to guarantee.
        let c = Cli::try_parse_from(["crust-render", "--log-file", "--bucket"]).unwrap();
        assert_eq!(c.log_file.as_deref(), Some(std::path::Path::new(".")));
        assert!(c.bucket);
    }

    /// The CLI parses straight into the engine's enums, through their own
    /// name tables: every name the engine knows is a CLI value, and parses to
    /// the value the engine means by it.
    #[test]
    fn cli_names_are_the_engine_names() {
        let parse = |flag: &str, value: &str| {
            Cli::try_parse_from(["crust-render", flag, value]).expect("a known name")
        };
        for &(value, name, _) in SamplingStrategy::CHOICES {
            assert_eq!(parse("--strategy", name).strategy, Some(value));
        }
        for &(value, name, _) in LightSelection::CHOICES {
            assert_eq!(
                parse("--light-selection", name).light_selection,
                Some(value)
            );
        }
        for &(value, name, _) in PixelFilter::CHOICES {
            assert_eq!(parse("--filter", name).filter, Some(value));
        }
        assert_eq!(
            parse("--strategy", "power").strategy,
            Some(SamplingStrategy::PowerMis)
        );
        assert_eq!(
            parse("--light-selection", "uniform").light_selection,
            Some(LightSelection::Uniform)
        );
        let cli = Cli::try_parse_from(["crust-render"]).unwrap();
        assert!(cli.light_selection.is_none());
    }

    #[test]
    fn cli_indirect_clamp_defaults_to_ten_and_zero_disables() {
        let cli = Cli::try_parse_from(["crust-render", "--indirect-clamp", "10"]).unwrap();
        assert_eq!(cli.indirect_clamp, Some(10.0));
        assert!(
            Cli::try_parse_from(["crust-render"])
                .unwrap()
                .indirect_clamp
                .is_none()
        );
        let (_, base) = crust_core::get_settings();
        assert_eq!(
            base.indirect_clamp(),
            Some(crust_core::DEFAULT_INDIRECT_CLAMP),
            "on by default"
        );
        assert_eq!(crust_core::DEFAULT_INDIRECT_CLAMP, 10.0);
        assert_eq!(base.with_indirect_clamp(10.0).indirect_clamp(), Some(10.0));
        assert_eq!(base.with_indirect_clamp(0.0).indirect_clamp(), None);
        assert_eq!(base.with_indirect_clamp(-3.0).indirect_clamp(), None);
        assert_eq!(base.with_indirect_clamp(f32::NAN).indirect_clamp(), None);
    }

    #[test]
    fn cli_filter_names_map_onto_the_engine_filters_at_their_default_radius() {
        let filter = |name: &str| {
            Cli::try_parse_from(["crust-render", "--filter", name])
                .unwrap()
                .filter
        };
        assert_eq!(filter("box"), Some(PixelFilter::BoxFilter { radius: 0.5 }));
        assert_eq!(
            filter("triangle"),
            Some(PixelFilter::Triangle { radius: 1.0 })
        );
        assert_eq!(
            filter("gaussian"),
            Some(PixelFilter::Gaussian { radius: 1.5 })
        );
        assert_eq!(
            filter("blackman"),
            Some(PixelFilter::Blackman { radius: 1.5 })
        );
        assert_eq!(
            filter("mitchell"),
            Some(PixelFilter::Mitchell { radius: 2.0 })
        );
    }

    #[test]
    fn cli_parses_its_flags() {
        let cli = Cli::try_parse_from([
            "crust-render",
            "-i",
            "scene.usda",
            "-o",
            "out.exr",
            "--bucket",
            "-s",
            "12",
            "--strategy",
            "balance",
            "--filter",
            "mitchell",
            "--filter-radius",
            "1.75",
            "--stats",
            "--profile",
            "-l",
            "debug",
            "--frame",
            "1012.5",
        ])
        .expect("valid flags");
        assert_eq!(cli.frame, Some(1012.5));
        assert_eq!(cli.input.as_deref(), Some("scene.usda"));
        assert_eq!(cli.output, "out.exr");
        assert!(cli.bucket, "the old flag still parses");
        assert!(!cli.scanline);
        assert_eq!(cli.samples, Some(12));
        assert_eq!(cli.strategy, Some(SamplingStrategy::BalanceMis));
        assert!(matches!(cli.filter, Some(PixelFilter::Mitchell { .. })));
        assert_eq!(cli.filter_radius, Some(1.75));
        assert!(cli.stats);
        assert!(cli.profile);
        assert!(matches!(cli.level, LoggerLevel::Debug));
        let scan = Cli::try_parse_from(["crust-render", "--scanline"]).expect("valid flags");
        assert!(scan.scanline);
    }

    #[test]
    fn cli_defaults_when_nothing_is_given() {
        let cli = Cli::try_parse_from(["crust-render"]).expect("no flags is valid");
        assert!(cli.input.is_none());
        assert_eq!(cli.output, "output.exr");
        assert!(!cli.scanline, "tiles are the default");
        assert!(cli.samples.is_none());
        assert!(cli.strategy.is_none());
        assert!(cli.filter.is_none());
        assert!(cli.filter_radius.is_none());
        assert!(cli.frame.is_none());
        assert!(
            cli.subdiv_level.is_none(),
            "the scene's level unless overridden"
        );
        assert!(!cli.stats);
        assert!(!cli.profile);
        assert!(matches!(cli.level, LoggerLevel::Info));
    }

    #[test]
    fn cli_subdiv_level_overrides_the_scene() {
        let cli = Cli::try_parse_from(["crust-render", "--subdiv-level", "0"]).expect("valid");
        assert_eq!(cli.subdiv_level, Some(0));
        let cli = Cli::try_parse_from(["crust-render", "--subdiv-level", "3"]).expect("valid");
        assert_eq!(cli.subdiv_level, Some(3));
        assert!(
            Cli::try_parse_from(["crust-render", "--subdiv-level", "-1"]).is_err(),
            "a level is a count"
        );
    }

    #[test]
    fn cli_subdiv_edge_length_is_a_positive_pixel_length() {
        let cli =
            Cli::try_parse_from(["crust-render", "--subdiv-edge-length", "2"]).expect("valid");
        assert_eq!(cli.subdiv_edge_length, Some(2.0));
        assert!(
            Cli::try_parse_from(["crust-render"])
                .unwrap()
                .subdiv_edge_length
                .is_none()
        );
        for bad in ["0", "-1", "inf", "NaN", "fast"] {
            let Err(err) = Cli::try_parse_from(["crust-render", "--subdiv-edge-length", bad])
            else {
                panic!("{bad} parsed as an edge length");
            };
            let err = err.to_string();
            assert!(err.contains("--subdiv-edge-length"), "{bad}: {err}");
        }
    }

    #[test]
    fn cli_accepts_a_negative_frame() {
        // Shots routinely start before 0 (handles, pre-roll), and clap
        // would otherwise read `-5` as an unknown short flag.
        let cli = Cli::try_parse_from(["crust-render", "-f", "-5"]).expect("negative frame");
        assert_eq!(cli.frame, Some(-5.0));
    }

    #[test]
    fn cli_rejects_a_non_finite_frame() {
        for bad in ["nan", "NaN", "inf", "-inf", "infinity", "-Infinity"] {
            assert!(
                Cli::try_parse_from(["crust-render", "--frame", bad]).is_err(),
                "--frame {bad} must be rejected"
            );
        }
        assert!(Cli::try_parse_from(["crust-render", "--frame", "twelve"]).is_err());
    }

    #[test]
    fn cli_rejects_a_radius_or_clamp_that_is_no_number_the_engine_uses() {
        for bad in ["-1", "nan", "inf", "0"] {
            assert!(
                Cli::try_parse_from(["crust-render", "--filter-radius", bad]).is_err(),
                "--filter-radius {bad}"
            );
        }
        for bad in ["-1", "nan", "inf"] {
            assert!(
                Cli::try_parse_from(["crust-render", "--indirect-clamp", bad]).is_err(),
                "--indirect-clamp {bad}"
            );
        }
        let ok = Cli::try_parse_from([
            "crust-render",
            "--indirect-clamp",
            "0",
            "--filter-radius",
            "1.5",
        ])
        .unwrap();
        assert_eq!(
            (ok.indirect_clamp, ok.filter_radius),
            (Some(0.0), Some(1.5))
        );
    }

    #[test]
    fn cli_rejects_unknown_enum_values() {
        assert!(Cli::try_parse_from(["crust-render", "--strategy", "random"]).is_err());
        assert!(Cli::try_parse_from(["crust-render", "--filter", "lanczos"]).is_err());
        assert!(Cli::try_parse_from(["crust-render", "--light-selection", "bvh"]).is_err());
        assert!(Cli::try_parse_from(["crust-render", "-l", "loud"]).is_err());
        assert!(Cli::try_parse_from(["crust-render", "-s", "many"]).is_err());
    }
}
