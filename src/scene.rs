use crate::{camera::Camera, hittable_list::HittableList, LightList};

pub struct Scene{
    camera: Camera,
    min_samples_per_pixel: i32,
    samples_per_pixel: i32,
    max_depth: i32,
    variance_threshold: f32,
    width: usize,
    height: usize,
    lights: LightList,
    hitables: HittableList,
}

impl Scene {
    pub fn new(
        camera: Camera,
        min_samples_per_pixel: i32,
        samples_per_pixel: i32,
        max_depth: i32,
        variance_threshold: f32,
        width: usize,
        height: usize,
        lights: LightList,
        hitables: HittableList,
    ) -> Self {
        Self {
            camera,
            min_samples_per_pixel,
            samples_per_pixel,
            max_depth,
            variance_threshold,
            width,
            height,
            lights,
            hitables,
        }
    }
    pub fn get_camera(&self) -> &Camera {
        &self.camera
    }

    pub fn get_samples_per_pixel(&self) -> i32 {
        self.samples_per_pixel
    }

    pub fn get_max_depth(&self) -> i32 {
        self.max_depth
    }

    pub fn get_width(&self) -> usize {
        self.width
    }

    pub fn get_height(&self) -> usize {
        self.height
    }
    pub fn get_lights(&self) -> &LightList {
        &self.lights
    }
    pub fn get_min_samples_per_pixel(&self) -> i32 {
        self.min_samples_per_pixel
    }
    pub fn get_variance_threshold(&self) -> f32 {
        self.variance_threshold
    }
    pub fn get_hitables(&self) -> &HittableList {
        &self.hitables
    }
}