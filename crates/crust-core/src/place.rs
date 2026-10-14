//! Placement against the geometry a scene renders: dropping prims onto the
//! surface below them, and scattering instances over a surface by area.
//!
//! Both read the committed [`World`] through the kernel's world-space
//! visitor ([`World::visit`]) and its geometric hit
//! ([`World::intersect_geometry`]), and the [`Provenance`]'s prim → geometry
//! runs; they keep nothing per triangle once they return, read no USD and
//! author nothing. A host (the MCP session) turns their numbers into
//! opinions. Only geometry the camera sees counts, at shutter open.

use std::ops::Range;

use glam::{DMat4, DQuat, DVec3, Vec3A};
use rayon::prelude::*;

use crate::PathSampler;
use crate::distribution::Distribution1D;
use crate::live::Provenance;
use crate::rt_world::World;
use crust_rt::{Piece, Ray, RayMask};

/// The most instances one scatter authors.
pub const MAX_INSTANCES: u64 = 100_000;

/// The stage's up axis (`upAxis`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpAxis {
    /// USD's fallback.
    #[default]
    Y,
    Z,
}

impl UpAxis {
    pub fn vector(self) -> Vec3A {
        match self {
            UpAxis::Y => Vec3A::Y,
            UpAxis::Z => Vec3A::Z,
        }
    }
}

/// Geometry rays and visits count: what the camera sees.
const MASK: RayMask = RayMask::CAMERA;

/// Hits a ray skips (the dropped prim's own, or not the target's) before it
/// gives up: past this many, the column counts as a miss.
const MAX_SKIPS: usize = 1024;

/// Side of the grid of columns a non-triangle primitive drops on.
const GRID: usize = 9;

/// Id ranges, sorted and merged, asked "does it hold this id?".
#[derive(Clone, Debug, Default)]
struct IdSet(Vec<Range<u32>>);

impl IdSet {
    fn new(mut ranges: Vec<Range<u32>>) -> Self {
        ranges.retain(|r| !r.is_empty());
        ranges.sort_by_key(|r| r.start);
        let mut merged: Vec<Range<u32>> = Vec::with_capacity(ranges.len());
        for r in ranges {
            match merged.last_mut() {
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => merged.push(r),
            }
        }
        IdSet(merged)
    }

    fn of(prov: &Provenance, paths: &[String]) -> Self {
        IdSet::new(paths.iter().flat_map(|p| prov.geometry_of(p)).collect())
    }

    fn contains(&self, id: u32) -> bool {
        let i = self.0.partition_point(|r| r.end <= id);
        self.0.get(i).is_some_and(|r| r.contains(&id))
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn ranges(&self) -> &[Range<u32>] {
        &self.0
    }
}

/// What a ray found: the hit point and the surface's geometric normal
/// there, turned against the ray.
#[derive(Clone, Copy, Debug)]
pub struct Found {
    pub point: Vec3A,
    pub normal: Vec3A,
}

/// The first hit from `origin` along `dir` (unit) whose geometry `accept`
/// takes, on the camera-visible geometry at shutter open, skipping the rest.
///
/// A skipped hit is stepped past by restarting the ray at it, never by
/// raising `t_min`: every intersection in the program asks the kernel for
/// `(TRACE_T_MIN, ∞)`, LLVM propagates those constants into it, and one
/// caller passing a variable costs every ray of every render (see
/// [`crate::TRACE_T_MIN`]). A surface within `TRACE_T_MIN` behind a skipped
/// one is skipped with it.
pub fn first_hit(
    world: &World,
    origin: Vec3A,
    dir: Vec3A,
    accept: &(dyn Fn(u32) -> bool + Sync),
) -> Option<Found> {
    let mut from = origin;
    for _ in 0..MAX_SKIPS {
        let ray = Ray::new(from, dir).with_mask(MASK).with_time(0.0);
        let hit = world.intersect_geometry(&ray)?;
        let point = from + dir * hit.t;
        if accept(hit.geom_id) {
            return Some(Found {
                point,
                normal: hit.normal,
            });
        }
        from = point;
    }
    None
}

/// How a prim was dropped, or why it was not.
#[derive(Clone, Debug, PartialEq)]
pub enum Dropped {
    /// Moved by `delta` (world space, applied after the prim's current
    /// world transform), so its lowest column rests at `contact`.
    Placed {
        delta: DMat4,
        contact: DVec3,
        /// The surface normal the prim was turned to, when aligned.
        normal: Option<DVec3>,
    },
    /// The scene holds no geometry the camera sees under this prim.
    NoGeometry,
    /// Nothing (of the target) lies beneath it.
    NothingBelow,
}

/// Drops each of `prims` along the stage's down axis until its geometry
/// rests on the highest surface beneath it: every other geometry of the
/// scene, or only `target`'s and its descendants' when given. With `align`,
/// first turns the prim so its up axis follows the normal of the surface
/// below its footprint's centre, about the bottom of that footprint. Each
/// prim is placed against the scene as it is, never against another prim's
/// new position.
pub fn drop_prims(
    world: &World,
    prov: &Provenance,
    prims: &[String],
    target: Option<&[String]>,
    up: UpAxis,
    align: bool,
) -> Vec<Dropped> {
    let target = target.map(|t| IdSet::of(prov, t));
    prims
        .iter()
        .map(|prim| {
            let own = IdSet::of(prov, std::slice::from_ref(prim));
            drop_one(world, &own, target.as_ref(), up, align)
        })
        .collect()
}

fn drop_one(
    world: &World,
    own: &IdSet,
    target: Option<&IdSet>,
    up: UpAxis,
    align: bool,
) -> Dropped {
    let up_v = up.vector();
    let down = -up_v;
    // The prim's columns: every vertex of its triangles, and a grid on the
    // bottom face of every other primitive's bounds.
    let mut points = Vec::new();
    for ids in own.ranges() {
        world.visit(ids.clone(), MASK, |_, piece| match piece {
            Piece::Triangle(vs) => points.extend(vs),
            Piece::Bounds(b) => points.extend(bottom_grid(b.minimum, b.maximum, up_v)),
        });
    }
    if points.is_empty() {
        return Dropped::NoGeometry;
    }
    // A mesh vertex is a corner of about six triangles: one ray each.
    points.sort_unstable_by_key(|p| p.to_array().map(f32::to_bits));
    points.dedup_by_key(|p| p.to_array().map(f32::to_bits));
    let accept = |id: u32| !own.contains(id) && target.is_none_or(|t| t.contains(id));

    let (lo, hi) = extent(&points);
    let mut turn = DMat4::IDENTITY;
    let mut normal = None;
    if align {
        // The footprint's centre, across the up axis; the pivot is there at
        // the prim's bottom, and the ray for the normal starts there at its
        // top.
        let centre = (lo + hi) * 0.5;
        let across = centre - up_v * up_v.dot(centre);
        let pivot = across + up_v * up_v.dot(lo);
        let top = across + up_v * (up_v.dot(hi) + lift(lo, hi));
        let Some(below) = first_hit(world, top, down, &accept) else {
            return Dropped::NothingBelow;
        };
        let n = below.normal.normalize();
        let pivot = pivot.as_dvec3();
        turn = DMat4::from_translation(pivot)
            * DMat4::from_quat(DQuat::from_rotation_arc(up_v.as_dvec3(), n.as_dvec3()))
            * DMat4::from_translation(-pivot);
        for p in &mut points {
            *p = turn.transform_point3(p.as_dvec3()).as_vec3().into();
        }
        normal = Some(n.as_dvec3());
    }

    // One ray per column, down from the height of the prim's top: its drop
    // is the column's height above the surface it meets.
    let (lo, hi) = extent(&points);
    let top = up_v.dot(hi) + lift(lo, hi);
    let best = points
        .par_iter()
        .enumerate()
        .filter_map(|(i, &p)| {
            let origin = p + up_v * (top - up_v.dot(p));
            let below = first_hit(world, origin, down, &accept)?;
            let d = up_v.dot(p) as f64 - up_v.dot(below.point) as f64;
            Some((d, i, below))
        })
        // Least drop, ties to the first column: the same answer on any
        // number of threads.
        .min_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    let Some((d, _, below)) = best else {
        return Dropped::NothingBelow;
    };
    Dropped::Placed {
        delta: DMat4::from_translation(-up_v.as_dvec3() * d) * turn,
        contact: below.point.as_dvec3(),
        normal,
    }
}

/// How far above a prim's top its rays start: clear of the top vertex's own
/// rounding, in proportion to the prim's size.
fn lift(lo: Vec3A, hi: Vec3A) -> f32 {
    1e-4 * (hi - lo).max_element().max(1.0)
}

fn extent(points: &[Vec3A]) -> (Vec3A, Vec3A) {
    points.iter().fold(
        (Vec3A::splat(f32::INFINITY), Vec3A::splat(f32::NEG_INFINITY)),
        |(lo, hi), &p| (lo.min(p), hi.max(p)),
    )
}

/// A `GRID` × `GRID` grid of points over the face of the box `lo..hi` that
/// faces down.
fn bottom_grid(lo: Vec3A, hi: Vec3A, up: Vec3A) -> impl Iterator<Item = Vec3A> {
    // The two axes across `up`, as indices.
    let (a, b) = if up.y != 0.0 { (0, 2) } else { (0, 1) };
    let low = up.dot(lo);
    (0..GRID * GRID).map(move |k| {
        let (i, j) = ((k % GRID) as f32, (k / GRID) as f32);
        let step = (GRID - 1) as f32;
        let mut p = up * low;
        p[a] = lo[a] + (hi[a] - lo[a]) * i / step;
        p[b] = lo[b] + (hi[b] - lo[b]) * j / step;
        p
    })
}

/// How many instances a scatter places.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Amount {
    Count(u64),
    /// Instances per square stage unit of eligible surface.
    Density(f64),
}

/// What a scatter is asked for.
#[derive(Clone, Debug, PartialEq)]
pub struct ScatterRequest {
    pub target: Vec<String>,
    pub amount: Amount,
    pub seed: u32,
    /// Only triangles whose geometric normal is within this many degrees of
    /// up are eligible.
    pub max_slope_deg: Option<f64>,
    pub random_rotation: bool,
    /// The range each instance's uniform scale is drawn from.
    pub scale: Option<[f64; 2]>,
    /// How many prototypes the instances choose from (equally likely).
    pub prototypes: u32,
    pub up: UpAxis,
}

/// One scattered instance, in world space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Instance {
    /// Where the prototype's origin goes, on the surface.
    pub position: DVec3,
    /// The turn about the up axis, in radians.
    pub angle: f64,
    pub scale: f64,
    pub prototype: u32,
}

/// A scatter's instances and what it covered.
#[derive(Clone, Debug, PartialEq)]
pub struct Scattered {
    pub instances: Vec<Instance>,
    /// The eligible triangles' area, in square stage units.
    pub area: f64,
    /// Prims of the target whose geometry is not triangles (curves, implicit
    /// shapes), skipped.
    pub skipped: Vec<String>,
}

/// The sampler domain a scatter's instance draws its point and prototype
/// from, and the one it draws its turn and scale from: keys under a root of
/// `PathSampler::new(0, 0, seed, index)`, which nothing else draws from.
const K_SCATTER: i32 = 0;
const K_SCATTER_SHAPE: i32 = 1;

/// Scatters instances over `request.target`'s triangles, distributed in
/// proportion to area. Instance `i` is a function of the seed, `i` and the
/// eligible triangles alone (in the fixed order [`World::visit`] gives them),
/// whatever the thread count or the render's sampler setting.
pub fn scatter(
    world: &World,
    prov: &Provenance,
    request: &ScatterRequest,
) -> Result<Scattered, String> {
    let ids = IdSet::of(prov, &request.target);
    if ids.is_empty() {
        return Err(format!(
            "{} has no geometry in the scene",
            request.target.join(", ")
        ));
    }
    let up = request.up.vector();
    let min_cos = request
        .max_slope_deg
        .map(|deg| deg.to_radians().cos() as f32);
    let mut triangles = Vec::new();
    let mut areas = Vec::new();
    let mut skipped_ids = Vec::new();
    for range in ids.ranges() {
        world.visit(range.clone(), MASK, |id, piece| match piece {
            Piece::Triangle([a, b, c]) => {
                let n = (b - a).cross(c - a);
                let len = n.length();
                if len <= 0.0 || !len.is_finite() {
                    return;
                }
                if min_cos.is_some_and(|m| n.dot(up) / len < m) {
                    return;
                }
                triangles.push([a, b, c]);
                areas.push(0.5 * len);
            }
            Piece::Bounds(_) => skipped_ids.push(id),
        });
    }
    let mut skipped: Vec<String> = skipped_ids
        .into_iter()
        .filter_map(|id| prov.prim_of_geometry(id).map(str::to_string))
        .collect();
    skipped.sort();
    skipped.dedup();
    let area: f64 = areas.iter().map(|&a| a as f64).sum();
    if triangles.is_empty() {
        return Err(format!(
            "{} has no eligible triangle{}",
            request.target.join(", "),
            if request.max_slope_deg.is_some() {
                " within max_slope"
            } else {
                ""
            }
        ));
    }
    let count = match request.amount {
        Amount::Count(n) => n,
        Amount::Density(d) => (d * area).round() as u64,
    };
    if count == 0 {
        return Err(format!(
            "no instance to place: the eligible area is {area} square units"
        ));
    }
    if count > MAX_INSTANCES {
        return Err(format!(
            "{count} instances asked for; a scatter places at most {MAX_INSTANCES}"
        ));
    }
    let cdf = Distribution1D::new(areas);
    let protos = request.prototypes.max(1);
    let instances = (0..count as i32)
        .into_par_iter()
        .map(|i| {
            let root = PathSampler::new(0, 0, request.seed as i32, i);
            let [u_tri, u0, u1, u_proto] = root.new_domain(K_SCATTER).draw_sample_f32::<4>();
            let [u_angle, u_scale] = root.new_domain(K_SCATTER_SHAPE).draw_sample_f32::<2>();
            let (_, _, bin) = cdf.sample(u_tri);
            let [a, b, c] = triangles[bin];
            let [w0, w1, w2] = utils::uniform_triangle([u0, u1]);
            let p = a.as_dvec3() * w0 as f64 + b.as_dvec3() * w1 as f64 + c.as_dvec3() * w2 as f64;
            Instance {
                position: p,
                angle: if request.random_rotation {
                    std::f64::consts::TAU * u_angle as f64
                } else {
                    0.0
                },
                scale: match request.scale {
                    Some([lo, hi]) => lo + (hi - lo) * u_scale as f64,
                    None => 1.0,
                },
                prototype: ((u_proto * protos as f32) as u32).min(protos - 1),
            }
        })
        .collect();
    Ok(Scattered {
        instances,
        area,
        skipped,
    })
}
