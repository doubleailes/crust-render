use clap::Parser;
use crust_render::camera::Camera;
use crust_render::convert;
use crust_render::tracer::{RenderSettings, Renderer};
use crust_render::world::simple_scene;
use exr::prelude::*;
use utils::Point3;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// The number of samples per pixel
    #[arg(short, long, default_value_t = 100)]
    samples_per_pixel: u32,

    /// The maximum depth of the ray tracing
    #[arg(short, long, default_value_t = 50)]
    max_depth: u32,
}

// Constants

const ASPECT_RATIO: f32 = 16.0 / 9.0;
const IMAGE_WIDTH: usize = 400;
const IMAGE_HEIGHT: usize = (IMAGE_WIDTH as f32 / ASPECT_RATIO) as usize;
const MIN_SAMPLES: u32 = 32;
const VARIANCE_THRESHOLD: f32 = 0.0; // You can tweak this!

fn main() {
    let cli = Cli::parse();
    let samples_per_pixel: u32 = cli.samples_per_pixel;
    let max_depth: u32 = cli.max_depth;
    // World

    let (world, lights) = simple_scene();
    // Camera

    let lookfrom = Point3::new(13.0, 2.0, 3.0);
    let lookat = Point3::new(0.0, 0.0, 0.0);
    let vup = Point3::new(0.0, 1.0, 0.0);
    let dist_to_focus = 10.0;
    let aperture = 0.1;

    let cam = Camera::new(
        lookfrom,
        lookat,
        vup,
        20.0,
        ASPECT_RATIO,
        aperture,
        dist_to_focus,
    );
    let render_settings = RenderSettings::new(
        samples_per_pixel,
        max_depth,
        IMAGE_WIDTH,
        IMAGE_HEIGHT,
        MIN_SAMPLES,
        VARIANCE_THRESHOLD,
    );
    let renderer = Renderer::new(cam, world, lights, render_settings);
    let buffer = renderer.render();
    // Render
    write_rgb_file("output.exr", IMAGE_WIDTH, IMAGE_HEIGHT, |x, y| {
        buffer.get_rgb(x, y)
    })
    .expect("writing image");
    convert::convert();
}
