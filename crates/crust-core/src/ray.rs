use crate::medium::Medium;
use glam::Vec3A;
use std::sync::Arc;

pub use crust_rt::{MASK_ALL, MASK_CAMERA, MASK_INDIRECT, MASK_SHADOW};

/// The renderer's ray: the kernel ray (origin, direction, shutter time,
/// visibility mask — see [`crust_rt::Ray`]) plus the renderer-side state
/// the kernel deliberately does not know about: the participating
/// `medium` the ray is currently travelling through, used by transmissive
/// OpenPBR materials so the tracer can apply Beer-Lambert attenuation
/// between surface hits.
#[derive(Default, Clone)]
pub struct Ray {
    rt: crust_rt::Ray,
    medium: Option<Arc<Medium>>,
}

impl Ray {
    /// Creates a new `Ray` with the specified origin and direction, in
    /// vacuum (no medium), at shutter time 0, visible to all geometry.
    pub fn new(origin: Vec3A, direction: Vec3A) -> Ray {
        Ray {
            rt: crust_rt::Ray::new(origin, direction),
            medium: None,
        }
    }

    /// Creates a new `Ray` travelling through the given medium. Use this on
    /// a refraction that enters a transmissive volume.
    pub fn new_in_medium(origin: Vec3A, direction: Vec3A, medium: Arc<Medium>) -> Ray {
        Ray {
            rt: crust_rt::Ray::new(origin, direction),
            medium: Some(medium),
        }
    }

    /// Same ray with the shutter time replaced.
    pub fn with_time(mut self, time: f32) -> Ray {
        self.rt.time = time;
        self
    }

    /// Same ray with the visibility mask replaced.
    pub fn with_mask(mut self, mask: u32) -> Ray {
        self.rt.mask = mask;
        self
    }

    /// The kernel view of this ray — what `crust_rt` queries take.
    pub fn rt(&self) -> &crust_rt::Ray {
        &self.rt
    }

    pub fn origin(&self) -> Vec3A {
        self.rt.origin
    }

    pub fn direction(&self) -> Vec3A {
        self.rt.dir
    }

    pub fn medium(&self) -> Option<&Arc<Medium>> {
        self.medium.as_ref()
    }

    pub fn time(&self) -> f32 {
        self.rt.time
    }

    pub fn mask(&self) -> u32 {
        self.rt.mask
    }

    pub fn at(&self, t: f32) -> Vec3A {
        self.rt.at(t)
    }
}
