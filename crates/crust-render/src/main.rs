//! The CLI: parse args, build a `Scene`, render, write the images.
//!
//! `forbid(unsafe_code)`, like every crate here but `crust-core` (a test-only
//! counting allocator) and `crust-jit` (calling generated code).
#![forbid(unsafe_code)]

mod logging;
mod products;

use logging::{LoggerLevel, STATS_TARGET};

use clap::{Args, Parser, Subcommand, ValueEnum};
use crust_assets::FileAssets;
use crust_core::Buffer;
use crust_core::LightSelection;
use crust_core::PixelFilter;
use crust_core::Renderer;
use crust_core::SamplingStrategy;
use crust_core::Scene;
use crust_core::{AovRequest, RenderSettings};
use crust_core::{get_settings, simple_scene};
use exr::prelude::*;
use indicatif::ProgressBar;
use std::path::Path;
use std::process::ExitCode;
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
    /// Without -i, renders the hard-coded procedural fallback scene.
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
    },
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

#[derive(Args)]
struct RenderArgs {
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

/// The scene to render: the USD stage `-i` names, imported under the CLI's
/// options, or the procedural fallback without one. A failure is already
/// logged; the error is the exit code.
fn load_scene(cli: &RenderArgs, assets: &FileAssets) -> std::result::Result<Scene, ExitCode> {
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
            working_space: cli.working_space.clone(),
        };
        match Scene::from_usd_with_options(input_path, assets, &options) {
            Ok(scene) => scene,
            Err(e) => {
                error!("Failed to load USD scene: {}", e);
                return Err(ExitCode::FAILURE);
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
    Ok(scene)
}

/// The scene's render settings with the CLI's overrides applied.
fn apply_overrides(cli: &RenderArgs, settings: RenderSettings) -> RenderSettings {
    let mut settings = match cli.samples {
        Some(spp) => {
            debug!("--samples {spp} overrides the scene's crust:samplesPerPixel");
            settings.with_samples_per_pixel(spp)
        }
        None => settings,
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
    settings
}

/// The stage's RenderProducts that can be written, with `-o` replacing the
/// first one's path; each refusal is warned about. Empty means the single
/// beauty EXR at `-o`.
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
}

/// Every failure returns through here rather than `std::process::exit`, so
/// the stack unwinds normally and every destructor runs on the way out.
fn main() -> ExitCode {
    let cli = Cli::parse();
    // A listing's stdout is its result, so its log goes to stderr; a
    // render's log stays where it always was.
    let (log_to, log_file) = match &cli.command {
        Command::Render(args) => (logging::Terminal::Stdout, args.log_file.as_deref()),
        Command::Ls { .. } => (logging::Terminal::Stderr, None),
    };
    if let Err(e) = logging::init(cli.level, log_file, log_to) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    match &cli.command {
        Command::Render(args) => render(args),
        Command::Ls { kind, input } => ls(*kind, input),
    }
}

/// `crust ls`: print the stage's `kind` prims, one path per line.
fn ls(kind: LsKind, input: &Path) -> ExitCode {
    match Scene::list_usd(input, kind.into()) {
        Ok(prims) => {
            if prims.is_empty() {
                let name = kind.to_possible_value().expect("no skipped variant");
                warn!("{} has no {}", input.display(), name.get_name());
            }
            for prim in prims {
                println!("{prim}");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            error!("Failed to read USD scene: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `crust render`: build the scene, render it, write the images.
fn render(cli: &RenderArgs) -> ExitCode {
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
            return ExitCode::FAILURE;
        }
    }
    let assets = FileAssets::new().with_auto_tx(cli.auto_tx);
    let load_start = Instant::now();
    let scene = match load_scene(cli, &assets) {
        Ok(scene) => scene,
        Err(code) => return code,
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
    select_products(&mut aovs, output.as_deref());
    let camera = scene.camera;
    let world = scene.world;
    let lights = scene.lights;
    let volumes = scene.volumes;
    // Import phases and scene counts come from the loader; render and
    // output are timed here.
    let mut stats = scene.stats;
    let settings = apply_overrides(cli, scene.settings);
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
        let out = crust_core::traversal_report(&renderer.world, ray_stats.camera_rays);
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

    /// `crust render <args>`, parsed down to the render's own arguments.
    fn render<const N: usize>(args: [&str; N]) -> std::result::Result<RenderArgs, clap::Error> {
        let cli = Cli::try_parse_from(["crust", "render"].into_iter().chain(args))?;
        match cli.command {
            Command::Render(args) => Ok(*args),
            Command::Ls { .. } => unreachable!("parsed as render"),
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
            let Command::Ls { kind, input } = c.command else {
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
    }

    #[test]
    fn clap_definition_is_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
