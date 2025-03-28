use crate::vec3::Vec3;

pub struct Buffer {
    width: usize,
    height: usize,
    data: Vec<Vec3>,
}
impl Buffer {
    pub fn new(width: usize, height: usize) -> Self {
        let data = vec![Vec3::new(0.0, 0.0, 0.0); width * height];
        Buffer {
            width,
            height,
            data,
        }
    }

    pub fn set_pixel(&mut self, x: usize, y: usize, color: Vec3) {
        if x < self.width && y < self.height {
            self.data[y * self.width + x] = color;
        }
    }

    pub fn get_pixel(&self, x: usize, y: usize) -> Vec3 {
        if x < self.width && y < self.height {
            self.data[y * self.width + x]
        } else {
            Vec3::new(0.0, 0.0, 0.0)
        }
    }
    pub fn get_rgb(&self, x: usize, y: usize) -> (f32, f32, f32) {
        let pixel = self.get_pixel(x, self.height - y);
        // Flip the y-coordinate to match the image coordinate system
        // and convert to RGB format
        (pixel.x(), pixel.y(), pixel.z())
    }
}
