use clap::Parser;
use crust_render::Document;
use crust_render::Renderer;
use crust_render::convert;
use exr::prelude::*;
use std::time::{Duration, Instant};

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Input Scene path should be a .ron file
    #[arg(short, long)]
    input: String,
    /// Output image path
    /// Default is output.exr
    /// If you want to use a different name, please specify it here
    output: Option<String>,
}

fn main() {
    // CLI
    let cli = Cli::parse();
    let input = cli.input;
    let input_path = std::path::Path::new(&input);
    let output = cli.output.unwrap_or_else(|| "output.exr".to_string());
    let doc: Document = Document::read(input_path).expect("Failed to read document");
    // Timer
    let start = Instant::now();
    // World
    let (world, lights) = doc.get_world();
    // Camera
    let renderer = Renderer::new(doc.camera(), world, lights, doc.settings());
    let buffer = renderer.render();
    // Close Timer
    let duration: Duration = start.elapsed();
    println!("Time elapsed in rendering() is: {:?}", duration);
    // Render
    let (img_width, img_height) = doc.settings().get_dimensions();
    write_rgb_file(output, img_width, img_height, |x, y| buffer.get_rgb(x, y))
        .expect("writing image");
    convert();
}
