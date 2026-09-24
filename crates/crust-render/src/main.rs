//! The CLI: parse args, build a `Scene`, render, write the images.
//!
//! `forbid(unsafe_code)`, like every crate here but `crust-core` — whose one
//! exception is a test-only counting allocator.
#![forbid(unsafe_code)]

use clap::Parser;
use crust_assets::FileAssets;
use crust_core::Buffer;
use crust_core::PixelFilter;
use crust_core::Renderer;
use crust_core::SamplingStrategy;
use crust_core::Scene;
use crust_core::{get_settings, simple_scene};
use exr::prelude::*;
use indicatif::ProgressBar;
use std::path::Path;
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
    /// Output image path. The linear EXR is written here and a tone-mapped
    /// sRGB PNG next to it (same path with a .png extension).
    #[arg(short, long, default_value = "output.exr")]
    output: String,
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
    /// Bucket rendering
    #[arg(short, long, default_value_t = false)]
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
    /// How light sampling and BSDF sampling combine. Overrides the scene's
    /// `crust:samplingStrategy` when set; `light` and `bsdf` render one
    /// strategy alone to visualize what MIS balances between.
    #[arg(long, value_enum)]
    strategy: Option<Strategy>,
    /// Pixel reconstruction filter. Overrides the scene's
    /// `crust:pixelFilter` when set.
    #[arg(long, value_enum)]
    filter: Option<Filter>,
    /// Pixel filter radius in pixels, measured from the pixel center
    /// (each filter has its own default: box 0.5, triangle 1, gaussian /
    /// blackman 1.5, mitchell 2). Overrides `crust:pixelFilterRadius`.
    #[arg(long)]
    filter_radius: Option<f32>,
    /// Print render statistics and a per-phase profile (parse, build,
    /// render, output) when the render finishes.
    #[arg(long, default_value_t = false)]
    stats: bool,
    /// Convert UV textures to a tiled, mip-mapped `.tx` beside the original
    /// (same path, extension `.tx`) on first use, when the `.tx` is missing or
    /// older than its source. A `.tx` beside a texture is always streamed when
    /// present; this only creates the missing ones.
    #[arg(long, default_value_t = false)]
    auto_tx: bool,
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

#[derive(clap::ValueEnum, Clone, Debug, Copy)]
enum Strategy {
    /// β=2 power-heuristic MIS (default)
    Power,
    /// Balance-heuristic MIS
    Balance,
    /// Light sampling (NEE) only
    Light,
    /// BSDF sampling only
    Bsdf,
}

impl From<Strategy> for SamplingStrategy {
    fn from(s: Strategy) -> Self {
        match s {
            Strategy::Power => SamplingStrategy::PowerMis,
            Strategy::Balance => SamplingStrategy::BalanceMis,
            Strategy::Light => SamplingStrategy::LightOnly,
            Strategy::Bsdf => SamplingStrategy::BsdfOnly,
        }
    }
}

#[derive(clap::ValueEnum, Clone, Debug, Copy)]
enum Filter {
    /// One-pixel box (the pre-filter jitter, bit-identical at radius 0.5)
    Box,
    /// Tent filter (default)
    Triangle,
    /// Truncated Gaussian
    Gaussian,
    /// 4-term Blackman-Harris window
    Blackman,
    /// Mitchell-Netravali (negative lobes: sharp, may ring)
    Mitchell,
}

impl From<Filter> for PixelFilter {
    fn from(f: Filter) -> Self {
        // The names match `PixelFilter::from_name`'s and cannot miss.
        PixelFilter::from_name(match f {
            Filter::Box => "box",
            Filter::Triangle => "triangle",
            Filter::Gaussian => "gaussian",
            Filter::Blackman => "blackman",
            Filter::Mitchell => "mitchell",
        })
        .expect("CLI filter names mirror PixelFilter::from_name")
    }
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
    target == STATS_TARGET || *level <= max
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

/// Compress a linear f32 into [0,1] and encode it as an sRGB byte.
fn tone_map(linear: f32) -> u8 {
    let clamped = linear.clamp(0.0, 1.0);
    let srgb = if clamped <= 0.0031308 {
        12.92 * clamped
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    };
    (srgb * 255.0 + 0.5).floor() as u8
}

/// Tone-map the render buffer to an sRGB PNG at `path`.
fn write_png(
    buffer: &Buffer,
    width: usize,
    height: usize,
    path: &Path,
) -> std::result::Result<(), image::ImageError> {
    let mut img = image::RgbaImage::new(width as u32, height as u32);
    for y in 0..height {
        for x in 0..width {
            let (r, g, b) = buffer.get_rgb(x, y);
            img.put_pixel(
                x as u32,
                y as u32,
                image::Rgba([tone_map(r), tone_map(g), tone_map(b), 255]),
            );
        }
    }
    img.save(path)
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
/// Fails the process rather than warning: nothing has been rendered yet when
/// this runs, so exiting costs no work, and a `--log-file` that quietly
/// produced no file would be discovered only after the render it was meant to
/// record.
fn open_log_file(dir: &Path) -> std::fs::File {
    let path = dir.join(format!("crust-render-{}.log", utc_stamp(SystemTime::now())));
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        eprintln!(
            "error: could not create log directory {}: {e}",
            parent.display()
        );
        std::process::exit(1);
    }
    match std::fs::File::create(&path) {
        Ok(f) => {
            // Said on stderr rather than through `tracing`: the subscriber
            // this file belongs to does not exist yet.
            eprintln!("Logging to {}", path.display());
            f
        }
        Err(e) => {
            eprintln!("error: could not create log file {}: {e}", path.display());
            std::process::exit(1);
        }
    }
}

fn main() {
    // CLI
    let cli = Cli::parse();
    // Add tracing. Two layers rather than one writer teed into both, because
    // ANSI is a per-layer setting: a single writer would either colour the
    // file with escape codes or strip the colour from the terminal. The
    // registry that composes them costs no new dependency — `sharded-slab`
    // and `thread_local` are already in the graph via the `fmt` feature.
    //
    // The file is written unbuffered, deliberately: several error paths here
    // end in `std::process::exit`, which runs no destructors, so a
    // `BufWriter` would drop exactly the lines explaining why the run
    // stopped. A log at these volumes is not worth a flush-on-exit guard.
    let log_file = cli.log_file.as_deref().map(open_log_file);
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
    let assets = FileAssets::new().with_auto_tx(cli.auto_tx);
    let load_start = Instant::now();
    let scene: Scene = if let Some(t) = input {
        let input_path = std::path::Path::new(&t);
        debug!("Loading USD scene from {}", input_path.display());
        match Scene::from_usd_at_frame(input_path, &assets, cli.frame) {
            Ok(scene) => scene,
            Err(e) => {
                error!("Failed to load USD scene: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        debug!("No -i/--input given: building the procedural fallback scene");
        if let Some(frame) = cli.frame {
            warn!(
                "--frame {frame} has no effect without -i/--input: the procedural scene is static"
            );
        }
        let (world, lights) = simple_scene();
        let (camera, settings) = get_settings();
        Scene::new(camera, world, lights, settings)
    };
    debug!("Scene built in {:?}", load_start.elapsed());
    // One line however many textures were converted — the per-file lines are
    // DEBUG, since their count grows with the stage.
    let (converted, failed, secs) = assets.tx_report();
    if converted + failed > 0 {
        info!("--auto-tx: converted {converted} texture tile(s) to .tx in {secs:.1}s");
        if failed > 0 {
            warn!("--auto-tx: {failed} tile(s) failed to convert; their textures were preloaded");
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
        debug!("--strategy {strategy:?} overrides the scene's crust:samplingStrategy");
        settings = settings.with_sampling_strategy(strategy.into());
    }
    // --filter replaces the scene's filter (at the filter's default radius);
    // --filter-radius then resizes whichever filter is in effect, so it also
    // works alone to widen the scene-authored one.
    if let Some(filter) = cli.filter {
        debug!("--filter {filter:?} overrides the scene's crust:pixelFilter");
        settings = settings.with_pixel_filter(filter.into());
    }
    if let Some(radius) = cli.filter_radius {
        debug!("--filter-radius {radius} overrides the filter's own radius");
        settings = settings.with_pixel_filter(settings.pixel_filter().with_radius(radius));
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
    // Timer
    let start = Instant::now();
    // World

    // Camera
    let (img_width, img_height) = settings.get_dimensions();
    let renderer = Renderer::new(camera, world, lights, settings).with_volumes(volumes);
    info!(
        "Rendering {}x{} at {} spp, max depth {} ({} order){}",
        img_width,
        img_height,
        settings.samples_per_pixel(),
        settings.max_depth(),
        if cli.bucket { "bucket" } else { "scanline" },
        match cli.frame {
            Some(frame) => format!(", frame {frame}"),
            None => String::new(),
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
    let (buffer, ray_stats) = renderer.render_with_stats(cli.bucket, &progress);
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
    info!("Render finished in {duration:?}");
    // Write the linear EXR, then the tone-mapped sRGB PNG next to it.
    let output_start = Instant::now();
    debug!(
        "Writing {}x{} linear EXR to {}",
        img_width, img_height, output
    );
    match write_rgb_file(&output, img_width, img_height, |x, y| buffer.get_rgb(x, y)) {
        Ok(_) => info!("Image written to: {:?}", output),
        Err(e) => {
            error!("Error writing image: {}", e);
            std::process::exit(1);
        }
    }
    let png_path = Path::new(&output).with_extension("png");
    debug!("Tone mapping to sRGB PNG at {}", png_path.display());
    match write_png(&buffer, img_width, img_height, &png_path) {
        Ok(_) => info!("Image written to: {:?}", png_path),
        Err(e) => {
            error!("Error writing PNG: {}", e);
            std::process::exit(1);
        }
    }
    let output_elapsed = output_start.elapsed();
    stats.record("Write output", 0, output_elapsed);
    debug!("Output written in {output_elapsed:?}");

    // Traversal counts, when built with the diagnostic feature. Printed
    // separately from RenderStats because they come from the kernel and
    // only exist in a feature-on build.
    #[cfg(feature = "traversal-stats")]
    if cli.stats {
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
        info!(target: STATS_TARGET, "{out}");
    }

    if cli.stats {
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
        assert!(!event_enabled(
            "crust_render::stats_extra",
            &Level::INFO,
            Level::ERROR
        ));
    }

    #[test]
    fn tone_map_anchors_black_and_white() {
        assert_eq!(tone_map(0.0), 0);
        assert_eq!(tone_map(1.0), 255);
        // Out-of-range input clamps rather than wrapping.
        assert_eq!(tone_map(-3.0), 0);
        assert_eq!(tone_map(50.0), 255);
        assert_eq!(tone_map(f32::INFINITY), 255);
    }

    #[test]
    fn tone_map_applies_the_srgb_curve() {
        // Linear 0.5 is display 188; linear 0.214 is display ~128.
        assert_eq!(tone_map(0.5), 188);
        assert!((tone_map(0.214) as i32 - 128).abs() <= 1);
        // The linear toe: 0.001 linear → 12.92 · 0.001 · 255 ≈ 3.3 → 3.
        assert_eq!(tone_map(0.001), 3);
    }

    #[test]
    fn tone_map_is_monotone() {
        let mut prev = 0u8;
        for i in 0..=1000 {
            let v = tone_map(i as f32 / 1000.0);
            assert!(v >= prev, "not monotone at {i}");
            prev = v;
        }
    }

    #[test]
    fn cli_strategy_names_map_onto_the_engine_enum() {
        assert_eq!(
            SamplingStrategy::from(Strategy::Power),
            SamplingStrategy::PowerMis
        );
        assert_eq!(
            SamplingStrategy::from(Strategy::Balance),
            SamplingStrategy::BalanceMis
        );
        assert_eq!(
            SamplingStrategy::from(Strategy::Light),
            SamplingStrategy::LightOnly
        );
        assert_eq!(
            SamplingStrategy::from(Strategy::Bsdf),
            SamplingStrategy::BsdfOnly
        );
    }

    #[test]
    fn cli_filter_names_map_onto_the_engine_filters_at_their_default_radius() {
        assert_eq!(
            PixelFilter::from(Filter::Box),
            PixelFilter::BoxFilter { radius: 0.5 }
        );
        assert_eq!(
            PixelFilter::from(Filter::Triangle),
            PixelFilter::Triangle { radius: 1.0 }
        );
        assert_eq!(
            PixelFilter::from(Filter::Gaussian),
            PixelFilter::Gaussian { radius: 1.5 }
        );
        assert_eq!(
            PixelFilter::from(Filter::Blackman),
            PixelFilter::Blackman { radius: 1.5 }
        );
        assert_eq!(
            PixelFilter::from(Filter::Mitchell),
            PixelFilter::Mitchell { radius: 2.0 }
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
        write_png(&buffer, w, h, &path).expect("png written");
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
            "-l",
            "debug",
            "--frame",
            "1012.5",
        ])
        .expect("valid flags");
        assert_eq!(cli.frame, Some(1012.5));
        assert_eq!(cli.input.as_deref(), Some("scene.usda"));
        assert_eq!(cli.output, "out.exr");
        assert!(cli.bucket);
        assert_eq!(cli.samples, Some(12));
        assert!(matches!(cli.strategy, Some(Strategy::Balance)));
        assert!(matches!(cli.filter, Some(Filter::Mitchell)));
        assert_eq!(cli.filter_radius, Some(1.75));
        assert!(cli.stats);
        assert!(matches!(cli.level, LoggerLevel::Debug));
    }

    #[test]
    fn cli_defaults_when_nothing_is_given() {
        let cli = Cli::try_parse_from(["crust-render"]).expect("no flags is valid");
        assert!(cli.input.is_none());
        assert_eq!(cli.output, "output.exr");
        assert!(!cli.bucket);
        assert!(cli.samples.is_none());
        assert!(cli.strategy.is_none());
        assert!(cli.filter.is_none());
        assert!(cli.filter_radius.is_none());
        assert!(cli.frame.is_none());
        assert!(!cli.stats);
        assert!(matches!(cli.level, LoggerLevel::Info));
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
    fn cli_rejects_unknown_enum_values() {
        assert!(Cli::try_parse_from(["crust-render", "--strategy", "random"]).is_err());
        assert!(Cli::try_parse_from(["crust-render", "--filter", "lanczos"]).is_err());
        assert!(Cli::try_parse_from(["crust-render", "-l", "loud"]).is_err());
        assert!(Cli::try_parse_from(["crust-render", "-s", "many"]).is_err());
    }
}
