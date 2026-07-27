use crate::camera::Camera;
use crate::environment::EnvironmentMap;
use crate::light::LightList;
use crate::rt_world::World;
use crate::stats::RenderStats;
use crate::tracer::RenderSettings;
use crate::volume::VolumeRegion;

/// The renderer's runtime scene, produced from a USD stage
/// (`Scene::from_usd`) or assembled by hand (`Scene::new`, e.g. from the
/// procedural `world::simple_scene`). `Renderer::new` consumes this
/// directly. `world` is a committed [`World`] — kernel scene plus the
/// per-geometry material table.
pub struct Scene {
    pub camera: Camera,
    pub world: World,
    pub lights: LightList,
    pub settings: RenderSettings,
    /// Participating-media regions (smoke, fog, …), kept outside `world`
    /// so their bounds never act as occluding geometry.
    pub volumes: Vec<VolumeRegion>,
    /// Import phase timings and scene counts. Populated by
    /// [`Scene::from_usd`]; empty for a hand-assembled scene. The host adds
    /// its own render and output phases before reporting.
    pub stats: RenderStats,
}

impl Scene {
    pub fn new(
        camera: Camera,
        world: World,
        lights: LightList,
        settings: RenderSettings,
    ) -> Self {
        Self {
            camera,
            world,
            lights,
            settings,
            volumes: Vec::new(),
            stats: RenderStats::new(),
        }
    }

    pub fn with_volumes(mut self, volumes: Vec<VolumeRegion>) -> Self {
        self.volumes = volumes;
        self
    }
}

mod usd_import;

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
    pub fn from_usd(path: &std::path::Path) -> Result<Scene, crate::Error> {
        Scene::from_usd_with_assets(path, &NoAssets)
    }

    /// [`Scene::from_usd`], with the host supplying decoded images.
    ///
    /// crust-core has no image-decoding dependencies by design, so it never
    /// opens a texture itself: when a `UsdLuxDomeLight` authors
    /// `inputs:texture:file`, the importer resolves the asset path against
    /// the USD layer and asks `assets` for the pixels. A host that cannot
    /// (or will not) decode returns `None` and the dome falls back to its
    /// uniform colour.
    pub fn from_usd_with_assets(
        path: &std::path::Path,
        assets: &dyn AssetLoader,
    ) -> Result<Scene, crate::Error> {
        usd_import::load_scene(path, assets)
    }
}

/// How the engine asks its host to decode an image.
///
/// The seam exists so `crust-core` stays free of image-format
/// dependencies — the CLI already links `exr` and `image`, so decoding
/// belongs there. It is also the natural place to grow general texture
/// support.
pub trait AssetLoader: Send + Sync {
    /// Decodes a lat-long environment map. `path` has already been resolved
    /// against the USD layer's directory. `None` — for an unreadable file,
    /// an unsupported format, or a host that does not decode at all — is
    /// not an error: the caller falls back.
    fn load_environment(&self, path: &std::path::Path) -> Option<EnvironmentMap>;
}

/// The default host: decodes nothing. `Scene::from_usd` uses it, so a
/// caller that does not care about textures needs no extra ceremony.
pub struct NoAssets;

impl AssetLoader for NoAssets {
    fn load_environment(&self, path: &std::path::Path) -> Option<EnvironmentMap> {
        tracing::warn!(
            "No asset loader: environment map {} ignored — the dome falls back \
             to its uniform colour. Use Scene::from_usd_with_assets to supply one.",
            path.display()
        );
        None
    }
}
