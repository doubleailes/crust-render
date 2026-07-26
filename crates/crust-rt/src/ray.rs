use glam::Vec3A;

/// Ray visibility categories, Embree-mask style: a ray carries the bit of
/// the category it belongs to, geometry carries a mask of the categories
/// it is visible to, and an intersection only counts when
/// `geometry_mask & ray_mask != 0`. Geometry and rays both default to
/// [`MASK_ALL`], so masking is zero-cost until something opts in.
pub const MASK_CAMERA: u32 = 1 << 0;
pub const MASK_SHADOW: u32 = 1 << 1;
pub const MASK_INDIRECT: u32 = 1 << 2;
pub const MASK_ALL: u32 = u32::MAX;

/// A ray: origin, (unnormalized) direction, shutter `time` in `[0, 1)` for
/// motion blur, and the visibility category `mask`. Plain `Copy` data —
/// renderer-side state like the participating medium a path is inside
/// belongs to the caller, not the kernel.
#[derive(Clone, Copy, Debug)]
pub struct Ray {
    pub origin: Vec3A,
    pub dir: Vec3A,
    pub time: f32,
    pub mask: u32,
}

impl Default for Ray {
    fn default() -> Self {
        Ray::new(Vec3A::ZERO, Vec3A::ZERO)
    }
}

impl Ray {
    /// A ray at shutter time 0, visible to all geometry.
    pub fn new(origin: Vec3A, dir: Vec3A) -> Ray {
        Ray {
            origin,
            dir,
            time: 0.0,
            mask: MASK_ALL,
        }
    }

    pub fn with_time(mut self, time: f32) -> Ray {
        self.time = time;
        self
    }

    pub fn with_mask(mut self, mask: u32) -> Ray {
        self.mask = mask;
        self
    }

    pub fn at(&self, t: f32) -> Vec3A {
        self.origin + t * self.dir
    }
}
