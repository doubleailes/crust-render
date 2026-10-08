//! [`PixelRect`]: the rectangle of the frame a render traces (its region),
//! and the one place that indexes a region-sized film.

use std::fmt;

/// A half-open rectangle of pixels, `x0..x1` × `y0..y1`.
///
/// The render's region ([`RenderSettings::region`](super::RenderSettings::region))
/// is one in **image** space: top-left origin, `y` growing downwards, as an
/// image viewer, `--region` and an EXR data window count. The tracer's own
/// raster rows grow upwards (row 0 is the bottom of the frame, where the
/// camera's `v = 0` is), so it works on the region's [`flip_y`](Self::flip_y);
/// either way the rectangle is in the full frame's pixels, never relative to
/// the region.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelRect {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl PixelRect {
    /// The rectangle `x0..x1` × `y0..y1`, as given: empty when `x1 <= x0`
    /// or `y1 <= y0`.
    pub const fn new(x0: usize, y0: usize, x1: usize, y1: usize) -> Self {
        PixelRect { x0, y0, x1, y1 }
    }

    /// The whole of a `width` × `height` frame.
    pub const fn full(width: usize, height: usize) -> Self {
        PixelRect::new(0, 0, width, height)
    }

    pub fn width(&self) -> usize {
        self.x1.saturating_sub(self.x0)
    }

    pub fn height(&self) -> usize {
        self.y1.saturating_sub(self.y0)
    }

    /// Pixels inside the rectangle.
    pub fn area(&self) -> usize {
        self.width() * self.height()
    }

    pub fn is_empty(&self) -> bool {
        self.area() == 0
    }

    pub fn contains(&self, x: usize, y: usize) -> bool {
        (self.x0..self.x1).contains(&x) && (self.y0..self.y1).contains(&y)
    }

    /// Row-major index of pixel `(x, y)` — frame coordinates — in a
    /// rectangle-sized plane. Every region-sized film is indexed through
    /// this, so an offset region cannot be read with the frame's stride.
    /// `(x, y)` must be inside the rectangle.
    #[inline]
    pub fn index(&self, x: usize, y: usize) -> usize {
        debug_assert!(self.contains(x, y), "({x}, {y}) outside {self}");
        (y - self.y0) * self.width() + (x - self.x0)
    }

    /// The part of the rectangle inside a `width` × `height` frame, or
    /// `None` when nothing of it is.
    pub fn clip_to(&self, width: usize, height: usize) -> Option<PixelRect> {
        let clipped = PixelRect::new(
            self.x0.min(width),
            self.y0.min(height),
            self.x1.min(width),
            self.y1.min(height),
        );
        (!clipped.is_empty()).then_some(clipped)
    }

    /// The same pixels with rows counted from the other edge of a frame
    /// `height` rows tall: image space (top-down) ↔ the tracer's raster
    /// space (bottom-up). Its own inverse.
    pub fn flip_y(&self, height: usize) -> PixelRect {
        PixelRect::new(self.x0, height - self.y1, self.x1, height - self.y0)
    }
}

/// `(x0, y0)–(x1, y1)`, half-open.
impl fmt::Display for PixelRect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "({}, {})–({}, {})", self.x0, self.y0, self.x1, self.y1)
    }
}

#[cfg(test)]
mod tests {
    use super::PixelRect;

    #[test]
    fn clipping_keeps_the_part_inside_the_frame() {
        let r = PixelRect::new(600, 300, 700, 400);
        assert_eq!(
            r.clip_to(640, 360),
            Some(PixelRect::new(600, 300, 640, 360))
        );
        assert_eq!(
            PixelRect::full(640, 360).clip_to(640, 360),
            Some(PixelRect::full(640, 360))
        );
    }

    #[test]
    fn a_rectangle_outside_the_frame_clips_to_nothing() {
        assert_eq!(PixelRect::new(700, 0, 800, 100).clip_to(640, 360), None);
        assert_eq!(PixelRect::new(0, 360, 10, 400).clip_to(640, 360), None);
        assert_eq!(PixelRect::new(5, 5, 5, 9).clip_to(640, 360), None);
        assert!(PixelRect::new(9, 0, 5, 1).is_empty());
    }

    #[test]
    fn index_is_relative_to_the_rectangle_origin() {
        let r = PixelRect::new(37, 21, 101, 77);
        assert_eq!((r.width(), r.height(), r.area()), (64, 56, 64 * 56));
        assert_eq!(r.index(37, 21), 0);
        assert_eq!(r.index(38, 21), 1);
        assert_eq!(r.index(37, 22), 64);
        assert_eq!(r.index(100, 76), r.area() - 1);
        assert!(r.contains(100, 76) && !r.contains(101, 76) && !r.contains(36, 21));
    }

    #[test]
    fn flip_y_mirrors_the_rows_and_is_its_own_inverse() {
        let r = PixelRect::new(37, 21, 101, 77);
        let f = r.flip_y(100);
        assert_eq!(f, PixelRect::new(37, 23, 101, 79));
        assert_eq!(f.flip_y(100), r);
        assert_eq!(PixelRect::full(8, 4).flip_y(4), PixelRect::full(8, 4));
    }
}
