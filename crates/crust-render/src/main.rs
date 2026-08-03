use clap::Parser;
use crust_core::Buffer;
use crust_core::PixelFilter;
use crust_core::Renderer;
use crust_core::SamplingStrategy;
use crust_core::{AssetLoader, EnvironmentMap, PtexTexture, Scene, Vec3A};
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

    fn load_ptex(&self, path: &Path) -> Option<std::sync::Arc<dyn PtexTexture>> {
        // A/B switch, in the spirit of CRUST_MESH_BAKE: decline every texture
        // so the same scene renders on its constant `baseColor` fallback. That
        // is how you tell "the Ptex lookup is wrong" from "the material or the
        // lighting is wrong", since both show up as an off-colour surface.
        if std::env::var("CRUST_PTEX").as_deref() == Ok("0") {
            debug!("CRUST_PTEX=0: ignoring {}", path.display());
            return None;
        }
        let started = Instant::now();
        match PtexColor::open(path) {
            Ok(tex) => {
                info!(
                    "Loaded Ptex {} ({} faces, {:.1} MiB resident) in {:?}",
                    path.display(),
                    PtexTexture::num_faces(&tex),
                    tex.bytes() as f64 / (1024.0 * 1024.0),
                    started.elapsed()
                );
                Some(std::sync::Arc::new(tex))
            }
            Err(e) => {
                error!("Could not load Ptex {}: {e}", path.display());
                None
            }
        }
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

// ---------------------------------------------------------------------------
// Host-side Ptex decoding
// ---------------------------------------------------------------------------
//
// The CLI's implementation of `crust_core::PtexTexture`, on top of the pure-Rust
// `ptex` reader. It lives here rather than in a module of its own because this
// crate is deliberately a single file — see the crate note in CLAUDE.md.
//
// Why the whole file is read up front: `ptex::PtexReader` takes `&mut self` for
// every pixel read and caches only level indexes, not pixel data, so a texel
// lookup means a seek, a read and possibly a zlib inflate. A path tracer asks
// for texels from every Rayon worker, millions of times per frame, in an order
// nothing can predict. Going back to the file per lookup would be both a
// lock-contention disaster and orders of magnitude too slow, so every face is
// decoded once at load time into an immutable buffer that threads then share
// without synchronisation.
//
// Why not at full resolution: production Ptex is authored for close-ups. The
// island's `isLavaRocks/Color/rockfacemain0001_geo.ptx` is 11 384 faces over
// 631 MB compressed, which inflates to several GB at full resolution — for an
// asset covering a few hundred thousand pixels in the reference framing. Faces
// load at the coarsest mip level that still exceeds what the render can
// resolve, capped by `DEFAULT_MAX_LOG2`. Ptex files carry stored mipmaps and the
// reader computes any level they lack, so this costs nothing but a smaller read.
// `CRUST_PTEX_MAX_LOG2` overrides the cap as a log2 edge length.

/// Default per-face resolution cap, as a log2 edge length: 32×32 texels.
///
/// A quad covering `n` pixels on screen cannot show more than about `n` texels,
/// and the island's meshes are dense — `mountain_geo` is 33 503 quads, so in a
/// 595×520 framing a quad lands on a handful of pixels. 32×32 is already far
/// past what such a framing resolves, while keeping a 3 272-face texture near
/// 10 MB instead of a gigabyte.
const DEFAULT_MAX_LOG2: i8 = 5;

/// One face's texels within [`PtexColor::texels`].
struct Face {
    offset: u32,
    width: u16,
    height: u16,
}

/// A Ptex colour texture, fully decoded to linear RGB.
struct PtexColor {
    faces: Vec<Face>,
    /// Interleaved RGB, row-major within each face, v-major as Ptex stores it.
    texels: Vec<f32>,
    /// Constant colour for a face that failed to decode.
    fallback: Vec3A,
}

impl PtexColor {
    /// Opens `path` and decodes every face to linear RGB.
    ///
    /// `Err` carries a message suitable for a warning; the caller falls back to
    /// a constant colour rather than failing the render.
    ///
    /// `std::result::Result` spelled out: this file glob-imports `exr::prelude`,
    /// whose own one-parameter `Result<T>` alias would otherwise shadow it.
    fn open(path: &Path) -> std::result::Result<Self, String> {
        let mut tx = ptex::PtexReader::open(path).map_err(|e| e.to_string())?;

        let n_chan = tx.num_channels();
        if n_chan == 0 {
            return Err("file has no channels".into());
        }
        let dt = tx.data_type();
        let scale = dt.one_value_inv();
        let max_log2 = max_log2_from_env();

        let n_faces = tx.num_faces();
        let mut faces = Vec::with_capacity(n_faces);
        let mut texels: Vec<f32> = Vec::new();

        for faceid in 0..n_faces {
            let info = *tx.face_info(faceid).map_err(|e| e.to_string())?;
            // Clamp each axis independently: Ptex faces are frequently
            // non-square (64x16 is common) and clamping the pair together
            // would distort the aspect the file chose.
            let res = ptex::Res::new(
                info.res.ulog2.min(max_log2),
                info.res.vlog2.min(max_log2),
            );
            let (w, h) = (res.u(), res.v());
            let offset = texels.len() as u32;
            faces.push(Face {
                offset,
                width: w as u16,
                height: h as u16,
            });
            texels.resize(texels.len() + w * h * 3, 0.0);

            let Ok(raw) = tx.get_data_at_res(faceid, res) else {
                // A single unreadable face should not sink the texture: it
                // stays the zero it was resized to, and the rest still loads.
                tracing::debug!("Ptex {}: face {faceid} unreadable", path.display());
                continue;
            };

            let px = dt.size() * n_chan;
            let out = &mut texels[offset as usize..];
            for i in 0..(w * h) {
                let src = &raw[i * px..];
                for ch in 0..3 {
                    // A single-channel (displacement-style) file feeds channel
                    // 0 to all three, so it reads as greyscale rather than red.
                    let c = if ch < n_chan { ch } else { 0 };
                    let v = read_channel(&src[c * dt.size()..], dt) * scale;
                    // Ptex colour here is display-encoded: the island's
                    // shading network runs it through a gamma-1/2.2 node
                    // (`PxrColorCorrect`) and the GL path declares
                    // `sourceColorSpace = "sRGB"`. Both mean decode by 2.2,
                    // and the engine works in linear light. Done once here
                    // rather than per lookup.
                    out[i * 3 + ch] = v.max(0.0).powf(2.2);
                }
            }
        }

        Ok(PtexColor {
            faces,
            texels,
            fallback: Vec3A::splat(0.5),
        })
    }

    /// Bytes held, for the load-time report.
    fn bytes(&self) -> usize {
        self.texels.len() * std::mem::size_of::<f32>()
            + self.faces.len() * std::mem::size_of::<Face>()
    }

    #[inline]
    fn texel(&self, f: &Face, x: usize, y: usize) -> Vec3A {
        let i = f.offset as usize + (y * f.width as usize + x) * 3;
        Vec3A::new(self.texels[i], self.texels[i + 1], self.texels[i + 2])
    }
}

impl PtexTexture for PtexColor {
    fn eval(&self, face_id: u32, u: f32, v: f32) -> Vec3A {
        let Some(f) = self.faces.get(face_id as usize) else {
            return self.fallback;
        };
        let (w, h) = (f.width as usize, f.height as usize);
        if w == 0 || h == 0 {
            return self.fallback;
        }

        // Bilinear with clamped borders — every island colour file authors
        // `uBorderMode`/`vBorderMode = clamp`. Filtering across face
        // boundaries (what a real `PtexFilter` does with the adjacency data)
        // is not attempted: at these face resolutions the seam is far below a
        // pixel, and getting adjacency edge-rotations wrong is worse than not
        // filtering across at all.
        let fu = if u.is_finite() { u.clamp(0.0, 1.0) } else { 0.0 };
        let fv = if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 };

        // Texel centres sit at (i + 0.5)/n.
        let x = fu * w as f32 - 0.5;
        let y = fv * h as f32 - 0.5;
        let x0 = x.floor();
        let y0 = y.floor();
        let tx = x - x0;
        let ty = y - y0;
        let cx = |c: f32| (c.max(0.0) as usize).min(w - 1);
        let cy = |c: f32| (c.max(0.0) as usize).min(h - 1);
        let (x0i, x1i) = (cx(x0), cx(x0 + 1.0));
        let (y0i, y1i) = (cy(y0), cy(y0 + 1.0));

        let top = self
            .texel(f, x0i, y0i)
            .lerp(self.texel(f, x1i, y0i), tx);
        let bot = self
            .texel(f, x0i, y1i)
            .lerp(self.texel(f, x1i, y1i), tx);
        top.lerp(bot, ty)
    }

    fn num_faces(&self) -> usize {
        self.faces.len()
    }
}

/// Reads one channel of Ptex data as an unnormalized float.
#[inline]
fn read_channel(src: &[u8], dt: ptex::DataType) -> f32 {
    match dt {
        ptex::DataType::UInt8 => src[0] as f32,
        ptex::DataType::UInt16 => u16::from_le_bytes([src[0], src[1]]) as f32,
        ptex::DataType::Half => ptex::half_to_float(u16::from_le_bytes([src[0], src[1]])),
        ptex::DataType::Float => f32::from_le_bytes([src[0], src[1], src[2], src[3]]),
    }
}

fn max_log2_from_env() -> i8 {
    match std::env::var("CRUST_PTEX_MAX_LOG2") {
        Ok(v) => match v.parse::<i8>() {
            // Ptex resolutions are log2-encoded in an i8; 14 is 16384, well
            // past any authored face.
            Ok(n) if (0..=14).contains(&n) => n,
            _ => {
                tracing::warn!(
                    "CRUST_PTEX_MAX_LOG2={v} is not an integer in 0..=14 — using {DEFAULT_MAX_LOG2}"
                );
                DEFAULT_MAX_LOG2
            }
        },
        Err(_) => DEFAULT_MAX_LOG2,
    }
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
