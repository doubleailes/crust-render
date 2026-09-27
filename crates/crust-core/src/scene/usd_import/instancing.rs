//! Instancing: `UsdGeomPointInstancer` and native `instanceable` prims.
//!
//! Both instancing mechanisms — `UsdGeomPointInstancer` and native
//! `instanceable` prims — reduce to the same thing: build a prototype's
//! geometry *once*, then place it many times by transform. The shared
//! currency is a [`ProtoPart`]: one leaf geometry of the prototype, held as
//! a committed kernel scene in its own local space.
//!
//! `World` maps materials per top-level geometry, so a hit has to say which
//! part it landed on. A part therefore carries one *slot* per geometry it
//! can report — its material and texture tables — and a part of several
//! slots is a *group*: one committed scene whose hits are labelled `0..n`
//! (`crust_rt::InstanceHitId`), placed as one instance and given `n`
//! consecutive `geom_id`s. That keeps a many-part prototype one box in the
//! BVH above it. It used to be one instance per part, which is harmless for
//! a handful of parts and ruinous for a scatter of trees of 16 181 branch
//! meshes each: every part then spans the whole scatter, and the Moana
//! island's isDunesB put 64 724 identical boxes over its dune field
//! (`docs/moana_profile.md`).

use std::sync::Arc;
use std::time::Instant;

use crust_rt::{Geometry, InstanceHitId, Scene as RtScene, SceneBuilder as RtSceneBuilder};
use glam::{Affine3A, Mat4 as GMat4, Vec3, Vec3A};
use openusd::gf::Vec3f;
use openusd::sdf;
use openusd::usd::{Prim, Stage};
use openusd_schemas::geom::{
    BasisCurves as UsdBasisCurves, Mesh as UsdMesh, PointInstancer, Sphere as UsdSphere,
};
use tracing::{debug, warn};

use crate::material::Material;
use crate::rt_world::{FaceMap, UvMap, WorldBuilder};

use super::attrs::{custom_token, prim_ray_mask};
use super::materials::resolve_material;
use super::mesh::{mesh_source, placement_scale};
use super::shapes::{curve_segments, sphere_radius};
use super::time::eval_time;
use super::xform::{local_matrix_at, resets_xform_stack_at};
use super::{ImportCaches, is_invisible, non_render_purpose, prim_at};

/// How deep prototypes may nest before the importer gives up. USD forbids
/// an instancing cycle, but a malformed stage can still describe one, and
/// each level multiplies traversal cost — so this is a backstop, set far
/// above any plausible authoring depth.
const MAX_INSTANCE_NESTING: usize = 8;

/// What a hit on one geometry of a [`ProtoPart`] resolves to: the material
/// and texture tables `World` keys by `geom_id`.
#[derive(Clone)]
pub(super) struct PartSlot {
    material: Arc<dyn Material>,
    /// Triangle-to-source-face table, when this slot's material samples a
    /// per-face texture.
    ///
    /// A slot is always exactly one leaf geometry — the walk splits per bound
    /// mesh — so one table serves every placement of it, and the `prim_id` a
    /// hit reports indexes that table unambiguously however many levels of
    /// instancing it passed through (the kernel forwards the innermost
    /// `prim_id` unchanged).
    faces: Option<Arc<FaceMap>>,
    /// Per-triangle texture coordinates, when this slot's material reads
    /// them. Carried on the same terms as `faces`, and — like every instanced
    /// placement — without tangents: the table is the prototype's and each
    /// placement transforms it differently. See [`UvMap::tangents`].
    uvs: Option<Arc<UvMap>>,
    /// Uniform scale from the slot's own geometry frame, which its tables'
    /// densities are in, to the part's frame. 1 for a leaf part; inside a
    /// group, the member's placement. Nested *instancer* placements are not
    /// folded in (they differ per placement against one table), so such
    /// geometry filters against the outer scale — see the texture filtering
    /// gaps in openspec/specs/textures/design.md.
    scale: f32,
}

/// A piece of a prototype, in the prototype root's frame: a committed
/// kernel scene in its own local space, the transform placing it relative
/// to the prototype root, its visibility mask, and what its hits resolve to.
///
/// With one slot it is a leaf — one bound geometry — and every hit in
/// `scene` is that slot. With several it is a group: `scene` labels its hits
/// with the slot index, and placing it takes as many `geom_id`s.
#[derive(Clone)]
pub(super) struct ProtoPart {
    pub(super) scene: Arc<RtScene>,
    /// Prototype-root-relative placement. An instance's world transform is
    /// composed onto the left of this.
    pub(super) local: GMat4,
    pub(super) mask: u32,
    pub(super) slots: Arc<[PartSlot]>,
}

impl ProtoPart {
    /// One bound geometry with its material and tables.
    fn leaf(
        scene: Arc<RtScene>,
        local: GMat4,
        mask: u32,
        material: Arc<dyn Material>,
        faces: Option<Arc<FaceMap>>,
        uvs: Option<Arc<UvMap>>,
    ) -> Self {
        ProtoPart {
            scene,
            local,
            mask,
            slots: Arc::new([PartSlot {
                material,
                faces,
                uvs,
                scale: 1.0,
            }]),
        }
    }

    /// How an instance of this part labels its hits when its first slot is
    /// numbered `first`: a group forwards the inner index on top of it, a
    /// leaf reports `first` whatever its scene says.
    fn label(&self, first: u32) -> InstanceHitId {
        if self.slots.len() > 1 {
            InstanceHitId::Offset(first)
        } else {
            InstanceHitId::As(first)
        }
    }
}

/// Walks a prototype subtree and builds its [`ProtoPart`]s, in the
/// prototype root's local space (the root itself contributes no
/// transform — an instance supplies the placement).
///
/// Abstract (`class`) prims are *not* skipped here, unlike in the main
/// traversal: naming a class as a prototype is exactly how one authors
/// "geometry that exists only to be instanced".
pub(super) fn collect_proto_parts(
    stage: &Stage,
    root: &Prim,
    caches: &mut ImportCaches<'_>,
    depth: usize,
) -> Vec<ProtoPart> {
    let mut parts = Vec::new();
    if depth > MAX_INSTANCE_NESTING {
        warn!(
            "Prototype {} exceeds {MAX_INSTANCE_NESTING} levels of instance nesting — not expanded",
            root.path()
        );
        return parts;
    }
    let mut stack: Vec<(Prim, GMat4)> = vec![(root.clone(), GMat4::IDENTITY)];
    // Mesh parts found by the walk, as (index into `parts`, mesh slot). Their
    // kernel scenes are built together once the walk is done — in parallel,
    // see `MeshArena::commit_slots` — and each part holds a placeholder until
    // then.
    let mut pending_meshes: Vec<(usize, u32)> = Vec::new();

    while let Some((prim, parent_local)) = stack.pop() {
        // Same pruning as the top-level traversal: an inactive prim (and
        // its subtree) is absent from the composed scene, prototype or not.
        if !prim.is_active().unwrap_or(true) {
            debug!(
                "Skipping inactive prim {} (prototype {})",
                prim.path(),
                root.path()
            );
            continue;
        }
        if let Some(purpose) = non_render_purpose(&prim) {
            debug!(
                "Skipping {purpose}-purpose prim {} (prototype {})",
                prim.path(),
                root.path()
            );
            continue;
        }
        // Visibility counts from the prototype root down, as UsdImaging
        // computes it for a prototype: an invisible part of a prototype is
        // missing from every instance. No camera is taken from a prototype,
        // so here the subtree is simply pruned.
        if is_invisible(&prim) {
            debug!(
                "Skipping invisible prim {} (prototype {})",
                prim.path(),
                root.path()
            );
            continue;
        }

        // The prototype root's own transform is deliberately excluded: a
        // `PointInstancer` prototype is placed entirely by its per-instance
        // transform, and a native prototype root carries none.
        let this_local = if prim.path() == root.path() {
            GMat4::IDENTITY
        } else if resets_xform_stack_at(stage, &prim) {
            local_matrix_at(stage, &prim)
        } else {
            parent_local * local_matrix_at(stage, &prim)
        };

        let mask = prim_ray_mask(&prim);

        // Checked before any schema lookup, because a schema `get()` reads
        // the prim's type name and that is exactly what aborts here.
        //
        // A natively-instanced prim *inside* a prototype is unreachable
        // with openusd 0.5.0: resolving its prototype, or reading the type
        // of any prim beneath it, trips an internal assertion
        // (`pcp/instancing.rs`: "materialized prototype root's
        // instanceable must be inert"), which aborts debug builds. The
        // prim itself is safe to inspect; its contents are not. So there
        // is no route to the geometry — not the prototype, not the proxy
        // subtree — and the honest response is to say so and move on
        // rather than abort. Nested *PointInstancer* is unaffected and is
        // expanded below.
        //
        // Four-line repro and the full diagnosis live in
        // `nested_native_instance_degrades_gracefully` in
        // `crates/crust-core/tests/usd_scene.rs`. Delete this arm when
        // upstream is fixed; `collect_proto_parts` can then splice the
        // inner prototype's parts in with composed transforms.
        if prim.path() != root.path() && prim.is_instance().unwrap_or(false) {
            warn!(
                "Nested native instance at {} skipped: openusd 0.5 cannot read \
                 an instanceable prim's contents inside a prototype. Author it \
                 as a PointInstancer, or flatten the inner instance.",
                prim.path()
            );
            continue;
        }

        if let Ok(Some(mesh)) = UsdMesh::get(stage, prim.path().clone()) {
            let material = resolve_material(stage, &prim, caches);
            // A prototype part is placed by an instance by definition, so it
            // always needs a real kernel scene — committing here is also what
            // marks the slot as ineligible for baking, so a mesh used both
            // directly and as a prototype is not stored twice.
            if let Some(src) = mesh_source(
                &prim,
                &mesh,
                material.face_texture().is_some(),
                material.uses_uv(),
                material.uv_primvar(),
            ) && let Some(slot) = caches.meshes.intern(&prim, &src, &material)
            {
                let faces = caches.meshes.slots[slot as usize].faces.clone();
                let uvs = caches.meshes.slots[slot as usize].uvs.clone();
                pending_meshes.push((parts.len(), slot));
                parts.push(ProtoPart::leaf(
                    placeholder_scene(),
                    this_local,
                    mask,
                    material,
                    faces,
                    uvs,
                ));
            }
        } else if let Ok(Some(sphere)) = UsdSphere::get(stage, prim.path().clone()) {
            let material = resolve_material(stage, &prim, caches);
            let radius = sphere_radius(&sphere);
            let mut b = RtSceneBuilder::new();
            // Local-space sphere at the origin: unlike the top-level
            // sphere path, which bakes the centre into world space, this
            // lets the instance transform scale it (a non-uniform scale
            // correctly yields an ellipsoid, since rays enter local space).
            b.attach(Geometry::Sphere {
                center: Vec3A::ZERO,
                radius,
            });
            parts.push(ProtoPart::leaf(
                Arc::new(b.commit()),
                this_local,
                mask,
                material,
                None,
                None,
            ));
        } else if let Ok(Some(curves)) = UsdBasisCurves::get(stage, prim.path().clone()) {
            let material = resolve_material(stage, &prim, caches);
            if let Some((segments, cubic_segments)) = curve_segments(&prim, &curves) {
                let mut b = RtSceneBuilder::new();
                if !segments.is_empty() {
                    b.attach(Geometry::RoundCurves { segments });
                }
                if !cubic_segments.is_empty() {
                    b.attach(Geometry::CubicCurves {
                        segments: cubic_segments,
                    });
                }
                parts.push(ProtoPart::leaf(
                    Arc::new(b.commit()),
                    this_local,
                    mask,
                    material,
                    None,
                    None,
                ));
            }
        } else if custom_token(&prim, "crust:volume:type").is_some() {
            // Volumes live outside the surface BVH entirely (their bounds
            // must not occlude shadow rays), so they cannot ride an
            // instance transform. Say so rather than dropping silently.
            warn!(
                "Volume at {} is inside a prototype — volumes cannot be instanced, skipped",
                prim.path()
            );
        } else if let Ok(Some(instancer)) = PointInstancer::get(stage, prim.path().clone()) {
            // A PointInstancer inside a prototype: expand it into nested
            // sub-scenes rather than flattening. Flattening would multiply
            // the *outer* instance count by this instancer's, which is
            // exactly the blow-up instancing exists to avoid — a prototype
            // holding 500 leaves, itself placed 500 times, must stay 500
            // outer instances, not 250 000.
            parts.extend(nested_instancer_parts(
                stage,
                &prim,
                &instancer,
                this_local,
                mask,
                caches,
                depth + 1,
            ));
            // Its prototypes are reached through it, never drawn directly.
            continue;
        }

        if let Ok(children) = prim.children() {
            for child in children {
                stack.push((child, this_local));
            }
        }
    }
    // Committing is also what marks each slot as ineligible for baking, so a
    // mesh used both directly and as a prototype is not stored twice.
    let slots: Vec<u32> = pending_meshes.iter().map(|&(_, slot)| slot).collect();
    caches.meshes.commit_slots(&slots);
    for (index, slot) in pending_meshes {
        parts[index].scene = caches.meshes.committed_scene(slot);
    }
    parts
}

/// What a prototype's mesh part holds until its kernel scene is built at the
/// end of the walk (see `collect_proto_parts`). Never attached to anything.
fn placeholder_scene() -> Arc<RtScene> {
    static EMPTY: std::sync::OnceLock<Arc<RtScene>> = std::sync::OnceLock::new();
    Arc::clone(EMPTY.get_or_init(|| Arc::new(RtSceneBuilder::new().commit())))
}

/// Expands a `PointInstancer` found *inside* a prototype into one part.
///
/// Each of its prototypes is grouped into a single scene ([`group_parts`]),
/// and the part is a scene of those groups, placed once per nested instance —
/// so the BVH over it sees one box per placement, which is what makes a
/// scatter cull. Hits come out labelled by slot, the prototypes' slots laid
/// end to end.
///
/// It used to be the other way round: one part per (prototype, part), each
/// a scene of that one piece placed everywhere, because `World` could only
/// tell parts apart by top-level instance. Each such part spans the whole
/// scatter, so a scatter of many-part prototypes became that many identical
/// boxes — 64 724 of them over the Moana island's dunes, 99% of a render's
/// instance descents (`docs/moana_profile.md`).
fn nested_instancer_parts(
    stage: &Stage,
    prim: &Prim,
    instancer: &PointInstancer,
    local: GMat4,
    mask: u32,
    caches: &mut ImportCaches<'_>,
    depth: usize,
) -> Vec<ProtoPart> {
    let Some(layout) = read_instancer(prim, instancer) else {
        return Vec::new();
    };
    let groups: Vec<Option<ProtoPart>> = layout
        .targets
        .iter()
        .map(|target| prototype_group(stage, target, caches, depth))
        .collect();

    // The placements that draw something: a prototype with no geometry, or a
    // zero-scale placement (the "hide this instance" idiom), draws nothing.
    let drawn: Vec<(usize, GMat4)> = layout
        .placements
        .iter()
        .filter_map(|&(k, xf)| {
            let placement = xf * groups[k].as_ref()?.local;
            (placement.determinant().abs() >= 1e-12).then_some((k, placement))
        })
        .collect();

    // Slots only for the prototypes something actually draws — every slot is
    // reserved again at each placement of this part, so a prototype placed
    // only by hidden entries would cost ids no hit can reach — in prototype
    // order so the layout is independent of placement order.
    let mut first: Vec<Option<u32>> = vec![None; groups.len()];
    for &(k, _) in &drawn {
        first[k] = Some(0);
    }
    let mut slots: Vec<PartSlot> = Vec::new();
    for (k, group) in groups.iter().enumerate() {
        if let (Some(f), Some(g)) = (first[k].as_mut(), group) {
            *f = slots.len() as u32;
            slots.extend(g.slots.iter().cloned());
        }
    }

    let mut sub = RtSceneBuilder::new();
    sub.reserve(drawn.len());
    for &(k, placement) in &drawn {
        let (Some(g), Some(f)) = (&groups[k], first[k]) else {
            unreachable!("a drawn placement has a group and a slot range");
        };
        sub.attach_labelled(
            Geometry::Instance {
                scene: g.scene.clone(),
                transform: Affine3A::from_mat4(placement),
                transform_end: None,
            },
            g.mask,
            g.label(f),
        );
    }

    debug!(
        "Expanded nested PointInstancer at {} ({} instances of {} prototype(s) -> {} slot(s))",
        prim.path(),
        layout.placements.len(),
        layout.targets.len(),
        slots.len()
    );
    if sub.count() == 0 {
        return Vec::new();
    }
    vec![ProtoPart {
        scene: Arc::new(sub.commit()),
        local,
        mask,
        slots: slots.into(),
    }]
}

/// Puts a prototype's parts into one scene, so it can be placed as one
/// instance: each part becomes a member instance at its prototype-relative
/// transform, labelled with the index of its first slot, and the group's
/// slots are the members' laid end to end. `None` when nothing is left.
///
/// A single part is returned as it is — grouping it would only add a level
/// of instancing.
fn group_parts(parts: &[ProtoPart]) -> Option<ProtoPart> {
    if let [only] = parts {
        return Some(only.clone());
    }
    let mut b = RtSceneBuilder::new();
    b.reserve(parts.len());
    let mut slots: Vec<PartSlot> = Vec::new();
    let mut mask = 0;
    for part in parts {
        if part.local.determinant().abs() < 1e-12 {
            continue;
        }
        let local = Affine3A::from_mat4(part.local);
        b.attach_labelled(
            Geometry::Instance {
                scene: part.scene.clone(),
                transform: local,
                transform_end: None,
            },
            part.mask,
            part.label(slots.len() as u32),
        );
        let scale = placement_scale(&local);
        slots.extend(part.slots.iter().map(|slot| PartSlot {
            scale: slot.scale * scale,
            ..slot.clone()
        }));
        // A ray enters the group if any member could take it; each member's
        // own mask then gates it inside.
        mask |= part.mask;
    }
    if slots.is_empty() {
        return None;
    }
    Some(ProtoPart {
        scene: Arc::new(b.commit()),
        local: GMat4::IDENTITY,
        mask,
        slots: slots.into(),
    })
}

/// A prototype as one part ([`group_parts`]), from the cache or freshly
/// built.
fn prototype_group(
    stage: &Stage,
    proto_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
    depth: usize,
) -> Option<ProtoPart> {
    let key = (caches.epoch, proto_path.to_string());
    if let Some(group) = caches.groups.get(&key) {
        return group.clone();
    }
    let parts = prototype_parts(stage, proto_path, caches, depth);
    let group = group_parts(&parts);
    caches.groups.insert(key, group.clone());
    group
}

/// The parts a top-level placement of `proto_path` attaches: the prototype
/// grouped into one instance when it has at least
/// [`TOP_LEVEL_GROUP_MIN_PARTS`] parts, else its parts one instance each.
fn placed_parts(
    stage: &Stage,
    proto_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
) -> Arc<Vec<ProtoPart>> {
    let parts = prototype_parts(stage, proto_path, caches, 0);
    if parts.len() < TOP_LEVEL_GROUP_MIN_PARTS {
        return parts;
    }
    Arc::new(
        prototype_group(stage, proto_path, caches, 0)
            .into_iter()
            .collect(),
    )
}

/// Parts from which a top-level placement is grouped into one instance.
///
/// Grouping costs every ray that enters the placement one more transform,
/// which a prototype of a few parts does not repay: its parts' boxes at the
/// top level are few and already local to the placement. A prototype of
/// thousands of parts — a Moana bay cedar is 16 181 — puts that many boxes
/// into the root BVH per placement. The threshold is a round number between
/// the two, not a measured optimum; nested instancers group always, since
/// there each ungrouped part spans the whole scatter.
const TOP_LEVEL_GROUP_MIN_PARTS: usize = 64;

/// Imports one natively-instanced prim (`instanceable = true` plus a
/// composition arc) by placing its shared prototype's parts.
///
/// This is the mechanism Moana-scale scenes rely on, and the reason it
/// matters is memory: without it the importer walks each instance's proxy
/// subtree and re-reads its geometry, so cost scales with the *instance*
/// count. Here every instance of a prototype shares one set of committed
/// kernel scenes, and costs only its transforms.
pub(super) fn emit_native_instance(
    stage: &Stage,
    world: &mut WorldBuilder,
    prim: &Prim,
    proto_path: &sdf::Path,
    world_xf: GMat4,
    caches: &mut ImportCaches<'_>,
) {
    let parts = placed_parts(stage, proto_path, caches);
    let first = world.count();
    attach_proto_parts(world, &parts, world_xf, "native instance");
    debug!(
        "Instance {} uses prototype {proto_path} ({} instance(s), geom ids {first}..{})",
        prim.path(),
        parts.len(),
        world.count()
    );
}

/// Attaches every part of a prototype at `placement`, one instance each,
/// and returns how many kernel instances that was. A group's instance takes
/// one `geom_id` per slot, so the ids consumed can exceed the count.
/// Non-invertible placements are skipped: the kernel's instance transform
/// must be invertible, and a zero scale is a common "hide this instance"
/// idiom rather than an error.
fn attach_proto_parts(
    world: &mut WorldBuilder,
    parts: &[ProtoPart],
    placement: GMat4,
    what: &str,
) -> usize {
    let mut attached = 0;
    for part in parts {
        let xf = placement * part.local;
        if xf.determinant().abs() < 1e-12 {
            debug!("{what}: non-invertible instance transform — skipped");
            continue;
        }
        let xf = Affine3A::from_mat4(xf);
        // A leaf reports its own id; a group forwards its slot index on top
        // of the first of the consecutive ids its slots take.
        let label = if part.slots.len() > 1 {
            InstanceHitId::Offset(world.count() as u32)
        } else {
            InstanceHitId::Own
        };
        let geom_id = world.attach_labelled(
            Geometry::Instance {
                scene: part.scene.clone(),
                transform: xf,
                transform_end: None,
            },
            part.slots[0].material.clone(),
            part.mask,
            label,
        );
        for slot in &part.slots[1..] {
            world.reserve_slot(slot.material.clone(), part.mask);
        }
        // `part.local` is already folded in, so this is the scale from the
        // prototype's own frame to world; each slot adds its own inside the
        // part. Geometry under *nested* instancer placements is the
        // exception: those scales live inside the committed kernel scene and
        // differ per placement, so it filters against the outer scale alone.
        // See the texture filtering gaps in openspec/specs/textures/design.md.
        let scale = placement_scale(&xf);
        for (i, slot) in part.slots.iter().enumerate() {
            let id = geom_id + i as u32;
            // An instance transforms the ray, not the triangles, so the
            // winding — and with it the barycentric order — is the
            // prototype's own: no swap.
            if let Some(map) = &slot.faces {
                world.set_face_map(id, map.clone(), false);
            }
            if let Some(map) = &slot.uvs {
                world.set_uv_map(id, map.clone(), false);
            }
            world.set_placement_scale(id, scale * slot.scale);
        }
        attached += 1;
    }
    attached
}

/// Imports a `UsdGeomPointInstancer`: every entry of the per-instance
/// arrays places the prototype selected by `protoIndices`.
///
/// The per-instance transform is USD's `translate ∘ orient ∘ scale`
/// (spec: scale first, then orientation, then position), composed under
/// the instancer's own world transform. `invisibleIds` prunes instances by
/// `ids`; where `ids` is absent the array index is the id, as USD
/// specifies.
///
/// Memory is what this is for: N instances of a prototype cost one copy of
/// its geometry plus N transforms, instead of N baked copies.
/// A `PointInstancer`'s prototypes and the placements that select them,
/// resolved once and reused by both the top-level emitter and the nested
/// (inside-a-prototype) path.
struct InstancerLayout {
    /// The `prototypes` relationship's ordered targets.
    pub(super) targets: Vec<sdf::Path>,
    /// Visible instances as `(index into targets, transform relative to
    /// the instancer)`. Instances hidden by `invisibleIds` are already
    /// dropped.
    pub(super) placements: Vec<(usize, GMat4)>,
    /// How many instances `invisibleIds` removed, for reporting.
    pub(super) hidden: usize,
}

/// Reads the per-instance arrays into placements. `None` when the prim is
/// not a usable instancer.
///
/// The transform is USD's `translate ∘ orient ∘ scale` — scale first, then
/// orientation, then position. `orientationsf` (single precision) wins over
/// `orientations` (half) where both are authored, and `invisibleIds`
/// prunes by `ids`, with the array index standing in as the id where `ids`
/// is absent.
fn read_instancer(prim: &Prim, instancer: &PointInstancer) -> Option<InstancerLayout> {
    let targets = match instancer.prototypes_rel().targets() {
        Ok(t) if !t.is_empty() => t,
        _ => {
            warn!(
                "PointInstancer at {} has no `prototypes` targets — skipped",
                prim.path()
            );
            return None;
        }
    };

    let Ok(Some(sdf::Value::IntVec(proto_indices))) = instancer
        .proto_indices_attr()
        .get_at::<sdf::Value>(eval_time())
    else {
        warn!(
            "PointInstancer at {} has no `protoIndices` — skipped",
            prim.path()
        );
        return None;
    };

    let positions = value_vec3f_array(&instancer.positions_attr()).unwrap_or_default();
    let scales = value_vec3f_array(&instancer.scales_attr());
    let orientations = instance_orientations(instancer);
    let ids = match instancer.ids_attr().get_at::<sdf::Value>(eval_time()) {
        Ok(Some(sdf::Value::Int64Vec(v))) => Some(v),
        _ => None,
    };
    let invisible: std::collections::HashSet<i64> = match instancer
        .invisible_ids_attr()
        .get_at::<sdf::Value>(eval_time())
    {
        Ok(Some(sdf::Value::Int64Vec(v))) => v.into_iter().collect(),
        _ => Default::default(),
    };

    if positions.len() < proto_indices.len() {
        warn!(
            "PointInstancer at {}: {} protoIndices but only {} positions — extra instances skipped",
            prim.path(),
            proto_indices.len(),
            positions.len()
        );
    }

    let mut placements = Vec::with_capacity(proto_indices.len());
    let mut hidden = 0usize;
    for (i, &proto_index) in proto_indices.iter().enumerate() {
        let Some(pos) = positions.get(i) else { break };

        let id = ids
            .as_ref()
            .map_or(i as i64, |ids| ids.get(i).copied().unwrap_or(i as i64));
        if invisible.contains(&id) {
            hidden += 1;
            continue;
        }

        let Some(k) = usize::try_from(proto_index)
            .ok()
            .filter(|k| *k < targets.len())
        else {
            warn!(
                "PointInstancer at {}: protoIndices[{i}] = {proto_index} is out of range — instance skipped",
                prim.path()
            );
            continue;
        };

        let scale = scales
            .as_ref()
            .and_then(|s| s.get(i))
            .map_or(Vec3::ONE, |s| Vec3::new(s.x, s.y, s.z));
        let rotation = orientations
            .as_ref()
            .and_then(|q| q.get(i).copied())
            .unwrap_or(glam::Quat::IDENTITY);
        placements.push((
            k,
            GMat4::from_scale_rotation_translation(scale, rotation, Vec3::new(pos.x, pos.y, pos.z)),
        ));
    }

    Some(InstancerLayout {
        targets,
        placements,
        hidden,
    })
}

/// A prototype's parts, from the cache or freshly built.
fn prototype_parts(
    stage: &Stage,
    proto_path: &sdf::Path,
    caches: &mut ImportCaches<'_>,
    depth: usize,
) -> Arc<Vec<ProtoPart>> {
    let key = (caches.epoch, proto_path.to_string());
    if let Some(parts) = caches.protos.get(&key) {
        debug!(
            "Prototype {} (epoch {}): reusing {} cached part(s)",
            key.1,
            key.0,
            parts.len()
        );
        return parts.clone();
    }
    let started = Instant::now();
    let root = prim_at(stage, proto_path.clone());
    let parts = Arc::new(collect_proto_parts(stage, &root, caches, depth));
    if parts.is_empty() {
        warn!("Prototype {} contributed no geometry", key.1);
    } else {
        debug!(
            "Prototype {} (epoch {}, nesting depth {depth}): built {} part(s) in {:?}",
            key.1,
            key.0,
            parts.len(),
            started.elapsed()
        );
    }
    caches.protos.insert(key, parts.clone());
    parts
}

/// Imports a `UsdGeomPointInstancer`: every entry of the per-instance
/// arrays places the prototype selected by `protoIndices`.
///
/// Memory is what this is for: N instances of a prototype cost one copy of
/// its geometry plus N transforms, instead of N baked copies.
pub(super) fn emit_point_instancer(
    stage: &Stage,
    world: &mut WorldBuilder,
    prim: &Prim,
    instancer: &PointInstancer,
    world_xf: GMat4,
    caches: &mut ImportCaches<'_>,
) {
    let Some(layout) = read_instancer(prim, instancer) else {
        return;
    };
    let proto_parts: Vec<Arc<Vec<ProtoPart>>> = layout
        .targets
        .iter()
        .map(|target| placed_parts(stage, target, caches))
        .collect();

    // A dense scatter can place millions of instances in this one call;
    // reserving the exact total up front avoids both the doubling-copy
    // cost and the over-allocation of growing the geometry table
    // incrementally (see `WorldBuilder::reserve`).
    let part_counts: Vec<usize> = proto_parts
        .iter()
        .map(|parts| parts.iter().map(|p| p.slots.len()).sum())
        .collect();
    let total_geometries: usize = layout.placements.iter().map(|&(k, _)| part_counts[k]).sum();
    world.reserve(total_geometries);

    let mut attached = 0usize;
    let first = world.count();
    for &(k, xf) in &layout.placements {
        attached += attach_proto_parts(
            world,
            &proto_parts[k],
            world_xf * xf,
            "PointInstancer instance",
        );
    }

    debug!(
        "Imported PointInstancer at {} ({} instances of {} prototype(s): {} kernel instances attached, geom ids {first}..{}{})",
        prim.path(),
        layout.placements.len(),
        layout.targets.len(),
        attached,
        world.count(),
        if layout.hidden > 0 {
            format!(", {} hidden by invisibleIds", layout.hidden)
        } else {
            String::new()
        }
    );
}

/// A `point3f[]` / `float3[]` attribute as a plain vector.
fn value_vec3f_array(attr: &openusd::usd::Attribute) -> Option<Vec<Vec3f>> {
    match attr.get_at::<sdf::Value>(eval_time()) {
        Ok(Some(sdf::Value::Vec3fVec(v))) => Some(v),
        _ => None,
    }
}

/// Per-instance rotations, preferring single-precision `orientationsf`
/// over half-precision `orientations` as USD specifies.
fn instance_orientations(instancer: &PointInstancer) -> Option<Vec<glam::Quat>> {
    let quat = |w: f32, x: f32, y: f32, z: f32| glam::Quat::from_xyzw(x, y, z, w).normalize();
    if let Ok(Some(sdf::Value::QuatfVec(v))) = instancer
        .orientationsf_attr()
        .get_at::<sdf::Value>(eval_time())
    {
        return Some(v.iter().map(|q| quat(q.w, q.x, q.y, q.z)).collect());
    }
    match instancer
        .orientations_attr()
        .get_at::<sdf::Value>(eval_time())
    {
        Ok(Some(sdf::Value::QuathVec(v))) => Some(
            v.iter()
                .map(|q| quat(q.w.to_f32(), q.x.to_f32(), q.y.to_f32(), q.z.to_f32()))
                .collect(),
        ),
        _ => None,
    }
}
