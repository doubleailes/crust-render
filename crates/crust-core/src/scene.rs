use crate::camera::Camera;
use crate::hittable_list::HittableList;
use crate::light::LightList;
use crate::tracer::RenderSettings;

/// The renderer's runtime scene. Produced either from a RON `Document`
/// (`Document::get_scene`) or from a USD stage (`Scene::from_usd`, behind
/// the `usd` feature). `Renderer::new` consumes this directly.
pub struct Scene {
    pub camera: Camera,
    pub world: HittableList,
    pub lights: LightList,
    pub settings: RenderSettings,
}

impl Scene {
    pub fn new(
        camera: Camera,
        world: HittableList,
        lights: LightList,
        settings: RenderSettings,
    ) -> Self {
        Self {
            camera,
            world,
            lights,
            settings,
        }
    }
}

#[cfg(feature = "usd")]
mod usd_import;

#[cfg(feature = "usd")]
impl Scene {
    /// Load a full runtime scene (camera, geometry, lights, render settings)
    /// from a USD stage — `.usda`, `.usdc`, or `.usdz`.
    ///
    /// * `UsdGeomCamera` → `Camera` (world transform + focal length +
    ///   aperture-derived vfov). Falls back to `world::get_settings`'s
    ///   camera when the stage authors none.
    /// * `UsdGeomMesh` → triangulated BVH with world-baked vertices. Bound
    ///   material resolved via `MaterialBindingAPI`.
    /// * `UsdGeomSphere` → analytic `crust::Sphere`.
    /// * `UsdLuxSphereLight` → an `Emissive` sphere that acts as both
    ///   geometry and light. Other lux schemas warn and are skipped.
    /// * `UsdRenderSettings` (plus `crust:*` custom attrs for spp / depth
    ///   / etc.) → `RenderSettings`. Falls back to sensible defaults.
    pub fn from_usd(path: &std::path::Path) -> std::io::Result<Scene> {
        usd_import::load_scene(path)
    }
}
