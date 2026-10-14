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
use crate::light::{Light, LightKind, LightList};
use crate::material::Material;
use crate::rt_world::World;
use crate::scene::Scene;
use crate::tracer::RenderSettings;

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
    /// `(first geom_id, prim path)` per prim that emitted geometry, in
    /// traversal order: the prim owns every id up to the next run's.
    pub(crate) geometry: Vec<(u32, String)>,
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
    pub(crate) subdiv_level: u32,
    /// What adaptive subdivision read from the render camera (its projection,
    /// target and ceiling), as bits; `None` for uniform subdivision.
    pub(crate) adaptive: Option<ScreenKey>,
    pub(crate) domes_seen_by_camera: bool,
    pub(crate) working: Option<crate::color::Space>,
    /// The camera rendered through, `None` for the procedural one.
    pub(crate) camera: Option<String>,
    /// The camera projection an implicit shape took its radial count from,
    /// as bits; `None` when no count came from it.
    pub(crate) on_screen_shapes: Option<ScreenKey>,
    /// Whether an unshared curved implicit shape took the fixed radial count
    /// because no camera was named: naming one then moves geometry.
    pub(crate) unsized_shapes: bool,
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
            .partition_point(|(first, _)| *first <= geom_id);
        i.checked_sub(1).map(|i| self.geometry[i].1.as_str())
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

/// Why a change cannot be applied in place: the host re-imports instead.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Structural(pub String);

impl std::fmt::Display for Structural {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Structural {}

/// The settings a settings change re-reads.
pub struct SettingsUpdate {
    /// The stage's render settings, before any host override.
    pub settings: RenderSettings,
    pub camera: Camera,
    /// The products the stage asks for.
    pub aovs: crate::AovRequest,
}

/// One rebuilt light: its new light-list entry and the materials of the
/// geometry it shares its `Emissive` with.
pub(crate) struct LightUpdate {
    pub(crate) index: usize,
    pub(crate) light: LightKind,
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
        }
        if let Some(s) = self.settings {
            scene.settings = s.settings;
            scene.camera = s.camera;
            scene.aovs = s.aovs;
            scene.stats.image = (&scene.settings).into();
        }
        Ok(())
    }
}

/// The properties of a material the world and the geometry were built on:
/// two materials with the same signature can swap slots without a re-import.
#[derive(Debug, PartialEq)]
pub(crate) struct Signature {
    kind: &'static str,
    cutout: bool,
    straight: bool,
    boundary: bool,
    emits: bool,
    uv: bool,
    uv_primvar: Option<String>,
    face_texture: Option<usize>,
}

impl Signature {
    pub(crate) fn of(m: &dyn Material) -> Signature {
        Signature {
            kind: m.kind(),
            cutout: m.has_cutout(),
            straight: m.has_straight_transmission(),
            boundary: m.is_medium_boundary(),
            emits: m.may_emit(),
            uv: m.uses_uv(),
            uv_primvar: m.uv_primvar().map(str::to_owned),
            face_texture: m.face_texture().map(|t| t.num_faces()),
        }
    }
}

/// A light's identity in the list: what a replacement must keep.
pub(crate) fn same_light_shape(old: &LightKind, new: &LightKind) -> bool {
    std::mem::discriminant(old) == std::mem::discriminant(new)
        && old.at_infinity() == new.at_infinity()
        && old.geom_id() == new.geom_id()
}
