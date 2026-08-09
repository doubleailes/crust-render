use glam::Vec3A;

/// The `Buffer` struct represents a 2D image buffer used to store pixel colors.
/// It provides methods to set and retrieve pixel values, as well as access RGB data.
#[derive(Clone)]
pub struct Buffer {
    /// The width of the buffer in pixels.
    width: usize,
    /// The height of the buffer in pixels.
    height: usize,
    /// A flat vector storing the color data for each pixel.
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
        let data = vec![Vec3A::new(0.0, 0.0, 0.0); width * height];
        Buffer {
            width,
            height,
            data,
        }
    }

    /// Sets the color of a specific pixel in the buffer.
    ///
    /// # Parameters
    /// - `x`: The x-coordinate of the pixel.
    /// - `y`: The y-coordinate of the pixel.
    /// - `color`: The `Vec3A` to set for the pixel.
    ///
    /// This method ensures that the coordinates are within bounds before setting the pixel.
    pub fn set_pixel(&mut self, x: usize, y: usize, color: Vec3A) {
        if x < self.width && y < self.height {
            self.data[y * self.width + x] = color;
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
    /// - Returns black (`Vec3A::new(0.0, 0.0, 0.0)`) if the coordinates are out of bounds.
    pub fn get_pixel(&self, x: usize, y: usize) -> Vec3A {
        if x < self.width && y < self.height {
            self.data[y * self.width + x]
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
    /// and converts the pixel color to RGB format.
    pub fn get_rgb(&self, x: usize, y: usize) -> (f32, f32, f32) {
        let pixel: Vec3A = self.get_pixel(x, self.height - 1 - y);
        (pixel.x, pixel.y, pixel.z)
    }

    /// The width of the buffer in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// The height of the buffer in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The raw pixel storage: row-major, linear RGB, with **row 0 at the
    /// bottom** of the image (`get_rgb` is the y-flipped, display-order
    /// view). Suited to bulk copies by a host that manages orientation
    /// itself; use [`Buffer::rows_top_down`] for display order.
    pub fn as_slice(&self) -> &[Vec3A] {
        &self.data
    }

    /// The buffer's rows in display order (top row first) — what a host
    /// copies row-by-row into its own top-down framebuffer.
    pub fn rows_top_down(&self) -> impl DoubleEndedIterator<Item = &[Vec3A]> + '_ {
        self.data.chunks_exact(self.width.max(1)).rev()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The raw accessors and the historical `get_rgb` view must describe
    /// the same image: `as_slice` row 0 is the bottom row, `rows_top_down`
    /// starts at the top — exactly `get_rgb`'s orientation.
    #[test]
    fn raw_access_agrees_with_get_rgb() {
        let (w, h) = (3, 2);
        let mut buf = Buffer::new(w, h);
        for y in 0..h {
            for x in 0..w {
                buf.set_pixel(x, y, Vec3A::new(x as f32, y as f32, 0.0));
            }
        }
        assert_eq!((buf.width(), buf.height()), (w, h));
        assert_eq!(buf.as_slice().len(), w * h);
        assert_eq!(buf.as_slice()[0], Vec3A::new(0.0, 0.0, 0.0)); // bottom-left
        assert_eq!(buf.as_slice()[w], Vec3A::new(0.0, 1.0, 0.0)); // second row up

        for (display_y, row) in buf.rows_top_down().enumerate() {
            for (x, pixel) in row.iter().enumerate() {
                let (r, g, b) = buf.get_rgb(x, display_y);
                assert_eq!((pixel.x, pixel.y, pixel.z), (r, g, b));
            }
        }
    }
}
