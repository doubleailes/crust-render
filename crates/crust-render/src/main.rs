//! The CLI: parse args, build a `Scene`, render, write the images.
//!
//! `forbid(unsafe_code)`, like every crate here but `crust-core` (a test-only
//! counting allocator) and `crust-jit` (calling generated code).
#![forbid(unsafe_code)]

use clap::Parser;
use crust_assets::FileAssets;
use crust_core::Buffer;
use crust_core::LightSelection;
use crust_core::PixelFilter;
use crust_core::Renderer;
use crust_core::SamplingStrategy;
use crust_core::Scene;
use crust_core::{get_settings, simple_scene};
use exr::prelude::*;
use indicatif::ProgressBar;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};
use tracing::{Level, debug, error, info, warn};
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[derive(clap::ValueEnum, Clone, Debug, Copy)]
enum LoggerLevel {
    Debug,
    Info,
    Warn,
    Error,
    Trace,
}

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Input scene path — .usda / .usdc / .usdz.
    /// When absent, falls back to a hard-coded procedural scene.
    #[arg(short, long)]
    input: Option<String>,
    /// Output image path. Without RenderProducts on the stage, the linear EXR
    /// is written here (default `output.exr`) and a tone-mapped sRGB PNG next
    /// to it (same path with a .png extension). When the stage authors
    /// RenderProducts, this replaces the first product's `productName`, as
    /// husk's `-o` does; the other products keep theirs.
    #[arg(short, long)]
    output: Option<String>,
    /// Verbose level
    #[arg(short, long, default_value = "info")]
    level: LoggerLevel,
    /// Also write the log to a file named for the time the run started
    /// (`crust-render-<UTC timestamp>.log`). Bare, it writes into the
    /// current directory; given a directory, it writes there and creates it
    /// if needed. The file receives the same events as the terminal, so
    /// `-l debug --log-file` is how a full record of a render is kept.
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
    /// Convert UV textures to a tiled, mip-mapped `.tx` beside the original
    /// (same path, extension `.tx`) on first use, when the `.tx` is missing or
    /// older than its source. A `.tx` beside a texture is always streamed when
    /// present; this only creates the missing ones.
    #[arg(long, default_value_t = false)]
    auto_tx: bool,
    /// The OpenColorIO config every colour is managed with: a `.ocio` file,
    /// an `.ocioz` archive or an `ocio://` builtin URI. Defaults to the
    /// builtin ACES CG config (cg-config-v4.0.0_aces-v2.0_ocio-v2.5). It must
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

/// Target the `--stats` report is emitted under.
///
/// It exists so the report can be exempted from `-l`: `--stats` is an
/// explicit request for the report, and honouring it only at `-l info` or
/// below would mean `--stats -l warn` silently produced nothing. The filter
/// in `main` admits this target at any level and applies `-l` to everything
/// else, which is what keeps the report a log event — reaching `--log-file`
/// like any other — without letting the log level decide whether it appears.
const STATS_TARGET: &str = "crust_render::stats";

/// Whether an event at `level` on `target` survives a `-l max` filter.
///
/// Named rather than inlined into the closure so it can be tested: the whole
/// point of it is the one case that is easy to regress into silence —
/// [`STATS_TARGET`] passing at a level that rejects everything else.
fn event_enabled(target: &str, level: &Level, max: Level) -> bool {
    target == STATS_TARGET || effective_level(target, level) <= max
}

/// The level an event is filtered at, which for a few dependencies is not
/// the level it was emitted at.
///
/// `cranelift_jit` logs the whole IR of every function it defines at INFO —
/// one multi-hundred-line dump per MaterialX program, so a default render's
/// INFO output grew with the number of materials, against the rule that INFO
/// lines do not scale with the scene. `tracing` cannot rewrite an event's
/// level, so it is *filtered* as DEBUG (shown from `-l debug` on) while still
/// printing its own `INFO` stamp. WARN and ERROR from cranelift are untouched.
fn effective_level(target: &str, level: &Level) -> Level {
    if *level == Level::INFO && target.starts_with("cranelift") {
        Level::DEBUG
    } else {
        *level
    }
}

fn get_logger_level(level: LoggerLevel) -> Level {
    match level {
        LoggerLevel::Debug => Level::DEBUG,
        LoggerLevel::Info => Level::INFO,
        LoggerLevel::Warn => Level::WARN,
        LoggerLevel::Error => Level::ERROR,
        LoggerLevel::Trace => Level::TRACE,
    }
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

/// Tone-map the render buffer to an 8-bit PNG at `path`.
fn write_png(
    buffer: &Buffer,
    width: usize,
    height: usize,
    path: &Path,
    color: &OutputColor,
) -> std::result::Result<(), image::ImageError> {
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
    img.save(path)
}

/// The render of a stage without RenderProducts: the beauty as an RGB EXR at
/// `output`, then the tone-mapped PNG next to it. What `write_rgb_file` writes
/// — in `lin_rec709` this output has the header and pixels it had before AOVs —
/// plus, in any other working space, the chromaticities and `colorInteropID`
/// that say which.
fn write_beauty(
    buffer: &Buffer,
    img_width: usize,
    img_height: usize,
    output: &str,
    color: &OutputColor,
) -> std::result::Result<(), ExitCode> {
    debug!(
        "Writing {}x{} linear EXR to {}",
        img_width, img_height, output
    );
    let channels = SpecificChannels::rgb(|Vec2(x, y)| buffer.get_rgb(x, y));
    let mut image = Image::from_channels((img_width, img_height), channels);
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
    match image.write().to_file(output) {
        Ok(_) => info!("Image written to: {:?}", output),
        Err(e) => {
            error!("Error writing image: {}", e);
            return Err(ExitCode::FAILURE);
        }
    }
    let png_path = Path::new(output).with_extension("png");
    debug!("Tone mapping to sRGB PNG at {}", png_path.display());
    match write_png(buffer, img_width, img_height, &png_path, color) {
        Ok(_) => info!("Image written to: {:?}", png_path),
        Err(e) => {
            error!("Error writing PNG: {}", e);
            return Err(ExitCode::FAILURE);
        }
    }
    Ok(())
}

/// A filename-safe UTC timestamp, `YYYYMMDDTHHMMSSZ`.
///
/// Hand-rolled rather than pulled from `chrono` or `time`: neither is in the
/// dependency graph, and adding one to name a file would be the largest
/// dependency in this binary. `tracing-subscriber` formats its own line
/// timestamps the same way and for the same reason, so the `Z` suffix here
/// matches what the log lines themselves carry.
///
/// The civil-from-days conversion is Howard Hinnant's, shifting the era to
/// start on 0000-03-01 so a leap day lands at the end of a 400-year cycle and
/// the month arithmetic needs no table. Valid for any date this can be handed.
fn utc_stamp(t: std::time::SystemTime) -> String {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        // A clock before 1970 is not worth a failure path; it only names a file.
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // Days since 1970-01-01 -> civil date.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    // `mp` counts from March; roll it back to a calendar month, and with it
    // the year, which only advances once January is reached.
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = era * 400 + yoe + i64::from(month <= 2);

    format!("{year:04}{month:02}{day:02}T{hour:02}{min:02}{sec:02}Z")
}

/// Opens the run's log file, creating any missing directories in `dir`.
///
/// A failure fails the run rather than warning: nothing has been rendered yet
/// when this runs, so stopping costs no work, and a `--log-file` that quietly
/// produced no file would be discovered only after the render it was meant to
/// record. The error is the message to print.
fn open_log_file(dir: &Path) -> std::result::Result<std::fs::File, String> {
    let path = dir.join(format!("crust-render-{}.log", utc_stamp(SystemTime::now())));
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return Err(format!(
            "could not create log directory {}: {e}",
            parent.display()
        ));
    }
    let f = std::fs::File::create(&path)
        .map_err(|e| format!("could not create log file {}: {e}", path.display()))?;
    // Said on stderr rather than through `tracing`: the subscriber this file
    // belongs to does not exist yet.
    eprintln!("Logging to {}", path.display());
    Ok(f)
}

/// Every failure returns through here rather than `std::process::exit`, so
/// the stack unwinds normally and every destructor runs on the way out.
fn main() -> ExitCode {
    // CLI
    let cli = Cli::parse();
    // Add tracing. Two layers rather than one writer teed into both, because
    // ANSI is a per-layer setting: a single writer would either colour the
    // file with escape codes or strip the colour from the terminal. The
    // registry that composes them costs no new dependency — `sharded-slab`
    // and `thread_local` are already in the graph via the `fmt` feature.
    //
    // The file is written unbuffered, deliberately: the subscriber that owns
    // it is the process-global one, which is never dropped, so a `BufWriter`
    // would never be flushed and would lose exactly the last lines — the ones
    // explaining why a run stopped. A log at these volumes is not worth a
    // flush guard.
    let log_file = match cli.log_file.as_deref().map(open_log_file).transpose() {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let level = get_logger_level(cli.level);
    tracing_subscriber::registry()
        // `-l` for everything except the `--stats` report, which the user
        // asked for by flag and which therefore is not the log level's to
        // suppress. See `STATS_TARGET`.
        .with(filter_fn(move |meta| {
            event_enabled(meta.target(), meta.level(), level)
        }))
        .with(fmt::layer())
        .with(log_file.map(|f| fmt::layer().with_ansi(false).with_writer(Mutex::new(f))))
        .init();
    let input = cli.input;
    let output = cli.output;
    // Built before the scene and kept until after the render: it owns the
    // streaming tile cache, whose counters the `--stats` report reads once the
    // last ray has been traced.
    if let Some(config) = &cli.ocio_config
        && let Err(e) = crust_core::color::use_config(config)
    {
        error!("{e}");
        return ExitCode::FAILURE;
    }
    let assets = FileAssets::new().with_auto_tx(cli.auto_tx);
    let load_start = Instant::now();
    let scene: Scene = if let Some(t) = input {
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
            working_space: cli.working_space.clone(),
        };
        match Scene::from_usd_with_options(input_path, &assets, &options) {
            Ok(scene) => scene,
            Err(e) => {
                error!("Failed to load USD scene: {}", e);
                return ExitCode::FAILURE;
            }
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
        if let Some(space) = &cli.working_space {
            warn!(
                "--working-space {space} has no effect without -i/--input: the procedural \
                 scene renders in lin_rec709"
            );
        }
        let (world, lights) = simple_scene();
        let (camera, settings) = get_settings();
        Scene::new(camera, world, lights, settings)
    };
    debug!("Scene built in {:?}", load_start.elapsed());
    let output_color = match OutputColor::new(scene.working_space, &cli.display, &cli.view) {
        Ok(c) => c,
        Err(e) => {
            error!("{e}");
            return ExitCode::FAILURE;
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
    if let (Some(first), Some(o)) = (aovs.products.first_mut(), &output) {
        debug!(
            "-o {o} replaces {}'s productName {:?}",
            first.prim_path, first.name
        );
        first.name = o.clone();
    }
    if !aovs.products.is_empty() {
        aovs.products.retain(|p| {
            if p.name.is_empty() {
                warn!(
                    "{} authors no productName; nothing written for it",
                    p.prim_path
                );
            } else if p.vars.is_empty() {
                warn!(
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
    let camera = scene.camera;
    let world = scene.world;
    let lights = scene.lights;
    let volumes = scene.volumes;
    // Import phases and scene counts come from the loader; render and
    // output are timed here.
    let mut stats = scene.stats;
    let mut settings = match cli.samples {
        Some(spp) => {
            debug!("--samples {spp} overrides the scene's crust:samplesPerPixel");
            scene.settings.with_samples_per_pixel(spp)
        }
        None => scene.settings,
    };
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
    info!(
        "Rendering {}x{} at {} spp, max depth {} ({} order){}{}",
        img_width,
        img_height,
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
    let bar = ProgressBar::new(0);
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
    let (buffer, film, ray_stats) = if aovs.products.is_empty() {
        let (buffer, rays) = renderer.render_with_stats(!cli.scanline, &progress);
        (buffer, None, rays)
    } else {
        let (buffer, film, rays) = renderer.render_with_aovs(!cli.scanline, &progress, &aovs);
        (buffer, Some(film), rays)
    };
    bar.finish();
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
    info!("Render finished in {duration:?}");
    let output_start = Instant::now();
    if let Some(film) = &film {
        // One EXR per product, then the PNG from the first one's beauty.
        let mut written = Vec::new();
        for product in &aovs.products {
            let path = Path::new(&product.name);
            match products::write_product(path, product, &buffer, film, &output_color) {
                Ok(channels) => {
                    debug!("{}: {}", path.display(), channels.join(" "));
                    written.push(format!("{} ({} channels)", path.display(), channels.len()));
                }
                Err(e) => {
                    error!("Error writing {}: {e}", product.prim_path);
                    return ExitCode::FAILURE;
                }
            }
        }
        info!("Products written: {}", written.join(", "));
        let first = &aovs.products[0];
        if first.beauty().is_some() {
            let png_path = Path::new(&first.name).with_extension("png");
            match write_png(&buffer, img_width, img_height, &png_path, &output_color) {
                Ok(_) => info!("Image written to: {:?}", png_path),
                Err(e) => {
                    error!("Error writing PNG: {}", e);
                    return ExitCode::FAILURE;
                }
            }
        } else {
            debug!("{} has no beauty var; no PNG preview", first.prim_path);
        }
    } else if let Err(code) = write_beauty(
        &buffer,
        img_width,
        img_height,
        output.as_deref().unwrap_or("output.exr"),
        &output_color,
    ) {
        return code;
    }
    let output_elapsed = output_start.elapsed();
    stats.record("Write output", 0, output_elapsed);
    debug!("Output written in {output_elapsed:?}");

    // Traversal counts, when built with the diagnostic feature. Printed
    // separately from RenderStats because they come from the kernel and
    // only exist in a feature-on build.
    #[cfg(feature = "traversal-stats")]
    if cli.stats || cli.profile {
        // Accumulated into one string and emitted as a single event, for the
        // reason the report below is: a `println!` per row would leave these
        // lines out of `--log-file`, and one event per row would stamp each
        // of them with a timestamp the table has no column for.
        use crust_core::rt::traversal_stats as ts;
        use std::fmt::Write as _;
        let rays = ray_stats.camera_rays.max(1) as f64;
        let per = |n: u64| n as f64 / rays;
        let rule = "-".repeat(84);
        let mut out = String::new();
        // Infallible: `write!` into a String only fails if the formatter
        // does, and none of these arguments can.
        let _ = write!(out, "\n{rule}\nBVH Traversal (per camera ray)\n{rule}");
        for (level, name) in [(0usize, "top-level"), (1, "instanced")] {
            let (q, nodes, leaves, packets, scalars) = ts::read_level(level);
            if q == 0 {
                continue;
            }
            let _ = write!(
                out,
                "\n  {name:<12} queries {:>8.2}  nodes {:>9.2}  leaves {:>8.2}  packets {:>7.2}  scalar {:>8.2}",
                per(q),
                per(nodes),
                per(leaves),
                per(packets),
                per(scalars),
            );
        }
        // Which top-level instances the descents went into. A top level that
        // culls well spreads them thinly; one that does not concentrates them
        // on whatever geometry every ray's path overlaps. The importer's
        // DEBUG lines give each instancer's `geom ids a..b` range, which is
        // how an id here is traced back to a prim.
        let descents = ts::top_level_descents();
        let total: u64 = descents.iter().map(|d| d.1).sum();
        if total > 0 {
            let mut acc = 0u64;
            let mut marks = vec![];
            for (i, d) in descents.iter().enumerate() {
                acc += d.1;
                for f in [0.5, 0.9, 0.99] {
                    if (acc as f64) >= f * total as f64 && !marks.iter().any(|&(g, _)| g == f) {
                        marks.push((f, i + 1));
                    }
                }
            }
            let _ = write!(
                out,
                "\n  top-level instances entered: {} of them, {:.1} descents per camera ray \
                 (closest-hit and shadow rays; the rows above count closest-hit only)",
                descents.len(),
                per(total)
            );
            for (f, n) in marks {
                let _ = write!(
                    out,
                    "\n    {:.0}% of descents go to {n} instances",
                    f * 100.0
                );
            }
            let top: Vec<_> = descents.iter().take(40).collect();
            let ids: std::collections::HashSet<u32> = top.iter().map(|d| d.0).collect();
            let info: std::collections::HashMap<u32, _> = renderer
                .world
                .describe_instances(&ids)
                .into_iter()
                .map(|(id, b, n, shared)| (id, (b, n, shared)))
                .collect();
            let _ = write!(
                out,
                "\n  {:>9} {:>7} {:>9} {:>8} {:>9}  bounds",
                "geom_id", "share", "per ray", "prims", "shared by"
            );
            for &&(id, n) in &top {
                let (b, prims, shared) = info[&id];
                let _ = write!(
                    out,
                    "\n  {id:>9} {:>6.2}% {:>9.2} {prims:>8} {shared:>9}  [{:.0} {:.0} {:.0}]..[{:.0} {:.0} {:.0}]",
                    100.0 * n as f64 / total as f64,
                    per(n),
                    b.minimum.x,
                    b.minimum.y,
                    b.minimum.z,
                    b.maximum.x,
                    b.maximum.y,
                    b.maximum.z,
                );
            }
        }
        info!(target: STATS_TARGET, "{out}");
    }

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
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `SystemTime` at a given Unix second, for pinning `utc_stamp` against
    /// dates whose answers are known independently.
    fn at(unix_secs: u64) -> SystemTime {
        std::time::UNIX_EPOCH + std::time::Duration::from_secs(unix_secs)
    }

    #[test]
    fn utc_stamp_names_known_instants() {
        assert_eq!(utc_stamp(at(0)), "19700101T000000Z");
        assert_eq!(utc_stamp(at(1_774_267_884)), "20260323T121124Z");
        // Last second of a year, and the first of the next.
        assert_eq!(utc_stamp(at(1_767_225_599)), "20251231T235959Z");
        assert_eq!(utc_stamp(at(1_767_225_600)), "20260101T000000Z");
    }

    #[test]
    fn utc_stamp_handles_leap_years() {
        // 2024 is a leap year: Feb 29 exists.
        assert_eq!(utc_stamp(at(1_709_164_800)), "20240229T000000Z");
        // 2000 is a leap year (divisible by 400) — the case a naive
        // "divisible by 4, except by 100" rule gets wrong.
        assert_eq!(utc_stamp(at(951_782_400)), "20000229T000000Z");
        // 1900 was NOT a leap year, but it predates the epoch, so check the
        // other end of the same rule: 2100 is not one either, and March 1
        // must follow February 28.
        assert_eq!(utc_stamp(at(4_107_456_000)), "21000228T000000Z");
        assert_eq!(utc_stamp(at(4_107_542_400)), "21000301T000000Z");
    }

    #[test]
    fn utc_stamp_is_filename_safe_and_sorts_chronologically() {
        let mut prev = utc_stamp(at(0));
        for day in 1..4000u64 {
            // Every 37 days, so the walk crosses month and year boundaries
            // at varied offsets rather than landing on the same day each time.
            let t = utc_stamp(at(day * 37 * 86_400 + 3661));
            assert!(
                t.chars().all(|c| c.is_ascii_alphanumeric()),
                "{t} is not filename-safe"
            );
            assert_eq!(t.len(), 16, "{t} is not a fixed-width stamp");
            // Fixed width and zero-padded, so lexical order is chronological
            // — which is the whole reason for this format over a locale one.
            assert!(t > prev, "{t} does not sort after {prev}");
            prev = t;
        }
    }

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

    #[test]
    fn the_stats_report_survives_every_log_level() {
        // `--stats` is an explicit request, so no `-l` may suppress it —
        // including the quietest, which is the regression this guards.
        for max in [
            Level::ERROR,
            Level::WARN,
            Level::INFO,
            Level::DEBUG,
            Level::TRACE,
        ] {
            assert!(
                event_enabled(STATS_TARGET, &Level::INFO, max),
                "the stats report was filtered out at -l {max}"
            );
        }
    }

    #[test]
    fn every_other_target_still_obeys_the_level() {
        // The exemption is for one target, not a hole in the filter.
        assert!(!event_enabled("crust_render", &Level::INFO, Level::ERROR));
        assert!(!event_enabled(
            "crust_core::scene::usd_import",
            &Level::DEBUG,
            Level::INFO
        ));
        assert!(event_enabled("crust_render", &Level::ERROR, Level::ERROR));
        assert!(event_enabled(
            "crust_core::tracer",
            &Level::DEBUG,
            Level::DEBUG
        ));
        assert!(event_enabled("crust_assets", &Level::WARN, Level::INFO));
        // A near-miss on the target name is not the stats target.
        assert!(!event_enabled("stats", &Level::INFO, Level::ERROR));
        // Cranelift's INFO IR dumps are filtered as DEBUG, and only those.
        let jit = "cranelift_jit::backend";
        assert!(!event_enabled(jit, &Level::INFO, Level::INFO));
        assert!(event_enabled(jit, &Level::INFO, Level::DEBUG));
        assert!(event_enabled(jit, &Level::WARN, Level::INFO));
        assert!(event_enabled("crust_render", &Level::INFO, Level::INFO));
        assert!(!event_enabled(
            "crust_render::stats_extra",
            &Level::INFO,
            Level::ERROR
        ));
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
    fn log_levels_map_one_to_one() {
        assert_eq!(get_logger_level(LoggerLevel::Trace), Level::TRACE);
        assert_eq!(get_logger_level(LoggerLevel::Debug), Level::DEBUG);
        assert_eq!(get_logger_level(LoggerLevel::Info), Level::INFO);
        assert_eq!(get_logger_level(LoggerLevel::Warn), Level::WARN);
        assert_eq!(get_logger_level(LoggerLevel::Error), Level::ERROR);
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
        write_png(&buffer, w, h, &path, &rec709()).expect("png written");
        let img = image::open(&path).expect("readable").to_rgba8();
        assert_eq!((img.width(), img.height()), (3, 2));
        // Image row 0 is the top: the scene's y = 1 row.
        assert_eq!(img.get_pixel(2, 0).0, [0, 188, 0, 255]);
        assert_eq!(img.get_pixel(0, 1).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(1, 1).0, [0, 0, 0, 255]);
        let _ = std::fs::remove_file(&path);
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
        assert_eq!(cli.output.as_deref(), Some("out.exr"));
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

// Inline rather than a file of its own: the CLI crate keeps one source file.
mod products {
    //! Writing a render's `RenderProduct`s: one single-part, scanline,
    //! ZIP16-compressed EXR per product, one layer of named channels per var.
    //!
    //! Channel names follow OpenEXR's `<layer>.<component>` convention and the
    //! ASWF Color Interop rule that only colour gets `R/G/B`: colour vars are
    //! `<layer>.R/.G/.B[/.A]`, vectors `<layer>.X/.Y/.Z`, UVs `<layer>.U/.V`, and
    //! a scalar is one channel named after its layer. The product's first beauty
    //! var is written bare (`R/G/B[/A]`) so every viewer shows it as the image.
    //!
    //! Scanline, not the `exr` crate's default tiling: tinyexr crashes on crust's
    //! tiled files (`docs/material_fidelity.md`). The no-products render does not
    //! come through here at all — it keeps `write_rgb_file`, byte for byte.

    use crust_core::{AovFilm, AovProduct, AovVar, Buffer, ChannelKind, Precision};
    use exr::meta::attribute::AttributeValue;
    use exr::prelude::{
        AnyChannel, AnyChannels, Blocks, Compression, Encoding, FlatSamples, Image, Layer,
        LayerAttributes, LineOrder, Text, WritableImage, f16,
    };
    use std::io;
    use std::path::Path;
    use tracing::warn;

    /// The Color Interop ID for a working space without one in the config.
    /// The file still says what it can: its chromaticities, when known.
    const UNKNOWN_INTEROP_ID: &str = "unknown";

    /// The channel names `var` writes, one per plane `AovFilm::var_channels`
    /// returns. `bare` makes the layer prefix empty — the product's beauty.
    pub fn channel_names(var: &AovVar, bare: bool) -> Vec<String> {
        let layer = match &var.channel_prefix {
            Some(prefix) => prefix.clone(),
            None if bare => String::new(),
            None => var.name.clone(),
        };
        let components: &[&str] = match var.source.channel_kind() {
            ChannelKind::Color if var.with_alpha() => &["R", "G", "B", "A"],
            ChannelKind::Color => &["R", "G", "B"],
            ChannelKind::Vector => &["X", "Y", "Z"],
            ChannelKind::Uv => &["U", "V"],
            ChannelKind::Scalar => {
                return vec![if layer.is_empty() {
                    var.name.clone()
                } else {
                    layer
                }];
            }
        };
        components
            .iter()
            .map(|c| {
                if layer.is_empty() {
                    (*c).to_owned()
                } else {
                    format!("{layer}.{c}")
                }
            })
            .collect()
    }

    /// `plane` as `precision` samples. A UINT channel holds integers; a negative
    /// one (the `-1` "no ID" clear value) is written as its two's-complement
    /// bit pattern.
    fn samples(plane: Vec<f32>, precision: Precision) -> FlatSamples {
        match precision {
            Precision::Half => FlatSamples::F16(plane.into_iter().map(f16::from_f32).collect()),
            Precision::Float => FlatSamples::F32(plane),
            Precision::Uint => {
                FlatSamples::U32(plane.into_iter().map(|v| v as i32 as u32).collect())
            }
        }
    }

    /// The layout of a product's file: its channels' names, in var order, with
    /// the var each comes from. A var whose names collide with an earlier one's
    /// is refused (warned) rather than written over it.
    pub fn product_channels(product: &AovProduct) -> Vec<(&AovVar, Vec<String>)> {
        let beauty = product.beauty().map(|v| v.prim_path.as_str());
        let mut taken: Vec<String> = Vec::new();
        let mut out = Vec::new();
        for var in &product.vars {
            let bare = Some(var.prim_path.as_str()) == beauty;
            let names = channel_names(var, bare);
            if let Some(clash) = names.iter().find(|n| taken.contains(n)) {
                warn!(
                    "{}: channel {clash:?} is already written by another var of {}; skipped",
                    var.prim_path, product.prim_path
                );
                continue;
            }
            taken.extend(names.iter().cloned());
            out.push((var, names));
        }
        out
    }

    /// Refuses (with a warning) every product whose path is one an earlier
    /// product already writes, keeping the first: the later write would replace
    /// the earlier file and silently lose its channels. Paths are compared
    /// lexically, component by component (`a/./b.exr` is `a/b.exr`), since the
    /// files need not exist yet.
    pub fn refuse_shared_paths(products: &mut Vec<AovProduct>) {
        let mut seen: Vec<(std::path::PathBuf, String)> = Vec::new();
        products.retain(|p| {
            let path: std::path::PathBuf = Path::new(&p.name).components().collect();
            if let Some((_, first)) = seen.iter().find(|(q, _)| *q == path) {
                warn!(
                    "{} writes {}, as {first} already does; nothing written for it",
                    p.prim_path,
                    path.display()
                );
                return false;
            }
            seen.push((path, p.prim_path.clone()));
            true
        });
    }

    /// Writes `product` to `path`, creating its parent directories. Returns the
    /// channel names written, in file (alphabetical) order.
    ///
    /// The colour channels are tagged with the working space's ASWF Color
    /// Interop ID (`colorInteropID`, `docs/color_management.md`) and, off
    /// Rec.709, its chromaticities.
    pub fn write_product(
        path: &Path,
        product: &AovProduct,
        beauty: &Buffer,
        film: &AovFilm,
        color: &super::OutputColor,
    ) -> io::Result<Vec<String>> {
        let interop = crust_core::color::interop_id(color.working);
        let interop = interop.as_deref().unwrap_or(UNKNOWN_INTEROP_ID);
        let (width, height) = film.dimensions();
        let mut channels = Vec::new();
        for (var, names) in product_channels(product) {
            for (name, plane) in names.iter().zip(film.var_channels(beauty, var)) {
                let name = Text::new_or_none(name).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("channel name {name:?} is not valid in an EXR"),
                    )
                })?;
                channels.push(AnyChannel::new(name, samples(plane, var.precision)));
            }
        }
        if channels.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "no channel to write",
            ));
        }
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        // Sorted, because EXR stores channels alphabetically and a reader finds
        // them by name — an unsorted list is a malformed file.
        let channels = AnyChannels::sort(channels.into_iter().collect());
        let names = channels.list.iter().map(|c| c.name.to_string()).collect();

        let mut attributes = LayerAttributes {
            software_name: Text::new_or_none(concat!("crust-render ", env!("CARGO_PKG_VERSION"))),
            ..LayerAttributes::default()
        };
        let mut text = vec![("colorInteropID", interop)];
        for (key, value) in &product.attributes {
            match key.as_str() {
                // Standard attributes `exr` exposes as typed fields.
                "comments" | "comment" => attributes.comments = Text::new_or_none(value),
                "owner" => attributes.owner = Text::new_or_none(value),
                // Describes the pixels crust wrote, so only crust may set it.
                "colorInteropID" => warn!(
                    "{}: driver:parameters colorInteropID = {value:?} is not copied: the colour \
                     channels are {interop}",
                    product.prim_path
                ),
                k if exr::meta::header::standard_names::ALL.contains(&k.as_bytes()) => {
                    warn!(
                        "{}: driver:parameters {k:?} names a standard EXR attribute crust sets \
                         itself; not copied",
                        product.prim_path
                    );
                }
                k => text.push((k, value)),
            }
        }
        for (key, value) in text {
            if let (Some(k), Some(v)) = (Text::new_or_none(key), Text::new_or_none(value)) {
                attributes.other.insert(k, AttributeValue::Text(v));
            }
        }

        let layer = Layer::new(
            (width, height),
            attributes,
            Encoding {
                compression: Compression::ZIP16,
                blocks: Blocks::ScanLines,
                line_order: LineOrder::Increasing,
            },
            channels,
        );
        let mut image = Image::from_layer(layer);
        image.attributes.chromaticities = color.exr_chromaticities();
        image
            .write()
            .to_file(path)
            .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))?;
        Ok(names)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use crust_core::{Accumulation, AovSource};
        use exr::prelude::{ReadChannels, ReadLayers, read};

        fn var(name: &str, source: AovSource) -> AovVar {
            AovVar {
                prim_path: format!("/Render/Vars/{name}"),
                name: name.to_owned(),
                channel_prefix: None,
                source,
                components: source.components(),
                precision: Precision::Float,
                accumulation: Accumulation::Filtered,
                clear: source.default_clear(),
                expression: None,
                raw: false,
            }
        }

        fn product(vars: Vec<AovVar>) -> AovProduct {
            AovProduct {
                prim_path: "/Render/p".into(),
                name: "p.exr".into(),
                vars,
                attributes: vec![
                    ("artist".into(), "someone".into()),
                    ("colorInteropID".into(), "srgb_rec709_display".into()),
                    ("comments".into(), "a note".into()),
                ],
            }
        }

        fn names(p: &AovProduct) -> Vec<String> {
            product_channels(p)
                .into_iter()
                .flat_map(|(_, names)| names)
                .collect()
        }

        #[test]
        fn channels_follow_the_layer_dot_component_convention() {
            let mut beauty = var("beauty", AovSource::Color);
            beauty.components = 4;
            let p = product(vec![
                beauty,
                var("Z", AovSource::Depth),
                var("N", AovSource::Normal),
                var("st", AovSource::St),
                var("diffuse", AovSource::Color),
            ]);
            // The beauty bare, scalars named after their layer, data in X/Y/Z or
            // U/V, a second colour var prefixed.
            assert_eq!(
                names(&p),
                [
                    "R",
                    "G",
                    "B",
                    "A",
                    "Z",
                    "N.X",
                    "N.Y",
                    "N.Z",
                    "st.U",
                    "st.V",
                    "diffuse.R",
                    "diffuse.G",
                    "diffuse.B"
                ]
            );
        }

        #[test]
        fn a_channel_prefix_replaces_the_layer_and_a_clash_is_refused() {
            let mut beauty = var("beauty", AovSource::Color);
            beauty.channel_prefix = Some("rgba".into());
            let mut p_world = var("P", AovSource::P);
            p_world.channel_prefix = Some("Pw".into());
            let clash = var("Z", AovSource::Depth);
            let p = product(vec![beauty, p_world, var("Z", AovSource::Depth), clash]);
            assert_eq!(
                names(&p),
                ["rgba.R", "rgba.G", "rgba.B", "Pw.X", "Pw.Y", "Pw.Z", "Z"]
            );
        }

        #[test]
        fn a_product_is_written_scanline_zip_with_its_header() {
            let (w, h) = (3, 2);
            let mut beauty = Buffer::new(w, h);
            beauty.set_pixel(0, h - 1, crust_core::Vec3A::new(1.0, 2.0, 3.0));
            let film = crust_core::AovFilm::empty(w, h);
            let dir = std::env::temp_dir().join("crust_render_products_test/nested");
            let _ = std::fs::remove_dir_all(&dir);
            let path = dir.join("p.exr");
            // A beauty-only product comes out of an empty film; the parent
            // directories do not exist yet.
            let p = product(vec![var("beauty", AovSource::Color)]);
            let rec709 = crate::tests::rec709();
            let written = write_product(&path, &p, &beauty, &film, &rec709).expect("written");
            assert_eq!(written, ["B", "G", "R"]);
            let image = read()
                .no_deep_data()
                .largest_resolution_level()
                .all_channels()
                .first_valid_layer()
                .all_attributes()
                .from_file(&path)
                .expect("reads back");
            let layer = &image.layer_data;
            assert_eq!(layer.encoding.blocks, Blocks::ScanLines);
            assert_eq!(layer.encoding.compression, Compression::ZIP16);
            assert_eq!(
                layer.attributes.other.get(&Text::from("colorInteropID")),
                Some(&AttributeValue::Text(Text::from("lin_rec709_scene")))
            );
            assert_eq!(
                layer.attributes.other.get(&Text::from("artist")),
                Some(&AttributeValue::Text(Text::from("someone")))
            );
            assert_eq!(layer.attributes.comments, Some(Text::from("a note")));
            // An authored colorInteropID does not replace crust's own: the
            // pixels are linear Rec.709 whatever the product says.
            assert_eq!(
                layer.attributes.other.get(&Text::from("colorInteropID")),
                Some(&AttributeValue::Text(Text::from("lin_rec709_scene")))
            );
            // Top-down rows: the buffer's top row (y = h - 1) is the file's first.
            let r = &layer.channel_data.list[2];
            assert_eq!(r.name, Text::from("R"));
            assert_eq!(r.sample_data.value_by_flat_index(0).to_f32(), 1.0);
        }

        #[test]
        fn a_product_sharing_an_earlier_path_is_refused() {
            let named = |prim: &str, name: &str| AovProduct {
                prim_path: prim.into(),
                name: name.into(),
                ..product(vec![var("beauty", AovSource::Color)])
            };
            let mut products = vec![
                named("/a", "out/a.exr"),
                named("/b", "out/./a.exr"),
                named("/c", "out/c.exr"),
            ];
            refuse_shared_paths(&mut products);
            let kept: Vec<_> = products.iter().map(|p| p.prim_path.as_str()).collect();
            assert_eq!(kept, ["/a", "/c"]);
        }

        #[test]
        fn samples_take_the_requested_precision() {
            assert!(matches!(
                samples(vec![0.5], Precision::Half),
                FlatSamples::F16(v) if v == [f16::from_f32(0.5)]
            ));
            assert!(matches!(
                samples(vec![0.5], Precision::Float),
                FlatSamples::F32(v) if v == [0.5]
            ));
            // `-1` ("no ID") is the all-ones bit pattern.
            assert!(matches!(
                samples(vec![-1.0, 7.0], Precision::Uint),
                FlatSamples::U32(v) if v == [0xFFFF_FFFF, 7]
            ));
        }
    }
}
