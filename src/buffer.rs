use crate::Color;

pub struct Buffer {
    width: usize,
    height: usize,
    data: Vec<Color>,
}
impl Buffer {
    pub fn new(width: usize, height: usize) -> Self {
        let data = vec![Color::new(0.0, 0.0, 0.0); width * height];
        Buffer {
            width,
            height,
            data,
        }
    }

    pub fn set_pixel(&mut self, x: usize, y: usize, color: Color) {
        if x < self.width && y < self.height {
            self.data[y * self.width + x] = color;
        }
    }

    pub fn get_pixel(&self, x: usize, y: usize) -> Color {
        if x < self.width && y < self.height {
            self.data[y * self.width + x]
        } else {
            Color::new(0.0, 0.0, 0.0)
        }
    }
    pub fn get_rgb(&self, x: usize, y: usize) -> (f32, f32, f32) {
        let pixel: Color = self.get_pixel(x, self.height - 1 - y);
        // Flip the y-coordinate to match the image coordinate system
        // and convert to RGB format
        pixel.rgb()
    }
}
