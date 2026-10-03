//! The Embree-shaped public API: attach [`Geometry`] objects to a
//! [`SceneBuilder`], `commit()` into an immutable [`Scene`], query with
//! `intersect` / `occluded`.

use crate::aabb::AABB;
use crate::bvh::{Bvh, Primitives};
use crate::prim::{
    CubicCurvePrim, CurvePrim, CylinderPrim, DEGENERATE_VERTEX, DiskPrim, GeomTable,
    InstanceMotion, InstancePrim, NO_ID_OFFSET, PrimHit, PrimNode, SpherePrim, TriangleRecord,
    transformed_aabb,
};
use crate::ray::{MASK_ALL, Ray, RayMask};
use glam::{Affine3A, Vec3A};
use std::sync::Arc;

/// One round (sphere-swept) curve segment: a cone frustum tangent to the
/// spheres `(p0, r0)` and `(p1, r1)`, with spherical caps.
#[derive(Clone, Copy, Debug)]
pub struct CurveSegment {
    pub p0: Vec3A,
    pub p1: Vec3A,
    pub r0: f32,
    pub r1: f32,
}

/// How a committed scene stores its triangle packets — see
/// [`SceneBuilder::commit_with`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PacketLayout {
    /// Each packet carries its four triangles' vertices (192 bytes): the
    /// fastest in cache. The layout before indexed packets existed.
    Gathered,
    /// Each packet carries vertex indices (92 bytes) and gathers from the
    /// scene's shared vertex table at every test; bit-identical hits. A
    /// memory trade, not a speed one: on the subdivision stress grid it is
    /// 104 → 79 kernel bytes per triangle for 13% less render throughput,
    /// and on a 4 M-triangle soup that does not fit the cache it is 22%
    /// fewer kernel bytes for 30% slower traversal — the twelve dependent
    /// vertex loads per packet cost more than the bandwidth they save.
    Indexed,
    /// The measured default: `Gathered`. The expectation was that a tree
    /// too large for the cache would favour the smaller packet; measured
    /// with `ray_throughput --layout` in and out of cache, no size does, so
    /// the indexed layout is an explicit opt-in for a scene that otherwise
    /// does not fit.
    #[default]
    Auto,
}

/// What [`SceneBuilder::commit_with`] lets a caller choose about the build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommitOptions {
    /// The triangle packet layout.
    pub layout: PacketLayout,
    /// Size all-triangle leaves by packet tests rather than triangle tests:
    /// a range of four or fewer triangles is one SIMD round whatever its
    /// count, so the SAH leaf decision charges `ceil(n / 4)` per side plus
    /// one node test for the split, and a range of five to eight triangles
    /// whose children overlap stays one leaf of two full packets instead of
    /// splitting into two half-empty ones. `false` is the per-triangle
    /// leaf cost before this option existed. Either way the build is
    /// deterministic; the two trees differ in shape, so their renders can
    /// differ on exact-tie hits only.
    pub packet_sah: bool,
}

impl Default for CommitOptions {
    fn default() -> Self {
        CommitOptions {
            layout: PacketLayout::Auto,
            packet_sah: true,
        }
    }
}

/// Exact bytes a committed [`Scene`] holds, by structure — the kernel's
/// side of a memory report. Counts `capacity`, not `len`, because unused
/// capacity is resident too, and deduplicates shared instanced scenes so
/// a prototype placed a thousand times is counted once.
///
/// Only the kernel's own allocations: the application's material tables,
/// the USD stage and anything else outside `crust-rt` are not visible
/// from here.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MemoryFootprint {
    /// The `PrimNode` arrays: spheres, disks, cylinders and linear curve
    /// segments.
    pub prim_nodes: usize,
    /// Instances, 96 bytes each inline, plus the endpoint transforms of the
    /// moving ones.
    pub instances: usize,
    /// Cubic curve spans, 96 bytes each inline.
    pub cubic_spans: usize,
    /// 4-wide BVH nodes.
    pub bvh_nodes: usize,
    pub leaves: usize,
    /// Triangle SIMD packets of the gathered layout (the vertices, SoA).
    pub packets: usize,
    /// Triangle SIMD packets of the indexed layout (vertex indices).
    pub packets_indexed: usize,
    /// Leaf primitive indices.
    pub indices: usize,
    /// Triangle records: vertex indices, ids and mask, 24 bytes each.
    pub triangle_records: usize,
    /// The shared vertex table, 12 bytes per vertex.
    pub vertices: usize,
    /// Per-vertex shading normals of the meshes that carry them, 12 bytes
    /// per vertex.
    pub vertex_normals: usize,
    /// The per-geometry table (where each geometry's entries start).
    pub geometry_tables: usize,
    /// Packet lanes in total — a count, not bytes, so `total` ignores it.
    pub lanes: usize,
    /// Packet lanes holding a triangle; `lanes_filled / lanes` is the fill.
    pub lanes_filled: usize,
}

impl MemoryFootprint {
    pub fn total(&self) -> usize {
        self.prim_nodes
            + self.instances
            + self.cubic_spans
            + self.bvh_nodes
            + self.leaves
            + self.packets
            + self.packets_indexed
            + self.indices
            + self.triangle_records
            + self.vertices
            + self.vertex_normals
            + self.geometry_tables
    }
}

/// Top-level primitives of a committed [`Scene`], split by kind. See
/// [`Scene::primitive_breakdown`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrimitiveBreakdown {
    pub triangles: usize,
    pub spheres: usize,
    pub disks: usize,
    pub cylinders: usize,
    pub curve_segments: usize,
    pub cubic_curve_spans: usize,
    pub instances: usize,
}

/// One authored cubic curve span — its own Bézier control points and end
/// radii, intersected analytically (`crate::curve::cubic_curve_intersect`)
/// rather than pre-flattened into several [`CurveSegment`]s. Round, same
/// as `RoundCurves` — this only changes how a span is stored and
/// intersected, not the surface it represents.
#[derive(Clone, Copy, Debug)]
pub struct CubicCurveSegment {
    pub cp: [Vec3A; 4],
    pub r0: f32,
    pub r1: f32,
}

/// A geometry to attach to a scene. The variants mirror Embree's geometry
/// types (the subset crust needs): triangle meshes, analytic spheres,
/// disks and open cylinders, round curves, and instances of another
/// committed scene. Instances nest:
/// an instanced scene may itself contain instances, and transforms,
/// normals and ray masks compose correctly through every level.
pub enum Geometry {
    /// Unpadded `[f32; 3]` arrays, which is how the committed scene stores
    /// them: a `Vec3A` is 16 bytes for 12 of data, and these arrays are
    /// the bulk of a scene's memory.
    TriangleMesh {
        vertices: Vec<[f32; 3]>,
        indices: Vec<[u32; 3]>,
        /// Optional per-vertex shading normals; hits interpolate them by
        /// the barycentrics (`SmoothTriangle` semantics).
        normals: Option<Vec<[f32; 3]>>,
    },
    Sphere {
        center: Vec3A,
        radius: f32,
    },
    /// A flat circular disk. `normal` names its front: hits report it as
    /// the outward normal, so `RayHit::front_face` tells the two sides apart.
    /// Need not be unit length; it is normalised at commit.
    Disk {
        center: Vec3A,
        normal: Vec3A,
        radius: f32,
    },
    /// The side wall of a circular cylinder from `p0` to `p1` — open, with no
    /// end caps. Hits report the radial outward normal.
    Cylinder {
        p0: Vec3A,
        p1: Vec3A,
        radius: f32,
    },
    RoundCurves {
        segments: Vec<CurveSegment>,
    },
    /// Cubic curve spans, intersected as true curves instead of being
    /// flattened to `RoundCurves` polylines — see [`CubicCurveSegment`].
    CubicCurves {
        segments: Vec<CubicCurveSegment>,
    },
    /// Another committed scene placed by `transform` (local-to-world).
    /// With `transform_end`, the placement interpolates linearly (per
    /// matrix element) at the ray's shutter time — transform motion blur.
    /// `transform` must be invertible.
    Instance {
        scene: Arc<Scene>,
        transform: Affine3A,
        /// Boxed because it's `None` for the overwhelming majority of
        /// instances (only motion-blurred placements set it): inline it
        /// and every `Geometry` value — the enum is sized by its largest
        /// variant — pays an extra 64 bytes it never uses. A scene with
        /// millions of `PointInstancer` placements makes that the
        /// dominant cost of the whole geometry table.
        transform_end: Option<Box<Affine3A>>,
    },
}

/// An intersection: distance, ray-facing normal (flipped to oppose the
/// ray, with `front_face` recording the original orientation), the hit
/// barycentrics where meaningful, and the IDs that let the application
/// map the hit back to its own data (materials, lights, …).
///
/// For hits inside an [`Geometry::Instance`], `geom_id` is the
/// *instance's* id in the queried scene and `prim_id` the primitive index
/// within the instanced scene — the application maps per top-level
/// geometry.
#[derive(Clone, Copy, Debug)]
pub struct RayHit {
    pub t: f32,
    pub normal: Vec3A,
    pub front_face: bool,
    pub u: f32,
    pub v: f32,
    pub geom_id: u32,
    pub prim_id: u32,
}

/// Which `geom_id` a hit found inside an [`Geometry::Instance`] reports.
///
/// By default an instance reports its *own* id and the inner ids are lost,
/// which is all a host that maps one material per top-level geometry needs.
/// A prototype of many parts wants more: to be placed as *one* instance
/// (so the BVH above it sees one box per placement, not one per part) and
/// still have a hit say which part it landed on. Embree answers with an
/// instance-id stack beside the inner `geomID`; this answers with one id,
/// computed as the hit passes back out through each instance level, which
/// keeps [`RayHit`] and the host's lookup a single index.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum InstanceHitId {
    /// The instance's own `geom_id` in the scene it is attached to.
    #[default]
    Own,
    /// A fixed id, whatever the inner scene reported.
    As(u32),
    /// `base` plus the id the inner scene reported, so an inner scene whose
    /// hits already carry `0..n` maps onto `base..base + n` here. Nests:
    /// each level adds its own base. [`SceneBuilder::commit`] panics if
    /// `base` plus the largest id the inner scene can report would leave
    /// the id space, rather than let a hit wrap onto another geometry.
    Offset(u32),
}

/// Accumulates geometries, then builds the acceleration structure once in
/// [`SceneBuilder::commit`] (Embree's `rtcCommitScene`).
#[derive(Default)]
pub struct SceneBuilder {
    geoms: Vec<(Geometry, RayMask, InstanceHitId)>,
}

impl SceneBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches a geometry visible to every ray category; returns its
    /// `geom_id` (dense, starting at 0 — usable as a table index).
    pub fn attach(&mut self, geometry: Geometry) -> u32 {
        self.attach_masked(geometry, MASK_ALL)
    }

    /// Attaches a geometry visible only to ray categories in `mask`.
    pub fn attach_masked(&mut self, geometry: Geometry, mask: RayMask) -> u32 {
        self.attach_labelled(geometry, mask, InstanceHitId::Own)
    }

    /// Attaches an instance whose hits report `label` rather than the
    /// instance's own id (see [`InstanceHitId`]). Returns the instance's own
    /// `geom_id` all the same: it still occupies a slot.
    ///
    /// # Panics
    /// If `label` is not [`InstanceHitId::Own`] and `geometry` is not an
    /// instance — only an instance has inner hits to relabel.
    pub fn attach_labelled(
        &mut self,
        geometry: Geometry,
        mask: RayMask,
        label: InstanceHitId,
    ) -> u32 {
        assert!(
            label == InstanceHitId::Own || matches!(geometry, Geometry::Instance { .. }),
            "only an instance can relabel its hits"
        );
        self.geoms.push((geometry, mask, label));
        (self.geoms.len() - 1) as u32
    }

    /// Reserves capacity for `additional` more geometries. Purely a
    /// performance/memory hint — callers that know an upcoming batch size
    /// (a `PointInstancer` with N placements, say) should use it: without
    /// it, growing a multi-million-entry `Vec` by repeated doubling both
    /// re-copies everything so far at each doubling and can leave up to
    /// ~2x the final size over-allocated.
    pub fn reserve(&mut self, additional: usize) {
        self.geoms.reserve(additional);
    }

    /// Number of geometries attached so far.
    pub fn count(&self) -> usize {
        self.geoms.len()
    }

    /// Replaces the geometry already attached at `id`, keeping its mask.
    ///
    /// For callers that must claim a `geom_id` before they can decide what
    /// geometry belongs in it. The importer needs this: whether a mesh is
    /// better placed by an instance or baked into world-space triangles
    /// depends on how many times it turns out to be placed, which is not
    /// known until the whole stage has been walked — but `geom_id`s are
    /// handed out in traversal order and are the key the host's material
    /// table is indexed by, so they cannot be assigned later.
    ///
    /// Attach a placeholder, keep the id, and fill it in here once the
    /// decision is made. Only valid before [`SceneBuilder::commit`], which is
    /// enforced by taking `&mut self`.
    ///
    /// # Panics
    /// If `id` was never attached.
    pub fn set_geometry(&mut self, id: u32, geometry: Geometry) {
        self.geoms[id as usize].0 = geometry;
    }

    /// The ray mask the geometry at `id` was attached with.
    ///
    /// # Panics
    /// If `id` was never attached.
    pub fn mask(&self, id: u32) -> RayMask {
        self.geoms[id as usize].1
    }

    /// Replaces the ray mask of the geometry already attached at `id`,
    /// keeping the geometry. The same deferred-decision escape hatch as
    /// [`SceneBuilder::set_geometry`], for a visibility that is only known
    /// once the whole input has been read. Only valid before
    /// [`SceneBuilder::commit`].
    ///
    /// # Panics
    /// If `id` was never attached.
    pub fn set_mask(&mut self, id: u32, mask: RayMask) {
        self.geoms[id as usize].1 = mask;
    }

    /// A geometry that expands to no primitives — the placeholder to pair
    /// with [`SceneBuilder::set_geometry`]. Allocates nothing.
    pub fn empty_geometry() -> Geometry {
        Geometry::TriangleMesh {
            vertices: Vec::new(),
            indices: Vec::new(),
            normals: None,
        }
    }

    /// How many primitives a geometry expands into. An upper bound: the
    /// expansion skips degenerate entries (out-of-range indices, an empty
    /// instanced scene), so the real count can be lower.
    fn prim_upper_bound(geom: &Geometry) -> usize {
        match geom {
            Geometry::TriangleMesh { indices, .. } => indices.len(),
            Geometry::RoundCurves { segments } => segments.len(),
            Geometry::CubicCurves { segments } => segments.len(),
            Geometry::Sphere { .. }
            | Geometry::Disk { .. }
            | Geometry::Cylinder { .. }
            | Geometry::Instance { .. } => 1,
        }
    }

    /// Expands every geometry into primitives and builds the BVH with the
    /// default [`CommitOptions`].
    #[must_use = "the committed scene is the only way to intersect it"]
    pub fn commit(self) -> Scene {
        self.commit_with(CommitOptions::default())
    }

    /// [`SceneBuilder::commit`] with the build choices made by the caller.
    ///
    /// Three phases: size every primitive array from the attached
    /// geometries (`Expansion::sized_for`), expand each geometry into
    /// primitives in attach order (`Expansion::push_geometry`), then build
    /// the BVH over them in the chosen layout.
    #[must_use = "the committed scene is the only way to intersect it"]
    pub fn commit_with(self, options: CommitOptions) -> Scene {
        let n_geoms = self.geoms.len() as u32;
        let mut expansion = Expansion::sized_for(&self.geoms, n_geoms);
        for (geom_id, (geom, mask, label)) in self.geoms.into_iter().enumerate() {
            expansion.push_geometry(geom_id as u32, geom, mask, label);
        }
        let Expansion {
            input,
            has_motion,
            max_hit_id,
        } = expansion;
        let layout = match options.layout {
            PacketLayout::Gathered | PacketLayout::Auto => crate::bvh::Layout::Gathered,
            PacketLayout::Indexed => crate::bvh::Layout::Indexed,
        };
        Scene {
            bvh: Bvh::new(input, layout, options.packet_sah),
            n_geoms,
            has_motion,
            max_hit_id,
        }
    }
}

/// The primitives [`SceneBuilder::commit_with`] expands the attached
/// geometries into, and what it learns about the scene on the way.
struct Expansion {
    input: Primitives,
    /// Whether anything placed so far moves (see [`Scene::has_motion`]).
    has_motion: bool,
    /// Largest id a hit in this scene can report. Every geometry can
    /// report its own id; labels can report more (see
    /// [`Expansion::instance_hit_id`]).
    max_hit_id: u32,
}

impl Expansion {
    /// Sizes every primitive array exactly once, from the total the
    /// geometries will expand into, instead of letting per-geometry
    /// `reserve` calls grow them by doubling. With many geometries that
    /// doubling leaves up to ~2x over-allocated, and `MemoryFootprint`
    /// counts capacity, not length: when every primitive was one 128-byte
    /// node, a scene of a few hundred thousand baked triangles wasted tens
    /// of MiB of genuinely committed memory. The arrays are split by kind
    /// now (24-byte triangle records, 64-byte `PrimNode`s, 96-byte
    /// instances and cubic spans), each sized here. The bound can only
    /// over-shoot by the number of degenerate primitives the expansion
    /// skips, normally zero.
    fn sized_for(geoms: &[(Geometry, RayMask, InstanceHitId)], n_geoms: u32) -> Self {
        let total: usize = geoms
            .iter()
            .map(|(g, _, _)| SceneBuilder::prim_upper_bound(g))
            .sum();
        let n_tris: usize = geoms
            .iter()
            .map(|(g, _, _)| match g {
                Geometry::TriangleMesh { indices, .. } => indices.len(),
                _ => 0,
            })
            .sum();
        let n_verts: usize = geoms
            .iter()
            .map(|(g, _, _)| match g {
                Geometry::TriangleMesh { vertices, .. } => vertices.len(),
                _ => 0,
            })
            .sum();
        assert!(
            n_verts < u32::MAX as usize,
            "{n_verts} vertices in one scene: the shared vertex table is indexed by u32"
        );
        // Each non-triangle kind has its own array; size each exactly.
        let (mut n_inst, mut n_cubic) = (0usize, 0usize);
        for (g, _, _) in geoms {
            match g {
                Geometry::Instance { .. } => n_inst += 1,
                Geometry::CubicCurves { segments } => n_cubic += segments.len(),
                _ => {}
            }
        }
        let n_nodes = total - n_tris - n_inst - n_cubic;
        let input = Primitives {
            tris: Vec::with_capacity(n_tris),
            vertices: Vec::with_capacity(n_verts),
            normals: Vec::new(),
            geoms: Vec::with_capacity(geoms.len()),
            prims: Vec::with_capacity(n_nodes),
            instances: Vec::with_capacity(n_inst),
            instance_bounds: Vec::with_capacity(n_inst),
            cubics: Vec::with_capacity(n_cubic),
            order: Vec::with_capacity(total - n_tris),
        };
        Expansion {
            input,
            has_motion: false,
            max_hit_id: n_geoms.saturating_sub(1),
        }
    }

    /// Expands geometry `geom_id` into its primitives, then records its
    /// [`GeomTable`]. A degenerate or non-finite disk or cylinder, and an
    /// instance of an empty scene, expand to nothing and return early.
    fn push_geometry(&mut self, geom_id: u32, geom: Geometry, mask: RayMask, label: InstanceHitId) {
        let input = &mut self.input;
        let mut table = GeomTable {
            vertex_base: input.vertices.len() as u32,
            tri_base: input.tris.len() as u32,
            ..GeomTable::default()
        };
        match geom {
            Geometry::TriangleMesh {
                vertices,
                indices,
                normals,
            } => {
                // Normals are per vertex, parallel to the vertex run, and
                // only when the array covers every vertex — a short one is
                // ignored whole, as the per-triangle check it replaces
                // ignored each triangle it fell short of.
                let n = vertices.len();
                let vertex_base = table.vertex_base;
                input.vertices.extend(vertices);
                if let Some(mut ns) = normals.filter(|ns| ns.len() >= n) {
                    table.normal_base = input.normals.len() as u32;
                    ns.truncate(n);
                    input.normals.extend(ns);
                }
                for (prim_id, [i0, i1, i2]) in indices.into_iter().enumerate() {
                    let in_range = (i0 as usize) < n && (i1 as usize) < n && (i2 as usize) < n;
                    let v = if in_range {
                        [vertex_base + i0, vertex_base + i1, vertex_base + i2]
                    } else {
                        [DEGENERATE_VERTEX; 3]
                    };
                    input.tris.push(TriangleRecord {
                        v,
                        geom_id,
                        prim_id: prim_id as u32,
                        mask: if in_range { mask } else { RayMask::NONE },
                    });
                }
            }
            Geometry::Sphere { center, radius } => {
                input.push_node(PrimNode::Sphere(SpherePrim {
                    center,
                    radius,
                    geom_id,
                    mask,
                }));
            }
            Geometry::Disk {
                center,
                normal,
                radius,
            } => {
                // Degenerate or non-finite: no front, no extent, or a
                // position that would poison the bounds.
                if !center.is_finite()
                    || !normal.is_finite()
                    || normal.length_squared() == 0.0
                    || !radius.is_finite()
                    || radius <= 0.0
                {
                    return;
                }
                input.push_node(PrimNode::Disk(DiskPrim {
                    center,
                    normal: normal.normalize(),
                    radius,
                    geom_id,
                    mask,
                }));
            }
            Geometry::Cylinder { p0, p1, radius } => {
                let length = (p1 - p0).length();
                if !p0.is_finite()
                    || !p1.is_finite()
                    || !length.is_finite()
                    || length <= 0.0
                    || !radius.is_finite()
                    || radius <= 0.0
                {
                    return;
                }
                input.push_node(PrimNode::Cylinder(CylinderPrim {
                    p0,
                    axis: (p1 - p0) / length,
                    length,
                    radius,
                    geom_id,
                    mask,
                }));
            }
            Geometry::RoundCurves { segments } => {
                for (prim_id, s) in segments.into_iter().enumerate() {
                    input.push_node(PrimNode::Curve(CurvePrim {
                        p0: s.p0.to_array(),
                        p1: s.p1.to_array(),
                        r0: s.r0,
                        r1: s.r1,
                        geom_id,
                        prim_id: prim_id as u32,
                        mask,
                    }));
                }
            }
            Geometry::CubicCurves { segments } => {
                for (prim_id, s) in segments.into_iter().enumerate() {
                    input.push_cubic(CubicCurvePrim {
                        cp: s.cp,
                        r0: s.r0,
                        r1: s.r1,
                        geom_id,
                        prim_id: prim_id as u32,
                        mask,
                    });
                }
            }
            Geometry::Instance {
                scene,
                transform,
                transform_end,
            } => {
                let Some(inner_bounds) = scene.bounds() else {
                    return; // empty instanced scene
                };
                let w2l = transform.inverse();
                let bounds = match &transform_end {
                    Some(end) => AABB::surrounding_box(
                        transformed_aabb(&inner_bounds, &transform),
                        transformed_aabb(&inner_bounds, end),
                    ),
                    None => transformed_aabb(&inner_bounds, &transform),
                };
                // A scene moves if this placement is blurred, or if the
                // thing being placed already moves. The inner scene carries
                // its own committed flag, so this stays O(1) per instance
                // however deeply they nest.
                self.has_motion |= transform_end.is_some() || scene.has_motion();
                let (geom_id, id_offset) = self.instance_hit_id(geom_id, label, &scene);
                self.input.push_instance(
                    InstancePrim {
                        scene,
                        w2l,
                        motion: transform_end.map(|end| {
                            Box::new(InstanceMotion {
                                l2w: transform,
                                l2w_end: *end,
                            })
                        }),
                        geom_id,
                        id_offset,
                        mask,
                    },
                    bounds,
                );
            }
        }
        self.input.geoms.push(table);
    }

    /// The `(geom_id, id_offset)` an instance attached at `geom_id` with
    /// `label` stores for its hits, widening `max_hit_id` to the largest id
    /// those hits can report.
    fn instance_hit_id(&mut self, geom_id: u32, label: InstanceHitId, scene: &Scene) -> (u32, u32) {
        match label {
            InstanceHitId::Own => (geom_id, NO_ID_OFFSET),
            InstanceHitId::As(id) => {
                assert!(id != crate::INVALID_ID, "hit id {id} is reserved");
                self.max_hit_id = self.max_hit_id.max(id);
                (id, NO_ID_OFFSET)
            }
            InstanceHitId::Offset(base) => {
                // Checked here, once per instance, so the hit path can add
                // without a branch: an offset that could carry an inner id
                // past the id space would otherwise wrap onto an unrelated
                // geometry, and a host would shade it with that geometry's
                // material.
                let top = base
                    .checked_add(scene.max_hit_id)
                    .filter(|&top| top != crate::INVALID_ID)
                    .unwrap_or_else(|| {
                        panic!(
                            "id offset {base} + inner ids up to {} overflows the geom_id space",
                            scene.max_hit_id
                        )
                    });
                self.max_hit_id = self.max_hit_id.max(top);
                (geom_id, base)
            }
        }
    }
}

/// A committed, immutable scene. Queries are `&self` and thread-safe.
pub struct Scene {
    bvh: Bvh,
    n_geoms: u32,
    has_motion: bool,
    /// Largest `geom_id` a hit in this scene can report — a bound, computed
    /// at commit, that lets an `Offset` label placing this scene be checked
    /// once instead of on every hit.
    max_hit_id: u32,
}

impl Scene {
    /// Closest hit in `(t_min, t_max)`, or `None` (Embree's
    /// `rtcIntersect1`).
    #[must_use]
    pub fn intersect(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<RayHit> {
        let hit = self.bvh.hit(ray, t_min, t_max)?;
        let front_face = ray.dir.dot(hit.outward) < 0.0;
        Some(RayHit {
            t: hit.t,
            normal: if front_face {
                hit.outward
            } else {
                -hit.outward
            },
            front_face,
            u: hit.u,
            v: hit.v,
            geom_id: hit.geom_id,
            prim_id: hit.prim_id,
        })
    }

    /// Does the ray hit *anything* in `(t_min, t_max)`? Early-exit
    /// traversal — the shadow-ray fast path (Embree's `rtcOccluded1`).
    #[must_use]
    pub fn occluded(&self, ray: &Ray, t_min: f32, t_max: f32) -> bool {
        self.bvh.hit_any(ray, t_min, t_max)
    }

    /// What the top-level instances `ids` are: `(geom_id, world bounds,
    /// inner top-level primitive count, how many top-level instances share
    /// the same inner scene)`. Diagnostic for a top level that will not
    /// cull, paired with [`crate::traversal_stats::top_level_descents`];
    /// a linear scan, so ask once.
    #[cfg(feature = "traversal-stats")]
    pub fn describe_instances(
        &self,
        ids: &std::collections::HashSet<u32>,
    ) -> Vec<(u32, AABB, usize, usize)> {
        let mut sharing = std::collections::HashMap::<*const Scene, usize>::new();
        let mut found = Vec::new();
        for inst in self.bvh.instances() {
            *sharing.entry(Arc::as_ptr(&inst.scene)).or_insert(0) += 1;
            if ids.contains(&inst.geom_id) {
                let bounds = inst
                    .approx_world_bounds()
                    .unwrap_or(AABB::new(Vec3A::ZERO, Vec3A::ZERO));
                found.push((
                    inst.geom_id,
                    bounds,
                    Arc::as_ptr(&inst.scene),
                    inst.scene.primitive_count(),
                ));
            }
        }
        found
            .into_iter()
            .map(|(id, b, ptr, n)| (id, b, n, sharing[&ptr]))
            .collect()
    }

    /// World bounds of everything in the scene; `None` when empty.
    pub fn bounds(&self) -> Option<AABB> {
        self.bvh.bounds()
    }

    /// Number of attached geometries (`geom_id`s are `0..count`).
    pub fn geometry_count(&self) -> u32 {
        self.n_geoms
    }

    /// Does anything in this scene move over the shutter interval — i.e. can
    /// `ray.time` change what a query returns?
    ///
    /// Transform motion blur is the only thing that reads `ray.time`
    /// (`InstancePrim::transforms_at`), so when this is `false` the shutter
    /// coordinate is unobservable and a host need not sample it. That is
    /// worth asking about: drawing one costs a full 4-dimensional
    /// quasi-random sample, which was 4.2% of the render on a static scene.
    ///
    /// True if any instance carries an end-of-shutter transform, at any depth
    /// of nesting (each level folds in its inner scene's answer at commit).
    pub fn has_motion(&self) -> bool {
        self.has_motion
    }

    /// Number of primitives the geometries expanded into.
    pub fn primitive_count(&self) -> usize {
        self.bvh.prim_count()
    }

    /// Top-level primitives split by kind, for reporting. Instances count
    /// as one primitive each and are *not* descended into — the instanced
    /// scene's own contents are its own `Scene`'s business, and a
    /// prototype shared by a thousand placements would otherwise be
    /// counted a thousand times.
    pub fn primitive_breakdown(&self) -> PrimitiveBreakdown {
        self.bvh.primitive_breakdown()
    }

    /// Primitives actually resident in memory: like
    /// [`Scene::primitive_breakdown`], but descending into instanced
    /// scenes, counting each distinct prototype **once** however many
    /// placements reference it.
    ///
    /// This is the count that tracks memory. `primitive_breakdown` says
    /// what the top-level BVH traverses; this says what is stored. For an
    /// instance-heavy scene the two differ enormously, and the gap is the
    /// whole benefit of instancing.
    pub fn unique_primitive_breakdown(&self) -> PrimitiveBreakdown {
        let mut visited = std::collections::HashSet::new();
        let mut acc = PrimitiveBreakdown::default();
        self.accumulate_unique_into(&mut visited, &mut acc);
        acc
    }

    pub(crate) fn accumulate_unique_into(
        &self,
        visited: &mut std::collections::HashSet<usize>,
        acc: &mut PrimitiveBreakdown,
    ) {
        self.bvh.accumulate_unique(visited, acc);
    }

    /// How big this scene's top-level primitives are relative to the
    /// scene itself: `(count, scene diagonal, mean prim diagonal, max prim
    /// diagonal)`.
    ///
    /// Diagnostic for a BVH that will not cull. A hierarchy can only
    /// separate primitives whose bounds are small against the whole; when
    /// the mean ratio approaches 1 every box covers everything, no split
    /// can divide them, and traversal degenerates to a linear scan however
    /// good the builder is.
    pub fn primitive_extents(&self) -> (usize, f32, f32, f32) {
        let scene_diag = self
            .bvh
            .bounds()
            .map(|b| (b.maximum - b.minimum).length())
            .unwrap_or(0.0);
        let (n, sum, max) = self.bvh.primitive_extent_sum();
        let mean = if n == 0 { 0.0 } else { sum / n as f32 };
        (n, scene_diag, mean, max)
    }

    /// The three vertices of triangle `prim_id` of the triangle mesh
    /// `geom_id`, in this scene's own space — local space for a scene that
    /// is placed through instances. `None` when `geom_id` is not a
    /// triangle mesh of this scene, `prim_id` is past its triangles, or
    /// the triangle's attached indices were out of range.
    ///
    /// What lets an application derive per-hit quantities (a tangent
    /// frame, a texture density) from the geometry it attached instead of
    /// storing them per triangle.
    pub fn triangle_vertices(&self, geom_id: u32, prim_id: u32) -> Option<[Vec3A; 3]> {
        self.bvh.triangle_vertices(geom_id, prim_id)
    }

    /// Exact resident bytes of this scene and every distinct scene it
    /// instances — see [`MemoryFootprint`].
    pub fn memory_footprint(&self) -> MemoryFootprint {
        let mut visited = std::collections::HashSet::new();
        let mut acc = MemoryFootprint::default();
        self.accumulate_footprint_into(&mut visited, &mut acc);
        acc
    }

    pub(crate) fn accumulate_footprint_into(
        &self,
        visited: &mut std::collections::HashSet<usize>,
        acc: &mut MemoryFootprint,
    ) {
        self.bvh.accumulate_footprint(visited, acc);
    }

    /// Internal closest-hit that keeps the *outward* (unoriented) normal,
    /// so instance transforms can map it without re-deriving orientation.
    pub(crate) fn intersect_outward(&self, ray: &Ray, t_min: f32, t_max: f32) -> Option<PrimHit> {
        self.bvh.hit(ray, t_min, t_max)
    }
}

#[cfg(test)]
mod tests;
