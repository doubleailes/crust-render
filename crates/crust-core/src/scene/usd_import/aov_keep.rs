//! What the render's identity and primvar AOVs ask the import to keep
//! ([`crate::identity`]), and keeping it: each emitting prim's asset, each
//! `PointInstancer` placement's index, each gprim's primvars, and at the end
//! the [`Identity`] tables. A render that asks for none of them keeps
//! nothing and reads nothing more than it did.

use crate::warning;
use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use glam::Vec3A;
use openusd::sdf;
use openusd::usd::{Attribute, Prim};
use openusd_schemas::geom::Mesh as UsdMesh;

use crate::aov::{AovRequest, NameKey, PrimvarRead};
use crate::identity::{
    CageTopology, Identity, IdentityBuilder, Interpolation, PrimvarData, PrimvarSet,
};
use crate::rt_world::WorldBuilder;

use super::attrs::{custom_i32_array, decode_text, in_working, value_at};
use super::mesh::mesh_arrays;
use super::time::eval_time;

/// What a hit on geometry with no material prim bound names in a material
/// matte (the default material, a light's own emitter). Not a path: a prim
/// name cannot hold `:`.
pub(super) const UNBOUND_MATERIAL: &str = "crust:unbound";

/// See the module documentation.
#[derive(Default)]
pub(super) struct AovKeep {
    /// Per [`NameKey`], whether its table is kept.
    names: [bool; 3],
    /// Whether `instanceId` is read.
    instance: bool,
    /// Whether `elementId` is read.
    element: bool,
    /// The primvars the render reads, in its order.
    primvars: Vec<PrimvarRead>,
    /// `(geom_ids, asset path)` per prim that emitted geometry, when asset
    /// names are kept.
    assets: Vec<(Range<u32>, String)>,
    /// `(geom_ids, index)` per top-level `PointInstancer` placement, when
    /// `instanceId` is read.
    instances: Vec<(Range<u32>, u32)>,
    /// Per `geom_id` placed through a nested native instance, where
    /// ([`MaterialScope`]), when material names are kept.
    scopes: HashMap<u32, MaterialScope>,
}

/// Where a nested native instance placed a prototype inside another, for
/// naming a material in the inner prototype by its instance-proxy path: the
/// nesting prims' paths, each with its own prototype's `/__Prototype_N`
/// stripped (`/Inner`, then `/Inner/Chair` a level deeper), relative to the
/// prim that places the outermost prototype — or absolute, when the
/// outermost nesting prim is in no prototype (inside a `PointInstancer`'s
/// prototype, whose prims have their own paths).
#[derive(Clone, Debug, PartialEq)]
pub(super) enum MaterialScope {
    Relative(Arc<str>),
    Absolute(Arc<str>),
}

impl MaterialScope {
    /// The scope the nested instance prim `prim` opens.
    pub(super) fn of(prim: &Prim) -> MaterialScope {
        let path = prim.path().to_string();
        match path.strip_prefix("/__Prototype_") {
            Some(rest) => MaterialScope::Relative(rest.find('/').map_or("", |i| &rest[i..]).into()),
            None => MaterialScope::Absolute(path.into()),
        }
    }

    /// This scope with `inner` (relative to this one's prototype, as every
    /// scope inside a native prototype is) under it.
    pub(super) fn within(&self, inner: Option<&MaterialScope>) -> MaterialScope {
        let suffix = match inner {
            Some(MaterialScope::Relative(s) | MaterialScope::Absolute(s)) => &**s,
            None => "",
        };
        match self {
            MaterialScope::Relative(s) => MaterialScope::Relative(format!("{s}{suffix}").into()),
            MaterialScope::Absolute(s) => MaterialScope::Absolute(format!("{s}{suffix}").into()),
        }
    }
}

impl AovKeep {
    pub(super) fn new(request: &AovRequest) -> Self {
        let needs = request.identity();
        AovKeep {
            names: needs.names,
            instance: needs.instance,
            element: needs.element,
            primvars: request.primvars(),
            ..AovKeep::default()
        }
    }

    /// Whether meshes keep where each triangle lies in the authored mesh:
    /// `elementId`, and every primvar spread over faces, read through it.
    pub(super) fn elements(&self) -> bool {
        self.element || !self.primvars.is_empty()
    }

    /// Records that the prim at `prim` emitted `geoms`: its asset, when
    /// asset names are kept. Called where light linking records its
    /// receivers, with the same prim — the instance or the instancer for
    /// instanced geometry.
    pub(super) fn saw(&mut self, prim: &Prim, geoms: Range<usize>) {
        if !self.names[NameKey::Asset.index()] || geoms.is_empty() {
            return;
        }
        self.assets
            .push((geoms.start as u32..geoms.end as u32, asset_of(prim)));
    }

    /// Records that `geoms` are the placement of index `index` in a
    /// top-level `PointInstancer`'s arrays.
    pub(super) fn instance(&mut self, geoms: Range<usize>, index: u32) {
        if self.instance && !geoms.is_empty() {
            self.instances
                .push((geoms.start as u32..geoms.end as u32, index));
        }
    }

    /// Gives every geometry in `geoms` the primvars of the gprim `prim`
    /// (a direct mesh, shape, sphere or curves prim), when the render reads
    /// any.
    pub(super) fn assign_primvars(
        &self,
        world: &mut WorldBuilder,
        prim: &Prim,
        mesh: Option<&UsdMesh>,
        geoms: Range<usize>,
        working: crate::color::Space,
    ) {
        if self.primvars.is_empty() || geoms.is_empty() {
            return;
        }
        if let Some(set) = self.primvars_of(prim, mesh, working) {
            for id in geoms {
                world.set_aov_primvars(id as u32, set.clone());
            }
        }
    }

    /// The primvars the render reads, as `prim` authors or inherits them —
    /// `mesh` is its schema when it is a mesh, the one prim whose faces a
    /// primvar can spread over. `None` when it authors none of them.
    pub(super) fn primvars_of(
        &self,
        prim: &Prim,
        mesh: Option<&UsdMesh>,
        working: crate::color::Space,
    ) -> Option<Arc<PrimvarSet>> {
        if self.primvars.is_empty() {
            return None;
        }
        // The mesh's cage, read once and only when a primvar needs it.
        let mut cage: Option<Option<Cage>> = None;
        let mut vars = Vec::with_capacity(self.primvars.len());
        let mut topology = false;
        let mut open = Vec::with_capacity(self.primvars.len());
        for read in &self.primvars {
            let found = match lookup(prim, &read.name) {
                Lookup::Found(attr, value) => Some((attr, value)),
                Lookup::Hidden => None,
                Lookup::Open => {
                    open.push(vars.len());
                    None
                }
            };
            let data = found
                .and_then(|(attr, value)| read_primvar(prim, read, attr, value, working))
                .and_then(|data| {
                    let Some(mesh) = mesh else {
                        if data.interpolation == Interpolation::Constant {
                            return Some(data);
                        }
                        warning!(
                            AovPrimvarUnreadable,
                            at = prim.path(),
                            "{}: primvars:{} is {:?}, which crust reads on meshes only — its AOV \
                         holds the clear value there",
                            prim.path(),
                            read.name,
                            data.interpolation
                        );
                        return None;
                    };
                    if data.interpolation == Interpolation::Constant {
                        return Some(data);
                    }
                    let cage = cage.get_or_insert_with(|| Cage::read(mesh)).as_ref()?;
                    let needed = match data.interpolation {
                        Interpolation::Constant => 1,
                        Interpolation::Uniform => cage.topology.starts.len() - 1,
                        Interpolation::Vertex | Interpolation::Varying => cage.points,
                        Interpolation::FaceVarying => cage.topology.points.len(),
                    };
                    let have = data.indices.as_ref().map_or(data.values.len(), Vec::len);
                    if have < needed {
                        warning!(
                            AovPrimvarUnreadable,
                            at = prim.path(),
                            "{}: primvars:{} has {have} values where its {:?} interpolation needs \
                         {needed} — its AOV holds the clear value there",
                            prim.path(),
                            read.name,
                            data.interpolation
                        );
                        return None;
                    }
                    topology |= data.interpolation != Interpolation::Uniform;
                    Some(data)
                });
            vars.push(data);
        }
        if vars.iter().all(Option::is_none) && open.is_empty() {
            return None;
        }
        let topology = topology
            .then(|| cage.flatten().map(|c| Arc::new(c.topology)))
            .flatten();
        Some(Arc::new(PrimvarSet {
            vars,
            topology,
            open,
        }))
    }

    /// `set` as `placer` places it: each primvar its prototype left open
    /// ([`PrimvarSet::open`]) looked up on `placer` and its ancestors, which
    /// hand down `constant` primvars only, as `FindPrimvarWithInheritance`
    /// does on the instance proxy. `None` when `set` has nothing open, and
    /// every placement shares it.
    pub(super) fn placed_primvars(
        &self,
        set: &PrimvarSet,
        placer: &Prim,
        working: crate::color::Space,
    ) -> Option<Arc<PrimvarSet>> {
        if set.open.is_empty() {
            return None;
        }
        let mut placed = set.clone();
        placed.open.clear();
        for &i in &set.open {
            let read = &self.primvars[i];
            match lookup_inherited(placer, &format!("primvars:{}", read.name)) {
                Lookup::Found(attr, value) => {
                    placed.vars[i] = read_primvar(placer, read, attr, value, working);
                }
                Lookup::Hidden => {}
                Lookup::Open => placed.open.push(i),
            }
        }
        Some(Arc::new(placed))
    }

    /// The identity tables, from the import's emitting prims (`runs`, the
    /// light links' run table: one per prim, in id order), the world's
    /// material slots, and what the traversal recorded. `None` when no
    /// identity var or matte asked.
    /// Records that `geom_id` was placed through `scope` (nothing when
    /// `None`, or when no material names are kept).
    pub(super) fn scope(&mut self, geom_id: u32, scope: Option<&MaterialScope>) {
        if let Some(scope) = scope
            && self.names[NameKey::Material.index()]
        {
            self.scopes.insert(geom_id, scope.clone());
        }
    }

    pub(super) fn build(
        self,
        runs: &[(Range<u32>, String)],
        world: &WorldBuilder,
        material_paths: &HashMap<usize, String>,
    ) -> Option<Identity> {
        if !self.names.contains(&true) && !self.instance {
            return None;
        }
        let mut b = IdentityBuilder::default();
        for (ids, path) in runs {
            if self.names[NameKey::Prim.index()] {
                b.name(NameKey::Prim, ids.clone(), path);
            }
            if self.names[NameKey::Material.index()] {
                for id in ids.clone() {
                    let material = world.material(id);
                    let address = Arc::as_ptr(material) as *const () as usize;
                    let name = match material_paths.get(&address) {
                        Some(m) => placed_path(m, path, self.scopes.get(&id)),
                        None => UNBOUND_MATERIAL.to_owned(),
                    };
                    b.name(NameKey::Material, id..id + 1, &name);
                }
            }
        }
        for (ids, asset) in &self.assets {
            b.name(NameKey::Asset, ids.clone(), asset);
        }
        for (ids, index) in self.instances {
            b.instance(ids, index);
        }
        Some(b.build())
    }
}

/// A material path as the prim placing it names it: a material inside a
/// native prototype (`/__Prototype_N/…`, numbered per stage) is named by its
/// instance-proxy path — the prim that placed the geometry (`<prim>/…`, the
/// instance when it is one), then the nested instances it was placed
/// through (`scope`) — so the name is the same in every frame and every
/// streamed chunk, and two nested prototypes' materials at the same
/// prototype-relative path keep two names.
fn placed_path(material: &str, prim: &str, scope: Option<&MaterialScope>) -> String {
    let Some(rest) = material.strip_prefix("/__Prototype_") else {
        return material.to_owned();
    };
    let rest = rest.find('/').map_or("", |i| &rest[i..]);
    match scope {
        None => format!("{prim}{rest}"),
        Some(MaterialScope::Relative(s)) => format!("{prim}{s}{rest}"),
        Some(MaterialScope::Absolute(s)) => format!("{s}{rest}"),
    }
}

/// The prim's asset: the nearest of it and its ancestors whose `kind` is
/// `component` or `assembly`, else the prim itself.
fn asset_of(prim: &Prim) -> String {
    let mut at = Some(prim.clone());
    while let Some(p) = at {
        if p.path().is_abs_root() {
            break;
        }
        if let Ok(Some(kind)) = p.kind()
            && matches!(kind.as_str(), "component" | "assembly")
        {
            return p.path().to_string();
        }
        at = p.parent();
    }
    prim.path().to_string()
}

/// A mesh's authored faces, for primvars spread over them.
struct Cage {
    topology: CageTopology,
    points: usize,
}

impl Cage {
    fn read(mesh: &UsdMesh) -> Option<Cage> {
        let (points, counts, indices) = mesh_arrays(mesh)?;
        Some(Cage {
            topology: CageTopology::new(&counts, &indices)?,
            points: points.len(),
        })
    }
}

/// The attribute `primvars:<name>` that applies to `prim`, as UsdGeom's
/// primvar inheritance finds it: its own where it authors a value, else the
/// nearest ancestor's that does — when that one is `constant`, since only a
/// constant primvar is inherited, and one of any other interpolation hides
/// those above it. A blocked value (`= None`) is authored too: it hides the
/// ancestors' and gives the prim none.
#[cfg(test)]
fn applying(prim: &Prim, name: &str) -> Option<(Attribute, sdf::Value)> {
    match lookup(prim, name) {
        Lookup::Found(attr, value) => Some((attr, value)),
        Lookup::Hidden | Lookup::Open => None,
    }
}

/// Where the walk for a primvar ended ([`lookup`]).
enum Lookup {
    /// The attribute that applies, and its value.
    Found(Attribute, sdf::Value),
    /// The prim has none: a block, or an ancestor's primvar of another
    /// interpolation, hides everything above, or nothing up to the stage's
    /// root authors one.
    Hidden,
    /// Nothing up to a native prototype's root (`/__Prototype_N`) authors
    /// one: the walk goes on from the prim that places the prototype
    /// ([`AovKeep::placed_primvars`]).
    Open,
}

/// [`applying`]'s walk, telling an open end from a hidden one.
fn lookup(prim: &Prim, name: &str) -> Lookup {
    let attr_name = format!("primvars:{name}");
    let own = prim.attribute(&attr_name);
    if let Some(value) = value_at(&own) {
        return Lookup::Found(own, value);
    }
    if blocked(&own) {
        return Lookup::Hidden;
    }
    match prim.parent() {
        Some(parent) => lookup_inherited(&parent, &attr_name),
        None => Lookup::Hidden,
    }
}

/// The walk over `from` and its ancestors, which hand down `constant`
/// primvars only.
fn lookup_inherited(from: &Prim, attr_name: &str) -> Lookup {
    let mut at = Some(from.clone());
    let mut top = None;
    while let Some(p) = at {
        if p.path().is_abs_root() {
            break;
        }
        let attr = p.attribute(attr_name);
        if let Some(value) = value_at(&attr) {
            return if interpolation(&attr) == Some(Interpolation::Constant) {
                Lookup::Found(attr, value)
            } else {
                Lookup::Hidden
            };
        }
        if blocked(&attr) {
            return Lookup::Hidden;
        }
        at = p.parent();
        top = Some(p);
    }
    if top.is_some_and(|p| p.is_prototype().unwrap_or(false)) {
        Lookup::Open
    } else {
        Lookup::Hidden
    }
}

/// Whether `attr`'s value is blocked at the evaluation time.
fn blocked(attr: &Attribute) -> bool {
    attr.resolve_info_at(eval_time())
        .is_ok_and(|info| info.value_is_blocked())
}

/// A primvar's authored interpolation, `constant` (UsdGeom's fallback) when
/// none is authored; `None` for a token UsdGeom does not define.
fn interpolation(attr: &Attribute) -> Option<Interpolation> {
    match attr
        .metadata::<sdf::Value>("interpolation")
        .ok()
        .flatten()
        .and_then(decode_text)
        .as_deref()
    {
        None | Some("constant") => Some(Interpolation::Constant),
        Some("uniform") => Some(Interpolation::Uniform),
        Some("vertex") => Some(Interpolation::Vertex),
        Some("varying") => Some(Interpolation::Varying),
        Some("faceVarying") => Some(Interpolation::FaceVarying),
        Some(_) => None,
    }
}

/// `read`'s primvar on `prim` (or inherited), decoded: `None` where nothing
/// applies, and — warned — where what applies cannot be written as the
/// var's type or is spread in a way no reader knows.
fn read_primvar(
    prim: &Prim,
    read: &PrimvarRead,
    attr: Attribute,
    value: sdf::Value,
    working: crate::color::Space,
) -> Option<PrimvarData> {
    let Some((mut values, components)) = decode(value) else {
        warning!(
            AovPrimvarTypeMismatch,
            at = prim.path(),
            "{}: primvars:{} is not numeric — its AOV holds the clear value there",
            prim.path(),
            read.name
        );
        return None;
    };
    if components != read.components {
        warning!(
            AovPrimvarTypeMismatch,
            at = prim.path(),
            "{}: primvars:{} has {components} component(s), but its RenderVar writes {} — \
             its AOV holds the clear value there",
            prim.path(),
            read.name,
            read.components
        );
        return None;
    }
    let Some(interpolation) = interpolation(&attr) else {
        warning!(
            AovPrimvarUnreadable,
            at = prim.path(),
            "{}: primvars:{} authors an interpolation UsdGeom does not define — its AOV \
             holds the clear value there",
            prim.path(),
            read.name
        );
        return None;
    };
    if let Ok(Some(sdf::Value::Int(n))) = attr.metadata::<sdf::Value>("elementSize")
        && n > 1
    {
        warning!(
            AovPrimvarUnreadable,
            at = prim.path(),
            "{}: primvars:{} has elementSize {n}, which crust does not read — its AOV holds \
             the clear value there",
            prim.path(),
            read.name
        );
        return None;
    }
    // A colour read as a colour is taken into the working space, as every
    // authored colour is; the alpha of a `color4` is not a colour.
    let colour_typed = attr
        .type_name()
        .ok()
        .flatten()
        .is_some_and(|t| t.as_str().starts_with("color"));
    if read.color && colour_typed && components >= 3 {
        for v in &mut values {
            let rgb = in_working(&attr, Vec3A::new(v[0], v[1], v[2]), working);
            v[..3].copy_from_slice(&[rgb.x, rgb.y, rgb.z]);
        }
    }
    // The indices beside the primvar, on whichever prim it applies from.
    let indices = attr
        .stage()
        .prim(attr.path().prim_path())
        .ok()
        .and_then(|holder| custom_i32_array(&holder, &format!("primvars:{}:indices", read.name)))
        .map(|ix| {
            ix.into_iter()
                .map(|i| u32::try_from(i).unwrap_or(u32::MAX))
                .collect()
        });
    Some(PrimvarData {
        interpolation,
        values,
        indices,
    })
}

/// A primvar's values, each padded to four components, and how many it
/// has: any numeric scalar or vector, in any precision, as an array or one
/// value. `None` for anything else (a string, a matrix).
fn decode(value: sdf::Value) -> Option<(Vec<[f32; 4]>, usize)> {
    use sdf::Value as V;
    fn one<T: Into<f64>>(x: T) -> [f32; 4] {
        [x.into() as f32, 0.0, 0.0, 0.0]
    }
    macro_rules! vec_n {
        ($v:expr, $n:literal) => {{
            let mut out = [0.0f32; 4];
            for (i, o) in out.iter_mut().enumerate().take($n) {
                *o = f64::from($v[i]) as f32;
            }
            out
        }};
    }
    Some(match value {
        V::Bool(b) => (vec![one(u8::from(b))], 1),
        V::BoolVec(v) => (v.into_iter().map(|b| one(u8::from(b))).collect(), 1),
        V::Uchar(x) => (vec![one(x)], 1),
        V::UcharVec(v) => (v.into_iter().map(one).collect(), 1),
        V::Int(x) => (vec![one(x)], 1),
        V::IntVec(v) => (v.into_iter().map(one).collect(), 1),
        V::Int64(x) => (vec![[x as f32, 0.0, 0.0, 0.0]], 1),
        V::Int64Vec(v) => (
            v.into_iter().map(|x| [x as f32, 0.0, 0.0, 0.0]).collect(),
            1,
        ),
        V::Half(x) => (vec![one(x.to_f32())], 1),
        V::HalfVec(v) => (v.into_iter().map(|x| one(x.to_f32())).collect(), 1),
        V::Float(x) => (vec![one(x)], 1),
        V::FloatVec(v) => (v.into_iter().map(one).collect(), 1),
        V::Double(x) => (vec![one(x)], 1),
        V::DoubleVec(v) => (v.into_iter().map(one).collect(), 1),
        V::Vec2h(p) => (vec![vec_n!([p.x.to_f32(), p.y.to_f32()], 2)], 2),
        V::Vec2f(p) => (vec![vec_n!([p.x, p.y], 2)], 2),
        V::Vec2d(p) => (vec![vec_n!([p.x, p.y], 2)], 2),
        V::Vec2i(p) => (vec![vec_n!([p.x, p.y], 2)], 2),
        V::Vec2hVec(v) => (
            v.iter()
                .map(|p| vec_n!([p.x.to_f32(), p.y.to_f32()], 2))
                .collect(),
            2,
        ),
        V::Vec2fVec(v) => (v.iter().map(|p| vec_n!([p.x, p.y], 2)).collect(), 2),
        V::Vec2dVec(v) => (v.iter().map(|p| vec_n!([p.x, p.y], 2)).collect(), 2),
        V::Vec2iVec(v) => (v.iter().map(|p| vec_n!([p.x, p.y], 2)).collect(), 2),
        V::Vec3h(p) => (
            vec![vec_n!([p.x.to_f32(), p.y.to_f32(), p.z.to_f32()], 3)],
            3,
        ),
        V::Vec3f(p) => (vec![vec_n!([p.x, p.y, p.z], 3)], 3),
        V::Vec3d(p) => (vec![vec_n!([p.x, p.y, p.z], 3)], 3),
        V::Vec3i(p) => (vec![vec_n!([p.x, p.y, p.z], 3)], 3),
        V::Vec3hVec(v) => (
            v.iter()
                .map(|p| vec_n!([p.x.to_f32(), p.y.to_f32(), p.z.to_f32()], 3))
                .collect(),
            3,
        ),
        V::Vec3fVec(v) => (v.iter().map(|p| vec_n!([p.x, p.y, p.z], 3)).collect(), 3),
        V::Vec3dVec(v) => (v.iter().map(|p| vec_n!([p.x, p.y, p.z], 3)).collect(), 3),
        V::Vec3iVec(v) => (v.iter().map(|p| vec_n!([p.x, p.y, p.z], 3)).collect(), 3),
        V::Vec4h(p) => (
            vec![vec_n!(
                [p.x.to_f32(), p.y.to_f32(), p.z.to_f32(), p.w.to_f32()],
                4
            )],
            4,
        ),
        V::Vec4f(p) => (vec![vec_n!([p.x, p.y, p.z, p.w], 4)], 4),
        V::Vec4d(p) => (vec![vec_n!([p.x, p.y, p.z, p.w], 4)], 4),
        V::Vec4i(p) => (vec![vec_n!([p.x, p.y, p.z, p.w], 4)], 4),
        V::Vec4hVec(v) => (
            v.iter()
                .map(|p| vec_n!([p.x.to_f32(), p.y.to_f32(), p.z.to_f32(), p.w.to_f32()], 4))
                .collect(),
            4,
        ),
        V::Vec4fVec(v) => (
            v.iter().map(|p| vec_n!([p.x, p.y, p.z, p.w], 4)).collect(),
            4,
        ),
        V::Vec4dVec(v) => (
            v.iter().map(|p| vec_n!([p.x, p.y, p.z, p.w], 4)).collect(),
            4,
        ),
        V::Vec4iVec(v) => (
            v.iter().map(|p| vec_n!([p.x, p.y, p.z, p.w], 4)).collect(),
            4,
        ),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A constant primvar inherits past a child that does not author it, but
    /// not past one that blocks it: the block is the child's own opinion.
    #[test]
    fn a_blocked_primvar_hides_the_ancestors_constant() {
        let dir = std::env::temp_dir().join(format!("crust_aov_keep_{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("blocked.usda");
        std::fs::write(
            &path,
            r#"#usda 1.0
def Xform "W"
{
    color3f[] primvars:displayColor = [(0.2, 0.4, 0.6)]
    def Xform "Plain" {}
    def Xform "Blocked"
    {
        color3f[] primvars:displayColor = None
        def Xform "Below" {}
    }
}
"#,
        )
        .expect("write stage");
        let stage = super::super::stage_builder()
            .open(path.to_str().unwrap())
            .expect("stage opens");
        let at = |p: &str| super::super::prim_at(&stage, sdf::path(p).unwrap());
        let (_, value) = applying(&at("/W/Plain"), "displayColor").expect("inherited");
        assert!(matches!(value, sdf::Value::Vec3fVec(_)), "{value:?}");
        assert!(applying(&at("/W/Blocked"), "displayColor").is_none());
        assert!(applying(&at("/W/Blocked/Below"), "displayColor").is_none());
    }

    #[test]
    fn a_prototype_scoped_material_is_named_after_the_placing_prim() {
        assert_eq!(
            placed_path("/__Prototype_3/Looks/Bark", "/World/tree_12", None),
            "/World/tree_12/Looks/Bark"
        );
        assert_eq!(placed_path("/__Prototype_3", "/World/t", None), "/World/t");
        assert_eq!(
            placed_path("/World/Looks/Red", "/World/t", None),
            "/World/Looks/Red"
        );
        // Two nested prototypes' materials at the same prototype-relative
        // path keep two names: the nested instance's path is in each.
        let chair = MaterialScope::Relative("/Chair".into());
        assert_eq!(
            placed_path("/__Prototype_2/Looks/M", "/World/house", Some(&chair)),
            "/World/house/Chair/Looks/M"
        );
        assert_eq!(
            placed_path("/__Prototype_1/Looks/M", "/World/house", None),
            "/World/house/Looks/M"
        );
        let deeper = chair.within(Some(&MaterialScope::Relative("/Leg".into())));
        assert_eq!(
            placed_path("/__Prototype_7/Looks/M", "/World/house", Some(&deeper)),
            "/World/house/Chair/Leg/Looks/M"
        );
        let under_instancer = MaterialScope::Absolute("/World/Pebbles/Protos/Rock/Inner".into());
        assert_eq!(
            placed_path(
                "/__Prototype_4/Looks/M",
                "/World/Pebbles",
                Some(&under_instancer)
            ),
            "/World/Pebbles/Protos/Rock/Inner/Looks/M"
        );
    }

    #[test]
    fn numeric_values_decode_with_their_component_count() {
        let (v, n) = decode(sdf::Value::Vec3fVec(vec![openusd::gf::Vec3f::from([
            0.25, 0.5, 1.0,
        ])]))
        .expect("numeric");
        assert_eq!((v, n), (vec![[0.25, 0.5, 1.0, 0.0]], 3));
        assert_eq!(
            decode(sdf::Value::Int(7)),
            Some((vec![[7.0, 0.0, 0.0, 0.0]], 1))
        );
        assert_eq!(decode(sdf::Value::String("x".into())), None);
    }
}
