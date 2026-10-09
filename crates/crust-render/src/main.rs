//! The CLI: parse args, build a `Scene`, render, write the images.
//!
//! `forbid(unsafe_code)`, like every crate here but `crust-core` (a test-only
//! counting allocator) and `crust-jit` (calling generated code).
#![forbid(unsafe_code)]

mod logging;
#[cfg(feature = "mcp")]
mod mcp;
mod products;

use logging::{LoggerLevel, STATS_TARGET};

use clap::{Args, Parser, Subcommand, ValueEnum};
use crust_assets::FileAssets;
use crust_core::Buffer;
use crust_core::LightSelection;
use crust_core::PixelFilter;
use crust_core::PixelRect;
use crust_core::RayStats;
use crust_core::Renderer;
use crust_core::SamplingStrategy;
use crust_core::Scene;
use crust_core::check::{CheckReport, ProductInfo};
use crust_core::diagnostic::checks::{self, Facts};
use crust_core::diagnostic::report::SceneInfo;
use crust_core::diagnostic::{SceneFlags, effective_settings};
use crust_core::stamp::SamplingStamp;
use crust_core::{AovRequest, RenderSettings};
use crust_core::{RenderControl, RenderOutcome};
use crust_core::{WarningKind, WarningScope, warning};
use crust_core::{get_settings, simple_scene};
use exr::prelude::*;
use indicatif::ProgressBar;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

#[derive(Parser)]
#[command(name = "crust", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Verbose level
    #[arg(short, long, default_value = "info", global = true)]
    level: LoggerLevel,
}

#[derive(Subcommand)]
enum Command {
    /// Render a USD stage to a linear EXR and a tone-mapped PNG preview.
    ///
    /// Without -i, renders the hard-coded procedural fallback scene. Ctrl-C
    /// while it renders stops the render and writes what it has traced, its
    /// EXRs marked `crust:renderStatus = "interrupted"`, and exits 130; a
    /// second Ctrl-C, or one before the render starts, exits 130 at once.
    Render(Box<RenderArgs>),
    /// List a USD stage's cameras, lights or materials, one prim path per
    /// line on stdout.
    ///
    /// What is listed is what a render would use: a camera listed is a path
    /// `crust render --camera` accepts. The log goes to stderr, so stdout is
    /// only the listing.
    Ls {
        /// What to list.
        kind: LsKind,
        /// Input scene path — .usda / .usdc / .usdz.
        #[arg(short, long)]
        input: std::path::PathBuf,
        /// Write each prim with the values a render reads for it as JSON
        /// (format `crust-ls/1`) to PATH, or to stdout with `-` instead of
        /// the paths: a camera's lens and whether a render goes through it,
        /// a light's type, intensity, exposure, color and normalize, a
        /// material's surface shader and whether anything is bound to it.
        #[arg(long, value_name = "PATH|-")]
        json: Option<std::path::PathBuf>,
        /// USD time code to evaluate the values at, as `render -f` takes it.
        /// Without it, values read their default (non-time-sampled) value.
        /// Which prims are listed does not depend on it.
        #[arg(short, long, allow_negative_numbers = true, value_parser = parse_frame)]
        frame: Option<f64>,
    },
    /// Measure how to make a stage's render faster or cleaner, within a
    /// time budget: a baseline, then every unbiased setting tried on
    /// representative crops and judged by efficiency.
    ///
    /// Writes no image and changes no file beside the stage (but for the
    /// `.tx` files `--auto-tx` creates, as a render would). The Markdown
    /// report goes to stdout, the JSON report to `--json`, the log to
    /// stderr. Exits 0 when the unbiased trials completed, 3 when the budget
    /// ran out first (the report is still written), 1 on error, 2 on a
    /// usage error.
    Diagnostic(Box<DiagnosticArgs>),
    /// Import a stage as `crust render` would and report on it, rendering
    /// nothing: the render it describes (camera, resolution, products), its
    /// effective settings, the import's costs, the findings that need no
    /// render, and what the import refused, approximated or skipped.
    ///
    /// Writes no image and changes no file beside the stage (but for the
    /// `.tx` files `--auto-tx` creates, as a render would). The text report
    /// goes to stdout, the log to stderr. Exits 0 when no denied warning was
    /// raised, 3 when one was (the reports are still written), 1 on error,
    /// 2 on a usage error.
    Check(Box<CheckArgs>),
    /// Compare two EXRs: did the image change, and by how much?
    ///
    /// Every channel of every layer is compared bitwise; on the beauty
    /// (`R`, `G`, `B`) it adds the error metrics against A, the reference.
    /// From both files' `crust:*` sampling stamps it says whether the pixels
    /// can be compared at all (on stderr, or in the JSON). Exits 0 when the
    /// files are identical, 1 when they differ, 2 on an error.
    Diff {
        /// The reference image.
        a: std::path::PathBuf,
        /// The image compared with it.
        b: std::path::PathBuf,
        /// Also write the report as JSON (format `crust-diff/1`) to PATH;
        /// with `-`, the JSON goes to stdout instead of the text report.
        #[arg(long, value_name = "PATH|-")]
        json: Option<std::path::PathBuf>,
    },
    /// Serve a live editing and rendering session to an MCP client (Claude
    /// Desktop) over stdin/stdout, until stdin closes.
    ///
    /// A session edits one USD stage through an override layer that
    /// sublayers it, and renders it. stdout carries only the protocol; the
    /// log goes to stderr. Nothing is loaded until the client opens a session.
    #[cfg(feature = "mcp")]
    Mcp,
}

#[derive(Args)]
struct DiagnosticArgs {
    #[command(flatten)]
    scene: SceneArgs,
    /// Time to spend after the import: `90s`, `5m`, `1m30s`, `1h`, or plain
    /// seconds. The baseline always runs; the trials then fit what is left.
    #[arg(long, value_name = "DURATION", default_value = "120s", value_parser = parse_duration)]
    budget: Duration,
    /// Where to write the JSON report (format `crust-diagnostic/1`).
    #[arg(long, value_name = "PATH", default_value = "crust-diagnostic.json")]
    json: std::path::PathBuf,
    /// A previous report of the same scene, frame, camera, resolution and
    /// region: adds a `deltas` section saying what changed since.
    #[arg(long, value_name = "PREV.json")]
    baseline: Option<std::path::PathBuf>,
    /// Interleaved baseline/trial pairs per crop. More resolves smaller
    /// differences on a noisy machine, at the cost of fewer trials.
    #[arg(long, value_name = "R", default_value_t = 3, value_parser = clap::value_parser!(u32).range(1..))]
    repeats: u32,
    /// The mean relative squared error the sample budget aims for. Defaults
    /// to the square of the scene's adaptive variance threshold.
    #[arg(long, value_name = "MRSE", value_parser = parse_target)]
    target_mrse: Option<f64>,
}

#[derive(Args)]
struct CheckArgs {
    #[command(flatten)]
    scene: SceneArgs,
    /// Also write the report as JSON (format `crust-check/1`) to PATH; with
    /// `-`, the JSON goes to stdout instead of the text report.
    #[arg(long, value_name = "PATH|-")]
    json: Option<std::path::PathBuf>,
    /// Exit 3 when the import raises a warning of one of these kinds:
    /// `refused`, `approximated`, `skipped`, or `all`, comma-separated. The
    /// reports are written either way, with the matching codes in `denied`.
    #[arg(long, value_name = "KIND[,KIND…]", value_enum, value_delimiter = ',')]
    deny: Vec<DenyKind>,
}

/// `crust ls`'s kinds, each the engine's [`crust_core::ListKind`].
#[derive(Clone, Copy, ValueEnum)]
enum LsKind {
    /// Cameras the render can go through (those under an invisible ancestor
    /// included).
    #[value(alias = "cameras")]
    Camera,
    /// The lights the render reads (invisible ones left out).
    #[value(alias = "lights")]
    Light,
    /// Material prims a binding can reach, bound or not.
    #[value(alias = "materials")]
    Material,
}

impl From<LsKind> for crust_core::ListKind {
    fn from(kind: LsKind) -> Self {
        match kind {
            LsKind::Camera => crust_core::ListKind::Camera,
            LsKind::Light => crust_core::ListKind::Light,
            LsKind::Material => crust_core::ListKind::Material,
        }
    }
}

/// The flags that shape the scene and its settings, shared by `render`,
/// `diagnostic` and `check` with the same names, values and defaults — so a
/// diagnostic suggestion given as a flag means the same in a render.
#[derive(Args)]
struct SceneArgs {
    /// Input scene path — .usda / .usdc / .usdz.
    /// When absent, `render` falls back to a hard-coded procedural scene.
    #[arg(short, long)]
    input: Option<String>,
    /// Render only a rectangle of the frame: `X0,Y0,X1,Y1` in pixels, from
    /// the image's top-left corner, `X1` and `Y1` excluded. Each pixel
    /// renders exactly as in the full frame; the EXR keeps the full
    /// resolution as its display window with the region as its data
    /// window, and the PNG holds only the region. Overrides the stage's
    /// `dataWindowNDC`. Clipped to the resolution; a region with nothing
    /// left is an error.
    #[arg(long, value_name = "X0,Y0,X1,Y1", value_parser = parse_region)]
    region: Option<PixelRect>,
    /// USD time code (frame) to render. Every animated attribute resolves
    /// its time samples here; unanimated ones read their default. Fractional
    /// values render a subframe. Also sets the sampler's frame seed,
    /// overriding the scene's `crust:frame`. When absent, attributes read
    /// their default (non-time-sampled) value.
    #[arg(short, long, allow_negative_numbers = true, value_parser = parse_frame)]
    frame: Option<f64>,
    /// Camera to render through, as an absolute USD prim path (e.g.
    /// `/root/camera01/renderCam`). Without it the stage's
    /// `RenderSettings.camera` is used, else the first camera found. A path
    /// that is not a camera on the stage stops the render.
    #[arg(long, value_name = "PRIM_PATH")]
    camera: Option<String>,
    /// Subdivision refinement level for every mesh whose `subdivisionScheme`
    /// is not `none` (unauthored means USD's fallback, `catmullClark`).
    /// Overrides the scene's `crust:subdivisionLevel` render setting
    /// (default 0: each cage shaded with smooth normals, unrefined). Clamped to 6.
    #[arg(long, value_name = "N")]
    subdiv_level: Option<u32>,
    /// Adaptive subdivision: refine each subdivision mesh only until its mean
    /// cage edge, at its nearest distance to the render camera, is at most
    /// this many pixels long. Overrides the scene's
    /// `crust:subdivisionEdgeLength`. `--subdiv-level` then caps the level
    /// (default 3). Needs `--camera` or the stage's `RenderSettings.camera`.
    #[arg(long, value_name = "PX", allow_negative_numbers = true, value_parser = parse_edge_length)]
    subdiv_edge_length: Option<f32>,
    /// How light sampling and BSDF sampling combine. Overrides the scene's
    /// `crust:samplingStrategy` when set; `light` and `bsdf` render one
    /// strategy alone to visualize what MIS balances between.
    #[arg(long, value_parser = choices(SamplingStrategy::CHOICES))]
    strategy: Option<SamplingStrategy>,
    /// How NEE picks the light it samples at each vertex. Overrides the
    /// scene's `crust:lightSelection` when set.
    #[arg(long, value_parser = choices(LightSelection::CHOICES))]
    light_selection: Option<LightSelection>,
    /// Light samples (shadow rays) per camera vertex: N stratified picks
    /// over the lights, MIS-combined with the bounce. Lowers direct-light
    /// noise about as 1/N at N times the shadow rays there. Overrides the
    /// scene's `crust:lightSamples` (default 1).
    #[arg(long, value_name = "N", value_parser = parse_count)]
    light_samples: Option<u32>,
    /// Light samples per later surface or volume vertex, paid at every
    /// bounce of the path. Overrides the scene's
    /// `crust:lightSamplesIndirect` (default 1).
    #[arg(long, value_name = "M", value_parser = parse_count)]
    light_samples_indirect: Option<u32>,
    /// Pixel reconstruction filter. Overrides the scene's
    /// `crust:pixelFilter` when set.
    #[arg(long, value_parser = choices(PixelFilter::CHOICES))]
    filter: Option<PixelFilter>,
    /// Pixel filter radius in pixels, measured from the pixel center
    /// (each filter has its own default: box 0.5, triangle 1, gaussian /
    /// blackman 1.5, mitchell 2). Overrides `crust:pixelFilterRadius`.
    #[arg(long, value_parser = parse_radius)]
    filter_radius: Option<f32>,
    /// Firefly clamp: cap each sample's indirect light at this value in its
    /// largest channel (linear, hue kept). Biased, and 0 turns it off.
    /// Overrides the scene's `crust:indirectClamp`.
    #[arg(long, value_parser = parse_clamp)]
    indirect_clamp: Option<f32>,
    /// Convert UV textures to a tiled, mip-mapped `.tx` beside the original
    /// (same path, extension `.tx`) on first use, when the `.tx` is missing or
    /// older than its source. A `.tx` beside a texture is always streamed when
    /// present; this only creates the missing ones.
    #[arg(long, default_value_t = false)]
    auto_tx: bool,
}

#[derive(Args)]
struct RenderArgs {
    #[command(flatten)]
    scene: SceneArgs,
    /// Output image path. Without RenderProducts on the stage, the linear EXR
    /// is written here (default `output.exr`) and a tone-mapped sRGB PNG next
    /// to it (same path with a .png extension). When the stage authors
    /// RenderProducts, this replaces the first product's `productName`, as
    /// husk's `-o` does; the other products keep theirs.
    #[arg(short, long)]
    output: Option<String>,
    /// Also write the log to a file named for the time the run started
    /// (`crust-<UTC timestamp>.log`). Bare, it writes into the
    /// current directory; given a directory, it writes there and creates it
    /// if needed. The file receives the same events as the terminal, so
    /// `-l debug --log-file` is how a full record of a render is kept.
    // A render flag, not a global one: its directory is optional, so before
    // a subcommand or a positional (`crust --log-file render`, `crust ls
    // --log-file camera`) it would take that word as the directory.
    // `render` has no positional for it to swallow.
    #[arg(long, value_name = "DIR", num_args = 0..=1, default_missing_value = ".")]
    log_file: Option<std::path::PathBuf>,
    /// Render by scanlines — a row is the work unit, rows in parallel, each
    /// written into the image in place — instead of the default 16x16 tiles.
    /// The image is bit-identical; kept as the A/B and for a progress bar in
    /// rows.
    #[arg(long, default_value_t = false)]
    scanline: bool,
    /// Tiles ("bucket" order) are the default now; accepted so existing
    /// command lines keep working, and ignored.
    #[arg(short, long, default_value_t = false, hide = true)]
    bucket: bool,
    /// Samples per pixel. Overrides the scene / default value when set.
    #[arg(short, long)]
    samples: Option<u32>,
    /// Print render statistics and a per-phase profile (parse, build,
    /// render, output) when the render finishes.
    #[arg(long, default_value_t = false)]
    stats: bool,
    /// Also time the render section by section (Trace, EvalBsdfs, Texture,
    /// SurfaceLighting, ...) and add Guerilla-style profiles of it to the
    /// `--stats` report, which it implies. Costs render time (the report
    /// prints its own estimate), so it is separate from `--stats`, whose
    /// Render phase must stay comparable between runs.
    #[arg(long, default_value_t = false)]
    profile: bool,
    /// Write the statistics as JSON (format `crust-stats/1`) to PATH, or to
    /// stdout with `-`, which moves the log to stderr. Collects what
    /// `--stats` reports without printing its table; pass `--stats` too for
    /// both. With `--profile`, the report gains a `profile` object.
    #[arg(long, value_name = "PATH|-")]
    stats_json: Option<std::path::PathBuf>,
    /// The OpenColorIO config every colour is managed with: a `.ocio` file,
    /// an `.ocioz` archive or an `ocio://` builtin URI. Defaults to `$OCIO`
    /// when that is set, else to the builtin ACES CG config
    /// (cg-config-v4.0.0_aces-v2.0_ocio-v2.5). It must
    /// define `raw`, `lin_rec709`, `srgb_texture`, `g22_rec709` and
    /// `g18_rec709`, as names or aliases; every ACES config does.
    #[arg(long, value_name = "CONFIG")]
    ocio_config: Option<String>,
    /// The scene-linear colour space to render in, by any name or alias of
    /// the OCIO config — `acescg`, `lin_rec2020`, `lin_rec709`, … Overrides
    /// the stage's `RenderSettings.renderingColorSpace`; `lin_rec709` when
    /// neither names one.
    #[arg(long, value_name = "SPACE")]
    working_space: Option<String>,
    /// The OCIO display the PNG preview is encoded for.
    #[arg(long, value_name = "DISPLAY", default_value = crust_core::color::PREVIEW_DISPLAY)]
    display: String,
    /// The OCIO view the PNG preview is encoded with. The default,
    /// `Un-tone-mapped`, clamps to [0, 1] and applies the display's curve;
    /// `"ACES 2.0 - SDR 100 nits (Rec.709)"` applies the ACES output
    /// transform instead. The EXR is never affected.
    #[arg(long, value_name = "VIEW", default_value = crust_core::color::PREVIEW_VIEW)]
    view: String,
    /// While the render runs, rewrite the tone-mapped PNG preview every
    /// SECONDS (a positive number, fractions allowed) from the image so far,
    /// at the path the final PNG takes, whenever the image changed since the
    /// last rewrite. The final EXR and PNG are the same with or without it.
    #[arg(long, value_name = "SECONDS", allow_negative_numbers = true, value_parser = parse_checkpoint)]
    checkpoint: Option<Duration>,
}

/// A render's scene flags read as its own (`cli.strategy`), as they were
/// before they moved into [`SceneArgs`]: the render code and its tests keep
/// their spelling, and `&RenderArgs` passes wherever `&SceneArgs` is taken.
impl std::ops::Deref for RenderArgs {
    type Target = SceneArgs;
    fn deref(&self) -> &SceneArgs {
        &self.scene
    }
}

/// `--checkpoint`'s parser: a positive, finite number of seconds that a
/// `Duration` can hold.
fn parse_checkpoint(s: &str) -> std::result::Result<Duration, String> {
    let secs: f64 = s
        .trim()
        .parse()
        .map_err(|_| format!("`{s}` is not a number of seconds"))?;
    if secs.is_nan() || secs <= 0.0 {
        return Err(format!("{s} is not a positive number of seconds"));
    }
    Duration::try_from_secs_f64(secs).map_err(|_| format!("{s} seconds is too long an interval"))
}

/// `--budget`'s parser: one or more `<number><unit>` terms (`h`, `m`, `s`,
/// `ms`), as in `90s`, `5m` or `1m30s`, or a bare number of seconds. The
/// total must be positive, and small enough for a `Duration`
/// (`from_secs_f64` would panic on `1e20`).
fn parse_duration(s: &str) -> std::result::Result<Duration, String> {
    let bad = || format!("{s:?} is not a duration like 90s, 5m or 1m30s");
    let positive = |secs: f64| {
        if secs > 0.0 {
            Duration::try_from_secs_f64(secs).map_err(|_| format!("{s:?} is too long a duration"))
        } else {
            Err(bad())
        }
    };
    let t = s.trim();
    if let Ok(secs) = t.parse::<f64>() {
        return positive(secs);
    }
    let mut total = 0.0f64;
    let mut rest = t;
    while !rest.is_empty() {
        let digits = rest
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .ok_or_else(bad)?;
        let value: f64 = rest[..digits].parse().map_err(|_| bad())?;
        rest = &rest[digits..];
        let unit_len = rest
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(rest.len());
        let scale = match &rest[..unit_len] {
            "h" => 3600.0,
            "m" => 60.0,
            "s" => 1.0,
            "ms" => 1e-3,
            _ => return Err(bad()),
        };
        total += value * scale;
        rest = &rest[unit_len..];
    }
    positive(total)
}

/// `--target-mrse`'s parser: a finite, positive error.
fn parse_target(s: &str) -> std::result::Result<f64, String> {
    let v: f64 = s.parse().map_err(|e| format!("{e}"))?;
    if v.is_finite() && v > 0.0 {
        Ok(v)
    } else {
        Err(format!("{s} is not a positive, finite error"))
    }
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

/// `--light-samples` / `--light-samples-indirect`'s parser: a count from 1
/// to `MAX_LIGHT_SAMPLES`. Zero would mean no light sampling at all, which
/// is `--strategy bsdf`'s job, and a count multiplies every vertex's shadow
/// rays, so a mistyped huge one is a render that never ends; the engine
/// clamps both silently, so they are refused here as usage errors instead.
fn parse_count(s: &str) -> std::result::Result<u32, String> {
    let n: u32 = s.parse().map_err(|e| format!("{e}"))?;
    if (1..=crust_core::MAX_LIGHT_SAMPLES).contains(&n) {
        Ok(n)
    } else {
        Err(format!(
            "{s} is not a count from 1 to {}",
            crust_core::MAX_LIGHT_SAMPLES
        ))
    }
}

/// `--region`'s parser: four non-negative integers `X0,Y0,X1,Y1` with
/// `X1 > X0` and `Y1 > Y0`. Whether the rectangle meets the frame needs
/// the resolution, so that is checked once the stage is loaded.
fn parse_region(s: &str) -> std::result::Result<PixelRect, String> {
    let parts: Vec<&str> = s.split(',').map(str::trim).collect();
    let [x0, y0, x1, y1] = parts[..] else {
        return Err(format!(
            "{s:?} is not four comma-separated integers X0,Y0,X1,Y1"
        ));
    };
    let int = |v: &str| {
        v.parse::<usize>()
            .map_err(|_| format!("{v:?} is not a non-negative integer"))
    };
    let (x0, y0, x1, y1) = (int(x0)?, int(y0)?, int(x1)?, int(y1)?);
    if x1 <= x0 || y1 <= y0 {
        return Err(format!(
            "{s} is empty: X1 must be greater than X0, and Y1 than Y0 (X1 and Y1 are excluded)"
        ));
    }
    Ok(PixelRect::new(x0, y0, x1, y1))
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

/// How the outputs describe and encode the working space's pixels: the space
/// itself, for the EXRs' colour metadata, and the OCIO display / view the
/// preview PNG is encoded through.
struct OutputColor {
    working: crust_core::color::Space,
    display: String,
    view: String,
}

impl OutputColor {
    /// `--display` / `--view` for a render in `working`, refused when the
    /// config cannot make that preview — before the render, not after it.
    fn new(
        working: crust_core::color::Space,
        display: &str,
        view: &str,
    ) -> std::result::Result<Self, String> {
        let color = OutputColor {
            working,
            display: display.to_owned(),
            view: view.to_owned(),
        };
        crust_core::color::encode_preview(&mut [0.0; 3], working, display, view)?;
        Ok(color)
    }

    /// The EXR header attributes for these pixels: the working space's ASWF
    /// Color Interop ID, and its chromaticities — set only off Rec.709,
    /// whose primaries are what an EXR without the attribute means, so a
    /// `lin_rec709` file is what it always was.
    fn exr_chromaticities(&self) -> Option<exr::meta::attribute::Chromaticities> {
        if self.working == crust_core::color::Space::LIN_REC709 {
            return None;
        }
        let [r, g, b, w] = crust_core::color::chromaticities(self.working)?;
        Some(exr::meta::attribute::Chromaticities {
            red: Vec2(r[0], r[1]),
            green: Vec2(g[0], g[1]),
            blue: Vec2(b[0], b[1]),
            white: Vec2(w[0], w[1]),
        })
    }
}

/// Encode linear working-space RGB as 8-bit preview bytes through the OCIO
/// display / view ([`crust_core::color::encode_preview`]). The default view
/// clamps to [0, 1] and applies the display's curve: for sRGB, the same
/// piecewise curve the texture decoders invert.
fn tone_map(rgb: &mut [f32], color: &OutputColor) -> Vec<u8> {
    crust_core::color::encode_preview(rgb, color.working, &color.display, &color.view)
        .expect("checked by OutputColor::new");
    rgb.iter()
        .map(|&c| (c.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8)
        .collect()
}

/// Tone-map the render buffer to an 8-bit PNG at `path`, whatever its
/// extension: the buffer's region, at the region's size — a PNG has no data
/// window to place it in a larger frame with.
fn write_png(
    buffer: &Buffer,
    path: &Path,
    color: &OutputColor,
) -> std::result::Result<(), image::ImageError> {
    let (width, height) = buffer.size();
    let mut rgb = Vec::with_capacity(width * height * 3);
    for y in 0..height {
        for x in 0..width {
            let (r, g, b) = buffer.get_rgb(x, y);
            rgb.extend_from_slice(&[r, g, b]);
        }
    }
    let bytes = tone_map(&mut rgb, color);
    let mut img = image::RgbaImage::new(width as u32, height as u32);
    for (i, &[r, g, b]) in bytes.as_chunks::<3>().0.iter().enumerate() {
        let (x, y) = (i % width, i / width);
        img.put_pixel(x as u32, y as u32, image::Rgba([r, g, b, 255]));
    }
    img.save_with_format(path, image::ImageFormat::Png)
}

/// The tone-mapped preview beside an EXR: the same path, extension `.png`.
/// Where the final PNG goes, and so where `--checkpoint` rewrites it.
fn preview_png(exr: &str) -> PathBuf {
    Path::new(exr).with_extension("png")
}

/// The render of a stage without RenderProducts: the beauty as an RGB EXR at
/// `output`, then the tone-mapped PNG next to it. What `write_rgb_file` writes
/// — in `lin_rec709` this output has the header and pixels it had before AOVs —
/// plus, in any other working space, the chromaticities and `colorInteropID`
/// that say which. A cropped render's EXR holds the region as its data
/// window inside the full frame's display window (`products::exr_windows`).
fn write_beauty(
    buffer: &Buffer,
    output: &str,
    color: &OutputColor,
    sampling: &SamplingStamp,
    interrupted: bool,
) -> std::result::Result<(), ExitCode> {
    let (img_width, img_height) = buffer.size();
    debug!(
        "Writing {}x{} linear EXR to {}",
        img_width, img_height, output
    );
    let channels = SpecificChannels::rgb(|Vec2(x, y)| buffer.get_rgb(x, y));
    let mut image = Image::from_channels((img_width, img_height), channels);
    let (position, display_window) = products::exr_windows(buffer);
    image.layer_data.attributes.layer_position = position;
    image.attributes.display_window = display_window;
    if let Some(chromaticities) = color.exr_chromaticities() {
        image.attributes.chromaticities = Some(chromaticities);
        if let Some(id) =
            crust_core::color::interop_id(color.working).and_then(|id| Text::new_or_none(&id))
        {
            image.layer_data.attributes.other.insert(
                Text::from("colorInteropID"),
                exr::meta::attribute::AttributeValue::Text(id),
            );
        }
    }
    products::stamp(&mut image.layer_data.attributes, sampling);
    if interrupted {
        products::mark_interrupted(&mut image.layer_data.attributes);
    }
    match image.write().to_file(output) {
        Ok(_) => info!("Image written to: {:?}", output),
        Err(e) => {
            error!("Error writing image: {}", e);
            return Err(ExitCode::FAILURE);
        }
    }
    let png_path = preview_png(output);
    debug!("Tone mapping to sRGB PNG at {}", png_path.display());
    match write_png(buffer, &png_path, color) {
        Ok(_) => info!("Image written to: {:?}", png_path),
        Err(e) => {
            error!("Error writing PNG: {}", e);
            return Err(ExitCode::FAILURE);
        }
    }
    Ok(())
}

/// Where a render of a stage without RenderProducts writes its beauty EXR:
/// `-o`, else `output.exr` — what `render` writes and `check` reports.
fn beauty_output(output: Option<&str>) -> &str {
    output.unwrap_or("output.exr")
}

/// The scene to render: the USD stage `-i` names, imported under the CLI's
/// options, or the procedural fallback without one. The error is the message
/// to log, or (in an MCP session) to answer with.
fn load_scene(
    cli: &SceneArgs,
    working_space: Option<&String>,
    assets: &FileAssets,
) -> std::result::Result<Scene, String> {
    let scene = if let Some(t) = &cli.input {
        let input_path = std::path::Path::new(&t);
        debug!("Loading USD scene from {}", input_path.display());
        let options = crust_core::UsdImportOptions {
            frame: cli.frame,
            camera: cli.camera.clone(),
            subdivision_level: cli.subdiv_level,
            subdivision_edge_length: cli.subdiv_edge_length,
            // The process renders once and exits, so freeing the composed
            // stage is pure delay before the render (45 s on ALab).
            skip_stage_teardown: true,
            working_space: working_space.cloned(),
        };
        match Scene::from_usd_with_options(input_path, assets, &options) {
            Ok(scene) => scene,
            Err(e) => return Err(format!("Failed to load USD scene: {e}")),
        }
    } else {
        debug!("No -i/--input given: building the procedural fallback scene");
        if let Some(frame) = cli.frame {
            warn!(
                "--frame {frame} has no effect without -i/--input: the procedural scene is static"
            );
        }
        if let Some(camera) = &cli.camera {
            warn!("--camera {camera} has no effect without -i/--input");
        }
        if let Some(space) = working_space {
            warn!(
                "--working-space {space} has no effect without -i/--input: the procedural \
                 scene renders in lin_rec709"
            );
        }
        let (world, lights) = simple_scene();
        let (camera, settings) = get_settings();
        Scene::new(camera, world, lights, settings)
    };
    Ok(scene)
}

/// The scene's render settings with a render's overrides applied: `-s`,
/// then the scene flags ([`apply_scene_overrides`]).
fn apply_overrides(cli: &RenderArgs, settings: RenderSettings) -> RenderSettings {
    let settings = match cli.samples {
        Some(spp) => {
            debug!("--samples {spp} overrides the scene's crust:samplesPerPixel");
            settings.with_samples_per_pixel(spp)
        }
        None => settings,
    };
    apply_scene_overrides(cli, settings)
}

/// The scene's render settings with the shared scene flags applied — what
/// `render` and `diagnostic` both run with.
fn apply_scene_overrides(cli: &SceneArgs, mut settings: RenderSettings) -> RenderSettings {
    if let Some(strategy) = cli.strategy {
        debug!("--strategy {strategy} overrides the scene's crust:samplingStrategy");
        settings = settings.with_sampling_strategy(strategy);
    }
    if let Some(selection) = cli.light_selection {
        debug!("--light-selection {selection} overrides the scene's crust:lightSelection");
        settings = settings.with_light_selection(selection);
    }
    // --filter replaces the scene's filter (at the filter's default radius);
    // --filter-radius then resizes whichever filter is in effect, so it also
    // works alone to widen the scene-authored one.
    if let Some(filter) = cli.filter {
        debug!("--filter {filter} overrides the scene's crust:pixelFilter");
        settings = settings.with_pixel_filter(filter);
    }
    if let Some(radius) = cli.filter_radius {
        debug!("--filter-radius {radius} overrides the filter's own radius");
        settings = settings.with_pixel_filter(settings.pixel_filter().with_radius(radius));
    }
    if let Some(limit) = cli.indirect_clamp {
        debug!("--indirect-clamp {limit} overrides the scene's crust:indirectClamp");
        settings = settings.with_indirect_clamp(limit);
    }
    if cli.light_samples.is_some() || cli.light_samples_indirect.is_some() {
        let camera = cli.light_samples.unwrap_or(settings.light_samples());
        let indirect = cli
            .light_samples_indirect
            .unwrap_or(settings.light_samples_indirect());
        if let Some(n) = cli.light_samples {
            debug!("--light-samples {n} overrides the scene's crust:lightSamples");
        }
        if let Some(m) = cli.light_samples_indirect {
            debug!("--light-samples-indirect {m} overrides the scene's crust:lightSamplesIndirect");
        }
        settings = settings.with_light_samples(camera, indirect);
    }
    settings
}

/// The stage's RenderProducts that can be written, with `-o` replacing the
/// first one's path; each refusal is a coded warning (`product.no_name`,
/// `product.no_writable_vars`, `product.shared_path`), which `crust check`
/// collects. Empty means the single beauty EXR at `-o`.
fn select_products(aovs: &mut AovRequest, output: Option<&str>) {
    if let (Some(first), Some(o)) = (aovs.products.first_mut(), output) {
        debug!(
            "-o {o} replaces {}'s productName {:?}",
            first.prim_path, first.name
        );
        first.name = o.to_owned();
    }
    if !aovs.products.is_empty() {
        aovs.products.retain(|p| {
            if p.name.is_empty() {
                warning!(
                    ProductNoName,
                    at = p.prim_path,
                    "{} authors no productName; nothing written for it",
                    p.prim_path
                );
            } else if p.vars.is_empty() {
                warning!(
                    ProductNoWritableVars,
                    at = p.prim_path,
                    "{}: no RenderVar crust can write; nothing written for it",
                    p.prim_path
                );
            } else {
                return true;
            }
            false
        });
        products::refuse_shared_paths(&mut aovs.products);
        if aovs.products.is_empty() {
            warn!("No RenderProduct can be written; writing the beauty to -o instead");
        }
    }
}

/// A JSON report's destination: `-` is stdout.
fn is_stdout(path: &Path) -> bool {
    path == Path::new("-")
}

/// Write a JSON report to `path`, or print it on stdout for `-`.
fn write_json(path: &Path, json: &str) -> std::io::Result<()> {
    if is_stdout(path) {
        use std::io::Write;
        let mut out = std::io::stdout().lock();
        out.write_all(json.as_bytes())?;
        out.flush()
    } else {
        std::fs::write(path, json)
    }
}

/// Every failure returns through here rather than `std::process::exit`, so
/// the stack unwinds normally and every destructor runs on the way out.
fn main() -> ExitCode {
    let cli = Cli::parse();
    // A listing's stdout is its result, so its log goes to stderr; a
    // render's log stays where it always was, unless its stdout carries a
    // JSON report (`--stats-json -`).
    let (log_to, log_file) = match &cli.command {
        Command::Render(args) => (
            if args.stats_json.as_deref().is_some_and(is_stdout) {
                logging::Terminal::Stderr
            } else {
                logging::Terminal::Stdout
            },
            args.log_file.as_deref(),
        ),
        Command::Ls { .. } | Command::Diagnostic(_) | Command::Check(_) | Command::Diff { .. } => {
            (logging::Terminal::Stderr, None)
        }
        // stdout is the protocol: a log line there would corrupt it.
        #[cfg(feature = "mcp")]
        Command::Mcp => (logging::Terminal::Stderr, None),
    };
    if let Err(e) = logging::init(cli.level, log_file, log_to) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    match &cli.command {
        Command::Render(args) => render(args),
        Command::Ls {
            kind,
            input,
            json,
            frame,
        } => ls(*kind, input, json.as_deref(), *frame),
        Command::Diagnostic(args) => diagnostic(args),
        Command::Check(args) => check(args),
        Command::Diff { a, b, json } => diff(a, b, json.as_deref()),
        #[cfg(feature = "mcp")]
        Command::Mcp => mcp::run(),
    }
}

/// `crust diff`'s exit status for an error: unreadable input or an output
/// that cannot be written (a usage error exits 2 through clap as well).
const DIFF_ERROR: u8 = 2;

/// `crust diff`: read both files through crust-assets, compare them in
/// crust-core, print or write the report. 0 identical, 1 differs, 2 error.
fn diff(a: &Path, b: &Path, json: Option<&Path>) -> ExitCode {
    let read = |path: &Path| {
        crust_assets::read_exr_planes(path).map_err(|e| {
            error!("{e}");
            ExitCode::from(DIFF_ERROR)
        })
    };
    let (a, b) = match (read(a), read(b)) {
        (Ok(a), Ok(b)) => (a, b),
        (Err(code), _) | (_, Err(code)) => return code,
    };
    let report = crust_core::compare::compare(&a, &b);
    match json {
        Some(path) if is_stdout(path) => {
            if let Err(e) = write_json(path, &report.to_json()) {
                error!("--json -: {e}");
                return ExitCode::from(DIFF_ERROR);
            }
        }
        _ => {
            print!("{}", report.to_text());
            // On stderr, so stdout reads as the report always did.
            let c = &report.comparability;
            if c.status != crust_core::compare::ComparabilityStatus::Ok {
                let status = match c.status {
                    crust_core::compare::ComparabilityStatus::Warn => "warn",
                    _ => "unknown",
                };
                eprintln!("comparability: {status}");
                for note in &c.notes {
                    eprintln!("  {note}");
                }
            }
            if let Some(path) = json
                && let Err(e) = write_json(path, &report.to_json())
            {
                error!("--json {}: {e}", path.display());
                return ExitCode::from(DIFF_ERROR);
            }
        }
    }
    if report.identical {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// `crust check`'s exit status when a denied warning was raised: the
/// diagnostic's "report written, condition not met".
const DENIED: u8 = 3;

/// A `--deny` entry: a warning kind, or every kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum DenyKind {
    /// An invalid authored value was replaced by a fallback.
    Refused,
    /// A valid authored value is rendered differently from what it asks for.
    Approximated,
    /// Something authored contributes nothing.
    Skipped,
    /// Every kind.
    All,
}

impl DenyKind {
    fn kinds(self) -> &'static [WarningKind] {
        match self {
            DenyKind::Refused => &[WarningKind::Refused],
            DenyKind::Approximated => &[WarningKind::Approximated],
            DenyKind::Skipped => &[WarningKind::Skipped],
            DenyKind::All => &WarningKind::ALL,
        }
    }
}

/// `crust check`: import a stage as `crust render` would, render nothing,
/// and report what the render would use, what the import refused,
/// approximated or skipped, and which settings are worth changing — as text
/// on stdout, or as `crust-check/1` JSON. 0 clean, 3 a denied warning was
/// raised, 1 an error.
fn check(args: &CheckArgs) -> ExitCode {
    if args.scene.input.is_none() {
        use clap::CommandFactory;
        Cli::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "crust check needs a stage: -i <INPUT>",
            )
            .exit();
    }
    if let Some(ocio) = &crust_core::config().ocio
        && let Err(e) = crust_core::color::use_config(ocio)
    {
        error!("$OCIO: {e}");
        return ExitCode::FAILURE;
    }
    let checked = match import_checked(args) {
        Ok(checked) => checked,
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    checked.assets.release_texture_files();
    let report = checked.report;

    // The reports first, the status after: a report that cannot be written
    // is an error even when a warning was denied. The JSON file before the
    // text, so a failed write prints no report.
    match args.json.as_deref() {
        Some(path) => {
            if let Err(e) = write_json(path, &report.to_json()) {
                error!("--json {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
            if !is_stdout(path) {
                print!("{}", report.to_text());
                info!("Report written to {}", path.display());
            }
        }
        None => print!("{}", report.to_text()),
    }
    if report.denied.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(DENIED)
    }
}

/// A stage imported as `crust render` would import it, with what `crust
/// check` reports on it: what `check` prints, and what an MCP session keeps
/// to render from and to answer its `check` tool with.
struct Checked {
    /// The imported scene, its settings with the scene flags applied. Its
    /// `aovs` and `warnings` are moved out, into `aovs` and the report.
    #[cfg_attr(
        not(feature = "mcp"),
        expect(dead_code, reason = "an MCP session renders it")
    )]
    scene: Scene,
    /// The products a render would write, resolved and refused as `render`
    /// resolves them (`select_products`); empty for the single beauty EXR.
    #[cfg_attr(
        not(feature = "mcp"),
        expect(dead_code, reason = "an MCP session renders them")
    )]
    aovs: AovRequest,
    report: CheckReport,
    /// The asset loader the scene was imported with, which owns its texture
    /// caches: kept as long as the scene is rendered.
    assets: FileAssets,
}

/// The body of `crust check`: import `args`' stage, choose its products and
/// build the `crust-check/1` report. The error is the message to log.
fn import_checked(args: &CheckArgs) -> std::result::Result<Checked, String> {
    let input = args
        .scene
        .input
        .as_ref()
        .ok_or("crust check needs a stage: -i <INPUT>")?;
    let assets = FileAssets::new().with_auto_tx(args.scene.auto_tx);
    let mut scene = load_scene(&args.scene, None, &assets)?;
    let import_peak = crust_core::peak_memory_bytes();
    let mut settings = apply_scene_overrides(&args.scene, scene.settings);
    if let Some(region) = args.scene.region {
        settings = settings
            .with_region(region)
            .map_err(|e| format!("--region: {e}"))?;
    }
    scene.settings = settings;
    let (w, h) = settings.get_dimensions();

    // What the render would write, resolved as it resolves it. The products
    // it refuses are the import's warnings too.
    let mut aovs = std::mem::take(&mut scene.aovs);
    let selecting = WarningScope::enter();
    select_products(&mut aovs, None);
    scene.warnings.extend(selecting.finish());
    let products = if aovs.products.is_empty() {
        vec![ProductInfo {
            prim: None,
            file: beauty_output(None).to_owned(),
            channels: ["R", "G", "B"].map(String::from).to_vec(),
        }]
    } else {
        aovs.products
            .iter()
            .map(|p| ProductInfo {
                prim: Some(p.prim_path.clone()),
                file: p.name.clone(),
                channels: crate::products::product_channels(p)
                    .into_iter()
                    .flat_map(|(_, names)| names)
                    .collect(),
            })
            .collect()
    };

    let flags = SceneFlags {
        subdivision_level: args.scene.subdiv_level,
        subdivision_edge_length: args.scene.subdiv_edge_length,
        auto_tx: args.scene.auto_tx,
    };
    let preloaded = assets.texture_cache_stats().preloaded;
    let facts = Facts::from_import(&scene, args.scene.auto_tx, preloaded, import_peak);
    let warnings = std::mem::take(&mut scene.warnings);
    let kinds: Vec<WarningKind> = args.deny.iter().flat_map(|d| d.kinds()).copied().collect();
    let report = CheckReport {
        scene: SceneInfo {
            path: input.clone(),
            frame: args.scene.frame,
            camera: scene.camera_path.clone(),
            resolution: [w, h],
            region: (!settings.is_full_frame()).then(|| {
                let r = settings.region();
                [r.x0, r.y0, r.x1, r.y1]
            }),
        },
        products,
        effective_settings: effective_settings(&settings, &flags),
        import: scene.stats.phases.clone(),
        counts: scene.stats.scene,
        findings: checks::run(&facts),
        denied: crust_core::check::denied(&warnings, &kinds),
        warnings,
    };
    Ok(Checked {
        scene,
        aovs,
        report,
        assets,
    })
}

/// `crust diagnostic`: import the stage once, diagnose it, print the
/// Markdown report and write the JSON one. The exit status is the report's.
fn diagnostic(args: &DiagnosticArgs) -> ExitCode {
    let Some(input) = &args.scene.input else {
        use clap::CommandFactory;
        Cli::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "crust diagnostic needs a stage: -i <INPUT>",
            )
            .exit();
    };
    // Read before the import, so a wrong path fails in a second.
    let previous = match &args.baseline {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(text) => Some(text),
            Err(e) => {
                error!("--baseline {}: {e}", path.display());
                return ExitCode::FAILURE;
            }
        },
        None => None,
    };
    if let Some(ocio) = &crust_core::config().ocio
        && let Err(e) = crust_core::color::use_config(ocio)
    {
        error!("$OCIO: {e}");
        return ExitCode::FAILURE;
    }
    let assets = FileAssets::new().with_auto_tx(args.scene.auto_tx);
    let import_start = Instant::now();
    let mut scene = match load_scene(&args.scene, None, &assets) {
        Ok(scene) => scene,
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let import = import_start.elapsed();
    let settings = apply_scene_overrides(&args.scene, scene.settings);
    let (w, h) = settings.get_dimensions();
    if let Some(region) = args.scene.region
        && region.clip_to(w, h).is_none()
    {
        error!("--region: {region} has no pixel inside the {w}x{h} frame");
        return ExitCode::FAILURE;
    }
    scene.settings = settings;
    info!(
        "Diagnosing {input} ({w}x{h}), imported in {import:.2?}, budget {:?}",
        args.budget
    );
    let cache_stats = || (assets.texture_cache_stats(), assets.ptex_stats());
    let options = crust_core::diagnostic::Options {
        frame: args.scene.frame,
        camera: args.scene.camera.clone(),
        region: args.scene.region,
        budget: args.budget,
        repeats: args.repeats,
        target_mrse: args.target_mrse,
        previous,
        import,
        auto_tx: args.scene.auto_tx,
        subdivision_level: args.scene.subdiv_level,
        subdivision_edge_length: args.scene.subdiv_edge_length,
        cache_stats: Some(&cache_stats),
        ..crust_core::diagnostic::Options::new(input.clone())
    };
    let report = crust_core::diagnostic::run(scene, &options);
    assets.release_texture_files();
    print!("{}", report.to_markdown());
    if let Err(e) = std::fs::write(&args.json, report.to_json()) {
        error!("--json {}: {e}", args.json.display());
        return ExitCode::FAILURE;
    }
    info!("Report written to {}", args.json.display());
    match report.run.exit {
        0 => ExitCode::SUCCESS,
        code => ExitCode::from(code as u8),
    }
}

/// `crust ls`: print the stage's `kind` prims, one path per line; with
/// `--json`, write them with their values as `crust-ls/1` (on stdout for
/// `-`, instead of the paths).
fn ls(kind: LsKind, input: &Path, json: Option<&Path>, frame: Option<f64>) -> ExitCode {
    let empty = |n: usize| {
        if n == 0 {
            let name = kind.to_possible_value().expect("no skipped variant");
            warn!("{} has no {}", input.display(), name.get_name());
        }
    };
    let Some(json) = json else {
        if frame.is_some() {
            debug!("-f only changes the values --json reports; the paths are the same");
        }
        return match Scene::list_usd(input, kind.into()) {
            Ok(prims) => {
                empty(prims.len());
                for prim in prims {
                    println!("{prim}");
                }
                ExitCode::SUCCESS
            }
            Err(e) => {
                error!("Failed to read USD scene: {e}");
                ExitCode::FAILURE
            }
        };
    };
    match Scene::list_usd_records(input, kind.into(), frame) {
        Ok(records) => {
            empty(records.len());
            if !is_stdout(json) {
                for r in &records {
                    println!("{}", r.path());
                }
            }
            let listing = crust_core::Listing {
                kind: crust_core::ListKind::from(kind).name(),
                frame,
                prims: records,
            };
            if let Err(e) = write_json(json, &listing.to_json()) {
                error!("--json {}: {e}", json.display());
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!("Failed to read USD scene: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The exit status of a render Ctrl-C stopped: 128 + SIGINT, what a shell
/// reports for a process the signal ended.
const INTERRUPTED: u8 = 130;

/// What Ctrl-C does to `crust render` depends on where it stands (design
/// D7); this is what the `ctrlc` handler, on a thread of its own, shares
/// with the render.
struct Interrupt {
    /// [`Interrupt::LOADING`], [`Interrupt::RENDERING`] or
    /// [`Interrupt::WRITING`].
    stage: AtomicU8,
    /// The render's control: the first Ctrl-C while rendering cancels it.
    control: RenderControl,
}

impl Interrupt {
    /// Importing the stage and building the renderer: nothing to keep yet,
    /// and nothing there can be cancelled.
    const LOADING: u8 = 0;
    /// Tracing: Ctrl-C stops the render, whose outputs are then written.
    const RENDERING: u8 = 1;
    /// Writing the outputs, or about to: Ctrl-C quits.
    const WRITING: u8 = 2;

    /// Loading, with a control nothing has cancelled, which takes the
    /// render's snapshots only when `snapshots` (`--checkpoint`) asks for
    /// them: publishing costs the render a copy per unit per stage.
    fn new(snapshots: bool) -> Self {
        Interrupt {
            stage: AtomicU8::new(Self::LOADING),
            control: if snapshots {
                RenderControl::new()
            } else {
                RenderControl::without_snapshots()
            },
        }
    }

    fn enter(&self, stage: u8) {
        self.stage.store(stage, Ordering::SeqCst);
    }

    /// Moves from rendering to writing once the render has returned, in one
    /// atomic step against the handler, and says whether a Ctrl-C was
    /// accepted while rendering — one that may have landed after the last
    /// sample, cancelling nothing, but that still asked the run to stop.
    fn leave_rendering(&self) -> bool {
        self.stage.swap(Self::WRITING, Ordering::SeqCst) == Self::WRITING
    }

    /// The SIGINT handler: the first Ctrl-C while rendering cancels the
    /// render and moves on to writing; any other quits at once, with
    /// [`INTERRUPTED`], writing nothing further. It runs on `ctrlc`'s own
    /// thread, not in signal context, so exiting from it is sound — and it
    /// is the one place this binary exits without returning through `main`.
    fn on_signal(&self) {
        let rendering = self.stage.compare_exchange(
            Self::RENDERING,
            Self::WRITING,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
        if rendering.is_ok() {
            self.control.cancel();
            info!(
                "Ctrl-C: stopping the render to write what it has traced; Ctrl-C again quits \
                 without writing"
            );
        } else {
            std::process::exit(i32::from(INTERRUPTED));
        }
    }
}

/// How often the `--checkpoint` thread looks whether the render has ended,
/// so the final outputs never wait on it for longer than this.
const CHECKPOINT_POLL: Duration = Duration::from_millis(50);

/// `--checkpoint`: until `finished`, every `every` rewrites the PNG preview
/// at `path` from `control`'s latest snapshot, when the render published
/// since the last rewrite. Each rewrite goes to a sibling file renamed over
/// `path`, so a viewer reloading it never reads half an image. A failed
/// write is said once; the render goes on either way.
fn checkpoints(
    control: &RenderControl,
    every: Duration,
    path: &Path,
    color: &OutputColor,
    finished: &AtomicBool,
) {
    // A product's directory is otherwise created by its EXR, at the end.
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(dir);
    }
    let partial = path.with_extension("png.partial");
    let (mut written, mut failed) = (0u64, false);
    let mut next = Instant::now() + every;
    while !finished.load(Ordering::Acquire) {
        let now = Instant::now();
        if now < next {
            std::thread::sleep((next - now).min(CHECKPOINT_POLL));
            continue;
        }
        next = now + every;
        if control.generation() == written {
            continue;
        }
        let Some((generation, image)) = control.snapshot() else {
            continue;
        };
        let wrote = write_png(&image, &partial, color)
            .map_err(|e| e.to_string())
            .and_then(|()| std::fs::rename(&partial, path).map_err(|e| e.to_string()));
        match wrote {
            // Once per interval: a count that grows with the render's length.
            Ok(()) => {
                written = generation;
                debug!("--checkpoint: {} rewritten", path.display());
            }
            Err(e) if !failed => {
                failed = true;
                warn!(
                    "--checkpoint: cannot write {}: {e}; the render goes on",
                    path.display()
                );
            }
            Err(_) => {}
        }
    }
}

/// What an interrupted render reached, for its warning: the fewest and most
/// samples a pixel of its final pass took. Every final pass fills those
/// counters, adaptive or not; a guided render has none when it stopped in
/// training, or before its final pass gave every pixel the two samples it
/// needs to join the blend — its image is then its training passes'.
fn samples_reached(rays: &RayStats, spp: u32) -> String {
    if rays.adaptive_pixels == 0 {
        "it stopped before path guiding's final pass gave every pixel two samples, so the \
         image is made of its training passes"
            .to_owned()
    } else if rays.spp_min == rays.spp_max {
        format!("every pixel took {} of {spp} samples", rays.spp_min)
    } else {
        format!(
            "pixels took {} to {} of {spp} samples",
            rays.spp_min, rays.spp_max
        )
    }
}

/// `crust render`: build the scene, render it, write the images.
fn render(cli: &RenderArgs) -> ExitCode {
    // First, so a Ctrl-C during the import quits as it always did, but with
    // the status a render it stopped exits with.
    let interrupt = Arc::new(Interrupt::new(cli.checkpoint.is_some()));
    {
        let interrupt = Arc::clone(&interrupt);
        if let Err(e) = ctrlc::set_handler(move || interrupt.on_signal()) {
            warn!("Ctrl-C cannot be caught ({e}): it will end the render without writing anything");
        }
    }
    let run = RenderRun {
        control: &interrupt.control,
        progress_bar: true,
        rendering: &|| interrupt.enter(Interrupt::RENDERING),
        rendered: &|| interrupt.leave_rendering(),
        anchor: None,
        beauty: None,
    };
    match render_and_write(cli, &run) {
        Ok(written) if written.stopped || written.interrupted => ExitCode::from(INTERRUPTED),
        Ok(_) => ExitCode::SUCCESS,
        Err(code) => code,
    }
}

/// What a render-and-write runs under besides its arguments: how its caller
/// watches and stops it, and where its files go. `crust render` and an MCP
/// session's final render differ only here (design D8 of `mcp-session`).
struct RenderRun<'a> {
    /// The render's control: cancelling it stops the render, whose outputs
    /// are then written; its snapshots feed `--checkpoint`.
    control: &'a RenderControl,
    /// Draw a progress bar on the terminal.
    progress_bar: bool,
    /// Called just before the render starts tracing (and before its banner).
    rendering: &'a dyn Fn(),
    /// Called once the render has returned: whether the caller was asked to
    /// stop while it rendered (`crust render`'s Ctrl-C), even if that
    /// cancelled nothing.
    rendered: &'a dyn Fn() -> bool,
    /// The directory relative product paths resolve against, instead of the
    /// working directory.
    anchor: Option<&'a Path>,
    /// Where the beauty goes on a stage without RenderProducts when `-o` is
    /// not given, instead of `output.exr`.
    beauty: Option<&'a str>,
}

/// What a render-and-write did.
struct Written {
    /// Every file written, the EXRs and the PNG preview, in order.
    #[cfg_attr(
        not(feature = "mcp"),
        expect(dead_code, reason = "render_final lists them")
    )]
    files: Vec<PathBuf>,
    /// The render was cancelled: the files hold what it traced.
    interrupted: bool,
    /// The caller was asked to stop while it rendered ([`RenderRun::rendered`]).
    stopped: bool,
}

/// The body of `crust render`, which an MCP session's `render_final` runs
/// too: import the stage, choose the products, render, write each product's
/// EXR (or the beauty's) and the PNG preview, and report the statistics. A
/// failure is already logged; the error is the exit code.
fn render_and_write(cli: &RenderArgs, run: &RenderRun) -> std::result::Result<Written, ExitCode> {
    let output = cli.output.clone();
    // Built before the scene and kept until after the render: it owns the
    // streaming tile cache, whose counters the `--stats` report reads once the
    // last ray has been traced.
    // `--ocio-config`, else `$OCIO` as every OCIO application reads it, else
    // the builtin config.
    let ocio = match (&cli.ocio_config, &crust_core::config().ocio) {
        (Some(flag), _) => Some((flag, "--ocio-config")),
        (None, Some(env)) => Some((env, "$OCIO")),
        (None, None) => None,
    };
    if let Some((config, from)) = ocio {
        debug!("OCIO config {config} (from {from})");
        if let Err(e) = crust_core::color::use_config(config) {
            error!("{from}: {e}");
            return Err(ExitCode::FAILURE);
        }
    }
    let assets = FileAssets::new().with_auto_tx(cli.auto_tx);
    let load_start = Instant::now();
    let scene = match load_scene(cli, cli.working_space.as_ref(), &assets) {
        Ok(scene) => scene,
        Err(e) => {
            error!("{e}");
            return Err(ExitCode::FAILURE);
        }
    };
    debug!("Scene built in {:?}", load_start.elapsed());
    let output_color = match OutputColor::new(scene.working_space, &cli.display, &cli.view) {
        Ok(c) => c,
        Err(e) => {
            error!("{e}");
            return Err(ExitCode::FAILURE);
        }
    };
    // One line however many textures were converted — the per-file lines are
    // DEBUG, since their count grows with the stage.
    let (converted, failed, secs) = assets.tx_report();
    if converted + failed > 0 {
        info!("--auto-tx: converted {converted} texture tile(s) to .tx in {secs:.1}s");
        if failed > 0 {
            warn!("--auto-tx: {failed} tile(s) failed to convert; their textures were preloaded");
        }
    }
    // What to write: the stage's RenderProducts, with `-o` replacing the
    // first one's path; with none, the single beauty EXR at `-o`.
    let mut aovs = scene.aovs;
    select_products(&mut aovs, output.as_deref());
    if let Some(dir) = run.anchor {
        for product in &mut aovs.products {
            if Path::new(&product.name).is_relative() {
                product.name = dir.join(&product.name).to_string_lossy().into_owned();
            }
        }
    }
    let beauty_path = match (output.as_deref(), run.beauty) {
        (None, Some(beauty)) => beauty,
        (output, _) => beauty_output(output),
    };
    let camera = scene.camera;
    let world = scene.world;
    let lights = scene.lights;
    let volumes = scene.volumes;
    let (camera_path, time) = (scene.camera_path, scene.time);
    // Import phases and scene counts come from the loader; render and
    // output are timed here.
    let mut stats = scene.stats;
    let mut settings = apply_overrides(cli, scene.settings);
    // After the import, because clipping needs the resolution; it replaces
    // whatever region the stage's `dataWindowNDC` chose.
    if let Some(region) = cli.region {
        settings = match settings.with_region(region) {
            Ok(s) => {
                debug!(
                    "--region {} overrides the scene's dataWindowNDC",
                    s.region()
                );
                s
            }
            Err(e) => {
                error!("--region: {e}");
                return Err(ExitCode::FAILURE);
            }
        };
    }
    // A BVH can only cull primitives whose bounds are small against the
    // whole scene. Report the ratio so a scene whose instance boxes all
    // span everything -- where no split can help -- is visible.
    {
        let (n, scene_diag, mean_diag, max_diag) = world.primitive_extents();
        if n > 0 && scene_diag > 0.0 {
            debug!(
                "top-level extents: {n} prims, scene diagonal {scene_diag:.1}, \
                 mean prim {mean_diag:.1} ({:.4} of scene), max prim {max_diag:.1} ({:.4})",
                mean_diag / scene_diag,
                max_diag / scene_diag
            );
        }
    }
    debug!("Render Settings: {:#?}", settings);
    // The loader recorded the scene's own settings; re-read them now that
    // the CLI's --samples / --strategy overrides have been applied, so the
    // report describes the render that actually ran.
    stats.image = (&settings).into();
    crust_core::profile::set_enabled(cli.profile);
    // Timer
    let start = Instant::now();
    // World

    // Camera
    let (img_width, img_height) = settings.get_dimensions();
    let renderer = Renderer::new(camera, world, lights, settings).with_volumes(volumes);
    // Before the banner, so a Ctrl-C after it always finds the render to stop.
    (run.rendering)();
    info!(
        "Rendering {}x{}{} at {} spp, max depth {} ({} order){}{}",
        img_width,
        img_height,
        if settings.is_full_frame() {
            String::new()
        } else {
            let r = settings.region();
            format!(" region {r} ({}x{})", r.width(), r.height())
        },
        settings.samples_per_pixel(),
        settings.max_depth(),
        if cli.scanline { "scanline" } else { "bucket" },
        match cli.frame {
            Some(frame) => format!(", frame {frame}"),
            None => String::new(),
        },
        // Biased, so a render that clamps says so in its one banner line.
        if let Some(limit) = settings.indirect_clamp() {
            format!(", indirect clamp {limit}")
        } else {
            String::new()
        }
    );
    // Progress bar over the engine's (completed, total) callback — the
    // total (rows vs. tiles) is only known once the pass starts.
    let bar = if run.progress_bar {
        ProgressBar::new(0)
    } else {
        ProgressBar::hidden()
    };
    bar.set_style(
        indicatif::ProgressStyle::default_bar()
            .template(
                "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {pos}/{len} ({eta})",
            )
            .unwrap(),
    );
    let progress_bar = bar.clone();
    let progress = move |done: u64, total: u64| {
        if progress_bar.length() != Some(total) {
            progress_bar.set_length(total);
        }
        progress_bar.set_position(done);
    };
    // `--checkpoint`'s preview: where the final PNG goes, if anywhere.
    let checkpoint = cli.checkpoint.and_then(|every| {
        let path = match aovs.products.first() {
            None => preview_png(beauty_path),
            Some(first) if first.beauty().is_some() => preview_png(&first.name),
            Some(first) => {
                warn!(
                    "--checkpoint: {} has no beauty var, so no preview will be written",
                    first.prim_path
                );
                return None;
            }
        };
        Some((every, path))
    });
    let control = run.control;
    let request = (!aovs.products.is_empty()).then_some(&aovs);
    let finished = AtomicBool::new(false);
    let rendered = std::thread::scope(|s| {
        if let Some((every, path)) = &checkpoint {
            let (color, finished) = (&output_color, &finished);
            s.spawn(move || checkpoints(control, *every, path, color, finished));
        }
        let rendered =
            renderer.render_with_control(!cli.scanline, Some(&progress), request, control);
        finished.store(true, Ordering::Release);
        rendered
    });
    // From here on a Ctrl-C quits: the render has returned, and what is left
    // is writing it. A Ctrl-C accepted while rendering ends the run with
    // `INTERRUPTED` whether or not it cut the render short; the outputs say
    // they are partial only when it did.
    let stopped = (run.rendered)();
    let interrupted = rendered.outcome == RenderOutcome::Cancelled;
    let (buffer, film, ray_stats) = (rendered.buffer, rendered.film, rendered.rays);
    if interrupted {
        bar.abandon();
    } else {
        bar.finish();
    }
    // Close Timer
    let duration: Duration = start.elapsed();
    stats.record("Render", 0, duration);
    stats.rays = ray_stats;
    // Snapshotted after the render rather than during it: the counters are
    // relaxed atomics bumped from every worker, so they are only meaningful
    // once the last one has stopped.
    stats.textures = assets.texture_cache_stats();
    stats.ptex = assets.ptex_stats();
    stats.inventory_from(&renderer.world, &renderer.lights);
    if cli.profile {
        stats.profile = crust_core::profile::take();
    }
    // Lost texels are said whether or not `--stats` is on: each failing file
    // was already named once, and this is the total they cost.
    if stats.textures.errors > 0 {
        warn!(
            "{} texture tile read(s) failed; those lookups used their texture's fallback colour",
            stats.textures.errors
        );
    }
    // Before any output is written, so a render that streamed thousands of
    // `.tx` files never fails its write for want of a descriptor.
    assets.release_texture_files();
    if interrupted {
        // The spp budget the stage or `-s` asked for was not honoured.
        warn!(
            "Render interrupted after {duration:?}: {}; writing what it traced",
            samples_reached(&ray_stats, settings.samples_per_pixel())
        );
    } else if stopped {
        info!("Render finished in {duration:?}, as Ctrl-C arrived; writing its complete outputs");
    } else {
        info!("Render finished in {duration:?}");
    }
    // How the pixels were sampled, recorded in every EXR written.
    let sampling = SamplingStamp::new(&settings, &ray_stats, camera_path.as_deref(), time)
        .for_outcome(rendered.outcome, &ray_stats);
    let output_start = Instant::now();
    let mut files = Vec::new();
    if let Some(film) = &film {
        // One EXR per product, then the PNG from the first one's beauty.
        let mut written = Vec::new();
        for product in &aovs.products {
            let path = Path::new(&product.name);
            match products::write_product(
                path,
                product,
                &buffer,
                film,
                &output_color,
                &sampling,
                interrupted,
            ) {
                Ok(channels) => {
                    debug!("{}: {}", path.display(), channels.join(" "));
                    written.push(format!("{} ({} channels)", path.display(), channels.len()));
                    files.push(path.to_path_buf());
                }
                Err(e) => {
                    error!("Error writing {}: {e}", product.prim_path);
                    return Err(ExitCode::FAILURE);
                }
            }
        }
        info!("Products written: {}", written.join(", "));
        let first = &aovs.products[0];
        if first.beauty().is_some() {
            let png_path = preview_png(&first.name);
            match write_png(&buffer, &png_path, &output_color) {
                Ok(_) => info!("Image written to: {:?}", png_path),
                Err(e) => {
                    error!("Error writing PNG: {}", e);
                    return Err(ExitCode::FAILURE);
                }
            }
            files.push(png_path);
        } else {
            debug!("{} has no beauty var; no PNG preview", first.prim_path);
        }
    } else {
        write_beauty(&buffer, beauty_path, &output_color, &sampling, interrupted)?;
        files.push(PathBuf::from(beauty_path));
        files.push(preview_png(beauty_path));
    }
    let output_elapsed = output_start.elapsed();
    stats.record("Write output", 0, output_elapsed);
    debug!("Output written in {output_elapsed:?}");

    // Traversal counts, when built with the diagnostic feature. Printed
    // separately from RenderStats because they come from the kernel and
    // only exist in a feature-on build.
    #[cfg(feature = "traversal-stats")]
    if cli.stats || cli.profile {
        let out = crust_core::traversal_report(&renderer.world, ray_stats.camera_rays);
        info!(target: STATS_TARGET, "{out}");
    }

    // One snapshot, after every output is written, for both forms of the
    // report.
    stats.peak_memory_bytes = crust_core::peak_memory_bytes();
    if cli.stats || cli.profile {
        // Through `tracing` rather than `println!`, so the report reaches
        // every sink the run configured — `--log-file` above all, which is
        // where a record of a render is least useful without its profile.
        // The level cannot suppress it (see `STATS_TARGET`), so the two
        // reasons it used to bypass the logger are both answered.
        //
        // One event rather than one per line, and opened with a newline: a
        // per-line emit would stamp all 40 rows, and without the newline the
        // event prefix would indent the first rule and only that one, so the
        // table's top edge would not line up with the rest of it.
        info!(target: STATS_TARGET, "\n{stats}");
    }
    if let Some(path) = &cli.stats_json {
        if let Err(e) = write_json(path, &stats.to_json()) {
            error!(
                "--stats-json {}: {e} (the images are written)",
                path.display()
            );
            return Err(ExitCode::FAILURE);
        }
        if !is_stdout(path) {
            debug!("Statistics written to {}", path.display());
        }
    }
    Ok(Written {
        files,
        interrupted,
        stopped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `crust render <args>`, parsed down to the render's own arguments.
    fn render<const N: usize>(args: [&str; N]) -> std::result::Result<RenderArgs, clap::Error> {
        let cli = Cli::try_parse_from(["crust", "render"].into_iter().chain(args))?;
        match cli.command {
            Command::Render(args) => Ok(*args),
            _ => unreachable!("parsed as render"),
        }
    }

    #[test]
    fn log_file_flag_is_optional_and_takes_an_optional_directory() {
        // Absent: no file.
        assert_eq!(render([]).unwrap().log_file, None);
        // Bare: the current directory.
        let r = render(["--log-file"]).unwrap();
        assert_eq!(r.log_file.as_deref(), Some(std::path::Path::new(".")));
        // With a directory.
        let r = render(["--log-file", "renders/logs"]).unwrap();
        assert_eq!(
            r.log_file.as_deref(),
            Some(std::path::Path::new("renders/logs"))
        );
        // Bare, followed by another flag: the flag must not be eaten as the
        // directory, which is what `num_args = 0..=1` is there to guarantee.
        let r = render(["--log-file", "--bucket"]).unwrap();
        assert_eq!(r.log_file.as_deref(), Some(std::path::Path::new(".")));
        assert!(r.bucket);
        // It is render's alone: before the subcommand it would take the
        // subcommand's name as its directory, and `ls` has a positional it
        // would take the same way. Both are refused, not misread.
        assert!(Cli::try_parse_from(["crust", "--log-file", "render"]).is_err());
        assert!(
            Cli::try_parse_from(["crust", "ls", "--log-file", "camera", "-i", "s.usda"]).is_err()
        );
        assert!(
            Cli::try_parse_from(["crust", "ls", "camera", "-i", "s.usda", "--log-file"]).is_err()
        );
    }

    /// The default outputs: `lin_rec709`, previewed un-tone-mapped on sRGB.
    pub(crate) fn rec709() -> OutputColor {
        let w = crust_core::color::Space::LIN_REC709;
        OutputColor::new(
            w,
            crust_core::color::PREVIEW_DISPLAY,
            crust_core::color::PREVIEW_VIEW,
        )
        .expect("the default preview")
    }

    /// One channel through [`tone_map`].
    fn tone_map1(linear: f32) -> u8 {
        tone_map(&mut [linear; 3], &rec709())[0]
    }

    #[test]
    fn a_preview_the_config_cannot_make_is_refused_before_the_render() {
        let w = crust_core::color::Space::LIN_REC709;
        assert!(OutputColor::new(w, "sRGB - Display", "no such view").is_err());
        assert!(OutputColor::new(w, "no such display", "Un-tone-mapped").is_err());
        let aces = crust_core::color::working_space("acescg").unwrap();
        let view = "ACES 2.0 - SDR 100 nits (Rec.709)";
        let c = OutputColor::new(aces, "sRGB - Display", view).expect("an ACES view");
        assert!(c.exr_chromaticities().is_some());
        assert!(rec709().exr_chromaticities().is_none());
    }

    #[test]
    fn tone_map_anchors_black_and_white() {
        assert_eq!(tone_map1(0.0), 0);
        assert_eq!(tone_map1(1.0), 255);
        // Out-of-range input clamps rather than wrapping.
        assert_eq!(tone_map1(-3.0), 0);
        assert_eq!(tone_map1(50.0), 255);
        assert_eq!(tone_map1(f32::INFINITY), 255);
    }

    #[test]
    fn tone_map_applies_the_srgb_curve() {
        // Linear 0.5 is display 188; linear 0.214 is display ~128.
        assert_eq!(tone_map1(0.5), 188);
        assert!((tone_map1(0.214) as i32 - 128).abs() <= 1);
        // The linear toe: 0.001 linear → 12.92 · 0.001 · 255 ≈ 3.3 → 3.
        assert_eq!(tone_map1(0.001), 3);
    }

    #[test]
    fn tone_map_is_monotone() {
        let mut prev = 0u8;
        for i in 0..=1000 {
            let v = tone_map1(i as f32 / 1000.0);
            assert!(v >= prev, "not monotone at {i}");
            prev = v;
        }
    }

    /// The CLI parses straight into the engine's enums, through their own
    /// name tables: every name the engine knows is a CLI value, and parses to
    /// the value the engine means by it.
    #[test]
    fn cli_names_are_the_engine_names() {
        let parse = |flag: &str, value: &str| render([flag, value]).expect("a known name");
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
        let cli = render([]).unwrap();
        assert!(cli.light_selection.is_none());
    }

    #[test]
    fn cli_indirect_clamp_defaults_to_ten_and_zero_disables() {
        let cli = render(["--indirect-clamp", "10"]).unwrap();
        assert_eq!(cli.indirect_clamp, Some(10.0));
        assert!(render([]).unwrap().indirect_clamp.is_none());
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

    /// The light sample counts parse as counts of at least one, override
    /// the scene one at a time, and leave the scene's value alone when
    /// absent; a zero is a usage error, not a silent clamp.
    #[test]
    fn cli_light_samples_are_counts_of_at_least_one() {
        let cli = render(["--light-samples", "4", "--light-samples-indirect", "2"]).unwrap();
        assert_eq!(cli.light_samples, Some(4));
        assert_eq!(cli.light_samples_indirect, Some(2));
        assert!(render(["--light-samples", "0"]).is_err());
        assert!(render(["--light-samples-indirect", "0"]).is_err());
        assert!(render(["--light-samples", "-1"]).is_err());
        assert!(render(["--light-samples", "1024"]).is_ok());
        assert!(render(["--light-samples", "1025"]).is_err());
        assert!(render(["--light-samples-indirect", "1000000000"]).is_err());
        let bare = render([]).unwrap();
        assert!(bare.light_samples.is_none() && bare.light_samples_indirect.is_none());

        let (_, base) = crust_core::get_settings();
        assert_eq!(
            (base.light_samples(), base.light_samples_indirect()),
            (1, 1)
        );
        let scene = base.with_light_samples(3, 5);
        let s = apply_overrides(&render(["--light-samples", "4"]).unwrap(), scene);
        assert_eq!((s.light_samples(), s.light_samples_indirect()), (4, 5));
        let s = apply_overrides(&render(["--light-samples-indirect", "2"]).unwrap(), scene);
        assert_eq!((s.light_samples(), s.light_samples_indirect()), (3, 2));
        let s = apply_overrides(&bare, scene);
        assert_eq!((s.light_samples(), s.light_samples_indirect()), (3, 5));
        assert_eq!(base.with_light_samples(0, 0).light_samples(), 1);
        assert_eq!(
            base.with_light_samples(u32::MAX, 5000)
                .light_samples_indirect(),
            crust_core::MAX_LIGHT_SAMPLES
        );
    }

    #[test]
    fn cli_filter_names_map_onto_the_engine_filters_at_their_default_radius() {
        let filter = |name: &str| render(["--filter", name]).unwrap().filter;
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
    fn cli_region_is_four_integers_with_x1_and_y1_past_x0_and_y0() {
        let region = |v: &str| render(["--region", v]).map(|c| c.region);
        assert_eq!(
            region("100,50,164,114").unwrap(),
            Some(PixelRect::new(100, 50, 164, 114))
        );
        assert_eq!(
            region(" 0, 0, 1, 1").unwrap(),
            Some(PixelRect::new(0, 0, 1, 1))
        );
        assert_eq!(render([]).unwrap().region, None);
        for bad in [
            "10,10,5,20",
            "0,0,4,0",
            "4,0,4,4",
            "1,2,3",
            "1,2,3,4,5",
            "-1,0,4,4",
            "a,b,c,d",
            "",
        ] {
            assert!(region(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn write_png_writes_only_the_region() {
        let mut buffer = Buffer::with_region(8, 4, PixelRect::new(2, 1, 5, 3));
        // Image (2, 1), the region's top-left: raster row 4 - 1 - 1.
        buffer.set_pixel(2, 2, crust_core::Vec3A::new(1.0, 0.0, 0.0));
        let dir = std::env::temp_dir().join("crust_render_png_region_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.png");
        write_png(&buffer, &path, &rec709()).expect("png written");
        let img = image::open(&path).expect("readable").to_rgba8();
        assert_eq!((img.width(), img.height()), (3, 2));
        assert_eq!(img.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(1, 1).0, [0, 0, 0, 255]);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn write_png_flips_rows_and_tone_maps() {
        let (w, h) = (3usize, 2usize);
        let mut buffer = Buffer::new(w, h);
        buffer.set_pixel(0, 0, crust_core::Vec3A::new(1.0, 0.0, 0.0)); // scene bottom-left
        buffer.set_pixel(2, 1, crust_core::Vec3A::new(0.0, 0.5, 0.0)); // scene top-right
        let dir = std::env::temp_dir().join("crust_render_png_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.png");
        write_png(&buffer, &path, &rec709()).expect("png written");
        let img = image::open(&path).expect("readable").to_rgba8();
        assert_eq!((img.width(), img.height()), (3, 2));
        // Image row 0 is the top: the scene's y = 1 row.
        assert_eq!(img.get_pixel(2, 0).0, [0, 188, 0, 255]);
        assert_eq!(img.get_pixel(0, 1).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(1, 1).0, [0, 0, 0, 255]);
        let _ = std::fs::remove_file(&path);
    }

    /// The Ctrl-C state machine keeps a Ctrl-C it accepted while rendering
    /// until the render returns, even one that landed after the render's
    /// last sample and so cancelled nothing: the run then still exits 130.
    /// Without one, leaving the render reports none.
    #[test]
    fn a_ctrl_c_accepted_while_rendering_survives_the_render_returning() {
        let undisturbed = Interrupt::new(false);
        undisturbed.enter(Interrupt::RENDERING);
        assert!(!undisturbed.leave_rendering());
        assert!(!undisturbed.control.is_cancelled());
        // The render has finished; the handler runs before the main thread
        // leaves the rendering stage.
        let late = Interrupt::new(false);
        late.enter(Interrupt::RENDERING);
        late.on_signal();
        assert!(late.control.is_cancelled());
        assert!(late.leave_rendering(), "the accepted Ctrl-C was lost");
    }

    /// The interruption warning says how far the final pass got, and that a
    /// guided render without one in its image shows its training passes.
    #[test]
    fn the_interruption_warning_names_the_samples_reached() {
        let pass = |pixels, min, max| crust_core::RayStats {
            adaptive_pixels: pixels,
            spp_min: min,
            spp_max: max,
            ..Default::default()
        };
        assert_eq!(
            samples_reached(&pass(1536, 4, 4), 64),
            "every pixel took 4 of 64 samples"
        );
        assert_eq!(
            samples_reached(&pass(1536, 0, 9), 64),
            "pixels took 0 to 9 of 64 samples"
        );
        assert!(samples_reached(&pass(0, 0, 0), 64).contains("training passes"));
    }

    /// `--checkpoint` takes a positive number of seconds, fractions
    /// allowed; zero, a negative, a non-number or an interval no `Duration`
    /// holds is a usage error. Without it, nothing is rewritten mid-render.
    #[test]
    fn cli_checkpoint_is_a_positive_number_of_seconds() {
        assert_eq!(render([]).unwrap().checkpoint, None);
        let every = |s: &str| render(["--checkpoint", s]).map(|r| r.checkpoint);
        assert_eq!(every("10").unwrap(), Some(Duration::from_secs(10)));
        assert_eq!(every("0.25").unwrap(), Some(Duration::from_millis(250)));
        for bad in ["0", "-1", "0.0", "nan", "inf", "1e30", "soon", ""] {
            let err = every(bad).expect_err(bad);
            assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation, "{bad}");
        }
    }

    #[test]
    fn cli_parses_its_flags() {
        let cli = render([
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
            "--frame",
            "1012.5",
        ])
        .expect("valid flags");
        assert_eq!(cli.frame, Some(1012.5));
        assert_eq!(cli.input.as_deref(), Some("scene.usda"));
        assert_eq!(cli.output.as_deref(), Some("out.exr"));
        assert!(cli.bucket, "the old flag still parses");
        assert!(!cli.scanline);
        assert_eq!(cli.samples, Some(12));
        assert_eq!(cli.strategy, Some(SamplingStrategy::BalanceMis));
        assert!(matches!(cli.filter, Some(PixelFilter::Mitchell { .. })));
        assert_eq!(cli.filter_radius, Some(1.75));
        assert!(cli.stats);
        assert!(cli.profile);
        let c = Cli::try_parse_from(["crust", "render", "-l", "debug"]).unwrap();
        assert!(matches!(c.level, LoggerLevel::Debug));
        let scan = render(["--scanline"]).expect("valid flags");
        assert!(scan.scanline);
    }

    #[test]
    fn cli_defaults_when_nothing_is_given() {
        let cli = render([]).expect("no flags is valid");
        assert!(cli.input.is_none());
        // No default here: `output.exr` applies only when the stage authors
        // no RenderProduct, which the CLI cannot know until it has loaded it.
        assert!(cli.output.is_none());
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
        let c = Cli::try_parse_from(["crust", "render"]).unwrap();
        assert!(matches!(c.level, LoggerLevel::Info));
    }

    #[test]
    fn cli_subdiv_level_overrides_the_scene() {
        let cli = render(["--subdiv-level", "0"]).expect("valid");
        assert_eq!(cli.subdiv_level, Some(0));
        let cli = render(["--subdiv-level", "3"]).expect("valid");
        assert_eq!(cli.subdiv_level, Some(3));
        assert!(
            render(["--subdiv-level", "-1"]).is_err(),
            "a level is a count"
        );
    }

    #[test]
    fn cli_subdiv_edge_length_is_a_positive_pixel_length() {
        let cli = render(["--subdiv-edge-length", "2"]).expect("valid");
        assert_eq!(cli.subdiv_edge_length, Some(2.0));
        assert!(render([]).unwrap().subdiv_edge_length.is_none());
        for bad in ["0", "-1", "inf", "NaN", "fast"] {
            let Err(err) = render(["--subdiv-edge-length", bad]) else {
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
        let cli = render(["-f", "-5"]).expect("negative frame");
        assert_eq!(cli.frame, Some(-5.0));
    }

    #[test]
    fn cli_rejects_a_non_finite_frame() {
        for bad in ["nan", "NaN", "inf", "-inf", "infinity", "-Infinity"] {
            assert!(
                render(["--frame", bad]).is_err(),
                "--frame {bad} must be rejected"
            );
        }
        assert!(render(["--frame", "twelve"]).is_err());
    }

    #[test]
    fn cli_rejects_a_radius_or_clamp_that_is_no_number_the_engine_uses() {
        for bad in ["-1", "nan", "inf", "0"] {
            assert!(
                render(["--filter-radius", bad]).is_err(),
                "--filter-radius {bad}"
            );
        }
        for bad in ["-1", "nan", "inf"] {
            assert!(
                render(["--indirect-clamp", bad]).is_err(),
                "--indirect-clamp {bad}"
            );
        }
        let ok = render(["--indirect-clamp", "0", "--filter-radius", "1.5"]).unwrap();
        assert_eq!(
            (ok.indirect_clamp, ok.filter_radius),
            (Some(0.0), Some(1.5))
        );
    }

    #[test]
    fn cli_rejects_unknown_enum_values() {
        assert!(render(["--strategy", "random"]).is_err());
        assert!(render(["--filter", "lanczos"]).is_err());
        assert!(render(["--light-selection", "bvh"]).is_err());
        assert!(render(["-l", "loud"]).is_err());
        assert!(Cli::try_parse_from(["crust", "render", "--camera"]).is_err());
        assert!(render(["-s", "many"]).is_err());
    }

    /// The subcommands: `render` takes the flags the bare binary used to,
    /// `ls <kind>` needs a kind and a stage, and a subcommand is required.
    #[test]
    fn cli_subcommands() {
        assert!(
            Cli::try_parse_from(["crust"]).is_err(),
            "a subcommand is required"
        );
        assert!(
            Cli::try_parse_from(["crust", "-i", "scene.usda"]).is_err(),
            "render flags belong to render"
        );
        for (name, want) in [
            ("camera", crust_core::ListKind::Camera),
            ("cameras", crust_core::ListKind::Camera),
            ("light", crust_core::ListKind::Light),
            ("lights", crust_core::ListKind::Light),
            ("material", crust_core::ListKind::Material),
            ("materials", crust_core::ListKind::Material),
        ] {
            let c = Cli::try_parse_from(["crust", "ls", name, "-i", "scene.usda", "-l", "warn"])
                .expect("valid");
            assert!(matches!(c.level, LoggerLevel::Warn));
            let Command::Ls { kind, input, .. } = c.command else {
                panic!("ls")
            };
            assert_eq!(crust_core::ListKind::from(kind), want, "{name}");
            assert_eq!(input, std::path::Path::new("scene.usda"));
        }
        assert!(
            Cli::try_parse_from(["crust", "ls", "camera"]).is_err(),
            "ls needs -i"
        );
        assert!(
            Cli::try_parse_from(["crust", "ls", "-i", "s.usda"]).is_err(),
            "and a kind"
        );
        assert!(Cli::try_parse_from(["crust", "ls", "meshes", "-i", "s.usda"]).is_err());
        let c =
            Cli::try_parse_from(["crust", "diff", "a.exr", "b.exr", "--json", "-"]).expect("diff");
        let Command::Diff { a, b, json } = c.command else {
            panic!("diff")
        };
        assert_eq!((a.to_str(), b.to_str()), (Some("a.exr"), Some("b.exr")));
        assert_eq!(json.as_deref(), Some(Path::new("-")));
        let err = Cli::try_parse_from(["crust", "diff", "a.exr"])
            .err()
            .expect("two files");
        assert_eq!(err.exit_code(), 2, "a usage error exits 2");
    }

    /// `crust diagnostic <args>`, parsed down to its own arguments.
    fn diagnose<const N: usize>(
        args: [&str; N],
    ) -> std::result::Result<DiagnosticArgs, clap::Error> {
        let cli = Cli::try_parse_from(["crust", "diagnostic"].into_iter().chain(args))?;
        match cli.command {
            Command::Diagnostic(args) => Ok(*args),
            _ => unreachable!("parsed as diagnostic"),
        }
    }

    #[test]
    fn diagnostic_defaults_and_budgets() {
        let d = diagnose(["-i", "s.usda"]).unwrap();
        assert_eq!(d.budget, Duration::from_secs(120));
        assert_eq!(d.json, std::path::Path::new("crust-diagnostic.json"));
        assert_eq!(d.repeats, 3);
        assert!(d.baseline.is_none() && d.target_mrse.is_none());
        for (text, secs) in [
            ("90s", 90.0),
            ("5m", 300.0),
            ("1m30s", 90.0),
            ("1h", 3600.0),
            ("1.5s", 1.5),
            ("250ms", 0.25),
            ("45", 45.0),
        ] {
            assert_eq!(
                parse_duration(text),
                Ok(Duration::from_secs_f64(secs)),
                "{text}"
            );
        }
        for bad in ["", "0s", "-5s", "5x", "s", "m5", "nan", "inf"] {
            assert!(parse_duration(bad).is_err(), "{bad:?}");
        }
        // Finite but past what a `Duration` holds: a usage error, not a
        // panic in the parser.
        for huge in ["1e20", "99999999999999999999h", "1e300", "1e400"] {
            assert!(parse_duration(huge).is_err(), "{huge:?}");
        }
        assert!(diagnose(["-i", "s.usda", "--budget", "1e20"]).is_err());
        assert!(diagnose(["--repeats", "0"]).is_err());
        assert!(diagnose(["--target-mrse", "0"]).is_err());
        assert_eq!(
            diagnose(["--target-mrse", "0.001"]).unwrap().target_mrse,
            Some(0.001)
        );
    }

    /// The `cli` spec's scenario: a shared flag means the same in both
    /// subcommands, and the render-only ones are refused by `diagnostic`.
    #[test]
    fn diagnostic_shares_the_scene_flags_with_render() {
        let args = [
            "-i",
            "scene.usda",
            "-f",
            "12",
            "--camera",
            "/cam",
            "--region",
            "0,0,256,256",
            "--strategy",
            "balance",
            "--light-selection",
            "learned",
            "--light-samples",
            "2",
            "--light-samples-indirect",
            "3",
            "--indirect-clamp",
            "0",
            "--filter",
            "box",
            "--filter-radius",
            "0.75",
            "--subdiv-level",
            "2",
            "--subdiv-edge-length",
            "4",
            "--auto-tx",
        ];
        let d = diagnose(args).expect("every shared flag");
        let r = render(args).expect("the same flags render");
        let (_, base) = crust_core::get_settings();
        let ds = apply_scene_overrides(&d.scene, base);
        let rs = apply_overrides(&r, base);
        assert_eq!(format!("{ds:?}"), format!("{rs:?}"));
        assert_eq!(ds.light_selection(), LightSelection::Learned);
        assert_eq!(d.scene.region, r.region);
        assert_eq!(
            (d.scene.frame, d.scene.camera.as_deref()),
            (Some(12.0), Some("/cam"))
        );
        for render_only in [
            &["-s", "64"][..],
            &["-o", "x.exr"],
            &["--log-file"],
            &["--scanline"],
            &["--ocio-config", "x.ocio"],
            &["--working-space", "acescg"],
            &["--display", "sRGB - Display"],
            &["--profile"],
        ] {
            let mut a = vec!["-i", "s.usda"];
            a.extend_from_slice(render_only);
            let cli = Cli::try_parse_from(["crust", "diagnostic"].into_iter().chain(a));
            assert!(cli.is_err(), "{render_only:?} accepted");
        }
    }

    #[test]
    fn clap_definition_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
