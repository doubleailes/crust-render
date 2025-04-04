use clap::Parser;
use crust_render::Document;
use crust_render::Renderer;
use crust_render::convert;
use exr::prelude::*;
use std::time::{Duration, Instant};
use tracing::{debug, info, Level, error};

#[derive(clap::ValueEnum, Clone, Debug, Copy)]
enum LogerLevel {
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
    level: LogerLevel,
}

fn get_loger_level(level: LogerLevel) -> Level {
    match level {
        LogerLevel::Debug => Level::DEBUG,
        LogerLevel::Info => Level::INFO,
        LogerLevel::Warn => Level::WARN,
        LogerLevel::Error => Level::ERROR,
        LogerLevel::Trace => Level::TRACE,
    }
}

fn main() {
    // CLI
    let cli = Cli::parse();
    // Add tracing
    tracing_subscriber::fmt()
    .with_max_level(get_loger_level(cli.level))
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
    let buffer = renderer.render();
    // Close Timer
    let duration: Duration = start.elapsed();
    info!("Time elapsed in rendering() is: {:?}", duration);
    // Render
    let (img_width, img_height) = doc.settings().get_dimensions();
    match write_rgb_file(&output, img_width, img_height, |x, y| buffer.get_rgb(x, y)){
        Ok(_) => info!("Image written to: {:?}", output),
        Err(e) => {
            error!("Error writing image: {}", e);
            std::process::exit(1);
        }
    }
    convert();
}
