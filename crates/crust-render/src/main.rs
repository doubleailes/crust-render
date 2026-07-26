mod checkpoint_io;

use clap::Parser;
use crust_core::Buffer;
use crust_core::Renderer;
use crust_core::SamplingStrategy;
use crust_core::Scene;
use crust_core::TileOrder;
use crust_core::{CheckpointState, RenderOptions};
use crust_core::{get_settings, simple_scene};
use exr::prelude::*;
use indicatif::ProgressBar;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tracing::{Level, debug, error, info, warn};

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
    /// Deprecated: rendering is always tile-based now; see --tile-order.
    #[arg(short, long, default_value_t = false, hide = true)]
    bucket: bool,
    /// Tile visit order. Overrides the scene's `crust:tileOrder` when set.
    #[arg(long, value_enum)]
    tile_order: Option<TileOrderArg>,
    /// Samples per pixel. Overrides the scene / default value when set.
    #[arg(short, long)]
    samples: Option<u32>,
    /// How light sampling and BSDF sampling combine. Overrides the scene's
    /// `crust:samplingStrategy` when set; `light` and `bsdf` render one
    /// strategy alone to visualize what MIS balances between.
    #[arg(long, value_enum)]
    strategy: Option<Strategy>,
    /// Write a resumable checkpoint EXR at most every SECS seconds (at
    /// render pass boundaries). Not supported for path-guided renders.
    #[arg(long, value_name = "SECS")]
    checkpoint_interval: Option<u64>,
    /// Where the checkpoint EXR is written (and read from by --resume).
    /// Defaults to the output path with a .checkpoint.exr suffix.
    #[arg(long, value_name = "PATH")]
    checkpoint_file: Option<String>,
    /// Resume from a checkpoint EXR (by default the --checkpoint-file
    /// path). The scene and settings must match the interrupted render;
    /// -s/--samples may be raised to extend it.
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
    resume: Option<String>,
}

#[derive(clap::ValueEnum, Clone, Debug, Copy)]
enum TileOrderArg {
    /// Morton (Z-curve) order — spatially coherent, the default
    Morton,
    /// Outward spiral from the image center
    Spiral,
    /// Row-major, top row first
    Scanline,
    /// Deterministic seeded shuffle
    Random,
}

impl From<TileOrderArg> for TileOrder {
    fn from(o: TileOrderArg) -> Self {
        match o {
            TileOrderArg::Morton => TileOrder::Morton,
            TileOrderArg::Spiral => TileOrder::Spiral,
            TileOrderArg::Scanline => TileOrder::Scanline,
            TileOrderArg::Random => TileOrder::Random,
        }
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
        match Scene::from_usd(input_path) {
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
    let mut settings = match cli.samples {
        Some(spp) => scene.settings.with_samples_per_pixel(spp),
        None => scene.settings,
    };
    if let Some(strategy) = cli.strategy {
        settings = settings.with_sampling_strategy(strategy.into());
    }
    if let Some(order) = cli.tile_order {
        settings = settings.with_tile_order(order.into());
    }
    debug!("Render Settings: {:#?}", settings);
    // Timer
    let start = Instant::now();
    // World

    debug!("World loaded with {} objects", world.count());
    debug!("Lights loaded with {} objects", lights.count());
    // Camera
    let renderer = Renderer::new(camera, world, lights, settings).with_volumes(volumes);
    info!("Let's start rendering...");
    if cli.bucket {
        info!("--bucket is deprecated: rendering is always tile-based (see --tile-order)");
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

    // Checkpoint/resume wiring: the engine snapshots at pass boundaries,
    // this closure persists them (and refreshes the preview PNG).
    let checkpoint_path: PathBuf = cli
        .checkpoint_file
        .clone()
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new(&output).with_extension("checkpoint.exr"));
    let resume_state: Option<CheckpointState> = cli.resume.as_ref().map(|p| {
        let path = if p.is_empty() {
            checkpoint_path.clone()
        } else {
            PathBuf::from(p)
        };
        match checkpoint_io::read_checkpoint(&path) {
            Ok(state) => {
                info!(
                    "resuming from {:?} at {} samples per pixel (target {})",
                    path,
                    state.next_sample,
                    settings.samples_per_pixel()
                );
                state
            }
            Err(e) => {
                error!("failed to load resume checkpoint: {}", e);
                std::process::exit(1);
            }
        }
    });
    let preview_png_path = Path::new(&output).with_extension("png");
    let on_checkpoint = |state: &CheckpointState| {
        match checkpoint_io::write_checkpoint(state, &checkpoint_path) {
            Ok(()) => info!(
                "checkpoint written to {:?} ({} spp so far)",
                checkpoint_path, state.next_sample
            ),
            Err(e) => warn!("failed to write checkpoint: {}", e),
        }
        let mut preview = Buffer::new(state.width, state.height);
        for y in 0..state.height {
            for x in 0..state.width {
                let i = y * state.width + x;
                if state.count[i] > 0 {
                    preview.set_pixel(x, y, state.sum[i] / state.count[i] as f32);
                }
            }
        }
        if let Err(e) = write_png(&preview, state.width, state.height, &preview_png_path) {
            warn!("failed to write preview PNG: {}", e);
        }
    };
    let checkpointing = cli.checkpoint_interval.is_some();
    let opts = RenderOptions {
        progress: Some(&progress),
        checkpoint_interval: cli.checkpoint_interval.map(Duration::from_secs),
        on_checkpoint: if checkpointing {
            Some(&on_checkpoint)
        } else {
            None
        },
        resume: resume_state,
    };
    let buffer = match renderer.render_with_options(opts) {
        Ok(buffer) => buffer,
        Err(e) => {
            bar.finish();
            error!("{}", e);
            std::process::exit(1);
        }
    };
    bar.finish();
    if checkpointing {
        info!(
            "checkpoint file kept at {:?} — safe to delete once the render is kept",
            checkpoint_path
        );
    }
    // Close Timer
    let duration: Duration = start.elapsed();
    info!("Time elapsed in rendering() is: {:?}", duration);
    // Write the linear EXR, then the tone-mapped sRGB PNG next to it.
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
}
