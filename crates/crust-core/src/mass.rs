//! The curve mass field: opted-in curves baked into a sparse 3D field around
//! them — the strands' coverage density, the signed distance to the surface
//! where that coverage reaches a threshold, and, when the material provides a
//! graph for it, a colour — so that two strands crossing at a point read the
//! same mass colour and the same mass normal there. The design record is
//! `openspec/specs/curve-mass-field/design.md`.
//!
//! The import collects what each opted-in prim contributes ([`MassSource`]:
//! its strands as straight pieces, and the colour graph with the strand
//! table it reads), bakes every field in parallel after the USD read
//! ([`bake`]), and keys each placement's lookup ([`MassLookup`]) on the
//! geometry's strand table, which is what a curve hit carries
//! ([`crate::CurveHit::mass`]). The MaterialX nodes that read the field are
//! `crust_mtlx`'s (`crust_mass_*`); the host fills a [`MassCtx`] per hit for
//! what the program reads.
//!
//! # The field
//!
//! Voxels of side `h = radius / 2`, on a grid aligned to multiples of `h`, in
//! bricks of 8³ held only where a strand's splat or the surface's distance
//! band reaches. Per voxel: coverage `ρ`, the signed distance `d` (negative
//! inside, clamped to a band of three radii), the same distance after one
//! binomial smoothing pass (what the normal is the gradient of: the sweep's
//! first-order staircase would otherwise band a tilted surface, while the
//! depth reads the unsmoothed distance so a surface half a voxel thick keeps
//! its inside), and, in a field with a colour, `Σ w·c` whose weight is `ρ`
//! itself — so the colour interpolates as a mean, never darkening where
//! coverage is sparse. Coverage is normalised so a lone straight strand as
//! wide as the radius has 1 on its centre line (`LINE_INTEGRAL`).
//!
//! # Determinism
//!
//! The splat accumulates per brick, each brick summing its pieces' samples
//! in piece order, so the field is bit-identical however the bricks are
//! shared among threads; the sweep and the smoothing run once over the whole
//! field. Nothing here draws a random number.

use std::sync::Arc;
use std::time::{Duration, Instant};

use glam::{Affine3A, Mat3A, Vec3A};
use rayon::prelude::*;

use crate::strands::{CurveHit, StrandTable};
use crust_mtlx::{CurveCtx, MassCtx, MassReads, Program, ShadeCtx, Val};

/// The `crust:mass:*` settings a field bakes with (`radius` in the field's
/// own units).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MassSettings {
    /// The splat radius; the voxel is half of it.
    pub radius: f32,
    /// The coverage the mass's surface lies at.
    pub threshold: f32,
    /// The most voxels along any axis.
    pub max_resolution: u32,
}

impl MassSettings {
    pub const DEFAULT_THRESHOLD: f32 = 0.5;
    pub const DEFAULT_MAX_RESOLUTION: u32 = 512;

    /// The defaults for `radius`.
    pub fn new(radius: f32) -> MassSettings {
        MassSettings {
            radius,
            threshold: Self::DEFAULT_THRESHOLD,
            max_resolution: Self::DEFAULT_MAX_RESOLUTION,
        }
    }

    /// The same settings with the radius scaled, for a prim whose curves are
    /// carried into the field's space by a transform.
    pub fn scaled(self, s: f32) -> MassSettings {
        MassSettings {
            radius: self.radius * s,
            ..self
        }
    }
}

/// One straight piece of a strand, as the bake splats it: its ends, the
/// strand's radius (half its width) at each, and where on the strand table
/// it lies — the kernel span it belongs to and the span's parameter range —
/// for the colour graph's curve properties and primvars.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MassPiece {
    pub a: Vec3A,
    pub b: Vec3A,
    pub ra: f32,
    pub rb: f32,
    pub span: u32,
    pub u0: f32,
    pub u1: f32,
}

/// A material's mass colour graph: the program of its
/// `outputs:crust:mass:color` terminal, evaluated at strand samples.
pub struct MassColor {
    program: Program,
    root: u32,
    /// A stable digest of the program, which an in-place material edit is
    /// checked against: a changed graph needs the field baked again
    /// ([`crate::rt_world::Signature`]).
    pub fingerprint: u64,
}

impl MassColor {
    /// The graph whose value is `program`'s slot `root`.
    pub fn new(program: Program, root: u32) -> MassColor {
        let text = format!(
            "{:?}|{:?}|{:?}",
            program.ops,
            program
                .consts
                .iter()
                .map(|v| v.v.map(f32::to_bits))
                .collect::<Vec<_>>(),
            program.primvars
        );
        MassColor {
            fingerprint: fnv1a(text.as_bytes()),
            program,
            root,
        }
    }

    /// The primvars the graph reads by name, in its order.
    pub fn primvars(&self) -> &[String] {
        &self.program.primvars
    }

    /// The nodes the graph has that a bake cannot evaluate (they need a view
    /// or a surface hit), by name; empty for a bakeable graph.
    pub fn needs_view_or_hit(&self) -> Vec<&'static str> {
        self.program.needs_view_or_hit()
    }

    /// The graph at a strand sample: `position`, and the strand's properties
    /// and primvars through `curve`. A bake has no view and no surface: the
    /// nodes that need one read their defaults (what `needs_view_or_hit`
    /// warned about), and a texture reads its finest level.
    fn eval(&self, slots: &mut Vec<Val>, position: Vec3A, curve: Option<CurveCtx<'_>>) -> Vec3A {
        let ctx = ShadeCtx {
            uv: (0.0, 0.0),
            normal: Vec3A::ZERO,
            tangent: Vec3A::ZERO,
            view: Vec3A::ZERO,
            position,
            uv_width: 0.0,
            curve,
            mass: None,
        };
        self.program.eval(&ctx, slots);
        let c = slots[self.root as usize].rgb();
        let f = |x: f32| if x.is_finite() { x.max(0.0) } else { 0.0 };
        Vec3A::new(f(c.x), f(c.y), f(c.z))
    }
}

impl std::fmt::Debug for MassColor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MassColor({} ops, root {}, {:016x})",
            self.program.ops.len(),
            self.root,
            self.fingerprint
        )
    }
}

/// What one curves prim contributes to a field: its strands as pieces in
/// the field's space, and its colour graph with the strand table the graph
/// reads the pieces' properties and primvars from.
pub struct MassSource {
    pub pieces: Vec<MassPiece>,
    pub color: Option<(Arc<MassColor>, StrandTable)>,
}

/// `∫ kernel(|t|) dt` over `t ∈ [−1, 1]`: what a straight line through a
/// point deposits there, per unit of radius, so a lone line as wide as the
/// radius sums to coverage 1 on its axis.
const LINE_INTEGRAL: f32 = 32.0 / 35.0;

/// The smooth compact splat kernel, 1 at the centre and 0 at `r = 1`.
#[inline]
fn kernel(r2: f32) -> f32 {
    if r2 >= 1.0 {
        0.0
    } else {
        let s = 1.0 - r2;
        s * s * s
    }
}

/// Samples along a piece, as a fraction of the voxel size.
const SAMPLES_PER_VOXEL: f32 = 4.0;

/// Bricks are 8³ voxels.
const B: usize = 8;
const BRICK: usize = B * B * B;
const NONE: u32 = u32::MAX;

/// The distance band, in voxels: three radii.
const BAND_VOXELS: f32 = 6.0;

/// A baked mass field. See the module documentation.
pub struct MassField {
    settings: MassSettings,
    origin: Vec3A,
    h: f32,
    /// Voxels along each axis.
    dims: [u32; 3],
    /// Bricks along each axis.
    bdims: [u32; 3],
    /// Dense over the brick grid: each brick's index into the channels, or
    /// `NONE`.
    brick_index: Vec<u32>,
    bricks: usize,
    /// Coverage, `bricks × 512`.
    rho: Vec<f32>,
    /// The signed distance, clamped to `±band`.
    dist: Vec<f32>,
    /// `dist` after one binomial pass along each axis: the normal's.
    smooth: Vec<f32>,
    /// `Σ w·c`, weighted exactly as `rho` is; `None` for a field without a
    /// colour.
    color: Option<Vec<[f32; 3]>>,
    /// The clamp: three radii.
    band: f32,
    /// Whether coverage reaches the threshold anywhere.
    surface: bool,
    /// Whether the resolution cap shrank the grid.
    capped: bool,
    /// The prims baked into it, in path order.
    prims: Vec<String>,
    bake_time: Duration,
}

impl std::fmt::Debug for MassField {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "MassField({:?}, {}x{}x{} voxels of {}, {} bricks, colour {}, surface {})",
            self.prims,
            self.dims[0],
            self.dims[1],
            self.dims[2],
            self.h,
            self.bricks,
            self.color.is_some(),
            self.surface
        )
    }
}

/// Bakes one field from `sources` with `settings`, for the prims `prims`
/// (named in the log and the stats). Deterministic: see the module
/// documentation.
pub fn bake(sources: &[MassSource], settings: MassSettings, prims: Vec<String>) -> MassField {
    let started = Instant::now();
    let r = settings.radius;
    let (mut lo, mut hi) = (Vec3A::splat(f32::MAX), Vec3A::splat(f32::MIN));
    let mut pieces = 0usize;
    for s in sources {
        for p in &s.pieces {
            lo = lo.min(p.a.min(p.b) - Vec3A::splat(p.ra.max(p.rb)));
            hi = hi.max(p.a.max(p.b) + Vec3A::splat(p.ra.max(p.rb)));
            pieces += 1;
        }
    }
    if pieces == 0 || !lo.is_finite() || !hi.is_finite() {
        return MassField::empty(settings, prims, started.elapsed());
    }
    // The splat reaches a radius; the band three more.
    let pad = Vec3A::splat(4.0 * r);
    let size = hi + pad - (lo - pad);
    let mut h = r / 2.0;
    let count = |h: f32| size.max_element() / h;
    let cap = settings.max_resolution.max(1) as f32;
    let capped = count(h).ceil() + 2.0 > cap;
    if capped {
        h = size.max_element() / (cap - 2.0).max(1.0);
        tracing::debug!(
            "Mass field {:?}: {} voxels of {} along the longest axis would exceed \
             crust:mass:maxResolution {}; the voxel grows to {h}",
            prims,
            count(r / 2.0).ceil() + 1.0,
            r / 2.0,
            settings.max_resolution
        );
    }
    // The grid is aligned to multiples of the voxel, so a point on a
    // lattice of the voxel's size — a clump's axis, a sheet's plane — lies
    // on voxels, where a symmetric field reads symmetrically.
    let origin = ((lo - pad) / h).floor() * h;
    let size = hi + pad - origin;
    let dims = [
        ((size.x / h).ceil() as u32 + 1).min(cap as u32).max(1),
        ((size.y / h).ceil() as u32 + 1).min(cap as u32).max(1),
        ((size.z / h).ceil() as u32 + 1).min(cap as u32).max(1),
    ];
    let bdims = dims.map(|n| n.div_ceil(B as u32));
    let mut field = MassField {
        settings,
        origin,
        h,
        dims,
        bdims,
        brick_index: vec![NONE; (bdims[0] * bdims[1] * bdims[2]) as usize],
        bricks: 0,
        rho: Vec::new(),
        dist: Vec::new(),
        smooth: Vec::new(),
        color: None,
        band: BAND_VOXELS * h,
        surface: false,
        capped,
        prims,
        bake_time: Duration::ZERO,
    };
    field.splat(sources);
    field.signed_distance();
    field.smooth_distance();
    field.bake_time = started.elapsed();
    field
}

/// Where a brick's pieces come from: `(source, piece)`.
type PieceRef = (u32, u32);

impl MassField {
    fn empty(settings: MassSettings, prims: Vec<String>, bake_time: Duration) -> MassField {
        MassField {
            settings,
            origin: Vec3A::ZERO,
            h: settings.radius.max(1e-6) / 2.0,
            dims: [0; 3],
            bdims: [0; 3],
            brick_index: Vec::new(),
            bricks: 0,
            rho: Vec::new(),
            dist: Vec::new(),
            smooth: Vec::new(),
            color: None,
            band: BAND_VOXELS * settings.radius / 2.0,
            surface: false,
            capped: false,
            prims,
            bake_time,
        }
    }

    // -- the grid ---------------------------------------------------------

    /// The channel index of voxel `(x, y, z)`, or `None` where no brick is
    /// held (or outside the grid).
    #[inline]
    fn index(&self, x: i64, y: i64, z: i64) -> Option<usize> {
        if x < 0 || y < 0 || z < 0 {
            return None;
        }
        let (x, y, z) = (x as u64, y as u64, z as u64);
        if x >= u64::from(self.dims[0])
            || y >= u64::from(self.dims[1])
            || z >= u64::from(self.dims[2])
        {
            return None;
        }
        let b = self.brick_at(x >> 3, y >> 3, z >> 3)?;
        Some(b * BRICK + ((x & 7) + 8 * ((y & 7) + 8 * (z & 7))) as usize)
    }

    #[inline]
    fn brick_at(&self, bx: u64, by: u64, bz: u64) -> Option<usize> {
        let i = (bx + u64::from(self.bdims[0]) * (by + u64::from(self.bdims[1]) * bz)) as usize;
        let b = self.brick_index[i];
        (b != NONE).then_some(b as usize)
    }

    /// The brick grid coordinates of brick slot `i`.
    fn brick_coords(&self, i: usize) -> [u64; 3] {
        let (bx, by) = (u64::from(self.bdims[0]), u64::from(self.bdims[1]));
        let i = i as u64;
        [i % bx, (i / bx) % by, i / (bx * by)]
    }

    /// The world (field-space) position of voxel `(x, y, z)`.
    #[inline]
    fn position(&self, x: i64, y: i64, z: i64) -> Vec3A {
        self.origin + Vec3A::new(x as f32, y as f32, z as f32) * self.h
    }

    /// Allocates the brick at grid slot `slot` if it is not held yet.
    fn hold(&mut self, slot: usize) {
        if self.brick_index[slot] == NONE {
            self.brick_index[slot] = self.bricks as u32;
            self.bricks += 1;
        }
    }

    // -- the bake ---------------------------------------------------------

    /// Splats every source's pieces into the bricks they reach, in piece
    /// order per brick.
    fn splat(&mut self, sources: &[MassSource]) {
        let r = self.settings.radius;
        let has_color = sources.iter().any(|s| s.color.is_some());
        // Bin pieces into bricks by the box their splat reaches, and hold
        // those bricks, in grid order.
        let slots = self.brick_index.len();
        let mut lists: Vec<Vec<PieceRef>> = vec![Vec::new(); slots];
        for (si, s) in sources.iter().enumerate() {
            for (pi, p) in s.pieces.iter().enumerate() {
                let reach = Vec3A::splat(r);
                let (lo, hi) = (p.a.min(p.b) - reach, p.a.max(p.b) + reach);
                let (lo, hi) = (self.voxel_floor(lo), self.voxel_ceil(hi));
                for bz in (lo[2] >> 3)..=(hi[2] >> 3) {
                    for by in (lo[1] >> 3)..=(hi[1] >> 3) {
                        for bx in (lo[0] >> 3)..=(hi[0] >> 3) {
                            let slot = (bx
                                + u64::from(self.bdims[0]) * (by + u64::from(self.bdims[1]) * bz))
                                as usize;
                            lists[slot].push((si as u32, pi as u32));
                        }
                    }
                }
            }
        }
        for (slot, list) in lists.iter().enumerate() {
            if !list.is_empty() {
                self.hold(slot);
            }
        }
        let n = self.bricks * BRICK;
        let work: Vec<(usize, &Vec<PieceRef>)> = lists
            .iter()
            .enumerate()
            .filter(|(_, l)| !l.is_empty())
            .collect();
        // Each brick's channels, computed independently, in parallel.
        let brick_results: Vec<(usize, Vec<f32>, Option<Vec<[f32; 3]>>)> = work
            .par_iter()
            .map(|&(slot, list)| {
                let b = self.brick_index[slot] as usize;
                let coords = self.brick_coords(slot);
                let mut rho = vec![0.0f32; BRICK];
                let mut color = has_color.then(|| vec![[0.0f32; 3]; BRICK]);
                let mut slots_buf = Vec::new();
                let mut primvars = Vec::new();
                for &(si, pi) in list {
                    let source = &sources[si as usize];
                    let piece = &source.pieces[pi as usize];
                    self.splat_piece(
                        piece,
                        source,
                        coords,
                        &mut rho,
                        color.as_deref_mut(),
                        &mut slots_buf,
                        &mut primvars,
                    );
                }
                (b, rho, color)
            })
            .collect();
        self.rho = vec![0.0; n];
        if has_color {
            self.color = Some(vec![[0.0; 3]; n]);
        }
        for (b, rho, color) in brick_results {
            self.rho[b * BRICK..(b + 1) * BRICK].copy_from_slice(&rho);
            if let (Some(all), Some(c)) = (&mut self.color, color) {
                all[b * BRICK..(b + 1) * BRICK].copy_from_slice(&c);
            }
        }
    }

    /// The voxel at or below `p`, clamped into the grid.
    fn voxel_floor(&self, p: Vec3A) -> [u64; 3] {
        let q = (p - self.origin) / self.h;
        [
            q.x.floor().clamp(0.0, (self.dims[0] - 1) as f32) as u64,
            q.y.floor().clamp(0.0, (self.dims[1] - 1) as f32) as u64,
            q.z.floor().clamp(0.0, (self.dims[2] - 1) as f32) as u64,
        ]
    }

    fn voxel_ceil(&self, p: Vec3A) -> [u64; 3] {
        let q = (p - self.origin) / self.h;
        [
            q.x.ceil().clamp(0.0, (self.dims[0] - 1) as f32) as u64,
            q.y.ceil().clamp(0.0, (self.dims[1] - 1) as f32) as u64,
            q.z.ceil().clamp(0.0, (self.dims[2] - 1) as f32) as u64,
        ]
    }

    /// Splats one piece's samples into brick `coords`' voxels (and nothing
    /// outside it).
    #[allow(clippy::too_many_arguments)]
    fn splat_piece(
        &self,
        piece: &MassPiece,
        source: &MassSource,
        coords: [u64; 3],
        rho: &mut [f32],
        mut color: Option<&mut [[f32; 3]]>,
        slots: &mut Vec<Val>,
        primvars: &mut Vec<Option<Val>>,
    ) {
        let r = self.settings.radius;
        let h = self.h;
        let (a, b) = (piece.a, piece.b);
        let len = (b - a).length();
        let step = h / SAMPLES_PER_VOXEL;
        let count = (len / step).ceil().max(1.0) as usize;
        let ds = len / count as f32;
        // The brick's voxel range, in global voxel coordinates.
        let base = [coords[0] * 8, coords[1] * 8, coords[2] * 8];
        let reach = (r / h).ceil() as i64;
        let r2 = r * r;
        for i in 0..count {
            let t = (i as f32 + 0.5) / count as f32;
            let p = a + (b - a) * t;
            let radius = piece.ra + (piece.rb - piece.ra) * t;
            // Width-weighted: a strand as wide as the radius deposits 1 on
            // its axis per unit length over `LINE_INTEGRAL`.
            let w = (2.0 * radius / r) * ds / (r * LINE_INTEGRAL);
            let c = source.color.as_ref().map(|(graph, table)| {
                let u = piece.u0 + (piece.u1 - piece.u0) * t;
                let curve = CurveHit::new(table, piece.span, u).map(|hit| {
                    hit.primvars_into(primvars);
                    CurveCtx {
                        v: hit.v(),
                        index: hit.index(),
                        random: hit.random(),
                        primvars: primvars.as_slice(),
                    }
                });
                graph.eval(slots, p, curve)
            });
            let q = (p - self.origin) / h;
            let centre = [q.x.round() as i64, q.y.round() as i64, q.z.round() as i64];
            for dz in -reach..=reach {
                let z = centre[2] + dz;
                if z < base[2] as i64 || z >= base[2] as i64 + 8 {
                    continue;
                }
                for dy in -reach..=reach {
                    let y = centre[1] + dy;
                    if y < base[1] as i64 || y >= base[1] as i64 + 8 {
                        continue;
                    }
                    for dx in -reach..=reach {
                        let x = centre[0] + dx;
                        if x < base[0] as i64 || x >= base[0] as i64 + 8 {
                            continue;
                        }
                        let d2 = (self.position(x, y, z) - p).length_squared();
                        let k = kernel(d2 / r2);
                        if k > 0.0 {
                            let local = ((x - base[0] as i64)
                                + 8 * ((y - base[1] as i64) + 8 * (z - base[2] as i64)))
                                as usize;
                            let wk = w * k;
                            rho[local] += wk;
                            if let (Some(color), Some(c)) = (color.as_deref_mut(), c) {
                                let cell = &mut color[local];
                                cell[0] += c.x * wk;
                                cell[1] += c.y * wk;
                                cell[2] += c.z * wk;
                            }
                        }
                    }
                }
            }
        }
    }

    /// The signed distance to `rho = threshold`, negative inside: interface
    /// voxels seeded by linear interpolation along each axis, then the
    /// unsigned distance swept out over every held brick in the eight
    /// diagonal orders (Zhao's fast sweeping, Godunov upwind update), twice
    /// over; clamped to the band, which also keeps a field with no surface
    /// finite, its gradient zero rather than not a number.
    fn signed_distance(&mut self) {
        let threshold = self.settings.threshold;
        let h = self.h;
        // Seeds, and the bricks they reach: a band of six voxels fits in a
        // neighbouring brick.
        let mut seeds: Vec<(usize, f32)> = Vec::new();
        let mut held_before = self.bricks;
        let grid_slots = self.brick_index.len();
        let mut extra: Vec<usize> = Vec::new();
        for slot in 0..grid_slots {
            let Some(b) =
                (self.brick_index[slot] != NONE).then_some(self.brick_index[slot] as usize)
            else {
                continue;
            };
            let coords = self.brick_coords(slot);
            let mut seeded = false;
            for local in 0..BRICK {
                let i = b * BRICK + local;
                let phi = self.rho[i] - threshold;
                let (x, y, z) = (
                    (coords[0] * 8 + (local % 8) as u64) as i64,
                    (coords[1] * 8 + ((local / 8) % 8) as u64) as i64,
                    (coords[2] * 8 + (local / 64) as u64) as i64,
                );
                let mut best = f32::INFINITY;
                for (dx, dy, dz) in [
                    (1, 0, 0),
                    (-1, 0, 0),
                    (0, 1, 0),
                    (0, -1, 0),
                    (0, 0, 1),
                    (0, 0, -1),
                ] {
                    // A neighbour outside every held brick has no coverage.
                    let phi_j = self
                        .index(x + dx, y + dy, z + dz)
                        .map_or(-threshold, |j| self.rho[j] - threshold);
                    if (phi > 0.0) != (phi_j > 0.0) {
                        let t = phi / (phi - phi_j);
                        best = best.min(t * h);
                    }
                }
                if best.is_finite() {
                    seeds.push((i, best));
                    seeded = true;
                }
            }
            if seeded {
                for dz in -1i64..=1 {
                    for dy in -1i64..=1 {
                        for dx in -1i64..=1 {
                            let (bx, by, bz) = (
                                coords[0] as i64 + dx,
                                coords[1] as i64 + dy,
                                coords[2] as i64 + dz,
                            );
                            if bx < 0
                                || by < 0
                                || bz < 0
                                || bx >= i64::from(self.bdims[0])
                                || by >= i64::from(self.bdims[1])
                                || bz >= i64::from(self.bdims[2])
                            {
                                continue;
                            }
                            let s = (bx as u64
                                + u64::from(self.bdims[0])
                                    * (by as u64 + u64::from(self.bdims[1]) * bz as u64))
                                as usize;
                            extra.push(s);
                        }
                    }
                }
            }
        }
        self.surface = !seeds.is_empty();
        extra.sort_unstable();
        extra.dedup();
        for slot in extra {
            self.hold(slot);
        }
        // The channels grow for the band's bricks: no coverage there.
        let n = self.bricks * BRICK;
        self.rho.resize(n, 0.0);
        if let Some(c) = &mut self.color {
            c.resize(n, [0.0; 3]);
        }
        held_before = held_before.min(self.bricks);
        let _ = held_before;

        let mut u = vec![f32::INFINITY; n];
        let mut fixed = vec![false; n];
        for &(i, d) in &seeds {
            u[i] = d;
            fixed[i] = true;
        }
        // Held bricks in grid order, each with its coordinates.
        let mut held: Vec<(usize, [u64; 3])> = (0..grid_slots)
            .filter(|&s| self.brick_index[s] != NONE)
            .map(|s| (self.brick_index[s] as usize, self.brick_coords(s)))
            .collect();
        if self.surface {
            for _ in 0..2 {
                for order in 0..8u32 {
                    let flip = [order & 1 != 0, order & 2 != 0, order & 4 != 0];
                    held.sort_by_key(|(_, c)| {
                        let k = |axis: usize| {
                            if flip[axis] {
                                u64::from(self.bdims[axis]) - 1 - c[axis]
                            } else {
                                c[axis]
                            }
                        };
                        (k(2), k(1), k(0))
                    });
                    for &(b, coords) in &held {
                        for lz in 0..8u64 {
                            for ly in 0..8u64 {
                                for lx in 0..8u64 {
                                    let l =
                                        |axis: usize, v: u64| if flip[axis] { 7 - v } else { v };
                                    let (lx, ly, lz) = (l(0, lx), l(1, ly), l(2, lz));
                                    let i = b * BRICK + (lx + 8 * (ly + 8 * lz)) as usize;
                                    if fixed[i] {
                                        continue;
                                    }
                                    let (x, y, z) = (
                                        (coords[0] * 8 + lx) as i64,
                                        (coords[1] * 8 + ly) as i64,
                                        (coords[2] * 8 + lz) as i64,
                                    );
                                    let along = |d: [i64; 3]| -> f32 {
                                        let a = self.index(x - d[0], y - d[1], z - d[2]);
                                        let bb = self.index(x + d[0], y + d[1], z + d[2]);
                                        a.map_or(f32::INFINITY, |a| u[a])
                                            .min(bb.map_or(f32::INFINITY, |bb| u[bb]))
                                    };
                                    let mut abc =
                                        [along([1, 0, 0]), along([0, 1, 0]), along([0, 0, 1])];
                                    abc.sort_by(f32::total_cmp);
                                    let [a, bb, c] = abc;
                                    if !a.is_finite() {
                                        continue;
                                    }
                                    let mut v = a + h;
                                    if v > bb {
                                        v = 0.5
                                            * (a + bb
                                                + (2.0 * h * h - (a - bb) * (a - bb))
                                                    .max(0.0)
                                                    .sqrt());
                                        if v > c {
                                            let s = a + bb + c;
                                            let q = s * s - 3.0 * (a * a + bb * bb + c * c - h * h);
                                            v = (s + q.max(0.0).sqrt()) / 3.0;
                                        }
                                    }
                                    if v < u[i] {
                                        u[i] = v;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        let band = self.band;
        self.dist = u
            .iter()
            .zip(&self.rho)
            .map(|(&u, &rho)| {
                let u = u.min(band);
                if rho - threshold > 0.0 { -u } else { u }
            })
            .collect();
    }

    /// One pass of the separable binomial filter (1 2 1)/4 along each axis
    /// over the distance, into `smooth`, a voxel with no neighbour on a side
    /// using its own value there, as at the grid's faces. See the design
    /// record: the sweep's first-order staircase would show as bands a voxel
    /// apart in the normal. The distance itself is kept as swept.
    fn smooth_distance(&mut self) {
        let n = self.dist.len();
        self.smooth = self.dist.clone();
        let held: Vec<(usize, [u64; 3])> = (0..self.brick_index.len())
            .filter(|&s| self.brick_index[s] != NONE)
            .map(|s| (self.brick_index[s] as usize, self.brick_coords(s)))
            .collect();
        for axis in 0..3 {
            let src = std::mem::take(&mut self.smooth);
            let mut out = vec![0.0f32; n];
            for &(b, coords) in &held {
                for local in 0..BRICK {
                    let i = b * BRICK + local;
                    let mut c = [
                        (coords[0] * 8 + (local % 8) as u64) as i64,
                        (coords[1] * 8 + ((local / 8) % 8) as u64) as i64,
                        (coords[2] * 8 + (local / 64) as u64) as i64,
                    ];
                    let at = |c: [i64; 3]| self.index(c[0], c[1], c[2]).map_or(src[i], |j| src[j]);
                    c[axis] -= 1;
                    let before = at(c);
                    c[axis] += 2;
                    let after = at(c);
                    out[i] = 0.25 * before + 0.5 * src[i] + 0.25 * after;
                }
            }
            self.smooth = out;
        }
    }

    // -- lookups ----------------------------------------------------------

    /// Trilinear interpolation of `get` at `p` (field space), a corner no
    /// brick holds contributing `missing`.
    #[inline]
    fn trilinear<T>(&self, p: Vec3A, missing: T, get: impl Fn(usize) -> T) -> T
    where
        T: Copy + std::ops::Mul<f32, Output = T> + std::ops::Add<Output = T>,
    {
        let q = (p - self.origin) / self.h;
        let (x0, y0, z0) = (q.x.floor(), q.y.floor(), q.z.floor());
        let (fx, fy, fz) = (q.x - x0, q.y - y0, q.z - z0);
        let (x0, y0, z0) = (x0 as i64, y0 as i64, z0 as i64);
        let mut sum: Option<T> = None;
        for (dz, wz) in [(0, 1.0 - fz), (1, fz)] {
            for (dy, wy) in [(0, 1.0 - fy), (1, fy)] {
                for (dx, wx) in [(0, 1.0 - fx), (1, fx)] {
                    let v = self.index(x0 + dx, y0 + dy, z0 + dz).map_or(missing, &get);
                    let term = v * (wx * wy * wz);
                    sum = Some(match sum {
                        Some(s) => s + term,
                        None => term,
                    });
                }
            }
        }
        sum.unwrap_or(missing)
    }

    /// Whether `p` lies inside the grid at all (outside it every lookup is
    /// its "no field" answer, without interpolating).
    #[inline]
    fn inside(&self, p: Vec3A) -> bool {
        if self.bricks == 0 {
            return false;
        }
        let q = (p - self.origin) / self.h;
        q.is_finite()
            && q.x >= 0.0
            && q.y >= 0.0
            && q.z >= 0.0
            && q.x < self.dims[0] as f32
            && q.y < self.dims[1] as f32
            && q.z < self.dims[2] as f32
    }

    /// The coverage density at `p`.
    pub fn density(&self, p: Vec3A) -> f32 {
        if !self.inside(p) {
            return 0.0;
        }
        self.trilinear(p, 0.0, |i| self.rho[i])
    }

    /// The signed distance at `p`, `+band` off the field.
    pub fn distance(&self, p: Vec3A) -> f32 {
        if !self.inside(p) {
            return self.band;
        }
        self.trilinear(p, self.band, |i| self.dist[i])
    }

    /// The smoothed signed distance at `p`, `+band` off the field.
    fn smoothed(&self, p: Vec3A) -> f32 {
        if !self.inside(p) {
            return self.band;
        }
        self.trilinear(p, self.band, |i| self.smooth[i])
    }

    /// Depth into the mass: the negated distance where negative, else 0.
    pub fn depth(&self, p: Vec3A) -> f32 {
        (-self.distance(p)).max(0.0)
    }

    /// The mass normal at `p`: the normalised central-difference gradient
    /// of the trilinear smoothed distance, one voxel apart; `None` where
    /// that gradient vanishes or is not finite (a symmetry axis, no surface
    /// within the band). "Vanishes" is relative to the voxel: a difference
    /// below a thousandth of one is rounding in a flat band, not a surface.
    pub fn normal(&self, p: Vec3A) -> Option<Vec3A> {
        if !self.inside(p) {
            return None;
        }
        let h = self.h;
        let d = |o: Vec3A| self.smoothed(p + o);
        let g = Vec3A::new(
            d(Vec3A::X * h) - d(-Vec3A::X * h),
            d(Vec3A::Y * h) - d(-Vec3A::Y * h),
            d(Vec3A::Z * h) - d(-Vec3A::Z * h),
        );
        let l2 = g.length_squared();
        let floor = 1e-3 * h;
        (l2.is_finite() && l2 > floor * floor).then(|| g / l2.sqrt())
    }

    /// The mass colour at `p`: the coverage-weighted mean of the strands'
    /// colours around it; `None` for a field without a colour, or where no
    /// coverage reaches.
    pub fn color(&self, p: Vec3A) -> Option<Vec3A> {
        let color = self.color.as_ref()?;
        if !self.inside(p) {
            return None;
        }
        let w = self.trilinear(p, 0.0, |i| self.rho[i]);
        if !(w > 0.0) {
            return None;
        }
        let c = self.trilinear(p, Vec3A::ZERO, |i| Vec3A::from_array(color[i]));
        Some(c / w)
    }

    // -- facts ------------------------------------------------------------

    pub fn settings(&self) -> MassSettings {
        self.settings
    }

    /// The voxel size the field was baked at.
    pub fn voxel(&self) -> f32 {
        self.h
    }

    /// Voxels along each axis of the grid (held or not).
    pub fn dims(&self) -> [u32; 3] {
        self.dims
    }

    /// Bricks of 8³ voxels held.
    pub fn bricks(&self) -> usize {
        self.bricks
    }

    /// Bytes held by the field's channels and brick index.
    pub fn bytes(&self) -> usize {
        std::mem::size_of::<MassField>()
            + self.brick_index.len() * 4
            + self.rho.len() * 4
            + self.dist.len() * 4
            + self.smooth.len() * 4
            + self.color.as_ref().map_or(0, |c| c.len() * 12)
    }

    /// Whether the field has a colour.
    pub fn has_color(&self) -> bool {
        self.color.is_some()
    }

    /// Whether coverage reaches the threshold anywhere: a field with no
    /// surface defines no normal and no depth.
    pub fn has_surface(&self) -> bool {
        self.surface
    }

    /// Whether `crust:mass:maxResolution` bound, growing the voxel.
    pub fn capped(&self) -> bool {
        self.capped
    }

    /// The prims baked into the field, in path order.
    pub fn prims(&self) -> &[String] {
        &self.prims
    }

    pub fn bake_time(&self) -> Duration {
        self.bake_time
    }

    /// A digest of every channel and the brick layout: two fields with the
    /// same checksum hold the same bits (what the determinism tests compare).
    pub fn checksum(&self) -> u64 {
        let mut bytes: Vec<u8> = Vec::new();
        bytes.extend(self.dims.iter().flat_map(|d| d.to_le_bytes()));
        bytes.extend(self.brick_index.iter().flat_map(|b| b.to_le_bytes()));
        for v in self.rho.iter().chain(&self.dist).chain(&self.smooth) {
            bytes.extend(v.to_bits().to_le_bytes());
        }
        if let Some(c) = &self.color {
            for v in c.iter().flatten() {
                bytes.extend(v.to_bits().to_le_bytes());
            }
        }
        fnv1a(&bytes)
    }
}

/// How a geometry reads a field: the field, and the placement that carries
/// a hit's world-space position into the field's space and the normal back
/// out. A top-level field is in world space ([`MassLookup::world`]); a
/// prototype's is in the prototype's, read through the instance's transform
/// ([`MassLookup::placed`]).
#[derive(Clone, Debug)]
pub struct MassLookup {
    field: Arc<MassField>,
    /// World → field space.
    to_field: Affine3A,
    /// Field → world, for the normal (a rotation and uniform scale, so the
    /// linear part renormalised is the inverse transpose's direction).
    normal_out: Mat3A,
    /// The placement's uniform scale, which the depth scales by.
    scale: f32,
}

impl MassLookup {
    /// A field in world space, read as is.
    pub fn world(field: Arc<MassField>) -> MassLookup {
        MassLookup {
            field,
            to_field: Affine3A::IDENTITY,
            normal_out: Mat3A::IDENTITY,
            scale: 1.0,
        }
    }

    /// A field in the space `l2w` places into the world, when `l2w` is a
    /// rotation (or reflection) with a uniform scale and a translation —
    /// `None` for any other transform, under which a depth would depend on
    /// its direction and the field cannot be read as the design has it.
    pub fn placed(field: Arc<MassField>, l2w: Affine3A) -> Option<MassLookup> {
        let m = l2w.matrix3;
        let det = m.determinant();
        if !det.is_finite() || det.abs() < 1e-12 {
            return None;
        }
        let scale = det.abs().cbrt();
        // `MᵀM = s²I` within rounding: equal column lengths, orthogonal columns.
        let g = m.transpose() * m;
        let s2 = scale * scale;
        let tol = 1e-4 * s2;
        let uniform = (g.x_axis.x - s2).abs() <= tol
            && (g.y_axis.y - s2).abs() <= tol
            && (g.z_axis.z - s2).abs() <= tol
            && g.x_axis.y.abs() <= tol
            && g.x_axis.z.abs() <= tol
            && g.y_axis.z.abs() <= tol;
        if !uniform {
            return None;
        }
        Some(MassLookup {
            field,
            to_field: l2w.inverse(),
            normal_out: m,
            scale,
        })
    }

    pub fn field(&self) -> &Arc<MassField> {
        &self.field
    }

    /// What the field holds at the world-space point `p`, for the
    /// properties `reads` asks for (the rest stay at their defaults, unread).
    pub fn read(&self, p: Vec3A, reads: MassReads) -> MassCtx {
        use crust_mtlx::MassProp;
        let q = self.to_field.transform_point3a(p);
        let mut ctx = MassCtx::default();
        if reads.has(MassProp::Density) {
            ctx.density = self.field.density(q);
        }
        if reads.has(MassProp::Depth) {
            ctx.depth = self.field.depth(q) * self.scale;
        }
        if reads.has(MassProp::Normal) {
            ctx.normal = self.field.normal(q).and_then(|n| {
                let w = self.normal_out * n;
                let l2 = w.length_squared();
                (l2.is_finite() && l2 > 0.0).then(|| w / l2.sqrt())
            });
        }
        if reads.has(MassProp::Color) {
            ctx.color = self.field.color(q);
        }
        ctx
    }
}

/// FNV-1a over `bytes`: stable across runs and toolchains.
fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strands::StrandSpan;
    use crust_mtlx::{Compiler, Doc, Host, MassProp};

    /// A straight strand from `a` to `b` of radius `r` (half its width),
    /// span 0 over the whole strand.
    fn strand(a: Vec3A, b: Vec3A, r: f32) -> MassPiece {
        MassPiece {
            a,
            b,
            ra: r,
            rb: r,
            span: 0,
            u0: 0.0,
            u1: 1.0,
        }
    }

    fn source(pieces: Vec<MassPiece>) -> MassSource {
        MassSource {
            pieces,
            color: None,
        }
    }

    fn lone(width: f32, radius: f32) -> MassField {
        let src = source(vec![strand(
            Vec3A::new(-2.0, 0.0, 0.0),
            Vec3A::new(2.0, 0.0, 0.0),
            width / 2.0,
        )]);
        bake(&[src], MassSettings::new(radius), vec!["/lone".into()])
    }

    #[test]
    fn a_lone_strand_as_wide_as_the_radius_has_coverage_one_on_its_axis() {
        let r = 0.1;
        let f = lone(r, r);
        let on_axis = f.density(Vec3A::ZERO);
        assert!((on_axis - 1.0).abs() < 0.03, "{on_axis}");
        let twice = lone(2.0 * r, r);
        let d = twice.density(Vec3A::ZERO);
        assert!((d - 2.0).abs() < 0.06, "{d}");
        // Beyond the radius, nothing.
        assert_eq!(f.density(Vec3A::new(0.0, 2.0 * r, 0.0)), 0.0);
        assert_eq!(f.prims(), ["/lone"]);
        assert!(f.bricks() > 0 && f.bytes() > 0);
    }

    #[test]
    fn a_lone_strands_distance_crosses_zero_between_its_axis_and_twice_the_radius() {
        let r = 0.1;
        let f = lone(r, r);
        assert!(f.has_surface());
        let axis = f.distance(Vec3A::ZERO);
        let out = f.distance(Vec3A::new(0.0, 2.0 * r, 0.0));
        assert!(axis < 0.0, "on the axis {axis}");
        assert!(out > 0.0, "at twice the radius {out}");
        // The depth is the negated distance inside, zero outside.
        assert_eq!(f.depth(Vec3A::ZERO), -axis);
        assert_eq!(f.depth(Vec3A::new(0.0, 2.0 * r, 0.0)), 0.0);
        // The normal at the side points away from the axis.
        let n = f.normal(Vec3A::new(0.0, 0.75 * r, 0.0)).expect("a normal");
        assert!(n.y > 0.95, "{n}");
        // Clamped to the band, finite everywhere.
        assert!(f.distance(Vec3A::new(0.0, 10.0 * r, 0.0)) <= f.band);
        assert!(f.distance(Vec3A::new(100.0, 100.0, 100.0)).is_finite());
    }

    #[test]
    fn a_strand_narrower_than_half_the_radius_bakes_no_surface() {
        let r = 0.1;
        let f = lone(0.4 * r, r);
        assert!(!f.has_surface());
        assert_eq!(f.normal(Vec3A::ZERO), None);
        assert_eq!(f.depth(Vec3A::ZERO), 0.0);
        let d = f.distance(Vec3A::ZERO);
        assert!(d.is_finite() && d > 0.0 && d <= f.band, "{d}");
        let rho = f.density(Vec3A::ZERO);
        assert!((rho - 0.4).abs() < 0.03, "{rho}");
    }

    /// A round clump of parallel strands along y, radius `clump` in x–z.
    fn clump(clump: f32, spacing: f32, radius: f32) -> MassField {
        let mut pieces = Vec::new();
        let n = (2.0 * clump / spacing).ceil() as i32;
        for i in -n..=n {
            for j in -n..=n {
                let (x, z) = (i as f32 * spacing, j as f32 * spacing);
                if x * x + z * z <= clump * clump {
                    pieces.push(strand(
                        Vec3A::new(x, -1.0, z),
                        Vec3A::new(x, 1.0, z),
                        0.6 * spacing,
                    ));
                }
            }
        }
        bake(
            &[source(pieces)],
            MassSettings::new(radius),
            vec!["/clump".into()],
        )
    }

    #[test]
    fn a_dense_round_clump_matches_its_analytic_distance() {
        let (c, spacing) = (0.25, 0.02);
        let f = clump(c, spacing, 3.0 * spacing);
        let h = f.voxel();
        // The surface lies where coverage falls to the threshold, about the
        // clump's edge: the distance along a radius is the analytic one
        // within half a voxel, inside and out, through the band.
        for rr in [0.05f32, 0.12, 0.2, 0.3, 0.35] {
            let p = Vec3A::new(rr, 0.0, 0.0);
            let got = f.distance(p);
            let want = rr - c;
            assert!(
                (got - want).abs() <= 0.5 * h + 0.5 * spacing,
                "at r {rr}: distance {got}, analytic {want}, voxel {h}"
            );
        }
        // The silhouette's normal is radial; halfway in it still points away
        // from the axis, unit length, with a positive depth.
        let n = f.normal(Vec3A::new(c, 0.0, 0.0)).expect("a normal");
        assert!(n.x > 0.98 && n.y.abs() < 0.1, "{n}");
        let half = Vec3A::new(0.5 * c, 0.0, 0.0);
        let n = f.normal(half).expect("a normal inside");
        assert!((n.length() - 1.0).abs() < 1e-4 && n.x > 0.95, "{n}");
        assert!(f.depth(half) > 0.0);
        // Exactly on the axis the two sides cancel: no normal.
        assert_eq!(f.normal(Vec3A::new(0.0, 0.0, 0.0)), None);
    }

    #[test]
    fn the_resolution_cap_grows_the_voxel() {
        let r = 0.1;
        let src = source(vec![strand(
            Vec3A::new(0.0, 0.0, 0.0),
            Vec3A::new(100.0 * r, 0.0, 0.0),
            r / 2.0,
        )]);
        let settings = MassSettings {
            radius: r,
            threshold: 0.5,
            max_resolution: 64,
        };
        let f = bake(&[src], settings, vec!["/long".into()]);
        assert!(f.capped());
        assert_eq!(f.dims()[0], 64);
        assert!(f.voxel() > r / 2.0);
        // Under a cap it does not reach, the voxel is half the radius.
        let short = source(vec![strand(
            Vec3A::new(0.0, 0.0, 0.0),
            Vec3A::new(r, 0.0, 0.0),
            r / 2.0,
        )]);
        let f = bake(&[short], settings, vec!["/short".into()]);
        assert!(!f.capped());
        assert_eq!(f.voxel(), r / 2.0);
    }

    #[test]
    fn bounds_a_thousand_radii_across_bake_the_default_cap() {
        let r = 0.01;
        let src = source(vec![strand(
            Vec3A::ZERO,
            Vec3A::new(1000.0 * r, 0.0, 0.0),
            r / 2.0,
        )]);
        let f = bake(&[src], MassSettings::new(r), vec!["/long".into()]);
        assert!(f.capped());
        assert_eq!(f.dims()[0], 512);
        assert!(f.dims()[1] < 64 && f.dims()[2] < 64);
        assert!(f.voxel() > r / 2.0);
    }

    #[test]
    fn a_bake_is_bit_identical_across_thread_counts() {
        let field = || clump(0.2, 0.02, 0.06);
        let one = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap()
            .install(field);
        let eight = rayon::ThreadPoolBuilder::new()
            .num_threads(8)
            .build()
            .unwrap()
            .install(field);
        assert_eq!(one.bricks(), eight.bricks());
        assert_eq!(one.brick_index, eight.brick_index);
        let bits = |v: &[f32]| v.iter().map(|x| x.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&one.rho), bits(&eight.rho));
        assert_eq!(bits(&one.dist), bits(&eight.dist));
    }

    /// A colour graph: `curve:v` as grey.
    fn ramp() -> Arc<MassColor> {
        let doc = Doc::parse(
            r#"<materialx>
                 <geompropvalue name="v" type="color3">
                   <input name="geomprop" type="string" value="curve:v" />
                 </geompropvalue>
               </materialx>"#,
        )
        .unwrap();
        let decline = |_: &str, _: Option<&str>| None;
        let mut c = Compiler::new(&doc, &Host::new(&decline));
        let root = c.compile_named("", "v", None);
        Arc::new(MassColor::new(c.program, root))
    }

    #[test]
    fn the_colour_is_a_coverage_weighted_mean_along_the_strand() {
        let r = 0.1;
        let table = StrandTable::new(
            vec![StrandSpan {
                curve: 0,
                v0: 0.0,
                v1: 1.0,
            }],
            "/ramp",
        );
        let src = MassSource {
            pieces: vec![strand(
                Vec3A::new(0.0, 0.0, 0.0),
                Vec3A::new(2.0, 0.0, 0.0),
                r / 2.0,
            )],
            color: Some((ramp(), table)),
        };
        let f = bake(&[src], MassSettings::new(r), vec!["/ramp".into()]);
        assert!(f.has_color());
        let at = |x: f32| f.color(Vec3A::new(x, 0.0, 0.0)).expect("a colour").x;
        assert!((at(0.2) - 0.1).abs() < 0.03, "{}", at(0.2));
        assert!((at(1.0) - 0.5).abs() < 0.03, "{}", at(1.0));
        assert!((at(1.8) - 0.9).abs() < 0.03, "{}", at(1.8));
        // Off the strand's coverage: no colour.
        assert_eq!(f.color(Vec3A::new(1.0, 1.0, 0.0)), None);
        // Two fields with the same graph share a fingerprint; a different
        // graph does not.
        assert_eq!(ramp().fingerprint, ramp().fingerprint);
        let other = {
            let doc = Doc::parse(r#"<materialx><constant name="k" type="color3"><input name="value" type="color3" value="1, 0, 0" /></constant></materialx>"#).unwrap();
            let decline = |_: &str, _: Option<&str>| None;
            let mut c = Compiler::new(&doc, &Host::new(&decline));
            let root = c.compile_named("", "k", None);
            MassColor::new(c.program, root)
        };
        assert_ne!(ramp().fingerprint, other.fingerprint);
    }

    #[test]
    fn two_colours_meet_without_darkening() {
        let r = 0.1;
        let constant = |rgb: &str| {
            let doc = Doc::parse(&format!(
                r#"<materialx><constant name="k" type="color3"><input name="value" type="color3" value="{rgb}" /></constant></materialx>"#
            ))
            .unwrap();
            let decline = |_: &str, _: Option<&str>| None;
            let mut c = Compiler::new(&doc, &Host::new(&decline));
            let root = c.compile_named("", "k", None);
            Arc::new(MassColor::new(c.program, root))
        };
        let table = || {
            StrandTable::new(
                vec![StrandSpan {
                    curve: 0,
                    v0: 0.0,
                    v1: 1.0,
                }],
                "/sheet",
            )
        };
        let red = MassSource {
            pieces: vec![strand(
                Vec3A::new(-2.0, 0.0, 0.0),
                Vec3A::new(0.0, 0.0, 0.0),
                r / 2.0,
            )],
            color: Some((constant("1, 0, 0"), table())),
        };
        let blue = MassSource {
            pieces: vec![strand(
                Vec3A::new(0.0, 0.0, 0.0),
                Vec3A::new(2.0, 0.0, 0.0),
                r / 2.0,
            )],
            color: Some((constant("0, 0, 1"), table())),
        };
        let f = bake(
            &[red, blue],
            MassSettings::new(r),
            vec!["/a".into(), "/b".into()],
        );
        let at = |x: f32| f.color(Vec3A::new(x, 0.0, 0.0)).expect("a colour");
        assert!((at(-1.0) - Vec3A::X).length() < 1e-3);
        assert!((at(1.0) - Vec3A::Z).length() < 1e-3);
        let mid = at(0.0);
        assert!(
            mid.x > 0.3 && mid.z > 0.3 && (mid.x + mid.z - 1.0).abs() < 1e-3,
            "{mid}"
        );
        // Sparse coverage: the colour stays the strands', not darker.
        let edge = f.color(Vec3A::new(-1.0, 0.8 * r, 0.0)).expect("a colour");
        assert!((edge - Vec3A::X).length() < 1e-3, "{edge}");
    }

    #[test]
    fn a_placed_lookup_carries_the_point_in_and_the_normal_out() {
        let r = 0.1;
        let f = Arc::new(lone(r, r));
        let world = MassLookup::world(Arc::clone(&f));
        let all = MassReads(15);
        let p = Vec3A::new(0.0, 0.75 * r, 0.0);
        let plain = world.read(p, all);
        assert!(plain.normal.unwrap().y > 0.95);
        assert_eq!(plain.color, None);
        // Rotated a quarter turn about x (y → z) and scaled by 2.
        let l2w = Affine3A::from_scale_rotation_translation(
            glam::Vec3::splat(2.0),
            glam::Quat::from_rotation_x(std::f32::consts::FRAC_PI_2),
            glam::Vec3::new(5.0, 0.0, 0.0),
        );
        let placed = MassLookup::placed(Arc::clone(&f), l2w).expect("rotation and uniform scale");
        let q = l2w.transform_point3a(p);
        let got = placed.read(q, all);
        assert!(
            (got.normal.unwrap() - Vec3A::Z).length() < 1e-3,
            "{:?}",
            got.normal
        );
        assert!((got.density - plain.density).abs() < 1e-5);
        // Depth scales with the instance; the normal does not.
        let inside = Vec3A::ZERO;
        let d0 = world.read(inside, all).depth;
        let d1 = placed.read(l2w.transform_point3a(inside), all).depth;
        assert!(d0 > 0.0 && (d1 - 2.0 * d0).abs() < 1e-5, "{d0} {d1}");
        assert!(matches!(
            MassProp::of_category("crust_mass_depth"),
            Some(MassProp::Depth)
        ));
        // Only what is read is filled.
        let only = placed.read(q, MassReads(MassProp::Density.bit()));
        assert_eq!(only.normal, None);
        assert_eq!(only.depth, 0.0);
        // A non-uniform scale cannot be placed.
        let squashed = Affine3A::from_scale(glam::Vec3::new(1.0, 2.0, 1.0));
        assert!(MassLookup::placed(Arc::clone(&f), squashed).is_none());
        let mirrored = Affine3A::from_scale(glam::Vec3::new(-1.0, 1.0, 1.0));
        assert!(MassLookup::placed(f, mirrored).is_some());
    }

    #[test]
    fn an_empty_source_bakes_an_empty_field() {
        let f = bake(
            &[source(Vec::new())],
            MassSettings::new(0.1),
            vec!["/none".into()],
        );
        assert_eq!(f.bricks(), 0);
        assert_eq!(f.density(Vec3A::ZERO), 0.0);
        assert_eq!(f.normal(Vec3A::ZERO), None);
        assert_eq!(f.color(Vec3A::ZERO), None);
        assert!(f.distance(Vec3A::ZERO).is_finite());
    }
}
