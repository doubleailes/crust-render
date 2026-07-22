use clap::Parser;
use crust_core::Buffer;
use crust_core::Renderer;
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
    let settings = match cli.samples {
        Some(spp) => scene.settings.with_samples_per_pixel(spp),
        None => scene.settings,
    };
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
    let buffer = renderer.render_with_progress(cli.bucket, &progress);
    bar.finish();
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
