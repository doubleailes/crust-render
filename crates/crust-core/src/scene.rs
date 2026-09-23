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
    pub fn new(camera: Camera, world: World, lights: LightList, settings: RenderSettings) -> Self {
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

mod subdiv;
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
    /// * `UsdLuxSphereLight` / `RectLight` / `DiskLight` / `CylinderLight`
    ///   → one-sided emissive geometry that acts as both surface and light;
    ///   `DistantLight` and `DomeLight` → lights at infinity. All in the
    ///   UsdLux spec's units (nits; `normalize`; colour temperature), with
    ///   `ShapingAPI` on the area lights.
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
    /// uniform colour. IES profiles (`inputs:shaping:ies:file`) cross the
    /// same seam.
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

    /// Opens a UV-addressed texture and returns something that can sample it.
    ///
    /// `path` has already been resolved against the authoring layer's
    /// directory and may still contain a tile token — **`<UDIM>`** or
    /// **`<UVTILE>`**, MaterialX's two spellings of the same grid. Expanding
    /// either into the tile set on disk is the host's job, since which tiles
    /// exist is a filesystem question and the cheapest place to decide how
    /// many of them to hold in memory. `space` says how to decode the stored values — the
    /// file does not say, and getting it wrong is silent (see
    /// [`crate::ColorSpace`]).
    ///
    /// Like [`Self::load_ptex`], the host keeps the pixels and hands back a
    /// sampler. `None` means the material falls back to the constant value
    /// authored beside the texture in the MaterialX graph. Defaulted, like
    /// `load_ptex`, so hosts that decode neither need no extra ceremony.
    fn load_texture(
        &self,
        path: &std::path::Path,
        space: crate::ColorSpace,
    ) -> Option<std::sync::Arc<dyn crate::Texture2D>> {
        let _ = space;
        tracing::warn!(
            "Asset loader does not decode UV textures: {} ignored — the input \
             falls back to its constant value.",
            path.display()
        );
        None
    }

    /// Opens a Ptex file and returns something that can sample it.
    ///
    /// Unlike [`Self::load_environment`], the host keeps ownership of the
    /// pixels and hands back a sampler — see [`crate::PtexTexture`] for why
    /// per-face textures cannot cross this seam as a pixel buffer. `path` has
    /// already been resolved against the USD layer's directory. `None` means
    /// the surface falls back to its constant `baseColor`, which is authored
    /// alongside the texture in every Ptex material the Moana island ships,
    /// so the fallback is a plausible flat colour rather than a black hole.
    ///
    /// Defaulted to `None` so a host that only cares about environment maps —
    /// and [`NoAssets`] — needs no extra ceremony.
    fn load_ptex(&self, path: &std::path::Path) -> Option<std::sync::Arc<dyn crate::PtexTexture>> {
        tracing::warn!(
            "Asset loader does not decode Ptex: {} ignored — the surface falls \
             back to its constant baseColor.",
            path.display()
        );
        None
    }

    /// Decodes an IES photometric profile (`inputs:shaping:ies:file`).
    ///
    /// `path` is resolved as for the other loaders. `None` drops the IES term
    /// from the light's shaping — the rest of it (focus, cone) still applies —
    /// so the light renders unshaped by the profile rather than not at all.
    fn load_ies(&self, path: &std::path::Path) -> Option<std::sync::Arc<crate::IesProfile>> {
        tracing::warn!(
            "Asset loader does not decode IES profiles: {} ignored — the light \
             renders without it.",
            path.display()
        );
        None
    }
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
