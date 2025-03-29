use clap::Parser;
use exr::prelude::*;
use ray_tracing::vec3::Point3;
use ray_tracing::world::random_scene;
use ray_tracing::camera::Camera;
use ray_tracing::tracer::run;
use ray_tracing::convert;

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    /// The number of samples per pixel
    #[arg(short, long, default_value_t = 100)]
    samples_per_pixel: i32,

    /// The maximum depth of the ray tracing
    #[arg(short, long, default_value_t = 50)]
    max_depth: i32,
}

// Constants

const ASPECT_RATIO: f32 = 16.0 / 9.0;
const IMAGE_WIDTH: usize = 400;
const IMAGE_HEIGHT: usize = (IMAGE_WIDTH as f32 / ASPECT_RATIO) as usize;
const MIN_SAMPLES: i32 = 8;
const VARIANCE_THRESHOLD: f32 = 0.0005; // You can tweak this!




fn main() {
    let cli = Cli::parse();
    let samples_per_pixel: i32 = cli.samples_per_pixel;
    let max_depth: i32 = cli.max_depth;
    // World

    let (world, lights) = random_scene();
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
    let scene = ray_tracing::Scene::new(
        cam,
        MIN_SAMPLES,
        samples_per_pixel,
        max_depth,
        VARIANCE_THRESHOLD,
        IMAGE_WIDTH,
        IMAGE_HEIGHT,
        lights,
        world,
    );
    let buffer = run(scene);
    // Render
    write_rgb_file("output.exr", IMAGE_WIDTH, IMAGE_HEIGHT, |x, y| {
        buffer.get_rgb(x, y)
    })
    .expect("writing image");
    convert::convert();
}
