use clap::Parser;
use crust_core::Buffer;
use crust_core::Renderer;
use crust_core::SamplingStrategy;
use crust_core::{AssetLoader, EnvironmentMap, Scene, Vec3A};
use crust_core::{get_settings, simple_scene};
use exr::prelude::*;
use indicatif::ProgressBar;
use std::path::Path;
use std::time::{Duration, Instant};
use tracing::{Level, debug, error, info};

/// The host side of `crust_core::AssetLoader`: the engine asks for pixels,
/// the CLI decodes them. That split is why `crust-core` carries no image
/// dependencies — everything that knows a file format lives here.
///
/// Formats follow the dependencies already linked for writing output:
/// OpenEXR through `exr`, Radiance `.hdr` and LDR images through `image`.
/// LDR pixels are un-gamma'd to linear, since the renderer works in linear
/// light and an sRGB-encoded sky would be noticeably wrong.
struct CliAssets;

impl AssetLoader for CliAssets {
    fn load_environment(&self, path: &Path) -> Option<EnvironmentMap> {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let started = Instant::now();
        let loaded = match ext.as_str() {
            "exr" => load_exr_environment(path),
            _ => load_image_environment(path),
        };
        match &loaded {
            Some(map) => info!(
                "Loaded environment {} ({}x{}) in {:?}",
                path.display(),
                map.width(),
                map.height(),
                started.elapsed()
            ),
            None => error!("Could not load environment {}", path.display()),
        }
        loaded
    }
}

fn load_exr_environment(path: &Path) -> Option<EnvironmentMap> {
    let image = read_first_rgba_layer_from_file(
        path,
        |resolution, _| {
            let (w, h) = (resolution.width(), resolution.height());
            (w, h, vec![Vec3A::ZERO; w * h])
        },
        |(w, _h, pixels): &mut (usize, usize, Vec<Vec3A>),
         pos,
         (r, g, b, _a): (f32, f32, f32, f32)| {
            pixels[pos.y() * *w + pos.x()] = Vec3A::new(r, g, b);
        },
    )
    .map_err(|e| error!("EXR decode failed for {}: {e}", path.display()))
    .ok()?;
    let (w, h, pixels) = image.layer_data.channel_data.pixels;
    EnvironmentMap::new(w, h, pixels)
}

fn load_image_environment(path: &Path) -> Option<EnvironmentMap> {
    // `image::open`'s default 512MiB decode-allocation limit is well below a
    // production-scale panorama (e.g. a 16k HDRI): lift it for this trusted,
    // locally-authored asset rather than have large dome lights fail to load.
    let mut reader = image::ImageReader::open(path)
        .map_err(|e| error!("Image decode failed for {}: {e}", path.display()))
        .ok()?
        .with_guessed_format()
        .map_err(|e| error!("Image decode failed for {}: {e}", path.display()))
        .ok()?;
    reader.no_limits();
    let decoded = reader
        .decode()
        .map_err(|e| error!("Image decode failed for {}: {e}", path.display()))
        .ok()?;
    let rgb = decoded.to_rgb32f();
    let (w, h) = (rgb.width() as usize, rgb.height() as usize);
    // `to_rgb32f` keeps HDR values as authored, but rescales integer
    // formats to 0..1 *without* removing their sRGB transfer curve. Undo it
    // for those, so an LDR sky lights the scene in linear light.
    let is_hdr = matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("hdr")
    );
    let to_linear = |c: f32| {
        if is_hdr {
            c
        } else if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let pixels = rgb
        .pixels()
        .map(|p| Vec3A::new(to_linear(p[0]), to_linear(p[1]), to_linear(p[2])))
        .collect();
    EnvironmentMap::new(w, h, pixels)
}

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
        match Scene::from_usd_with_assets(input_path, &CliAssets) {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The host side of the asset seam: an EXR written to disk must come
    /// back as pixels the engine can build a map from, with the geometry
    /// and values intact. `crust-core` cannot test this — it has no
    /// decoder, which is the whole point of the split.
    #[test]
    fn exr_environment_round_trips() {
        let dir = std::env::temp_dir().join("crust_env_round_trip");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("env.exr");

        let (w, h) = (8usize, 4usize);
        // A value per texel that is unmistakable and not 0..1, so an
        // accidental LDR clamp or gamma would show up.
        let value = |x: usize, y: usize| (x as f32 + 10.0 * y as f32, 2.0, 0.5);
        exr::prelude::write_rgb_file(&path, w, h, |x, y| value(x, y)).expect("write exr");

        let map = load_exr_environment(&path).expect("decode the EXR we just wrote");
        assert_eq!((map.width(), map.height()), (w, h));

        // Row 0 is the +Y pole by convention, so straight up must read the
        // first row. Sampling nearest-texel, +Y lands in column 0.
        let up = map.radiance(Vec3A::Y);
        assert!(
            (up.x - value(0, 0).0).abs() < 1e-4 && (up.y - 2.0).abs() < 1e-4,
            "top-row lookup returned {up:?}"
        );

        // And a high dynamic range value survives unclamped.
        let low = map.radiance(-Vec3A::Y);
        assert!(low.x > 20.0, "bottom row was clamped or gamma'd: {low:?}");

        let _ = std::fs::remove_file(&path);
    }

    /// LDR images are sRGB-encoded; the renderer works in linear light, so
    /// the loader must undo the transfer curve or an image-based sky is
    /// noticeably wrong.
    #[test]
    fn ldr_images_are_converted_to_linear() {
        let dir = std::env::temp_dir().join("crust_env_round_trip");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("env.png");

        // Mid-grey in sRGB (188/255 ~ 0.7373) is ~0.5 in linear light.
        let mut img = image::RgbImage::new(4, 2);
        for p in img.pixels_mut() {
            *p = image::Rgb([188, 188, 188]);
        }
        img.save(&path).expect("write png");

        let map = load_image_environment(&path).expect("decode the PNG we just wrote");
        let c = map.radiance(Vec3A::Y);
        assert!(
            (c.x - 0.5).abs() < 0.02,
            "sRGB 188 should be ~0.5 linear, got {}",
            c.x
        );

        let _ = std::fs::remove_file(&path);
    }
}
