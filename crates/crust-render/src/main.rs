use clap::Parser;
use crust_render::Document;
use crust_render::Renderer;
use crust_render::convert;
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
    input: String,
    /// Output image path
    /// Default is output.exr
    /// If you want to use a different name, please specify it here
    #[arg(short, long, default_value = "output.exr")]
    output: String,
    /// Verbose level
    #[arg(short, long, default_value = "info")]
    level: LoggerLevel,
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
    let input_path = std::path::Path::new(&input);
    let output = cli.output;
    let doc: Document = Document::read(input_path).expect("Failed to read document");
    debug!("Document loaded at path: {:?}", input_path);
    debug!("Render Settings: {:#?}", doc.settings());
    // Timer
    let start = Instant::now();
    // World
    let (world, lights) = doc.get_world();
    // Camera
    let renderer = Renderer::new(doc.camera(), world, lights, doc.settings());
    info!("Let's start rendering...");
    let buffer = renderer.render();
    // Close Timer
    let duration: Duration = start.elapsed();
    info!("Time elapsed in rendering() is: {:?}", duration);
    // Render
    let (img_width, img_height) = doc.settings().get_dimensions();
    match write_rgb_file(&output, img_width, img_height, |x, y| buffer.get_rgb(x, y)) {
        Ok(_) => info!("Image written to: {:?}", output),
        Err(e) => {
            error!("Error writing image: {}", e);
            std::process::exit(1);
        }
    }
    convert();
}
