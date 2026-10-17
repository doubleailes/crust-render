//! Placement tools (`session-placement-tools`): `drop_to_surface` and
//! `scatter_on_surface`. Both bring the scene up to date with the sources
//! on disk, compute against the geometry it renders (`crust_core::place`),
//! and author the result as one edit batch through [`Open::edit`].

use super::edit::{
    PLACE_OP, ProtoSource, Prototype, Snippet, instancer_body, overs_tree, placement_body, reanchor,
};
use super::query::prim_at;
use super::session::Open;
use crust_core::live::Provenance;
use crust_core::place::{self, Amount, Dropped, ScatterRequest, UpAxis};
use glam::{DMat4, DVec3};
use openusd::sdf;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// What `scatter_on_surface` is asked for, as the tool takes it.
pub struct ScatterArgs {
    pub target: Vec<String>,
    pub prototypes: Vec<String>,
    pub path: String,
    pub seed: u32,
    pub count: Option<u64>,
    pub density: Option<f64>,
    pub max_slope: Option<f64>,
    pub random_rotation: bool,
    pub scale: Option<[f64; 2]>,
}

/// The files a prototype asset may be: USD layers and packages.
const USD_EXTENSIONS: &[&str] = &["usd", "usda", "usdc", "usdz"];

fn vec3(v: DVec3) -> [f64; 3] {
    v.to_array()
}

/// `paths` as prims of the session's stage, refused at the first that is
/// not one, and when one is listed twice.
fn prims_of(open: &Open, paths: &[String], what: &str) -> Result<Vec<sdf::Path>, String> {
    if paths.is_empty() {
        return Err(format!("{what}: no prim given"));
    }
    let mut out: Vec<sdf::Path> = Vec::with_capacity(paths.len());
    for p in paths {
        let path = sdf::Path::new(p).map_err(|e| format!("{p:?} is not a USD path: {e}"))?;
        if !path.is_prim() {
            return Err(format!("{p} is not a prim path"));
        }
        prim_at(&open.stage, &path)?;
        if out.contains(&path) {
            return Err(format!("{what}: {p} is listed twice"));
        }
        out.push(path);
    }
    Ok(out)
}

impl Open {
    /// The stage's up axis: the override layer's `upAxis`, else the input's
    /// root layer's (stage metadata is the root layer's, and the override
    /// layer authors none of its own), else USD's fallback, Y.
    fn up_axis(&self) -> Result<UpAxis, String> {
        let read = |path: &Path| -> Option<String> {
            let layer = sdf::Layer::open(path.to_str()?).ok()?;
            match layer
                .data()
                .try_field(&sdf::Path::abs_root(), "upAxis")
                .ok()??
                .as_ref()
            {
                sdf::Value::Token(t) => Some(t.as_str().to_owned()),
                sdf::Value::String(s) => Some(s.clone()),
                _ => None,
            }
        };
        let axis = read(&self.output).or_else(|| read(&self.input));
        match axis.as_deref() {
            None | Some("Y") => Ok(UpAxis::Y),
            Some("Z") => Ok(UpAxis::Z),
            Some(other) => Err(format!(
                "the stage's upAxis is {other:?}; placement knows \"Y\" and \"Z\""
            )),
        }
    }

    fn provenance(&self) -> Result<&Provenance, String> {
        self.scene
            .provenance()
            .ok_or_else(|| "the session's scene has no provenance to place against".to_owned())
    }

    /// `drop_to_surface`: each of `prims` dropped along the stage's down
    /// axis onto the surface beneath it (`target`'s only, when given), and
    /// turned to that surface's normal with `align`; the moves authored as
    /// one edit batch. Refused, authoring nothing, when a path is not a prim
    /// of the stage, lies inside an instance, or names a prim that cannot
    /// hold a transform.
    pub fn drop_to_surface(
        &mut self,
        prims: &[String],
        target: Option<&[String]>,
        align: bool,
    ) -> Result<serde_json::Value, String> {
        self.refresh()?;
        let paths = prims_of(self, prims, "prims")?;
        for p in &paths {
            self.editable_prim(&p.to_string())?;
        }
        // Each move is computed against the scene before the call: a prim
        // dropped with its ancestor would take the ancestor's move on top of
        // its own.
        for a in &paths {
            if let Some(b) = paths.iter().find(|b| *b != a && b.has_prefix(a)) {
                return Err(format!(
                    "{b} is under {a}, which this call drops too: drop them in separate calls"
                ));
            }
        }
        if let Some(target) = target {
            prims_of(self, target, "target")?;
        }
        let sources_changed = self.bring_current()?;
        let up = self.up_axis()?;
        let started = Instant::now();
        let frames = self
            .provenance()?
            .prim_frames(&self.output, prims, PLACE_OP)
            .map_err(|e| e.to_string())?;
        if let Some(f) = frames.iter().find(|f| !f.xformable) {
            return Err(format!(
                "{} cannot hold a transform (its type is not Xformable); drop its parent, or \
                 the prims under it",
                f.path
            ));
        }
        let dropped = place::drop_prims(
            &self.scene.renderer.world,
            self.provenance()?,
            prims,
            target,
            up,
            align,
        );
        let place_s = started.elapsed().as_secs_f64();
        let mut entries = Vec::new();
        let mut report = Vec::new();
        for ((path, frame), d) in paths.iter().zip(&frames).zip(&dropped) {
            let status = match d {
                Dropped::Placed {
                    delta,
                    contact,
                    normal,
                } => {
                    entries.push((path.clone(), placement_body(frame, delta)));
                    let origin = frame.local_to_world.w_axis.truncate();
                    serde_json::json!({
                        "prim": path.to_string(),
                        "status": "placed",
                        "origin_moved_by": vec3(delta.transform_point3(origin) - origin),
                        "contact": vec3(*contact),
                        "normal": normal.map(vec3),
                    })
                }
                Dropped::NoGeometry => serde_json::json!({
                    "prim": path.to_string(),
                    "status": "no geometry",
                }),
                Dropped::NothingBelow => serde_json::json!({
                    "prim": path.to_string(),
                    "status": "nothing below",
                }),
            };
            report.push(status);
        }
        let mut result = if entries.is_empty() {
            serde_json::json!({ "applied": null })
        } else {
            self.edit(&Snippet::parse(&overs_tree(&entries)?)?)?
        };
        result["prims"] = report.into();
        result["place_s"] = place_s.into();
        result["sources_changed"] = serde_json::json!(sources_changed);
        Ok(result)
    }

    /// `scatter_on_surface`: a new `PointInstancer` at `args.path` whose
    /// instances lie on `args.target`'s triangles, by area, from the seed.
    pub fn scatter_on_surface(&mut self, args: &ScatterArgs) -> Result<serde_json::Value, String> {
        self.refresh()?;
        let amount = match (args.count, args.density) {
            (Some(n), None) => Amount::Count(n),
            (None, Some(d)) if d > 0.0 && d.is_finite() => Amount::Density(d),
            (None, Some(d)) => return Err(format!("density {d} is not a positive number")),
            _ => return Err("give either count or density, not both".to_owned()),
        };
        if let Some([lo, hi]) = args.scale
            && !(0.0 < lo && lo <= hi && hi.is_finite())
        {
            return Err(format!("scale [{lo}, {hi}] is not 0 < min <= max"));
        }
        if let Some(s) = args.max_slope
            && !(s > 0.0 && s <= 180.0)
        {
            return Err(format!("max_slope {s} is not in (0, 180] degrees"));
        }
        prims_of(self, &args.target, "target")?;
        let path = sdf::Path::new(&args.path)
            .ok()
            .filter(|p| p.is_prim() && p.is_abs())
            .ok_or_else(|| format!("{:?} is not an absolute prim path", args.path))?;
        if prim_at(&self.stage, &path).is_ok() {
            return Err(format!(
                "{path} is a prim of the stage already: scatter into a new path (undo a \
                 previous scatter to redo it)"
            ));
        }
        let parent = path.parent().unwrap_or_else(sdf::Path::abs_root);
        if parent != sdf::Path::abs_root() {
            prim_at(&self.stage, &parent)
                .map_err(|_| format!("{path}'s parent {parent} is not a prim of the stage"))?;
            // USD reads nothing authored under an instance or inside one.
            self.editable_prim(&parent.to_string())?;
            if prim_at(&self.stage, &parent)?
                .is_instance()
                .unwrap_or(false)
            {
                return Err(format!(
                    "{parent} is an instance: USD ignores prims authored under it; scatter \
                     under a prim that is not instanceable"
                ));
            }
        }
        let prototypes = self.prototypes(&args.prototypes)?;
        let sources_changed = self.bring_current()?;
        let up = self.up_axis()?;
        let started = Instant::now();
        let parent_to_world = if parent == sdf::Path::abs_root() {
            DMat4::IDENTITY
        } else {
            self.provenance()?
                .prim_frames(&self.output, &[parent.to_string()], PLACE_OP)
                .map_err(|e| e.to_string())?[0]
                .local_to_world
        };
        let request = ScatterRequest {
            target: args.target.clone(),
            amount,
            seed: args.seed,
            max_slope_deg: args.max_slope,
            random_rotation: args.random_rotation,
            scale: args.scale,
            prototypes: prototypes.len() as u32,
            up,
        };
        let scattered = place::scatter(&self.scene.renderer.world, self.provenance()?, &request)?;
        let name = path.name().unwrap_or_default().to_owned();
        let body = instancer_body(
            &name,
            &path,
            &parent_to_world,
            up.vector().as_dvec3(),
            &scattered.instances,
            &prototypes,
        )?;
        let place_s = started.elapsed().as_secs_f64();
        let mut result = self.edit(&Snippet::parse(&overs_tree(&[(parent, body)])?)?)?;
        result["path"] = path.to_string().into();
        result["count"] = scattered.instances.len().into();
        result["area"] = scattered.area.into();
        result["prototypes"] = prototypes
            .iter()
            .map(|p| format!("{path}/Prototypes/{}", p.name))
            .collect::<Vec<_>>()
            .into();
        result["skipped"] = scattered.skipped.into();
        result["place_s"] = place_s.into();
        result["sources_changed"] = serde_json::json!(sources_changed);
        Ok(result)
    }

    /// Each prototype as given: a prim of the stage (an absolute path), or an
    /// asset file — relative to the input stage's directory, as `query`
    /// shows the input's own paths — written for the layer. Names come from
    /// the prim's name or the file's stem, made identifiers and unique.
    fn prototypes(&self, given: &[String]) -> Result<Vec<Prototype>, String> {
        if given.is_empty() {
            return Err("prototypes: none given".to_owned());
        }
        let (input_dir, layer_dir) = (self.input_dir(), self.layer_dir());
        let mut out: Vec<Prototype> = Vec::new();
        for g in given {
            // An absolute path that names a file is an asset, not a prim.
            let is_prim = g.starts_with('/') && !Path::new(g).is_file();
            let (stem, source) = if is_prim {
                let prim = sdf::Path::new(g)
                    .ok()
                    .filter(sdf::Path::is_prim)
                    .ok_or_else(|| format!("prototype {g:?} is not a prim path"))?;
                prim_at(&self.stage, &prim)
                    .map_err(|_| format!("prototype {g} is not a prim of the stage"))?;
                (
                    prim.name().unwrap_or("proto").to_owned(),
                    ProtoSource::Prim(prim),
                )
            } else {
                let file: PathBuf = if Path::new(g).is_absolute() {
                    g.into()
                } else {
                    input_dir.join(g)
                };
                if !file.is_file() {
                    return Err(format!(
                        "prototype {g} is neither a prim path nor a file ({} does not exist)",
                        file.display()
                    ));
                }
                // Scenes come from USD alone: a reference to any other file
                // would not compose.
                let usd = file
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| USD_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()));
                if !usd {
                    return Err(format!(
                        "prototype {g} is not a USD file: a prototype is a prim of the stage or \
                         a .usd, .usda, .usdc or .usdz asset"
                    ));
                }
                let stem = file
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "proto".to_owned());
                (
                    stem,
                    ProtoSource::Asset(reanchor(g, &input_dir, &layer_dir)),
                )
            };
            let mut name: String = stem
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
                name.insert(0, '_');
            }
            let base = name.clone();
            let mut k = 1;
            while out.iter().any(|p| p.name == name) {
                name = format!("{base}_{k}");
                k += 1;
            }
            out.push(Prototype { name, source });
        }
        Ok(out)
    }
}
