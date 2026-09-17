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
use std::time::{Duration, Instant};
use tracing::{Level, debug, error, info};

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
    /// Bucket rendering
    #[arg(short, long, default_value_t = false)]
    bucket: bool,
    /// Samples per pixel. Overrides the scene / default value when set.
    #[arg(short, long)]
    samples: Option<u32>,
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

fn main() {
    // CLI
    let cli = Cli::parse();
    // Add tracing
    tracing_subscriber::fmt()
        .with_max_level(get_logger_level(cli.level))
        .init();
    let input = cli.input;
    let output = cli.output;
    let scene: Scene = if let Some(t) = input {
        let input_path = std::path::Path::new(&t);
        debug!("Scene loaded at path: {:?}", input_path);
        match Scene::from_usd_with_assets(input_path, &FileAssets) {
            Ok(scene) => scene,
            Err(e) => {
                error!("Failed to load USD scene: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        let (world, lights) = simple_scene();
        let (camera, settings) = get_settings();
        Scene::new(camera, world, lights, settings)
    };
    let camera = scene.camera;
    let world = scene.world;
    let lights = scene.lights;
    let volumes = scene.volumes;
    // Import phases and scene counts come from the loader; render and
    // output are timed here.
    let mut stats = scene.stats;
    let mut settings = match cli.samples {
        Some(spp) => scene.settings.with_samples_per_pixel(spp),
        None => scene.settings,
    };
    if let Some(strategy) = cli.strategy {
        settings = settings.with_sampling_strategy(strategy.into());
    }
    // --filter replaces the scene's filter (at the filter's default radius);
    // --filter-radius then resizes whichever filter is in effect, so it also
    // works alone to widen the scene-authored one.
    if let Some(filter) = cli.filter {
        settings = settings.with_pixel_filter(filter.into());
    }
    if let Some(radius) = cli.filter_radius {
        settings = settings.with_pixel_filter(settings.pixel_filter().with_radius(radius));
    }
    // A BVH can only cull primitives whose bounds are small against the
    // whole scene. Report the ratio so a scene whose instance boxes all
    // span everything -- where no split can help -- is visible.
    {
        let (n, scene_diag, mean_diag, max_diag) = world.primitive_extents();
        if n > 0 && scene_diag > 0.0 {
            info!(
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

    debug!("World loaded with {} objects", world.count());
    debug!("Lights loaded with {} objects", lights.count());
    // Camera
    let renderer = Renderer::new(camera, world, lights, settings).with_volumes(volumes);
    info!("Let's start rendering...");
    if cli.bucket {
        info!("Bucket rendering is enabled");
    } else {
        info!("Bucket rendering is disabled");
    }
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
    info!("Time elapsed in rendering() is: {:?}", duration);
    // Write the linear EXR, then the tone-mapped sRGB PNG next to it.
    let output_start = Instant::now();
    let (img_width, img_height) = settings.get_dimensions();
    match write_rgb_file(&output, img_width, img_height, |x, y| buffer.get_rgb(x, y)) {
        Ok(_) => info!("Image written to: {:?}", output),
        Err(e) => {
            error!("Error writing image: {}", e);
            std::process::exit(1);
        }
    }
    let png_path = Path::new(&output).with_extension("png");
    match write_png(&buffer, img_width, img_height, &png_path) {
        Ok(_) => info!("Image written to: {:?}", png_path),
        Err(e) => {
            error!("Error writing PNG: {}", e);
            std::process::exit(1);
        }
    }
    stats.record("Write output", 0, output_start.elapsed());

    // Traversal counts, when built with the diagnostic feature. Printed
    // separately from RenderStats because they come from the kernel and
    // only exist in a feature-on build.
    #[cfg(feature = "traversal-stats")]
    if cli.stats {
        use crust_core::rt::traversal_stats as ts;
        let rays = ray_stats.camera_rays.max(1) as f64;
        let per = |n: u64| n as f64 / rays;
        println!("{}", "-".repeat(84));
        println!("BVH Traversal (per camera ray)");
        println!("{}", "-".repeat(84));
        for (level, name) in [(0usize, "top-level"), (1, "instanced")] {
            let (q, nodes, leaves, packets, scalars) = ts::read_level(level);
            if q == 0 {
                continue;
            }
            println!(
                "  {name:<12} queries {:>8.2}  nodes {:>9.2}  leaves {:>8.2}  packets {:>7.2}  scalar {:>8.2}",
                per(q),
                per(nodes),
                per(leaves),
                per(packets),
                per(scalars),
            );
        }
    }

    if cli.stats {
        // Straight to stdout, not through `tracing`: this is a report to
        // read, not a log line, and it should not be filtered out by the
        // log level or interleaved with per-prim messages.
        println!("{stats}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        ])
        .expect("valid flags");
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
        assert!(!cli.stats);
        assert!(matches!(cli.level, LoggerLevel::Info));
    }

    #[test]
    fn cli_rejects_unknown_enum_values() {
        assert!(Cli::try_parse_from(["crust-render", "--strategy", "random"]).is_err());
        assert!(Cli::try_parse_from(["crust-render", "--filter", "lanczos"]).is_err());
        assert!(Cli::try_parse_from(["crust-render", "-l", "loud"]).is_err());
        assert!(Cli::try_parse_from(["crust-render", "-s", "many"]).is_err());
    }
}
