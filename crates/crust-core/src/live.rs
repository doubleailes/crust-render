//! Following a stage live: an imported [`Scene`] remembers where its parts
//! came from ([`Provenance`]), and a change confined to render settings,
//! light inputs or material inputs is applied to it in place ([`Change`],
//! [`Provenance::rebuild`], [`Update`]) rather than by importing the stage
//! again.
//!
//! **An in-place update gives exactly the scene a fresh import would.** Every
//! rebuilt part is built by the import's own code — the same material
//! resolution, the same light emitters, the same settings and camera readers
//! — at the scene's time code, and anything a rebuild cannot vouch for is
//! refused with [`Structural`], for the host to fall back to a full import:
//!
//! - a material whose swap would move a flag the world or the geometry was
//!   built on (cutout, straight transmission, medium boundary, emission, the
//!   UV or per-face tables, a displacement), or whose network reaches outside
//!   its own prim;
//! - a light whose rebuild would add or drop a light, change how many
//!   geometries it owns, or move it to or from infinity, and a light the
//!   scene's light links took out of the list (it is no longer one);
//! - a settings change that would move the working colour space, the
//!   subdivision level, what tessellation read from the camera (adaptive
//!   subdivision, implicit shapes sized on screen), the dome's camera
//!   visibility or the render camera;
//! - any rebuild that raises a warning about the rebuilt prim, or a scene
//!   whose warnings already name it: the warnings list must stay the one a
//!   fresh import reports.
//!
//! The provenance and the entry points are keyed by prim path strings and
//! take built values, so a host other than the USD importer (a Hydra
//! delegate) could drive them.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use crate::camera::Camera;
use crate::light::{LightKind, LightList};
use crate::material::Material;
use crate::rt_world::World;
use crate::scene::Scene;
use crate::tracer::RenderSettings;

// Refusals come from the world and the light list too, so the type sits
// below them.
pub use crate::error::Structural;

/// Where an imported scene's parts came from. Built by the USD import in its
/// traversal order; memory in proportion to the material, light and emitting
/// prim counts, nothing per triangle.
#[derive(Clone, Debug, Default)]
pub struct Provenance {
    /// Material prim path → the `World` material slots holding it.
    pub(crate) materials: HashMap<String, MaterialOrigin>,
    /// `(prim, material)`: a prim outside a material that the material's
    /// network reads, through a composition arc or a connection — every
    /// material the import resolved, prototype-scoped ones included.
    pub(crate) material_reads: Vec<(String, String)>,
    /// Light prim path → its place in the light list and its geometry.
    pub(crate) lights: HashMap<String, LightOrigin>,
    /// `(geom_ids, prim path)` per prim that emitted geometry, in traversal
    /// order, so sorted by first id. [`Provenance::prim_of_geometry`] reads a
    /// run as owning every id up to the next run's, as light linking does;
    /// [`Provenance::geometry_of`] reads the ids the prim emitted.
    pub(crate) geometry: Vec<(Range<u32>, String)>,
    /// What the import decided once for the whole stage.
    pub(crate) stage: StageFacts,
}

/// The slots one material fills.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MaterialOrigin {
    /// Every `World` slot whose material is this prim's: by `Arc` identity,
    /// every streamed epoch's `Arc` of it included.
    pub slots: Vec<u32>,
    /// Whether a mesh displaced by it: its geometry depends on its inputs.
    pub displaced: bool,
}

/// One light's place in the scene.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LightOrigin {
    /// Its index in the light list.
    pub index: usize,
    /// The `geom_id`s of the geometry it emitted (an area light's emitter,
    /// which shares its `Emissive`); empty for a light at infinity.
    pub geoms: Range<u32>,
}

/// The stage-wide decisions an import made, which a settings change must
/// not move for an in-place update to stand.
#[derive(Clone, Debug, Default)]
pub(crate) struct StageFacts {
    /// The host's import options.
    pub(crate) options: crate::UsdImportOptions,
    /// The `RenderSettings` prim the import read, when one.
    pub(crate) render_settings: Option<String>,
    /// The camera rendered through, `None` for the procedural one.
    pub(crate) camera: Option<String>,
    /// The camera the settings named before the traversal, `None` when they
    /// named none (the first camera met was rendered).
    pub(crate) named_camera: Option<String>,
    /// Everything the import built into the scene from the settings.
    pub(crate) key: ImportKey,
    /// How the import's implicit shapes used the camera projection.
    pub(crate) used: ProjectionUse,
}

/// What an import builds into the scene from the settings it reads: the
/// settings that, read differently, would give other geometry, other
/// materials or other warnings. Two renders whose settings give the same key
/// can share one import — an MCP settings edit applied in place, the passes
/// of `--all-passes` imported once — and [`ImportKey::refusal`] is the one
/// predicate both ask (design D3 of `render-visibility-and-all-passes`).
/// Everything else the settings say (the camera, the resolution, the
/// products, the sample counts) a render reads, and an import does not.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ImportKey {
    pub(crate) working: Option<crate::color::Space>,
    pub(crate) subdiv_level: u32,
    /// What adaptive subdivision read from the render camera (its projection,
    /// target and ceiling), as bits; `None` for uniform subdivision.
    pub(crate) adaptive: Option<ScreenKey>,
    /// The render camera's projection, as bits, when a camera is named before
    /// the traversal: what an implicit shape would take its radial count
    /// from. Compared only when the import used it ([`ProjectionUse`]).
    pub(crate) projection: Option<ScreenKey>,
    pub(crate) domes_seen_by_camera: bool,
    /// The primvars the geometry keeps for the render's primvar vars, as
    /// `(name, components)`: a product that asks for others needs geometry
    /// read again.
    pub(crate) primvars: Vec<(String, u8)>,
    /// The shutter motion is measured over: geometry is placed at its ends,
    /// so another shutter is another scene. Without the camera it was read
    /// from, which only a check after the traversal reads.
    pub(crate) shutter: ShutterKey,
}

/// How an import's implicit shapes used the camera projection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProjectionUse {
    /// An implicit shape took its radial count from the projection.
    pub(crate) on_screen: bool,
    /// An unshared curved implicit shape took the fixed radial count because
    /// no camera was named: naming one then moves geometry.
    pub(crate) fixed_count: bool,
}

impl ImportKey {
    /// Why a scene imported under `self`, whose implicit shapes used the
    /// projection as `used` says, cannot serve settings whose key is `other`;
    /// `None` when it can.
    pub(crate) fn refusal(&self, used: ProjectionUse, other: &ImportKey) -> Option<&'static str> {
        if other.working != self.working {
            return Some("the working colour space changed");
        }
        if other.subdiv_level != self.subdiv_level {
            return Some("the subdivision level changed");
        }
        // Tessellation that reads the camera stands only when what it read is
        // what it would read now, bit for bit: an exposure keeps it, a move,
        // a lens or a resolution does not.
        if other.adaptive != self.adaptive {
            return Some("adaptive subdivision would read another camera projection");
        }
        if used.on_screen && other.projection != self.projection {
            return Some("implicit shapes would be sized for another camera projection");
        }
        if used.fixed_count && other.projection.is_some() {
            return Some("implicit shapes would be sized for the newly named camera");
        }
        // Geometry was placed at the shutter's ends, so another shutter
        // re-imports, as a transform edit does.
        if other.shutter != self.shutter {
            return Some("the camera shutter changed");
        }
        if other.domes_seen_by_camera != self.domes_seen_by_camera {
            return Some("the dome's camera visibility changed");
        }
        // The geometry kept the primvars the products asked for when it was
        // read, and nothing else: asking for another needs it read again.
        if other.primvars != self.primvars {
            return Some("the products ask for other primvars than the geometry kept");
        }
        None
    }
}

/// The shutter an import measured motion over, as bits: the camera it was
/// read from, its offsets as authored, the interval in time codes (`None`
/// when instantaneous), and whether prims were asked whether their transform
/// varies. The last two decide warnings as well as geometry
/// (`time.motion_needs_frame`, `camera.shutter_instantaneous`), so a change to
/// any part re-imports. `frame_pose` is whether moving placements were posed
/// at the frame (`disableMotionBlur` over a shutter that does not open at
/// the frame), which moves geometry, so turning that flag on or off
/// re-imports too where it matters, and stays an in-place settings edit
/// elsewhere.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ShutterKey {
    pub(crate) camera: Option<String>,
    pub(crate) offsets: [u64; 2],
    pub(crate) interval: Option<[u64; 2]>,
    pub(crate) query: bool,
    pub(crate) frame_pose: bool,
}

/// A value the import's tessellation read from the render camera, as the
/// bits of every number in it: two keys are equal exactly when the
/// tessellation would come out the same, `-0.0` and `NaN` included.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ScreenKey(pub(crate) Vec<u32>);

impl Provenance {
    /// The material prims the scene uses, and the slots each fills.
    pub fn materials(&self) -> impl Iterator<Item = (&str, &MaterialOrigin)> {
        self.materials.iter().map(|(p, o)| (p.as_str(), o))
    }

    /// The material at `path`, when the scene uses it.
    pub fn material(&self, path: &str) -> Option<&MaterialOrigin> {
        self.materials.get(path)
    }

    /// The materials other than `owner` whose network reads `prim` or a prim
    /// above or below it, through a composition arc or a connection: an edit
    /// of `prim` changes them too, so it cannot be applied to `owner` alone.
    pub fn other_readers<'a>(
        &'a self,
        prim: &'a str,
        owner: &'a str,
    ) -> impl Iterator<Item = &'a str> + 'a {
        let related = |a: &str, b: &str| {
            a.strip_prefix(b).is_some_and(|rest| {
                rest.is_empty() || rest.starts_with('/') || rest.starts_with('.')
            })
        };
        self.material_reads
            .iter()
            .filter(move |(read, reader)| {
                reader != owner && (related(prim, read) || related(read, prim))
            })
            .map(|(_, reader)| reader.as_str())
    }

    /// The light at `path`, when the scene has it.
    pub fn light(&self, path: &str) -> Option<&LightOrigin> {
        self.lights.get(path)
    }

    /// The lights the scene has.
    pub fn lights(&self) -> impl Iterator<Item = (&str, &LightOrigin)> {
        self.lights.iter().map(|(p, o)| (p.as_str(), o))
    }

    /// The prim that emitted the geometry `geom_id`.
    pub fn prim_of_geometry(&self, geom_id: u32) -> Option<&str> {
        let i = self
            .geometry
            .partition_point(|(ids, _)| ids.start <= geom_id);
        i.checked_sub(1).map(|i| self.geometry[i].1.as_str())
    }

    /// The geom_ids the prim at `path` and its descendants emitted, in id
    /// order: a native instance's and a PointInstancer's under the instance
    /// or instancer prim, nothing under a prototype's own path. A scan of the
    /// runs, one per emitting prim; `path` matches by whole path components
    /// (`/World/a` is not an ancestor of `/World/ab`).
    pub fn geometry_of(&self, path: &str) -> Vec<Range<u32>> {
        let path = path.trim_end_matches('/');
        self.geometry
            .iter()
            .filter(|(_, prim)| {
                prim.strip_prefix(path)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
            })
            .map(|(ids, _)| ids.clone())
            .collect()
    }

    /// The `RenderSettings` prim the import read, when one.
    pub fn render_settings(&self) -> Option<&str> {
        self.stage.render_settings.as_deref()
    }

    /// The camera prim rendered through, `None` for the procedural camera.
    pub fn camera(&self) -> Option<&str> {
        self.stage.camera.as_deref()
    }

    /// Rebuilds what `change` names from the stage at `layer`, as the import
    /// that built this scene would build it now, without touching the scene.
    /// `warnings` are the scene's own: a rebuild about a prim they name is
    /// refused. Apply the result with [`Update::apply`].
    pub fn rebuild(
        &self,
        layer: &std::path::Path,
        assets: &dyn crate::AssetLoader,
        warnings: &[crate::Warning],
        change: &Change,
    ) -> Result<Update, Structural> {
        crate::scene::rebuild(self, layer, assets, warnings, change)
    }

    /// Each prim of `paths` on the stage at `layer` as the import that built
    /// this scene composed it (payloads loaded, at its time code), with the
    /// composed value of the `matrix4d` attribute `place_op`. Refused when a
    /// path is not a prim of the stage.
    pub fn prim_frames(
        &self,
        layer: &std::path::Path,
        paths: &[String],
        place_op: &str,
    ) -> Result<Vec<PrimFrame>, Structural> {
        crate::scene::prim_frames(self, layer, paths, place_op)
    }
}

/// A prim's place in the transform hierarchy, as the import composed it:
/// what placement needs to turn a world-space move into an opinion on the
/// prim ([`Provenance::prim_frames`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PrimFrame {
    pub path: String,
    /// The parent's local-to-world, the import's own `f32` matrix: what the
    /// renderer placed the prim's geometry under.
    pub parent_to_world: glam::DMat4,
    /// The prim's own local-to-world, likewise (the parent's when the prim
    /// is not `Xformable`; its local alone after a `!resetXformStack!`).
    pub local_to_world: glam::DMat4,
    /// The composed `xformOpOrder`, payloads loaded; empty when none.
    pub xform_op_order: Vec<String>,
    /// Whether `xform_op_order` lists `!resetXformStack!`.
    pub resets_xform_stack: bool,
    /// Whether the prim's type is `Xformable`: only then are its ops read.
    pub xformable: bool,
    /// The composed default value of the op [`Provenance::prim_frames`] was
    /// asked about, when the prim has one.
    pub place_op: Option<glam::DMat4>,
}

/// What changed on the stage since the scene was imported, as far as an
/// in-place update can follow it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// The `RenderSettings` prim the render uses, or the render camera.
    pub settings: bool,
    /// Material prims whose inputs changed.
    pub materials: Vec<String>,
    /// Prims under one of [`Change::materials`] whose inputs changed but whose
    /// type the host could not read (a payload it leaves unloaded): the
    /// rebuild applies the change only if each is a `Shader` of that
    /// material's network on the material's own, payload-loaded stage.
    pub shaders: Vec<String>,
    /// Light prims whose inputs changed.
    pub lights: Vec<String>,
}

impl Change {
    pub fn is_empty(&self) -> bool {
        !self.settings && self.materials.is_empty() && self.lights.is_empty()
    }
}

/// The settings a settings change re-reads.
pub struct SettingsUpdate {
    /// The stage's render settings, before any host override.
    pub settings: RenderSettings,
    pub camera: Camera,
    /// The products the stage asks for.
    pub aovs: crate::AovRequest,
    /// What the stamp records of the re-read camera and stage
    /// ([`Scene::source`]); the layer stack is the session's, unchanged.
    pub(crate) lens: Option<crate::scene::Lens>,
    pub(crate) motion_blur: bool,
    pub(crate) invisible_overrides: u32,
}

/// One rebuilt light: its new light-list entry and the materials of the
/// geometry it shares its `Emissive` with.
pub(crate) struct LightUpdate {
    pub(crate) index: usize,
    pub(crate) light: LightKind,
    /// Its per-lobe multipliers, which the light list keeps beside it.
    pub(crate) gains: crate::LobeGains,
    pub(crate) emitters: Vec<(u32, Arc<dyn Material>)>,
}

/// Rebuilt parts, ready to replace the scene's ([`Update::apply`]).
#[derive(Default)]
pub struct Update {
    pub(crate) settings: Option<SettingsUpdate>,
    /// Each rebuilt material and the slots it goes into.
    pub(crate) materials: Vec<(Vec<u32>, Arc<dyn Material>)>,
    pub(crate) lights: Vec<LightUpdate>,
}

impl Update {
    /// Whether the update re-read the settings and camera.
    pub fn has_settings(&self) -> bool {
        self.settings.is_some()
    }

    /// Refuses an update the scene's world and lights cannot take without a
    /// re-import (see [`World::check_materials`], [`LightList::check_replace`]).
    pub fn check(&self, world: &World, lights: &LightList) -> Result<(), Structural> {
        world.check_materials(&self.swaps())?;
        for l in &self.lights {
            lights.check_replace(l.index, &l.light)?;
        }
        Ok(())
    }

    fn swaps(&self) -> Vec<(&[u32], &Arc<dyn Material>)> {
        let mut swaps: Vec<(&[u32], &Arc<dyn Material>)> = self
            .materials
            .iter()
            .map(|(slots, m)| (slots.as_slice(), m))
            .collect();
        for l in &self.lights {
            for (id, m) in &l.emitters {
                swaps.push((std::slice::from_ref(id), m));
            }
        }
        swaps
    }

    /// Puts the rebuilt parts into `scene`, after [`Update::check`]: the
    /// materials into their slots, each light into its place (its emitter's
    /// slot with it, so the light and its geometry keep one `Emissive`), the
    /// settings, camera and products over the old. The light selection is
    /// the renderer's to rebuild (`Renderer::new` / `reconfigure`).
    pub fn apply(self, scene: &mut Scene) -> Result<(), Structural> {
        self.check(&scene.world, &scene.lights)?;
        scene.world.replace_materials(&self.swaps());
        for l in self.lights {
            scene.lights.replace(l.index, l.light);
            scene.lights.set_lobe_gains(l.index, l.gains);
        }
        if let Some(s) = self.settings {
            scene.settings = s.settings;
            scene.camera = s.camera;
            scene.aovs = s.aovs;
            scene.source.lens = s.lens;
            scene.source.depth_of_field = scene.camera.has_depth_of_field();
            scene.source.motion_blur = s.motion_blur;
            scene.source.invisible_overrides = Some(s.invisible_overrides);
            scene.stats.image = (&scene.settings).into();
        }
        Ok(())
    }
}
