use glam::Vec3A;

use crate::tracer::PixelRect;

/// The `Buffer` struct represents a 2D image buffer used to store pixel colors.
/// It provides methods to set and retrieve pixel values, as well as access RGB data.
///
/// A buffer holds the pixels of one rectangle of a frame — the whole frame
/// unless the render was given a region — and is addressed in the frame's
/// coordinates either way: [`set_pixel`](Self::set_pixel) and
/// [`get_pixel`](Self::get_pixel) take frame raster coordinates (rows
/// bottom-up), [`get_rgb`](Self::get_rgb) the rectangle's own top-down ones.
pub struct Buffer {
    /// The width of the frame in pixels.
    width: usize,
    /// The height of the frame in pixels.
    height: usize,
    /// The pixels held, in raster space (rows bottom-up).
    rect: PixelRect,
    /// A flat vector storing the color data for each pixel of `rect`.
    data: Vec<Vec3A>,
}

impl Buffer {
    /// Creates a new `Buffer` with the specified width and height.
    ///
    /// # Parameters
    /// - `width`: The width of the buffer in pixels.
    /// - `height`: The height of the buffer in pixels.
    ///
    /// # Returns
    /// - A new instance of `Buffer` initialized with black pixels.
    pub fn new(width: usize, height: usize) -> Self {
        Buffer::for_raster_rect(width, height, PixelRect::full(width, height))
    }

    /// A black buffer of a `width` × `height` frame holding only `region`
    /// — image space (top-left origin), inside the frame — as a render
    /// with that region returns.
    pub fn with_region(width: usize, height: usize, region: PixelRect) -> Self {
        assert!(
            region.clip_to(width, height) == Some(region),
            "region {region} is not a non-empty part of the {width}x{height} frame"
        );
        Buffer::for_raster_rect(width, height, region.flip_y(height))
    }

    /// A black buffer holding only `rect` — raster space, inside the
    /// `width` × `height` frame — of the frame.
    pub(crate) fn for_raster_rect(width: usize, height: usize, rect: PixelRect) -> Self {
        Buffer {
            width,
            height,
            rect,
            data: vec![Vec3A::ZERO; rect.area()],
        }
    }

    /// The full frame's size, in pixels.
    pub fn frame_size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// The size of what the buffer holds: the region's.
    pub fn size(&self) -> (usize, usize) {
        (self.rect.width(), self.rect.height())
    }

    /// The pixels the buffer holds, in image space (top-left origin, rows
    /// down) — what an EXR's data window records.
    pub fn region(&self) -> PixelRect {
        self.rect.flip_y(self.height)
    }

    /// Sets the color of a specific pixel in the buffer.
    ///
    /// # Parameters
    /// - `x`: The x-coordinate of the pixel.
    /// - `y`: The y-coordinate of the pixel.
    /// - `color`: The `Vec3A` to set for the pixel.
    ///
    /// This method ensures that the coordinates are within bounds before setting the pixel.
    /// A pixel outside the buffer's region is ignored.
    pub fn set_pixel(&mut self, x: usize, y: usize, color: Vec3A) {
        if self.rect.contains(x, y) {
            self.data[self.rect.index(x, y)] = color;
        }
    }

    /// Retrieves the color of a specific pixel in the buffer.
    ///
    /// # Parameters
    /// - `x`: The x-coordinate of the pixel.
    /// - `y`: The y-coordinate of the pixel.
    ///
    /// # Returns
    /// - The `Vec3A` of the pixel at the specified coordinates.
    /// - Returns black (`Vec3A::new(0.0, 0.0, 0.0)`) if the coordinates are out of bounds,
    ///   or outside the buffer's region.
    pub fn get_pixel(&self, x: usize, y: usize) -> Vec3A {
        if self.rect.contains(x, y) {
            self.data[self.rect.index(x, y)]
        } else {
            Vec3A::new(0.0, 0.0, 0.0)
        }
    }

    /// Retrieves the RGB values of a specific pixel in the buffer.
    ///
    /// # Parameters
    /// - `x`: The x-coordinate of the pixel.
    /// - `y`: The y-coordinate of the pixel.
    ///
    /// # Returns
    /// - A tuple `(f32, f32, f32)` representing the RGB values of the pixel.
    ///
    /// This method flips the y-coordinate to match the image coordinate system
    /// and converts the pixel color to RGB format. `(x, y)` counts from the
    /// top-left of the buffer's region — of the frame, without one — as an
    /// image of the region's [`size`](Self::size) is written.
    pub fn get_rgb(&self, x: usize, y: usize) -> (f32, f32, f32) {
        let pixel: Vec3A = self.get_pixel(self.rect.x0 + x, self.rect.y1 - 1 - y);
        (pixel.x, pixel.y, pixel.z)
    }
}
