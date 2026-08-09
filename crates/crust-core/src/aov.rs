//! Arbitrary output variables (AOVs) for hosts that need more than beauty —
//! a Hydra delegate binds depth for compositing and `[geom_id, prim_id]` for
//! viewport picking.
//!
//! Deliberately a **dedicated primary-hit probe pass**, not instrumentation
//! of the path tracer: one `World::intersect` per *pixel* through the pixel
//! center with the lens sample pinned to the lens center (so depth-of-field
//! cameras probe a pinhole view). That costs nothing in the integrator's hot
//! loop, and it is also the correct semantics for id/depth AOVs: they must
//! be point-sampled, never averaged across a reconstruction filter — half a
//! `geom_id` is meaningless.

use crate::tracer::Renderer;
use glam::Vec3A;
use rayon::prelude::*;

/// Which AOVs [`Renderer::render_aovs`] should fill in. Unrequested channels
/// stay `None` in [`AovBuffers`] and allocate nothing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AovRequest {
    /// Camera-forward hit distance (a host converts to its own depth
    /// convention with its projection matrix).
    pub depth: bool,
    /// Outward world-space geometric normal.
    pub normal: bool,
    /// `[geom_id, prim_id]` of the primary hit — what a Hydra delegate maps
    /// back to a prim path for picking.
    pub prim_id: bool,
    /// 1.0 where the probe hit geometry, 0.0 where it escaped.
    pub alpha: bool,
}

/// Per-pixel AOV planes. Same storage convention as [`crate::Buffer`]:
/// row-major with **row 0 at the bottom**, index `y * width + x`.
pub struct AovBuffers {
    pub width: usize,
    pub height: usize,
    /// `(hit_point − camera origin) · camera.forward()`; `+INFINITY` on miss.
    pub depth: Option<Vec<f32>>,
    /// Outward world normal (`front_face ? n : −n`); `Vec3A::ZERO` on miss.
    pub normal: Option<Vec<Vec3A>>,
    /// `[geom_id, prim_id]`; `[u32::MAX, u32::MAX]` on miss. Instanced hits
    /// report the instance's top-level `geom_id` with the inner `prim_id`,
    /// exactly as the kernel does.
    pub prim_id: Option<Vec<[u32; 2]>>,
    /// 1.0 hit / 0.0 miss.
    pub alpha: Option<Vec<f32>>,
}

/// What one probe ray learned about its pixel.
#[derive(Clone, Copy)]
struct Probe {
    depth: f32,
    normal: Vec3A,
    ids: [u32; 2],
    alpha: f32,
}

const MISS: Probe = Probe {
    depth: f32::INFINITY,
    normal: Vec3A::ZERO,
    ids: [u32::MAX, u32::MAX],
    alpha: 0.0,
};

impl Renderer {
    /// Renders the requested AOV planes with one primary-hit probe per
    /// pixel. Independent of any beauty render — call it before, after, or
    /// instead of `render()`.
    ///
    /// Probe rays go through the pixel center at shutter time 0 with the
    /// camera's `MASK_CAMERA` visibility (so geometry hidden from camera
    /// rays — light sources by default — is hidden here too, consistent with
    /// the beauty image).
    pub fn render_aovs(&self, req: AovRequest) -> AovBuffers {
        let width = self.settings.width();
        let height = self.settings.height();
        let forward = self.camera.forward();

        let probes: Vec<Probe> = (0..height)
            .into_par_iter()
            .flat_map_iter(|j| {
                (0..width).map(move |i| {
                    let u = (i as f32 + 0.5) / width as f32;
                    let v = (j as f32 + 0.5) / height as f32;
                    // [0.5, 0.5] maps to the center of the concentric lens
                    // disk, so a nonzero aperture still probes a pinhole.
                    let ray = self.camera.get_ray(u, v, [0.5, 0.5], 0.0);
                    match self.world.intersect(&ray, 0.001, f32::INFINITY) {
                        Some(hit) => {
                            let n = if hit.rec.front_face {
                                hit.rec.normal
                            } else {
                                -hit.rec.normal
                            };
                            Probe {
                                depth: (hit.rec.p - ray.origin()).dot(forward),
                                normal: n,
                                ids: [hit.geom_id, hit.prim_id],
                                alpha: 1.0,
                            }
                        }
                        None => MISS,
                    }
                })
            })
            .collect();

        AovBuffers {
            width,
            height,
            depth: req
                .depth
                .then(|| probes.iter().map(|p| p.depth).collect()),
            normal: req
                .normal
                .then(|| probes.iter().map(|p| p.normal).collect()),
            prim_id: req
                .prim_id
                .then(|| probes.iter().map(|p| p.ids).collect()),
            alpha: req
                .alpha
                .then(|| probes.iter().map(|p| p.alpha).collect()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::material::OpenPBR;
    use crate::rt_world::WorldBuilder;
    use crate::tracer::{RenderSettings, Renderer};
    use crate::{Camera, LightList, rt};
    use std::sync::Arc;

    /// One sphere dead ahead: the center pixel must report the hit (id,
    /// analytic depth, camera-facing normal), a corner pixel the miss
    /// sentinels — and unrequested channels must stay unallocated.
    #[test]
    fn probe_reports_hits_and_misses() {
        let mut builder = WorldBuilder::new();
        let geom_id = builder.attach(
            rt::Geometry::Sphere {
                center: glam::Vec3A::ZERO,
                radius: 1.0,
            },
            Arc::new(OpenPBR::diffuse(glam::Vec3A::splat(0.5))),
        );
        let world = builder.commit();
        let camera = Camera::new(
            glam::Vec3A::new(0.0, 0.0, 5.0),
            glam::Vec3A::ZERO,
            glam::Vec3A::Y,
            40.0,
            1.0,
            0.0,
            5.0,
        );
        let settings = RenderSettings::new(1, 4, 32, 32, 0, 0.0, 0);
        let renderer = Renderer::new(camera, world, LightList::new(), settings);

        let aovs = renderer.render_aovs(AovRequest {
            depth: true,
            normal: true,
            prim_id: true,
            alpha: true,
        });
        let center = 16 * 32 + 16;
        let corner = 0;

        let depth = aovs.depth.as_ref().unwrap();
        // Camera at z=5 looking at a unit sphere: front pole at z=1 → 4.
        assert!(
            (depth[center] - 4.0).abs() < 0.05,
            "center depth {} != 4",
            depth[center]
        );
        assert_eq!(depth[corner], f32::INFINITY);

        let normal = aovs.normal.as_ref().unwrap();
        // Pixel (16,16)'s center sits at (16.5/32, 16.5/32) — half a pixel
        // off the optical axis — so the probe hits ~2.6° off the pole.
        assert!(
            (normal[center] - glam::Vec3A::Z).length() < 0.1,
            "center normal {:?} should face the camera",
            normal[center]
        );
        assert_eq!(normal[corner], glam::Vec3A::ZERO);

        let ids = aovs.prim_id.as_ref().unwrap();
        assert_eq!(ids[center], [geom_id, 0]);
        assert_eq!(ids[corner], [u32::MAX, u32::MAX]);

        let alpha = aovs.alpha.as_ref().unwrap();
        assert_eq!(alpha[center], 1.0);
        assert_eq!(alpha[corner], 0.0);

        let sparse = renderer.render_aovs(AovRequest {
            depth: true,
            ..AovRequest::default()
        });
        assert!(sparse.depth.is_some());
        assert!(sparse.normal.is_none() && sparse.prim_id.is_none() && sparse.alpha.is_none());
    }
}
