use exr::prelude as exrs;
use exr::prelude::*;
use image as png;
pub fn convert() {
    // read from the exr file directly into a new `png::RgbaImage` image without intermediate buffers
    let reader = exrs::read()
        .no_deep_data()
        .largest_resolution_level()
        .rgba_channels(
            |resolution, _channels: &RgbaChannels| -> png::RgbaImage {
                png::ImageBuffer::new(resolution.width() as u32, resolution.height() as u32)
            },
            // set each pixel in the png buffer from the exr file
            |png_pixels, position, (r, g, b, a): (f32, f32, f32, f32)| {
                // TODO implicit argument types!
                png_pixels.put_pixel(
                    position.x() as u32,
                    position.y() as u32,
                    png::Rgba([tone_map(r), tone_map(g), tone_map(b), (a * 255.0) as u8]),
                );
            },
        )
        .first_valid_layer()
        .all_attributes();

    // an image that contains a single layer containing an png rgba buffer
    let image: Image<Layer<SpecificChannels<png::RgbaImage, RgbaChannels>>> = reader
        .from_file("output.exr")
        .expect("run the `1_write_rgba` example to generate the required file");

    /// compress any possible f32 into the range of [0,1].
    /// and then convert it to an unsigned byte.
    fn tone_map(linear: f32) -> u8 {
        let clamped = linear.clamp(0.0, 1.0);
        let srgb = if clamped <= 0.0031308 {
            12.92 * clamped
        } else {
            1.055 * clamped.powf(1.0 / 2.4) - 0.055
        };
        (srgb * 255.0 + 0.5).floor() as u8
    }

    // save the png buffer to a png file
    let png_buffer = &image.layer_data.channel_data.pixels;
    png_buffer.save("rgb.png").unwrap();
    println!("created image rgb.png")
}
