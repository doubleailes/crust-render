use clap::Parser;
use crust_core::Document;
use crust_core::Renderer;
use crust_core::convert;
use exr::prelude::*;
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
    /// Input Scene path should be a .ron file
    #[arg(short, long)]
    input: Option<String>,
    /// Output image path
    /// Default is output.exr
    /// If you want to use a different name, please specify it here
    #[arg(short, long, default_value = "output.exr")]
    output: String,
    /// Verbose level
    #[arg(short, long, default_value = "info")]
    level: LoggerLevel,
    /// Bucket rendering
    #[arg(short, long, default_value_t = false)]
    bucket: bool,
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

fn main() {
    // CLI
    let cli = Cli::parse();
    // Add tracing
    tracing_subscriber::fmt()
        .with_max_level(get_logger_level(cli.level))
        .init();
    let input = cli.input;
    let output = cli.output;
    let (world, lights, settings, camera) = if input.is_some() {
        let t = input.unwrap();
        let input_path = std::path::Path::new(&t);
        debug!("Document loaded at path: {:?}", input_path);
        let doc: Document = Document::read(input_path).expect("Failed to read document");
        let (world, lights) = doc.get_world();
        (world, lights, doc.settings(), doc.camera())
    } else {
        let (world, lights) = simple_scene();
        let (camera, settings) = get_settings();
        (world, lights, settings, camera)
    };
    debug!("Render Settings: {:#?}", settings);
    // Timer
    let start = Instant::now();
    // World

    debug!("World loaded with {} objects", world.count());
    debug!("Lights loaded with {} objects", lights.count());
    // Camera
    let renderer = Renderer::new(camera, world, lights, settings);
    info!("Let's start rendering...");
    let buffer = if cli.bucket {
        info!("Bucket rendering is enabled");
        renderer.render_with_tiles()
    } else {
        info!("Bucket rendering is disabled");
        renderer.render()
    };
    // Close Timer
    let duration: Duration = start.elapsed();
    info!("Time elapsed in rendering() is: {:?}", duration);
    // Render
    let (img_width, img_height) = settings.get_dimensions();
    match write_rgb_file(&output, img_width, img_height, |x, y| buffer.get_rgb(x, y)) {
        Ok(_) => info!("Image written to: {:?}", output),
        Err(e) => {
            error!("Error writing image: {}", e);
            std::process::exit(1);
        }
    }
    convert();
}
