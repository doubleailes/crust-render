use glam::Vec3A;

/// The `Buffer` struct represents a 2D image buffer used to store pixel colors.
/// It provides methods to set and retrieve pixel values, as well as access RGB data.
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

    /// Adds the provided color to the existing color of a specific pixel in the buffer.
    ///
    /// # Parameters
    /// - `x`: The x-coordinate of the pixel.
    /// - `y`: The y-coordinate of the pixel.
    /// - `color`: The `Vec3A` color to add to the existing pixel color.
    ///
    /// This method ensures that the coordinates are within bounds before modifying the pixel.
    pub fn set_mut_pixel(&mut self, x: usize, y: usize, color: Vec3A) {
        if x < self.width && y < self.height {
            let index = y * self.width + x;
            self.data[index] += color;
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
}
