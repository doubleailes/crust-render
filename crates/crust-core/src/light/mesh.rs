//! [`MeshShape`]: a `MeshLightAPI` mesh's emitting surface — the world-space
//! triangles it renders with, sampled by area.

use glam::Vec3A;

use super::shape::{LightShape, ShapeExtent, ShapeHits};
use crate::ray::TRACE_T_MIN;

/// The triangles of one mesh-light placement, in world space, with the
/// cumulative distribution of their areas (lighting design record, "Mesh
/// lights").
///
/// Sampled uniformly by area: a triangle is picked by its share of the area,
/// then a point uniformly on it, so the area density is `1 / area()` at every
/// point of the mesh and [`LightShape::inv_pdf_area`] keeps its default. With
/// a uniform radiance, picking by area is picking by power. There is no
/// solid-angle strategy (Arvo's spherical triangles are a known gap), so
/// [`AreaLight`](super::AreaLight) converts the area density for both halves
/// of MIS, from the normal of the triangle the point lies on: the one NEE
/// sampled ([`LightShape::sample_point_on`]), or the one a bounce ray hit
/// (its `prim_id`). The triangles are in the kernel's own order, so the two
/// name one triangle by one index.
#[derive(Clone)]
pub struct MeshShape {
    /// Three vertices per triangle: 36 bytes a triangle.
    tris: Vec<[[f32; 3]; 3]>,
    /// `cdf[i]`: the share of the area in triangles `0..=i`, the last exactly
    /// 1. One `f32` a triangle.
    cdf: Vec<f32>,
    area: f32,
    min: Vec3A,
    max: Vec3A,
}

impl MeshShape {
    /// The mesh of world-space `tris`, in the order the kernel numbers them.
    /// `None` when they enclose no area (no triangle, or every one
    /// degenerate) or a non-finite one: nothing to sample.
    pub fn new(tris: Vec<[[f32; 3]; 3]>) -> Option<Self> {
        let mut cdf = Vec::with_capacity(tris.len());
        let mut sum = 0.0f64;
        let (mut min, mut max) = (Vec3A::INFINITY, Vec3A::NEG_INFINITY);
        for t in &tris {
            let [a, b, c] = t.map(Vec3A::from_array);
            // Summed in f64: a dense mesh adds millions of small areas.
            sum += 0.5 * (b - a).cross(c - a).length() as f64;
            cdf.push(sum);
            for p in [a, b, c] {
                min = min.min(p);
                max = max.max(p);
            }
        }
        let area = sum as f32;
        if !(area.is_finite() && area > 0.0) {
            return None;
        }
        let cdf = cdf
            .into_iter()
            .map(|c| (c / sum) as f32)
            .collect::<Vec<_>>();
        Some(MeshShape {
            tris,
            cdf,
            area,
            min,
            max,
        })
    }

    /// How many triangles the mesh holds.
    pub fn triangle_count(&self) -> usize {
        self.tris.len()
    }

    /// Triangle `prim`'s vertices.
    #[inline]
    fn triangle(&self, prim: u32) -> [Vec3A; 3] {
        let i = (prim as usize).min(self.tris.len() - 1);
        self.tris[i].map(Vec3A::from_array)
    }
}

impl LightShape for MeshShape {
    fn kind(&self) -> &'static str {
        "mesh"
    }

    fn sample_point(&self, u: f32, v: f32) -> Vec3A {
        self.sample_point_on(u, v).0
    }

    /// The triangle whose share of the area `u` falls in, then `u` rescaled
    /// to that share and `v` as the two numbers of a uniform point on it: one
    /// pair of numbers drives both choices.
    fn sample_point_on(&self, u: f32, v: f32) -> (Vec3A, u32) {
        let last = self.cdf.len() - 1;
        let i = self.cdf.partition_point(|&c| c <= u).min(last);
        let lo = if i == 0 { 0.0 } else { self.cdf[i - 1] };
        let hi = self.cdf[i];
        let w = hi - lo;
        let u = if w > 0.0 {
            ((u - lo) / w).clamp(0.0, 1.0)
        } else {
            0.5
        };
        // Uniform on the triangle (pbrt-v4's `SampleUniformTriangle`, the
        // square-root map).
        let su = u.sqrt();
        let (b0, b1) = (1.0 - su, v * su);
        let [a, b, c] = self.triangle(i as u32);
        (a * b0 + b * b1 + c * (1.0 - b0 - b1), i as u32)
    }

    /// Triangle `prim`'s geometric normal, by its winding: the same arithmetic
    /// for the triangle NEE sampled and the one a bounce ray hit. Zero for a
    /// degenerate triangle, which is then seen edge-on and refused by both
    /// halves of MIS.
    fn normal_at(&self, _p: Vec3A, prim: u32) -> Vec3A {
        let [a, b, c] = self.triangle(prim);
        (b - a).cross(c - a).normalize_or_zero()
    }

    fn area(&self) -> f32 {
        self.area
    }

    /// Every triangle the ray crosses, the nearest two kept: a scan, not a
    /// tree, so linear in the triangle count. Only a shadow-linked mesh
    /// light's bounce-side estimate asks (`LightKind::found_along`), once per
    /// bounce at a vertex that sees it; a mesh light is never a transparent
    /// emitter. Möller–Trumbore, two-sided like the emission.
    fn hits(&self, origin: Vec3A, dir: Vec3A) -> ShapeHits {
        let mut near = [(f32::INFINITY, 0u32); 2];
        for (i, _) in self.tris.iter().enumerate() {
            let [a, b, c] = self.triangle(i as u32);
            let (e1, e2) = (b - a, c - a);
            let p = dir.cross(e2);
            let det = e1.dot(p);
            if det == 0.0 || !det.is_finite() {
                continue;
            }
            let inv = 1.0 / det;
            let s = origin - a;
            let bu = s.dot(p) * inv;
            if !(0.0..=1.0).contains(&bu) {
                continue;
            }
            let q = s.cross(e1);
            let bv = dir.dot(q) * inv;
            if bv < 0.0 || bu + bv > 1.0 {
                continue;
            }
            let t = e2.dot(q) * inv;
            // Behind the near bound `ShapeHits` keeps, it must not crowd out
            // a hit ahead.
            if t.is_nan() || t <= TRACE_T_MIN {
                continue;
            }
            if t < near[0].0 {
                near = [(t, i as u32), near[0]];
            } else if t < near[1].0 {
                near[1] = (t, i as u32);
            }
        }
        let mut hits = ShapeHits::default();
        for (t, prim) in near {
            hits.push_on(t, prim);
        }
        hits
    }

    /// The triangles' box. No single normal, even for a flat card: a
    /// two-sided emitter is bounded as a sphere would be (per-triangle
    /// leaves are the known gap that tightens it).
    fn extent(&self) -> ShapeExtent {
        ShapeExtent {
            min: self.min,
            max: self.max,
            normal: None,
        }
    }
}
