//! The session's scene: the saved override layer imported exactly as `crust
//! render <output>` imports it (design D1), kept as a renderer that every
//! session render retunes, beside the `crust-check/2` report of that
//! same import — or that import updated in place, which gives the same scene
//! (`live-stage-scene-deltas`, [`Imported::update`]).

use super::delta::Applied;
use crate::{CheckArgs, Checked, Cli, Command};
use clap::Parser;
use crust_assets::FileAssets;
use crust_core::check::CheckReport;
use crust_core::live::{Change, Provenance, Structural};
use crust_core::{AovRequest, LightList, RenderSettings, Renderer, Scene, WorldBuilder};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tracing::debug;

/// What the scene was imported with that the renderer does not hold: what
/// an in-place update puts back together with the renderer's parts.
struct Rest {
    /// The scene's own settings, before the scene flags.
    settings: RenderSettings,
    /// The products the stage asks for, before any is refused.
    aovs: AovRequest,
    /// The import's warnings, before the product selection's.
    warnings: Vec<crust_core::Warning>,
    stats: crust_core::RenderStats,
    camera_path: Option<String>,
    time: Option<f64>,
    provenance: Option<Provenance>,
    /// How many UV textures the import preloaded.
    preloaded: u64,
}

/// One import of the override layer.
pub struct Imported {
    /// The scene, ready to render. Shared with a running render, so it is
    /// retuned only once that render has been joined.
    pub renderer: Arc<Renderer>,
    /// The scene's own render settings, which a session render overrides
    /// with its region and samples only.
    pub settings: RenderSettings,
    /// The products a render of the layer writes, as `crust render` selects
    /// them: a session render requests the same AOVs, so its beauty is the
    /// CLI's.
    pub aovs: Arc<AovRequest>,
    /// What `crust check -i <output> --json -` reports on this import.
    pub report: CheckReport,
    /// The loader the scene was imported with: held, never read, because it
    /// owns the texture caches the scene's textures stream through.
    pub _assets: Arc<FileAssets>,
    /// The scene-linear space the scene renders in, which its images are
    /// encoded from.
    pub working_space: crust_core::color::Space,
    /// How long the import (or the in-place update) took.
    pub took: Duration,
    /// The file of every USD layer the import composed
    /// ([`crust_core::Scene::layers`]), kept by an in-place update, which
    /// adds or removes no composition arc.
    pub layers: Vec<PathBuf>,
    /// How the layer's last change was applied to give this scene.
    pub applied: Applied,
    /// Which import built the scene's geometry: new with every full import,
    /// kept by an in-place update, which moves no geometry. A render's hits
    /// are named against the scene only while it is the one it rendered.
    pub geometry: u64,
    /// `World` slot (a hit's `geom_id`) → the material prim filling it: the
    /// provenance's per-material slot lists turned around once per scene,
    /// so a `probe` looks a hit's material up instead of scanning them.
    material_of_slot: HashMap<u32, String>,
    rest: Rest,
}

/// The source of [`Imported::geometry`].
static GEOMETRY: AtomicU64 = AtomicU64::new(0);

/// What a camera ray hit, named by where the scene's parts came from.
pub struct HitNames<'a> {
    pub geom_id: u32,
    /// The prim that emitted the hit geometry: for a hit inside a native
    /// instance or a `PointInstancer`, the instance or instancer prim, which
    /// is what the import's geometry runs record.
    pub prim: Option<&'a str>,
    /// The material prim bound to it, `None` for geometry no material prim
    /// fills (an area light's emitter, the fallback material).
    pub material: Option<&'a str>,
}

impl Imported {
    /// The warning codes the import raised, in report order.
    pub fn warning_codes(&self) -> Vec<&'static str> {
        self.report
            .warnings
            .iter()
            .map(|w| w.code.as_str())
            .collect()
    }

    /// Where the scene's parts came from, `None` for a scene the import did
    /// not build from a stage.
    pub fn provenance(&self) -> Option<&Provenance> {
        self.rest.provenance.as_ref()
    }

    /// The `geom_id` of the first surface the camera ray through the centre
    /// of image pixel (`x`, `y`) meets — through the lens centre, at shutter
    /// open, before any opacity — for a render of `width` × `height` pixels
    /// through `camera`. `None` when it meets nothing.
    pub fn hit(
        &self,
        camera: &crust_core::Camera,
        (width, height): (usize, usize),
        (x, y): (usize, usize),
    ) -> Option<u32> {
        // Image rows go down, the camera's `t` up (as the tracer's raster).
        let s = (x as f32 + 0.5) / width as f32;
        let t = ((height - 1 - y) as f32 + 0.5) / height as f32;
        let ray = camera.get_ray(s, t, [0.5, 0.5], 0.0);
        self.renderer
            .world
            .intersect(&ray, crust_core::TRACE_T_MIN, f32::INFINITY)
            .map(|h| h.geom_id)
    }

    /// The prim and the material of the geometry `geom_id`.
    pub fn names(&self, geom_id: u32) -> HitNames<'_> {
        HitNames {
            geom_id,
            prim: self.provenance().and_then(|p| p.prim_of_geometry(geom_id)),
            material: self.material_of_slot.get(&geom_id).map(String::as_str),
        }
    }

    /// `change`, made to the saved layer at `layer` since this scene was
    /// imported, applied in place (spec: "Edits apply in place when they are
    /// confined"): the parts it names rebuilt by the import's own code and
    /// put into the scene, the scene re-checked, and the renderer rebuilt
    /// around the same world, so its light selection is a fresh import's.
    /// Refused, with the scene untouched, when the change is not confined
    /// after all; the caller then imports the layer.
    pub fn update(
        &mut self,
        layer: &Path,
        change: &Change,
        applied: Applied,
    ) -> Result<(), Structural> {
        let started = Instant::now();
        let provenance = self
            .rest
            .provenance
            .as_ref()
            .ok_or_else(|| Structural("the scene has no provenance".into()))?;
        let update = provenance.rebuild(layer, &*self._assets, &self.rest.warnings, change)?;
        update.check(&self.renderer.world, &self.renderer.lights)?;
        let args = check_args(layer).map_err(Structural)?;
        // Past the checks nothing refuses: take the renderer apart into the
        // scene it was built from.
        let renderer = Arc::get_mut(&mut self.renderer)
            .ok_or_else(|| Structural("the scene is still held by a render".into()))?;
        let world = std::mem::replace(&mut renderer.world, WorldBuilder::new().commit());
        let lights = std::mem::replace(&mut renderer.lights, LightList::new());
        let volumes = std::mem::take(&mut renderer.volumes).into_regions();
        let mut scene =
            Scene::new(renderer.camera, world, lights, self.rest.settings).with_volumes(volumes);
        // The report's phases are the import's, then this update's; the
        // next update starts from the import's again.
        let import_stats = std::mem::take(&mut self.rest.stats);
        scene.stats = import_stats.clone();
        scene
            .stats
            .record(format!("Update in place ({applied})"), 0, started.elapsed());
        scene.aovs = std::mem::take(&mut self.rest.aovs);
        scene.working_space = self.working_space;
        scene.camera_path = self.rest.camera_path.take();
        scene.time = self.rest.time;
        scene.warnings = std::mem::take(&mut self.rest.warnings);
        scene.provenance = self.rest.provenance.take();
        scene.layers = std::mem::take(&mut self.layers);
        update
            .apply(&mut scene)
            .expect("the update was checked against this world and these lights");
        let checked = crate::check_scene(
            &args,
            scene,
            self.rest.preloaded,
            crust_core::peak_memory_bytes(),
        )
        .expect("a session's check arguments set no region to refuse");
        let took = started.elapsed();
        debug!(
            "Session update ({applied}) of {} in {took:.2?}",
            layer.display()
        );
        *self = Imported::of(checked, self._assets.clone(), took, applied, self.geometry);
        self.rest.stats = import_stats;
        Ok(())
    }

    /// The session's scene from a checked import.
    fn of(
        checked: Checked,
        assets: Arc<FileAssets>,
        took: Duration,
        applied: Applied,
        geometry: u64,
    ) -> Imported {
        let Checked {
            scene,
            settings,
            aovs,
            report,
            preloaded,
        } = checked;
        let renderer = Renderer::new(scene.camera, scene.world, scene.lights, settings)
            .with_volumes(scene.volumes);
        let material_of_slot = scene
            .provenance
            .iter()
            .flat_map(|p| p.materials())
            .flat_map(|(path, origin)| origin.slots.iter().map(move |&s| (s, path.to_owned())))
            .collect();
        Imported {
            renderer: Arc::new(renderer),
            settings,
            aovs: Arc::new(aovs),
            report,
            _assets: assets,
            working_space: scene.working_space,
            took,
            layers: scene.layers,
            applied,
            geometry,
            material_of_slot,
            rest: Rest {
                settings: scene.settings,
                aovs: scene.aovs,
                warnings: scene.warnings,
                stats: scene.stats,
                camera_path: scene.camera_path,
                time: scene.time,
                provenance: scene.provenance,
                preloaded,
            },
        }
    }
}

/// The arguments `crust <args>` would parse to for `check`: the session
/// imports with the CLI's own defaults, by construction.
fn check_args(layer: &Path) -> Result<CheckArgs, String> {
    let layer = layer.to_str().ok_or("the layer path is not UTF-8")?;
    match Cli::try_parse_from(["crust", "check", "-i", layer]) {
        Ok(Cli {
            command: Command::Check(args),
            ..
        }) => Ok(*args),
        Ok(_) => unreachable!("parsed as check"),
        Err(e) => Err(e.to_string()),
    }
}

/// Imports the saved layer at `layer` through `assets`.
pub fn import(layer: &Path, assets: Arc<FileAssets>) -> Result<Imported, String> {
    let args = check_args(layer)?;
    let started = Instant::now();
    // The session imports once per edit batch and lives on: each stage is
    // freed, and with it any mapping of the layer it read.
    let checked = crate::import_checked(&args, false, &assets)?;
    let took = started.elapsed();
    // One per edit batch: a count that grows with the session.
    debug!("Session import of {} in {took:.2?}", layer.display());
    let geometry = GEOMETRY.fetch_add(1, Ordering::Relaxed) + 1;
    Ok(Imported::of(checked, assets, took, Applied::Full, geometry))
}
