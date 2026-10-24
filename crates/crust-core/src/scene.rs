use crate::camera::Camera;
use crate::environment::EnvironmentMap;
use crate::light::LightList;
use crate::rt_world::World;
use crate::stats::RenderStats;
use crate::tracer::RenderSettings;
use crate::volume::VolumeRegion;
use crate::warning;

/// The render camera's lens as the import read it at the frame, unauthored
/// values at the render's fallbacks.
#[derive(Debug, Clone, PartialEq)]
pub struct Lens {
    /// Millimetres.
    pub focal_length: f32,
    /// Horizontal and vertical, in millimetres, as authored; the vertical
    /// defaults to the horizontal over the image's aspect ratio.
    pub aperture: [f32; 2],
    /// The `aspectRatioConformPolicy` the aperture was conformed to the
    /// image by.
    pub conform: String,
    /// `0` is a pinhole.
    pub f_stop: f32,
    pub focus_distance: f32,
}

/// What shaped a render beyond its settings: the lens it was seen through,
/// whether its blurs were on, and the layers the stage was composed from.
/// Recorded by the import ([`Scene::source`]); the default is a
/// hand-assembled scene's, which has no camera prim and no layers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Source {
    /// `None` for the procedural camera.
    pub lens: Option<Lens>,
    /// Whether the camera rendered through has an open lens: an `fStop`
    /// above 0 and depth of field not disabled.
    pub depth_of_field: bool,
    /// Whether moving geometry was blurred: motion blur on, and a shutter
    /// that opens an interval at a frame.
    pub motion_blur: bool,
    /// The files of the stage's root layer stack in strength order: the
    /// session layer when there is one, the root layer, its sublayers. A
    /// layer inside a package is the package; an anonymous layer is left out.
    pub layer_stack: Vec<std::path::PathBuf>,
    /// The prims the strongest layer of that stack hides with an `over`
    /// authoring `visibility = "invisible"` (as a default or in a time
    /// sample): the visibility overrides a session or override layer
    /// applied. `None` for a hand-assembled scene.
    pub invisible_overrides: Option<u32>,
}

/// The renderer's runtime scene, produced from a USD stage
/// (`Scene::from_usd`) or assembled by hand (`Scene::new`, e.g. from the
/// procedural `world::simple_scene`). Its camera, world, lights and settings
/// construct a [`crate::Renderer`]; attach its volumes with `with_volumes`.
/// `world` is a committed [`World`] — kernel scene plus the
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
    /// The `RenderProduct`s the stage asks for and the AOVs in each — see
    /// [`AovRequest`](crate::AovRequest). Empty when the stage authors none,
    /// which means "write the single beauty image".
    pub aovs: crate::AovRequest,
    /// The colour space every pixel is in: the working space the stage was
    /// imported into ([`crate::color`]). An output that records its colour
    /// space (an EXR's chromaticities and `colorInteropID`) or encodes for a
    /// display (the preview PNG) reads it here.
    pub working_space: crate::color::Space,
    /// The camera prim the render goes through, after any fallback (a
    /// `RenderSettings.camera` that is not on the stage falls back to the
    /// first camera met); `None` for the procedural camera.
    pub camera_path: Option<String>,
    /// The time code the stage was evaluated at, as given — a subframe
    /// included, unlike the sampler seed [`RenderSettings::frame`] holds,
    /// which is its integer part. `None` when attributes read their defaults.
    pub time: Option<f64>,
    /// The coded warnings the import raised, one record per code in the
    /// order they first fired ([`crate::warnings`]). Empty for a
    /// hand-assembled scene.
    pub warnings: Vec<crate::Warning>,
    /// Where the scene's materials, lights and geometry came from, for an
    /// in-place update ([`crate::live`]). `None` for a hand-assembled scene.
    pub provenance: Option<crate::live::Provenance>,
    /// The file of every USD layer the import composed — the root layer, its
    /// sublayers and the layers behind references and payloads, a layer inside
    /// a package as the package — sorted. What a host watches to see the
    /// stage change on disk. Empty for a hand-assembled scene.
    pub layers: Vec<std::path::PathBuf>,
    /// The active render pass and what it held out and hid
    /// ([`UsdImportOptions::pass`]); `None` with no pass.
    pub pass: Option<crate::PassSummary>,
    /// The `RenderSettings` prim the import read its settings from (the
    /// active pass's `renderSource`, else the stage's); `None` when the stage
    /// has none there and the settings are the defaults.
    pub settings_prim: Option<String>,
    /// What shaped the image beyond its settings — the lens, the blurs, the
    /// root layer stack and the visibility overrides — for the stamp
    /// ([`Source`]). A hand-assembled scene records its
    /// camera's depth of field and nothing else.
    pub source: Source,
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
            aovs: crate::AovRequest::default(),
            working_space: crate::color::Space::LIN_REC709,
            camera_path: None,
            time: None,
            warnings: Vec::new(),
            provenance: None,
            layers: Vec::new(),
            pass: None,
            settings_prim: None,
            source: Source {
                depth_of_field: camera.has_depth_of_field(),
                ..Default::default()
            },
        }
    }

    pub fn with_volumes(mut self, volumes: Vec<VolumeRegion>) -> Self {
        self.volumes = volumes;
        self
    }
}

mod displace;
mod subdiv;
mod tessellate;
mod usd_import;

pub(crate) use usd_import::implicit;
pub use usd_import::{PassSummary, usd_schema_registry};
pub(crate) use usd_import::{prim_frames, rebuild};

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
        usd_import::load_scene(path, assets, &UsdImportOptions::default())
    }

    /// [`Scene::from_usd_with_assets`], evaluated at USD time code `frame`.
    ///
    /// Every attribute the importer reads — transforms, points, camera,
    /// lights, instancer arrays, render settings — resolves its time
    /// samples at `frame` (interpolated per the stage's interpolation
    /// mode), and an attribute with no samples reads its default value. The
    /// sampler's frame seed is set to `frame`'s integer part, overriding
    /// `crust:frame`, so an image sequence gets independent noise per frame.
    ///
    /// `None` reads every attribute's *default* value, exactly as
    /// [`Scene::from_usd_with_assets`] does — which on a stage that authors
    /// only `timeSamples` is not frame 0 but the schema fallback.
    ///
    /// A non-finite `frame` (`NaN`, `±inf`) is refused with
    /// [`crate::Error::InvalidFrame`] before the stage is opened.
    pub fn from_usd_at_frame(
        path: &std::path::Path,
        assets: &dyn AssetLoader,
        frame: Option<f64>,
    ) -> Result<Scene, crate::Error> {
        Scene::from_usd_with_options(
            path,
            assets,
            &UsdImportOptions {
                frame,
                ..UsdImportOptions::default()
            },
        )
    }

    /// [`Scene::from_usd_with_assets`] with every import choice spelled out
    /// — see [`UsdImportOptions`].
    pub fn from_usd_with_options(
        path: &std::path::Path,
        assets: &dyn AssetLoader,
        options: &UsdImportOptions,
    ) -> Result<Scene, crate::Error> {
        usd_import::load_scene(path, assets, options)
    }

    /// The `kind` prims of the USD stage at `path` — cameras, lights or
    /// materials — as absolute prim paths in namespace order, without
    /// importing anything else.
    ///
    /// They are found by the import's own walk, so what is listed is what a
    /// render would use (see [`ListKind`] for each kind's rules): a camera
    /// listed is a path [`UsdImportOptions::camera`] accepts.
    pub fn list_usd(path: &std::path::Path, kind: ListKind) -> Result<Vec<String>, crate::Error> {
        usd_import::list_prims(path, kind)
    }

    /// The prims [`Scene::list_usd`] lists, in the same order, each with the
    /// values a render reads for it (`crust ls --json`), evaluated at time
    /// code `frame` (`None`: attribute defaults, as a render without one
    /// reads them). Read through the import's own readers, an unauthored
    /// value at the fallback the render uses. Costlier than the paths alone:
    /// a material's `bound` resolves the binding of every geometry prim.
    ///
    /// A non-finite `frame` is refused with [`crate::Error::InvalidFrame`].
    pub fn list_usd_records(
        path: &std::path::Path,
        kind: ListKind,
        frame: Option<f64>,
    ) -> Result<Vec<ListRecord>, crate::Error> {
        usd_import::list_records(path, kind, frame)
    }
}

/// The `crust-ls/1` report's `format`.
pub const LS_FORMAT: &str = "crust-ls/1";

/// The `crust-ls/1` report: what was listed, at which time code, and the
/// records in the text listing's order.
#[derive(Clone, Debug, serde::Serialize)]
pub struct Listing {
    /// `camera`, `light`, `material` or `pass`.
    pub kind: &'static str,
    /// The time code the values were evaluated at; `null` for defaults.
    pub frame: Option<f64>,
    pub prims: Vec<ListRecord>,
}

impl Listing {
    pub fn to_json(&self) -> String {
        crate::report::Report::new(LS_FORMAT, self).to_json()
    }
}

/// One listed prim and the values a render reads for it.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(untagged)]
pub enum ListRecord {
    Camera(CameraRecord),
    Light(LightRecord),
    Material(MaterialRecord),
    Pass(PassRecord),
}

impl ListRecord {
    /// The prim's absolute path.
    pub fn path(&self) -> &str {
        match self {
            ListRecord::Camera(r) => &r.path,
            ListRecord::Light(r) => &r.path,
            ListRecord::Material(r) => &r.path,
            ListRecord::Pass(r) => &r.path,
        }
    }
}

/// A `UsdGeomCamera`: its lens as the render reads it, and whether a render
/// goes through it.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct CameraRecord {
    pub path: String,
    #[serde(serialize_with = "crate::report::finite_or_null")]
    pub focal_length_mm: f32,
    /// Horizontal, vertical; the vertical defaults to the horizontal over
    /// the image's aspect ratio, as the render reads it.
    pub aperture_mm: [f32; 2],
    /// `aperture_mm` conformed to the image's aspect by the render's
    /// `aspectRatioConformPolicy`: the aperture the render frames through
    /// this camera.
    pub conformed_aperture_mm: [f32; 2],
    /// `0` is a pinhole.
    #[serde(serialize_with = "crate::report::finite_or_null")]
    pub f_stop: f32,
    #[serde(serialize_with = "crate::report::finite_or_null")]
    pub focus_distance: f32,
    /// The camera `crust render` without `--camera` goes through: the first
    /// RenderProduct's, else `RenderSettings.camera`, else (that missing, or
    /// nothing named) the first one the import meets. At most one is.
    pub is_render_camera: bool,
    /// Under an invisible ancestor (it still renders).
    pub hidden: bool,
}

/// A UsdLux light's `LightAPI` inputs as authored, no transform applied.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct LightRecord {
    pub path: String,
    /// `sphere`, `rect`, `disk`, `cylinder`, `distant`, `dome`, or `mesh` for a
    /// `Mesh` applying `MeshLightAPI`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(serialize_with = "crate::report::finite_or_null")]
    pub intensity: f32,
    #[serde(serialize_with = "crate::report::finite_or_null")]
    pub exposure: f32,
    pub color: [f32; 3],
    pub normalize: bool,
}

/// A `RenderPass`: the settings it renders with.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct PassRecord {
    pub path: String,
    /// The first target of its `renderSource`, as authored; `None` when it
    /// authors none. A target that is not a `RenderSettings` prim is listed
    /// as authored, and a render with the pass warns and reads the stage's.
    pub render_source: Option<String>,
}

/// A `UsdShadeMaterial`: the surface shader the render decodes, and whether
/// anything it renders is bound to it.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct MaterialRecord {
    pub path: String,
    /// The surface shader's `info:id`; `None` when the material has none
    /// the import can name (a MaterialX reference composes no shader prim).
    pub surface: Option<String>,
    /// Whether the import's binding resolution — inheritance, collections,
    /// binding strength, the `full` purpose falling back to all-purpose —
    /// resolves at least one geometry prim it renders to this material.
    pub bound: bool,
}

/// What [`Scene::list_usd`] lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListKind {
    /// `UsdGeomCamera` prims the render can go through: none under an
    /// inactive, abstract or proxy- / guide-purpose ancestor, inside an
    /// instance's prototype or beneath a `PointInstancer`; those under an
    /// invisible ancestor included, as a hidden camera still renders.
    Camera,
    /// The UsdLux lights the import reads (sphere, rect, disk, cylinder,
    /// distant, dome): pruned like geometry, so an invisible light, and a
    /// light inside a prototype or beneath a `PointInstancer`, is not listed.
    /// Whether the import then accepts one is a property of its values at
    /// the rendered time code and its composed transform — a zero radius or
    /// size, a transform that collapses it — which a listing evaluates at no
    /// frame; such a light is listed, and the render skips it with a warning.
    Light,
    /// `UsdShadeMaterial` prims a binding can reach: all but those under an
    /// inactive ancestor or inside an instance's prototype, whether or not
    /// anything binds them.
    Material,
    /// `RenderPass` prims [`UsdImportOptions::pass`] accepts: all but those
    /// under an inactive ancestor.
    Pass,
}

impl ListKind {
    /// The kind's name, as `crust ls` takes it.
    pub fn name(self) -> &'static str {
        match self {
            ListKind::Camera => "camera",
            ListKind::Light => "light",
            ListKind::Material => "material",
            ListKind::Pass => "pass",
        }
    }
}

/// Choices a host makes about how a USD stage is imported.
#[derive(Clone, Debug, Default)]
pub struct UsdImportOptions {
    /// USD time code to evaluate the stage at; see
    /// [`Scene::from_usd_at_frame`]. `None` reads attribute defaults.
    pub frame: Option<f64>,
    /// The camera to render through, as an absolute prim path.
    ///
    /// Chosen in this order: this path, then the stage's
    /// `RenderSettings.camera` relationship, then the first `UsdGeomCamera`
    /// the traversal meets (whose order is unspecified, so a stage with
    /// several cameras — ALab carries one trailer camera per shot beside its
    /// shot camera — needs one of the first two). A path given here that is not a
    /// camera on the stage is an error ([`crate::Error::CameraNotFound`]); a
    /// dangling `RenderSettings.camera` only warns and falls back.
    pub camera: Option<String>,
    /// The subdivision refinement level (the CLI's `--subdiv-level`), over
    /// the stage's `crust:subdivisionLevel` render setting and the default
    /// of 0. Applies to every mesh whose `subdivisionScheme` is not `none`
    /// (unauthored is USD's fallback, `catmullClark`); 0 renders each cage
    /// with smooth normals. Clamped to 6.
    pub subdivision_level: Option<u32>,
    /// Adaptive subdivision (the CLI's `--subdiv-edge-length`), over the
    /// stage's `crust:subdivisionEdgeLength` render setting: a target cage-edge
    /// length in pixels. Each subdivision mesh is then refined per placement,
    /// only as far as its size on screen asks, and
    /// [`subdivision_level`](Self::subdivision_level) caps the level (default
    /// 3). Needs the render camera to be named, by [`camera`](Self::camera)
    /// or `RenderSettings.camera`; a value that is not positive and finite is
    /// ignored with a warning.
    pub subdivision_edge_length: Option<f32>,
    /// Leave the last composed USD stage allocated instead of freeing it.
    ///
    /// Tearing a composed stage down is not free: openusd's index cache is
    /// millions of small allocations, and freeing ALab's took 45 s — 22% of
    /// its import — right before the render could start. A host that renders
    /// once and exits (the CLI) loses nothing by skipping it; the memory goes
    /// back to the OS at exit, and glibc keeps most of a freed heap mapped in
    /// the meantime anyway. A host that loads several scenes in one process
    /// must leave this off, or each load leaks its stage.
    ///
    /// Applies to a **single-stage** import only. A streamed import still drops
    /// every chunk's stage as it goes, the last one included: its memory bound
    /// is the point of streaming, and its peak often comes after the traversal
    /// (the top-level BVH commit), where a kept stage would add to it. Nothing
    /// in the returned [`Scene`] borrows from the stage, and the image is the
    /// same either way.
    pub skip_stage_teardown: bool,
    /// The working colour space to render in (the CLI's `--working-space`),
    /// as any name or alias of the OCIO config — `acescg`, `lin_rec2020`, … —
    /// over the stage's `RenderSettings.renderingColorSpace` and the default,
    /// `lin_rec709`. Must be scene-linear
    /// ([`Error::InvalidWorkingSpace`](crate::Error::InvalidWorkingSpace)
    /// otherwise). See [`crate::color`].
    pub working_space: Option<String>,
    /// The `RenderPass` prim to render (the CLI's `--pass`), as an absolute
    /// prim path. Its `matte` collection's geometry is held out, what its
    /// `cameraVisibility` collection leaves out is hidden from camera rays,
    /// and its `renderSource`, when it targets a `RenderSettings` prim, is
    /// the settings the render reads. `None` renders with no active pass,
    /// whatever passes the stage defines. A path that is not a `RenderPass`
    /// on the stage is an error ([`crate::Error::PassNotFound`]).
    pub pass: Option<String>,
    /// Keep a copy of the top-level geometry in the scene's world
    /// ([`crate::WorldBuilder::keep_inputs`]), so a live session can move or
    /// reshape a mesh in place ([`crate::live`]). Costs the baked triangles'
    /// memory a second time; the image is the same either way.
    pub keep_geometry: bool,
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
    ///
    /// `space` is the file's colour space and the working space to bring it
    /// to: usually `auto` (an 8-bit image is sRGB, a float one is already in
    /// the working space), or what the texture attribute's `colorSpace`
    /// metadatum names. The map comes back as linear light in the working
    /// space.
    fn load_environment(
        &self,
        path: &std::path::Path,
        space: crate::ColorSpace,
    ) -> Option<EnvironmentMap>;

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
        warning!(
            AssetUnsupportedByHost,
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
    /// `space` says how to decode the stored samples, exactly as for
    /// [`Self::load_texture`]: a colour map is display-encoded
    /// ([`crate::ColorSpace::GAMMA22`], the island's convention) while a
    /// displacement map is data ([`crate::ColorSpace::RAW`]). The file does
    /// not say which it is, and decoding a height map by 2.2 would bend every
    /// offset toward zero.
    ///
    /// Defaulted to `None` so a host that only cares about environment maps —
    /// and [`NoAssets`] — needs no extra ceremony.
    fn load_ptex(
        &self,
        path: &std::path::Path,
        space: crate::ColorSpace,
    ) -> Option<std::sync::Arc<dyn crate::PtexTexture>> {
        let _ = space;
        warning!(
            AssetUnsupportedByHost,
            "Asset loader does not decode Ptex: {} ignored — the surface falls \
             back to its constant baseColor.",
            path.display()
        );
        None
    }

    /// Decodes a light's colour map (`RectLight`'s `inputs:texture:file`) to
    /// linear float RGB — the decode [`Self::load_environment`] does, without
    /// the lat-long importance sampling. `None` means the light emits its
    /// uniform colour.
    ///
    /// `space` is as for [`Self::load_environment`].
    fn load_light_texture(
        &self,
        path: &std::path::Path,
        space: crate::ColorSpace,
    ) -> Option<std::sync::Arc<crate::LightTexture>> {
        let _ = space;
        warning!(
            AssetUnsupportedByHost,
            "Asset loader does not decode light textures: {} ignored — the light \
             emits its uniform colour.",
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
        warning!(
            AssetUnsupportedByHost,
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
    fn load_environment(
        &self,
        path: &std::path::Path,
        _space: crate::ColorSpace,
    ) -> Option<EnvironmentMap> {
        warning!(
            AssetUnsupportedByHost,
            "No asset loader: environment map {} ignored — the dome falls back \
             to its uniform colour. Use Scene::from_usd_with_assets to supply one.",
            path.display()
        );
        None
    }
}
