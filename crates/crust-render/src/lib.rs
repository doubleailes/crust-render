//! The CLI's import-and-check policy, as a library: the scene flags and how a
//! stage is imported, overridden and checked with them, which `crust render`,
//! `crust check`, `crust mcp` and the Hydra delegate (`crust-hydra`) all go
//! through, so a stage imports the same in every host. With the `session`
//! feature, the live session's core: the override layer followed, its changes
//! classified and applied in place or by a re-import (`session`).
//!
//! The binary (`main.rs`) keeps argument parsing, the writers and exit codes.
#![forbid(unsafe_code)]

pub mod products;
#[cfg(feature = "session")]
pub mod session;

use clap::{Args, ValueEnum};
use crust_assets::FileAssets;
use crust_core::AssetLoader;
use crust_core::LightSelection;
use crust_core::PixelFilter;
use crust_core::PixelRect;
use crust_core::SamplerKind;
use crust_core::SamplingStrategy;
use crust_core::Scene;
use crust_core::check::{CheckReport, ProductInfo};
use crust_core::diagnostic::checks::{self, Facts};
use crust_core::diagnostic::report::SceneInfo;
use crust_core::diagnostic::{SceneFlags, effective_settings};
use crust_core::{AovRequest, RenderSettings};
use crust_core::{WarningKind, WarningScope, warning};
use crust_core::{get_settings, simple_scene};
use tracing::{debug, warn};

#[derive(Args)]
pub struct CheckArgs {
    #[command(flatten)]
    pub scene: SceneArgs,
    /// Also write the report as JSON (format `crust-check/2`) to PATH; with
    /// `-`, the JSON goes to stdout instead of the text report.
    #[arg(long, value_name = "PATH|-")]
    pub json: Option<std::path::PathBuf>,
    /// Exit 3 when the import raises a warning of one of these kinds:
    /// `refused`, `approximated`, `skipped`, or `all`, comma-separated. The
    /// reports are written either way, with the matching codes in `denied`.
    #[arg(long, value_name = "KIND[,KIND…]", value_enum, value_delimiter = ',')]
    pub deny: Vec<DenyKind>,
}

/// The flags that shape the scene and its settings, shared by `render`,
/// `diagnostic` and `check` with the same names, values and defaults — so a
/// diagnostic suggestion given as a flag means the same in a render.
#[derive(Args)]
pub struct SceneArgs {
    /// Input scene path — .usda / .usdc / .usdz.
    /// When absent, `render` falls back to a hard-coded procedural scene.
    #[arg(short, long)]
    pub input: Option<String>,
    /// Render only a rectangle of the frame: `X0,Y0,X1,Y1` in pixels, from
    /// the image's top-left corner, `X1` and `Y1` excluded. Each pixel
    /// renders exactly as in the full frame; the EXR keeps the full
    /// resolution as its display window with the region as its data
    /// window, and the PNG holds only the region. Overrides the stage's
    /// `dataWindowNDC`. Clipped to the resolution; a region with nothing
    /// left is an error.
    #[arg(long, value_name = "X0,Y0,X1,Y1", value_parser = parse_region)]
    pub region: Option<PixelRect>,
    /// USD time code (frame) to render. Every animated attribute resolves
    /// its time samples here; unanimated ones read their default. Fractional
    /// values render a subframe. Also sets the sampler's frame seed,
    /// overriding the scene's `crust:frame`. When absent, attributes read
    /// their default (non-time-sampled) value.
    #[arg(short, long, allow_negative_numbers = true, value_parser = parse_frame)]
    pub frame: Option<f64>,
    /// Camera to render through, as an absolute USD prim path (e.g.
    /// `/root/camera01/renderCam`). Without it the stage's
    /// `RenderSettings.camera` is used, else the first camera found. A path
    /// that is not a camera on the stage stops the render.
    #[arg(long, value_name = "PRIM_PATH")]
    pub camera: Option<String>,
    /// Render pass to render, as the absolute path of a `RenderPass` prim
    /// (e.g. `/Render/Passes/char`). The geometry in its `matte` collection
    /// is held out (zero colour, zero alpha), what its `cameraVisibility`
    /// collection leaves out is hidden from camera rays, and its
    /// `renderSource`, when it targets a `RenderSettings` prim, is the
    /// settings read. Without it no pass is active, whatever passes the stage
    /// defines. A path that is not a `RenderPass` stops the render.
    #[arg(long, value_name = "PRIM_PATH")]
    pub pass: Option<String>,
    /// Subdivision refinement level for every mesh whose `subdivisionScheme`
    /// is not `none` (unauthored means USD's fallback, `catmullClark`).
    /// Overrides the scene's `crust:subdivisionLevel` render setting
    /// (default 0: each cage shaded with smooth normals, unrefined). Clamped to 6.
    #[arg(long, value_name = "N")]
    pub subdiv_level: Option<u32>,
    /// Adaptive subdivision: refine each subdivision mesh only until its mean
    /// cage edge, at its nearest distance to the render camera, is at most
    /// this many pixels long. Overrides the scene's
    /// `crust:subdivisionEdgeLength`. `--subdiv-level` then caps the level
    /// (default 3). Needs `--camera` or the stage's `RenderSettings.camera`.
    #[arg(long, value_name = "PX", allow_negative_numbers = true, value_parser = parse_edge_length)]
    pub subdiv_edge_length: Option<f32>,
    /// How light sampling and BSDF sampling combine. Overrides the scene's
    /// `crust:samplingStrategy` when set; `light` and `bsdf` render one
    /// strategy alone to visualize what MIS balances between.
    #[arg(long, value_parser = choices(SamplingStrategy::CHOICES))]
    pub strategy: Option<SamplingStrategy>,
    /// How NEE picks the light it samples at each vertex. Overrides the
    /// scene's `crust:lightSelection` when set.
    #[arg(long, value_parser = choices(LightSelection::CHOICES))]
    pub light_selection: Option<LightSelection>,
    /// Light samples (shadow rays) per camera vertex: N stratified picks
    /// over the lights, MIS-combined with the bounce. Lowers direct-light
    /// noise about as 1/N at N times the shadow rays there. Overrides the
    /// scene's `crust:lightSamples` (default 1).
    #[arg(long, value_name = "N", value_parser = parse_count)]
    pub light_samples: Option<u32>,
    /// Light samples per later surface or volume vertex, paid at every
    /// bounce of the path. Overrides the scene's
    /// `crust:lightSamplesIndirect` (default 1).
    #[arg(long, value_name = "M", value_parser = parse_count)]
    pub light_samples_indirect: Option<u32>,
    /// Pixel reconstruction filter. Overrides the scene's
    /// `crust:pixelFilter` when set.
    #[arg(long, value_parser = choices(PixelFilter::CHOICES))]
    pub filter: Option<PixelFilter>,
    /// Pixel filter radius in pixels, measured from the pixel center
    /// (each filter has its own default: box 0.5, triangle 1, gaussian /
    /// blackman 1.5, mitchell 2). Overrides `crust:pixelFilterRadius`.
    #[arg(long, value_parser = parse_radius)]
    pub filter_radius: Option<f32>,
    /// The QMC sampler every pass draws from. Overrides the scene's
    /// `crust:sampler` when set.
    #[arg(long, value_parser = choices(SamplerKind::CHOICES))]
    pub sampler: Option<SamplerKind>,
    /// Firefly clamp: cap each sample's indirect light at this value in its
    /// largest channel (linear, hue kept). Biased, and 0 turns it off.
    /// Overrides the scene's `crust:indirectClamp`.
    #[arg(long, value_parser = parse_clamp)]
    pub indirect_clamp: Option<f32>,
    /// Adaptive sampling: a pixel may stop once the relative standard error
    /// of its mean is below this. 0 turns adaptive sampling off. Overrides
    /// the scene's `crust:varianceThreshold`.
    #[arg(long, value_name = "T", value_parser = parse_threshold)]
    pub variance_threshold: Option<f32>,
    /// Adaptive sampling: the samples every pixel takes before it may stop
    /// (floored at the square root of the sample count, and at 2).
    /// Overrides the scene's `crust:minSamplesPerPixel`.
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
    pub min_samples: Option<u32>,
    /// Adaptive sampling: how far a still-sampling cross neighbour's
    /// convergence index may sit above a pixel's own before it holds the
    /// pixel back. Negative turns the comparison off. Overrides the scene's
    /// `crust:adaptiveNeighbourTolerance`.
    #[arg(long, value_name = "T", allow_negative_numbers = true, value_parser = parse_tolerance)]
    pub neighbour_tolerance: Option<f32>,
    /// Convert UV textures to a tiled, mip-mapped `.tx` beside the original on
    /// first use, as Arnold's auto-tx does, when the `.tx` is missing or its
    /// modification time is not its source's. The file is named and written
    /// as Arnold 7.5 names and writes it (`<stem>_raw.<ext>.tx`, or
    /// `<stem>_<source space>_<--tx-color-space>.<ext>.tx`), so the two
    /// renderers share it. A `.tx` beside a texture is always streamed when
    /// present; this only creates the missing ones.
    #[arg(long, default_value_t = false)]
    pub auto_tx: bool,
    /// The colour space a `.tx` converts to, which its name records, by any
    /// name or alias of the OCIO config. Defaults to `ACEScg`, Arnold's
    /// rendering space, so crust finds and writes the files Arnold uses (in a
    /// config without `ACEScg`, every `.tx` is `_raw`); independent of
    /// `--working-space`.
    #[arg(long, value_name = "SPACE")]
    pub tx_color_space: Option<String>,
}

/// `--frame`'s parser: an `f64` that is also finite. `f64::from_str` accepts
/// `nan`, `inf` and `infinity`, none of which is a time code; crust-core
/// refuses them too, but rejecting them here reports it as a usage error
/// before any scene is opened.
pub fn parse_frame(s: &str) -> std::result::Result<f64, String> {
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

/// `--variance-threshold`'s parser: a finite, non-negative threshold, `0`
/// turning adaptive sampling off.
fn parse_threshold(s: &str) -> std::result::Result<f32, String> {
    let t: f32 = s.parse().map_err(|e| format!("{e}"))?;
    if t.is_finite() && t >= 0.0 {
        Ok(t)
    } else {
        Err(format!(
            "{s} is not a finite, non-negative threshold (0 turns adaptive sampling off)"
        ))
    }
}

/// `--neighbour-tolerance`'s parser: any finite number, a negative one
/// turning the neighbour comparison off. The engine ignores a non-finite
/// tolerance silently; refusing it here says so.
fn parse_tolerance(s: &str) -> std::result::Result<f32, String> {
    let t: f32 = s.parse().map_err(|e| format!("{e}"))?;
    if t.is_finite() {
        Ok(t)
    } else {
        Err(format!("{s} is not a finite tolerance"))
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
pub fn choices<T>(
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

/// Where a render of a stage without RenderProducts writes its beauty EXR:
/// `-o` with its frame variables expanded at `frame`, else `output.exr` —
/// what `render` writes and `check` reports.
pub fn beauty_output(output: Option<&str>, frame: f64) -> String {
    output.map_or_else(
        || "output.exr".to_owned(),
        |o| expand_output(o, frame, None),
    )
}

/// `path` with its frame variables expanded at `frame`
/// ([`products::expand_frame_vars`]), each issue raised as a coded warning
/// naming the product prim `at`, or `-o` when there is none.
pub fn expand_output(path: &str, frame: f64, at: Option<&str>) -> String {
    let (expanded, issues) = products::expand_frame_vars(path, frame);
    let whose = at.unwrap_or("-o");
    // One warning per path and code, naming every unknown name in it.
    let unknown: Vec<&str> = issues
        .iter()
        .filter_map(|i| match i {
            products::PathIssue::Unknown(name) => Some(name.as_str()),
            products::PathIssue::Subframe => None,
        })
        .collect();
    if !unknown.is_empty() {
        let names = unknown.join(", ");
        let message =
            format!("{whose}: {names} in {path:?} is not a frame variable; left as written");
        match at {
            Some(prim) => warning!(ProductUnknownVariable, at = prim, "{message}"),
            None => warning!(ProductUnknownVariable, "{message}"),
        }
    }
    if issues.contains(&products::PathIssue::Subframe) {
        let message = format!(
            "{whose}: frame {frame} rounded to {} in {path:?}; use $FF to keep the fraction",
            frame.round()
        );
        match at {
            Some(prim) => warning!(ProductSubframeFrame, at = prim, "{message}"),
            None => warning!(ProductSubframeFrame, "{message}"),
        }
    }
    if expanded != path {
        debug!("{whose}: {path:?} expands to {expanded:?} at frame {frame}");
    }
    expanded
}

/// What an import needs from its loader beyond decoding: how many UV
/// textures it preloaded (the check report counts them) and, for a session
/// that keeps its decodes across imports, revalidating and sweeping them.
/// `FileAssets` is the one implementation the renderer uses; a test can bring
/// its own loader through the session (`crust-hydra`'s panicking texture).
pub trait Assets: AssetLoader {
    /// How many UV textures the loader preloaded.
    fn preloaded(&self) -> u64;
    /// Drops the kept decodes whose file changed on disk; how many.
    fn revalidate(&self) -> usize {
        0
    }
    /// Lets go of the kept decodes the last import did not use; how many.
    fn sweep(&self) -> usize {
        0
    }
}

impl Assets for FileAssets {
    fn preloaded(&self) -> u64 {
        self.texture_cache_stats().preloaded
    }
    fn revalidate(&self) -> usize {
        FileAssets::revalidate(self)
    }
    fn sweep(&self) -> usize {
        FileAssets::sweep(self)
    }
}

/// The scene to render: the USD stage `-i` names, imported under the CLI's
/// options, or the procedural fallback without one. The error is the message
/// to log, or (in an MCP session) to answer with.
///
/// `keep_stage` leaves the final composed stage allocated
/// (`UsdImportOptions::skip_stage_teardown`): right for a command that imports
/// once and exits, where freeing it is pure delay (45 s on ALab); wrong for the
/// MCP session, which imports once per edit batch and would keep every one of
/// those stages, and the files they map, for as long as it runs.
pub fn load_scene(
    cli: &SceneArgs,
    working_space: Option<&String>,
    assets: &dyn AssetLoader,
    keep_stage: bool,
) -> std::result::Result<Scene, String> {
    let scene = if let Some(t) = &cli.input {
        let input_path = std::path::Path::new(&t);
        debug!("Loading USD scene from {}", input_path.display());
        let options = crust_core::UsdImportOptions {
            frame: cli.frame,
            camera: cli.camera.clone(),
            pass: cli.pass.clone(),
            subdivision_level: cli.subdiv_level,
            subdivision_edge_length: cli.subdiv_edge_length,
            skip_stage_teardown: keep_stage,
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
        if let Some(pass) = &cli.pass {
            warn!("--pass {pass} has no effect without -i/--input");
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

/// The scene's render settings with the shared scene flags applied — what
/// `render` and `diagnostic` both run with.
pub fn apply_scene_overrides(cli: &SceneArgs, mut settings: RenderSettings) -> RenderSettings {
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
    if let Some(sampler) = cli.sampler {
        debug!("--sampler {sampler} overrides the scene's crust:sampler");
        settings = settings.with_sampler(sampler);
    }
    if let Some(limit) = cli.indirect_clamp {
        debug!("--indirect-clamp {limit} overrides the scene's crust:indirectClamp");
        settings = settings.with_indirect_clamp(limit);
    }
    if cli.variance_threshold.is_some() || cli.min_samples.is_some() {
        let min = cli.min_samples.unwrap_or(settings.min_samples_per_pixel());
        let threshold = cli
            .variance_threshold
            .unwrap_or(settings.variance_threshold());
        if let Some(n) = cli.min_samples {
            debug!("--min-samples {n} overrides the scene's crust:minSamplesPerPixel");
        }
        if let Some(t) = cli.variance_threshold {
            debug!("--variance-threshold {t} overrides the scene's crust:varianceThreshold");
        }
        settings = settings.with_adaptive_sampling(min, threshold);
    }
    if let Some(t) = cli.neighbour_tolerance {
        debug!("--neighbour-tolerance {t} overrides the scene's crust:adaptiveNeighbourTolerance");
        settings = settings.with_adaptive_neighbour_tolerance(t);
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
/// first one's path and every path's frame variables expanded at `frame`;
/// each refusal is a coded warning (`product.no_name`,
/// `product.no_writable_vars`, `product.shared_path`), which `crust check`
/// collects. Collisions are judged on the expanded paths. Empty means the
/// single beauty EXR at `-o`.
///
/// Returns `-o` as expanded when it named the first product, so that a
/// render whose products are all refused writes its beauty there without
/// expanding (and warning about) the same path a second time.
pub fn select_products(aovs: &mut AovRequest, output: Option<&str>, frame: f64) -> Option<String> {
    if let (Some(first), Some(o)) = (aovs.products.first_mut(), output) {
        debug!(
            "-o {o} replaces {}'s productName {:?}",
            first.prim_path, first.name
        );
        first.name = o.to_owned();
    }
    for product in &mut aovs.products {
        product.name = expand_output(&product.name, frame, Some(&product.prim_path));
    }
    let expanded_output = output
        .and(aovs.products.first())
        .map(|first| first.name.clone());
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
    expanded_output
}

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

/// A stage imported as `crust render` would import it, with what `crust
/// check` reports on it: what `check` prints, and what an MCP session keeps
/// to render from and to answer its `check` tool with.
pub struct Checked {
    /// The imported scene, as the import left it: its own settings,
    /// products and warnings.
    pub scene: Scene,
    /// The scene's settings with the scene flags applied: what it renders
    /// with.
    pub settings: RenderSettings,
    /// The products a render would write, resolved and refused as `render`
    /// resolves them (`select_products`); empty for the single beauty EXR.
    pub aovs: AovRequest,
    pub report: CheckReport,
    /// How many UV textures the import preloaded, which the report's facts
    /// count: kept for an MCP session's re-check of a scene it updated in
    /// place, which loads none.
    pub preloaded: u64,
}

/// The body of `crust check`: import `args`' stage, choose its products and
/// build the `crust-check/2` report. The error is the message to log.
/// `keep_stage` is [`load_scene`]'s.
pub fn import_checked(
    args: &CheckArgs,
    keep_stage: bool,
    assets: &dyn Assets,
) -> std::result::Result<Checked, String> {
    if args.scene.input.is_none() {
        return Err("crust check needs a stage: -i <INPUT>".to_owned());
    }
    let scene = load_scene(&args.scene, None, assets, keep_stage)?;
    let import_peak = crust_core::peak_memory_bytes();
    let preloaded = assets.preloaded();
    check_scene(args, scene, preloaded, import_peak)
}

/// [`import_checked`] past the import: the scene's settings under `args`'
/// overrides, its products chosen, and the report built. Also how an MCP
/// session re-checks a scene it updated in place. The scene keeps its own
/// products and warnings; the chosen products and the report's warnings
/// are copies. `preloaded` is how many UV textures the import preloaded.
pub fn check_scene(
    args: &CheckArgs,
    mut scene: Scene,
    preloaded: u64,
    import_peak: Option<u64>,
) -> std::result::Result<Checked, String> {
    let input = args
        .scene
        .input
        .as_ref()
        .ok_or("crust check needs a stage: -i <INPUT>")?;
    let mut settings = apply_scene_overrides(&args.scene, scene.settings);
    if let Some(region) = args.scene.region {
        settings = settings
            .with_region(region)
            .map_err(|e| format!("--region: {e}"))?;
    }
    let (w, h) = settings.get_dimensions();

    // What the render would write, resolved as it resolves it. The products
    // it refuses are the import's warnings too.
    let mut aovs = scene.aovs.clone();
    // Without -f, frame variables expand as frame 1, husk's default.
    let frame = args.scene.frame.unwrap_or(1.0);
    let selecting = WarningScope::enter();
    let _ = select_products(&mut aovs, None, frame);
    let mut warnings = scene.warnings.clone();
    warnings.extend(selecting.finish());
    let products = if aovs.products.is_empty() {
        vec![ProductInfo {
            prim: None,
            file: beauty_output(None, frame),
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
    // The facts are about the settings the scene renders with.
    let own = std::mem::replace(&mut scene.settings, settings);
    let facts = Facts::from_import(&scene, args.scene.auto_tx, preloaded, import_peak);
    scene.settings = own;
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
        pass: scene.pass.as_ref().map(Into::into),
        settings: scene.settings_prim.clone(),
        products,
        precomp: scene.aovs.precomp.as_deref().map(Into::into),
        effective_settings: effective_settings(&settings, &flags),
        import: scene.stats.phases.clone(),
        counts: scene.stats.scene,
        findings: checks::run(&facts),
        denied: crust_core::check::denied(&warnings, &kinds),
        warnings,
    };
    Ok(Checked {
        scene,
        settings,
        aovs,
        report,
        preloaded,
    })
}

/// How the outputs describe and encode the working space's pixels: the space
/// itself, for the EXRs' colour metadata, and the OCIO display / view the
/// preview PNG is encoded through.
pub struct OutputColor {
    pub working: crust_core::color::Space,
    pub display: String,
    pub view: String,
}

impl OutputColor {
    /// `--display` / `--view` for a render in `working`, refused when the
    /// config cannot make that preview — before the render, not after it.
    pub fn new(
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
    pub fn exr_chromaticities(&self) -> Option<exr::meta::attribute::Chromaticities> {
        if self.working == crust_core::color::Space::LIN_REC709 {
            return None;
        }
        let [r, g, b, w] = crust_core::color::chromaticities(self.working)?;
        Some(exr::meta::attribute::Chromaticities {
            red: exr::math::Vec2(r[0], r[1]),
            green: exr::math::Vec2(g[0], g[1]),
            blue: exr::math::Vec2(b[0], b[1]),
            white: exr::math::Vec2(w[0], w[1]),
        })
    }
}

/// Encode linear working-space RGB as 8-bit preview bytes through the OCIO
/// display / view ([`crust_core::color::encode_preview`]). The default view
/// clamps to [0, 1] and applies the display's curve: for sRGB, the same
/// piecewise curve the texture decoders invert.
pub fn tone_map(rgb: &mut [f32], color: &OutputColor) -> Vec<u8> {
    crust_core::color::encode_preview(rgb, color.working, &color.display, &color.view)
        .expect("checked by OutputColor::new");
    rgb.iter()
        .map(|&c| (c.clamp(0.0, 1.0) * 255.0 + 0.5).floor() as u8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::OutputColor;

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
}
