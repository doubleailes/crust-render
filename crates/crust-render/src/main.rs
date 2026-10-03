//! The CLI: parse args, build a `Scene`, render, write the images.
//!
//! `forbid(unsafe_code)`, like every crate here but `crust-core` (a test-only
//! counting allocator) and `crust-jit` (calling generated code).
#![forbid(unsafe_code)]

mod cli;
mod logging;
mod output;
#[cfg(feature = "traversal-stats")]
mod traversal_report;

use clap::Parser;
use crust_assets::FileAssets;
use crust_core::{RenderSettings, Renderer, Scene, get_settings, simple_scene};
use exr::prelude::write_rgb_file;
use indicatif::ProgressBar;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::fmt;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use cli::Cli;
use logging::{STATS_TARGET, event_enabled, get_logger_level, open_log_file};
use output::write_png;

/// Every failure returns through here rather than `std::process::exit`, so
/// the stack unwinds normally and every destructor runs on the way out.
fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(e) = init_logging(&cli) {
        eprintln!("error: {e}");
        return ExitCode::FAILURE;
    }
    // Built before the scene and kept until after the render: it owns the
    // streaming tile cache, whose counters the `--stats` report reads once the
    // last ray has been traced.
    let assets = FileAssets::new().with_auto_tx(cli.auto_tx);
    let Some(scene) = load_scene(&cli, &assets) else {
        return ExitCode::FAILURE;
    };
    let camera = scene.camera;
    let world = scene.world;
    let lights = scene.lights;
    let volumes = scene.volumes;
    // Import phases and scene counts come from the loader; render and
    // output are timed here.
    let mut stats = scene.stats;
    let settings = apply_overrides(&cli, scene.settings);
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
    let start = Instant::now();
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
    let (buffer, ray_stats) = renderer.render_with_stats(!cli.scanline, &progress);
    bar.finish();
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
    if let Err(code) = write_images(&cli.output, &buffer, img_width, img_height) {
        return code;
    }
    let output_elapsed = output_start.elapsed();
    stats.record("Write output", 0, output_elapsed);
    debug!("Output written in {output_elapsed:?}");

    if cli.stats || cli.profile {
        // Traversal counts, when built with the diagnostic feature, ahead of
        // the report: they come from the kernel, not from `RenderStats`.
        #[cfg(feature = "traversal-stats")]
        info!(
            target: STATS_TARGET,
            "{}",
            traversal_report::traversal_report(ray_stats.camera_rays, &renderer.world)
        );
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

/// Installs the global subscriber: the terminal, plus `--log-file` when
/// given. The error is the message to print.
///
/// Two layers rather than one writer teed into both, because ANSI is a
/// per-layer setting: a single writer would either colour the file with
/// escape codes or strip the colour from the terminal. The registry that
/// composes them costs no new dependency — `sharded-slab` and `thread_local`
/// are already in the graph via the `fmt` feature.
///
/// The file is written unbuffered, deliberately: the subscriber that owns it
/// is the process-global one, which is never dropped, so a `BufWriter` would
/// never be flushed and would lose exactly the last lines — the ones
/// explaining why a run stopped. A log at these volumes is not worth a flush
/// guard.
fn init_logging(cli: &Cli) -> Result<(), String> {
    let log_file = cli.log_file.as_deref().map(open_log_file).transpose()?;
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
    Ok(())
}

/// The scene `-i` names, or the procedural fallback without one. `None`
/// once the failure has been logged.
fn load_scene(cli: &Cli, assets: &FileAssets) -> Option<Scene> {
    let load_start = Instant::now();
    let scene: Scene = if let Some(t) = &cli.input {
        let input_path = std::path::Path::new(t);
        debug!("Loading USD scene from {}", input_path.display());
        let options = crust_core::UsdImportOptions {
            frame: cli.frame,
            camera: cli.camera.clone(),
            subdivision_level: cli.subdiv_level,
            subdivision_edge_length: cli.subdiv_edge_length,
            // The process renders once and exits, so freeing the composed
            // stage is pure delay before the render (45 s on ALab).
            skip_stage_teardown: true,
        };
        match Scene::from_usd_with_options(input_path, assets, &options) {
            Ok(scene) => scene,
            Err(e) => {
                error!("Failed to load USD scene: {}", e);
                return None;
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
    Some(scene)
}

/// The scene's settings with every command-line override applied.
fn apply_overrides(cli: &Cli, scene_settings: RenderSettings) -> RenderSettings {
    let mut settings = match cli.samples {
        Some(spp) => {
            debug!("--samples {spp} overrides the scene's crust:samplesPerPixel");
            scene_settings.with_samples_per_pixel(spp)
        }
        None => scene_settings,
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

/// Writes the linear EXR at `output`, then the tone-mapped sRGB PNG next to
/// it. The error is the exit code, once the failure has been logged.
fn write_images(
    output: &str,
    buffer: &crust_core::Buffer,
    img_width: usize,
    img_height: usize,
) -> Result<(), ExitCode> {
    debug!(
        "Writing {}x{} linear EXR to {}",
        img_width, img_height, output
    );
    match write_rgb_file(output, img_width, img_height, |x, y| buffer.get_rgb(x, y)) {
        Ok(_) => info!("Image written to: {:?}", output),
        Err(e) => {
            error!("Error writing image: {}", e);
            return Err(ExitCode::FAILURE);
        }
    }
    let png_path = Path::new(output).with_extension("png");
    debug!("Tone mapping to sRGB PNG at {}", png_path.display());
    match write_png(buffer, img_width, img_height, &png_path) {
        Ok(_) => info!("Image written to: {:?}", png_path),
        Err(e) => {
            error!("Error writing PNG: {}", e);
            return Err(ExitCode::FAILURE);
        }
    }
    Ok(())
}
