//! The curve-mass-field spike (openspec change `curve-mass-field`, tasks
//! 1.1–1.3): does a field baked from the strands hide where they cross?
//!
//! Reads every linear `BasisCurves` prim of a stage (no transforms), bakes a
//! dense, unbricked mass field from them — coverage splatted with a compact
//! kernel and weighted by width, a coverage-weighted colour from each prim's
//! `displayColor`, and the signed distance to the coverage threshold by fast
//! sweeping — then casts orthographic rays straight down through the
//! renderer's own world and compares, at each first hit, the tube's normal and
//! the strand's colour against the field's normal and colour.
//!
//! It prints the gradient energy of each across the line `x = 0`, where
//! `samples/mass_spike.usda`'s two sheets cross, against the same measure away
//! from it, and how far the field's normal moves when every strand point is
//! jittered from frame to frame. It writes the four images it measured.
//!
//! ```sh
//! cargo run --release -p crust-render --example mass_spike -- \
//!     samples/mass_spike.usda [radius 0.12] [threshold 0.5] [out dir .] [voxel / radius 0.5] \
//!     [smooth 1]
//!
//! The normal is the central-difference gradient of the trilinear distance —
//! which, with a one-voxel step, is the voxels' own central differences
//! interpolated trilinearly — taken after one separable binomial (1 2 1)/4
//! pass over the distance unless `smooth` is 0. The design record has what
//! the spike measured (`openspec/changes/curve-mass-field/design.md`).
//! ```

use crust_core::{Ray, Scene, Vec3A};
use openusd::{sdf, usd};
use std::path::{Path, PathBuf};

/// One strand: its polyline, width and colour.
struct Strand {
    points: Vec<Vec3A>,
    width: f32,
    colour: Vec3A,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(stage_path) = args.first() else {
        eprintln!(
            "usage: mass_spike <stage.usda> [radius] [threshold] [out dir] [voxel/radius] [smooth]"
        );
        std::process::exit(2);
    };
    let arg = |i: usize, default: f32| -> f32 {
        args.get(i)
            .map(|a| a.parse().expect("a number"))
            .unwrap_or(default)
    };
    let (radius, threshold) = (arg(1, 0.12), arg(2, 0.5));
    let out = PathBuf::from(args.get(3).map_or(".", String::as_str));
    let voxel = arg(4, 0.5);
    let smooth = arg(5, 1.0) != 0.0;
    std::fs::create_dir_all(&out).expect("output directory");

    let strands = read_strands(stage_path);
    println!(
        "{} strands, radius {radius}, threshold {threshold}, voxel {voxel} radius, smoothed {smooth}",
        strands.len()
    );
    let started = std::time::Instant::now();
    let field = Field::bake(&strands, radius, threshold, voxel, smooth);
    println!(
        "field {}x{}x{} voxels of {} ({:.2} s)",
        field.n[0],
        field.n[1],
        field.n[2],
        field.h,
        started.elapsed().as_secs_f32()
    );
    if !field.surface {
        println!(
            "coverage never reaches the threshold {threshold}: the field has no surface, and no \
             hit reads a mass normal"
        );
    }

    let scene = Scene::from_usd(Path::new(stage_path)).expect("the stage loads");
    let view = View::cast(&scene, &strands, &field);
    view.write(&out);
    view.report(radius);
    jitter_report(&strands, &view, radius, threshold, voxel, smooth);
}

/// Every linear `BasisCurves` under the stage's default prim, read as authored.
fn read_strands(path: &str) -> Vec<Strand> {
    let stage = usd::Stage::builder()
        .schema_registry(openusd_schemas::schema_registry())
        .load(usd::InitialLoadSet::LoadAll)
        .open(path)
        .expect("the stage opens");
    let world = stage
        .prim(sdf::Path::new("/World").expect("a path"))
        .expect("a /World prim");
    let mut strands = Vec::new();
    for prim in world.children().expect("children") {
        if prim.type_name().ok().flatten().unwrap_or_default() != "BasisCurves" {
            continue;
        }
        let value = |name: &str| prim.attribute(name).get::<sdf::Value>().ok().flatten();
        // The field is baked from straight segments between authored points,
        // which is the curve the renderer traces only for `linear`; USD's
        // default type is `cubic`.
        let ty = match value("type") {
            Some(sdf::Value::Token(t)) => t.as_str().to_owned(),
            _ => "cubic".to_owned(),
        };
        if ty != "linear" {
            eprintln!(
                "{}: type is {ty}; the spike bakes linear curves only",
                prim.path()
            );
            std::process::exit(2);
        }
        let Some(sdf::Value::Vec3fVec(points)) = value("points") else {
            continue;
        };
        let Some(sdf::Value::IntVec(counts)) = value("curveVertexCounts") else {
            continue;
        };
        let width = match value("widths") {
            Some(sdf::Value::FloatVec(w)) => w.first().copied().unwrap_or(0.01),
            _ => 0.01,
        };
        let colour = match value("primvars:displayColor") {
            Some(sdf::Value::Vec3fVec(c)) => c
                .first()
                .map_or(Vec3A::splat(0.5), |c| Vec3A::new(c.x, c.y, c.z)),
            _ => Vec3A::splat(0.5),
        };
        let mut at = 0usize;
        for &n in &counts {
            let n = n.max(0) as usize;
            strands.push(Strand {
                points: points[at..at + n]
                    .iter()
                    .map(|p| Vec3A::new(p.x, p.y, p.z))
                    .collect(),
                width,
                colour,
            });
            at += n;
        }
    }
    strands
}

/// The smooth compact splat kernel, 1 at the centre and 0 at `r = 1`.
fn kernel(r: f32) -> f32 {
    if r >= 1.0 {
        0.0
    } else {
        let s = 1.0 - r * r;
        s * s * s
    }
}

/// `∫ kernel(|t|) dt` over `t ∈ [-1, 1]`: what a straight line through a point
/// deposits there, per unit of radius.
const LINE_INTEGRAL: f32 = 32.0 / 35.0;

/// A dense mass field: coverage, coverage-weighted colour, signed distance.
struct Field {
    min: Vec3A,
    h: f32,
    n: [usize; 3],
    rho: Vec<f32>,
    /// `Σ w k c` and `Σ w k`, interpolated separately so the mean stays a
    /// mean between voxels.
    colour: Vec<Vec3A>,
    weight: Vec<f32>,
    dist: Vec<f32>,
    /// Per-voxel central-difference gradients of the (smoothed) distance.
    grad: Vec<Vec3A>,
    /// Whether coverage reaches the threshold anywhere.
    surface: bool,
}

impl Field {
    fn bake(strands: &[Strand], radius: f32, threshold: f32, voxel: f32, smooth: bool) -> Field {
        let h = radius * voxel;
        let (mut lo, mut hi) = (Vec3A::splat(f32::MAX), Vec3A::splat(f32::MIN));
        for p in strands.iter().flat_map(|s| &s.points) {
            lo = lo.min(*p);
            hi = hi.max(*p);
        }
        let pad = Vec3A::splat(3.0 * radius);
        let min = lo - pad;
        let size = hi + pad - min;
        let n = [
            (size.x / h).ceil() as usize + 1,
            (size.y / h).ceil() as usize + 1,
            (size.z / h).ceil() as usize + 1,
        ];
        let cells = n[0] * n[1] * n[2];
        let mut f = Field {
            min,
            h,
            n,
            rho: vec![0.0; cells],
            colour: vec![Vec3A::ZERO; cells],
            weight: vec![0.0; cells],
            dist: vec![0.0; cells],
            grad: Vec::new(),
            surface: false,
        };
        // Samples a quarter voxel apart, each standing for `ds` of strand:
        // a lone line as wide as the radius sums to coverage 1 on its axis.
        let step = h / 4.0;
        let reach = (radius / h).ceil() as isize;
        for s in strands {
            for seg in s.points.windows(2) {
                let (a, b) = (seg[0], seg[1]);
                let len = (b - a).length();
                let count = (len / step).ceil().max(1.0) as usize;
                let ds = len / count as f32;
                let w = (s.width / radius) * ds / (radius * LINE_INTEGRAL);
                for i in 0..count {
                    let p = a + (b - a) * ((i as f32 + 0.5) / count as f32);
                    let c = f.cell_of(p);
                    for dz in -reach..=reach {
                        for dy in -reach..=reach {
                            for dx in -reach..=reach {
                                let Some(idx) = f.index(c[0] + dx, c[1] + dy, c[2] + dz) else {
                                    continue;
                                };
                                let k = kernel((f.position(idx) - p).length() / radius);
                                if k > 0.0 {
                                    f.rho[idx] += w * k;
                                    f.colour[idx] += s.colour * (w * k);
                                    f.weight[idx] += w * k;
                                }
                            }
                        }
                    }
                }
            }
        }
        f.signed_distance(threshold);
        let d = if smooth {
            f.smoothed_distance()
        } else {
            f.dist.clone()
        };
        f.grad = (0..d.len()).map(|i| f.voxel_gradient(&d, i)).collect();
        f
    }

    /// One pass of the separable binomial filter (1 2 1)/4 along each axis.
    fn smoothed_distance(&self) -> Vec<f32> {
        let mut d = self.dist.clone();
        for axis in 0..3 {
            let src = d.clone();
            for (i, out) in d.iter_mut().enumerate() {
                let c = self.coords(i);
                let at = |k: isize| -> f32 {
                    let mut q = c;
                    q[axis] += k;
                    self.index(q[0], q[1], q[2]).map_or(src[i], |j| src[j])
                };
                *out = 0.25 * at(-1) + 0.5 * src[i] + 0.25 * at(1);
            }
        }
        d
    }

    fn coords(&self, i: usize) -> [isize; 3] {
        let (nx, ny) = (self.n[0], self.n[1]);
        [
            (i % nx) as isize,
            ((i / nx) % ny) as isize,
            (i / (nx * ny)) as isize,
        ]
    }

    /// The central-difference gradient of `d` at voxel `i` (one-sided at the
    /// grid's faces).
    fn voxel_gradient(&self, d: &[f32], i: usize) -> Vec3A {
        let c = self.coords(i);
        let mut g = [0.0f32; 3];
        for (axis, g) in g.iter_mut().enumerate() {
            let step = |k: isize| {
                let mut q = c;
                q[axis] += k;
                self.index(q[0], q[1], q[2])
            };
            *g = match (step(-1), step(1)) {
                (Some(a), Some(b)) => (d[b] - d[a]) / (2.0 * self.h),
                (None, Some(b)) => (d[b] - d[i]) / self.h,
                (Some(a), None) => (d[i] - d[a]) / self.h,
                (None, None) => 0.0,
            };
        }
        Vec3A::from(g)
    }

    fn cell_of(&self, p: Vec3A) -> [isize; 3] {
        let q = (p - self.min) / self.h;
        [
            q.x.round() as isize,
            q.y.round() as isize,
            q.z.round() as isize,
        ]
    }

    fn index(&self, x: isize, y: isize, z: isize) -> Option<usize> {
        let [nx, ny, nz] = self.n.map(|v| v as isize);
        ((0..nx).contains(&x) && (0..ny).contains(&y) && (0..nz).contains(&z))
            .then(|| (x + nx * (y + ny * z)) as usize)
    }

    fn position(&self, idx: usize) -> Vec3A {
        let (nx, ny) = (self.n[0], self.n[1]);
        let (x, y, z) = (idx % nx, (idx / nx) % ny, idx / (nx * ny));
        self.min + Vec3A::new(x as f32, y as f32, z as f32) * self.h
    }

    /// The signed distance to `rho = threshold`, negative inside: interface
    /// voxels seeded by linear interpolation along each axis, then the
    /// unsigned distance swept out in the eight diagonal orders (Zhao's fast
    /// sweeping, Godunov upwind update), twice over.
    fn signed_distance(&mut self, threshold: f32) {
        let [nx, ny, nz] = self.n;
        let phi: Vec<f32> = self.rho.iter().map(|r| r - threshold).collect();
        let mut u = vec![f32::INFINITY; phi.len()];
        let mut fixed = vec![false; phi.len()];
        for z in 0..nz {
            for y in 0..ny {
                for x in 0..nx {
                    let i = x + nx * (y + ny * z);
                    let mut best = f32::INFINITY;
                    for (dx, dy, dz) in [
                        (1, 0, 0),
                        (-1, 0, 0),
                        (0, 1, 0),
                        (0, -1, 0),
                        (0, 0, 1),
                        (0, 0, -1),
                    ] {
                        let Some(j) = self.index(x as isize + dx, y as isize + dy, z as isize + dz)
                        else {
                            continue;
                        };
                        if (phi[i] > 0.0) != (phi[j] > 0.0) {
                            let t = phi[i] / (phi[i] - phi[j]);
                            best = best.min(t * self.h);
                        }
                    }
                    if best.is_finite() {
                        u[i] = best;
                        fixed[i] = true;
                    }
                }
            }
        }
        let h = self.h;
        for _ in 0..2 {
            for order in 0..8 {
                let xs: Vec<usize> = if order & 1 == 0 {
                    (0..nx).collect()
                } else {
                    (0..nx).rev().collect()
                };
                let ys: Vec<usize> = if order & 2 == 0 {
                    (0..ny).collect()
                } else {
                    (0..ny).rev().collect()
                };
                let zs: Vec<usize> = if order & 4 == 0 {
                    (0..nz).collect()
                } else {
                    (0..nz).rev().collect()
                };
                for &z in &zs {
                    for &y in &ys {
                        for &x in &xs {
                            let i = x + nx * (y + ny * z);
                            if fixed[i] {
                                continue;
                            }
                            let along = |d: [isize; 3]| -> f32 {
                                let a = self.index(
                                    x as isize - d[0],
                                    y as isize - d[1],
                                    z as isize - d[2],
                                );
                                let b = self.index(
                                    x as isize + d[0],
                                    y as isize + d[1],
                                    z as isize + d[2],
                                );
                                a.map_or(f32::INFINITY, |a| u[a])
                                    .min(b.map_or(f32::INFINITY, |b| u[b]))
                            };
                            let mut abc = [along([1, 0, 0]), along([0, 1, 0]), along([0, 0, 1])];
                            abc.sort_by(f32::total_cmp);
                            let [a, b, c] = abc;
                            if !a.is_finite() {
                                continue;
                            }
                            let mut v = a + h;
                            if v > b {
                                v = 0.5
                                    * (a + b + (2.0 * h * h - (a - b) * (a - b)).max(0.0).sqrt());
                                if v > c {
                                    let s = a + b + c;
                                    let q = s * s - 3.0 * (a * a + b * b + c * c - h * h);
                                    v = (s + q.max(0.0).sqrt()) / 3.0;
                                }
                            }
                            u[i] = u[i].min(v);
                        }
                    }
                }
            }
        }
        self.surface = fixed.iter().any(|f| *f);
        // Clamped beyond a band of three radii (six voxels), as the design
        // has it — which also keeps a field with no surface finite, its
        // gradient zero rather than not a number.
        let band = 6.0 * self.h;
        for (d, (u, p)) in self.dist.iter_mut().zip(u.iter().zip(&phi)) {
            let u = u.min(band);
            *d = if *p > 0.0 { -u } else { u };
        }
    }

    /// Trilinear interpolation of `get` at `p`; `None` outside the grid.
    fn trilinear<T>(&self, p: Vec3A, get: impl Fn(usize) -> T) -> Option<T>
    where
        T: std::ops::Mul<f32, Output = T> + std::ops::Add<Output = T>,
    {
        let q = (p - self.min) / self.h;
        let (x0, y0, z0) = (q.x.floor(), q.y.floor(), q.z.floor());
        let (fx, fy, fz) = (q.x - x0, q.y - y0, q.z - z0);
        let (x0, y0, z0) = (x0 as isize, y0 as isize, z0 as isize);
        let mut sum: Option<T> = None;
        for (dz, wz) in [(0, 1.0 - fz), (1, fz)] {
            for (dy, wy) in [(0, 1.0 - fy), (1, fy)] {
                for (dx, wx) in [(0, 1.0 - fx), (1, fx)] {
                    let i = self.index(x0 + dx, y0 + dy, z0 + dz)?;
                    let term = get(i) * (wx * wy * wz);
                    sum = Some(match sum {
                        Some(s) => s + term,
                        None => term,
                    });
                }
            }
        }
        sum
    }

    /// `normalize(∇d)`, the voxels' gradients interpolated trilinearly.
    fn normal(&self, p: Vec3A) -> Option<Vec3A> {
        let g = self.trilinear(p, |i| self.grad[i])?;
        (g.length_squared() > 0.0).then(|| g.normalize())
    }

    fn colour(&self, p: Vec3A) -> Option<Vec3A> {
        let c = self.trilinear(p, |i| self.colour[i])?;
        let w = self.trilinear(p, |i| self.weight[i])?;
        (w > 0.0).then(|| c / w)
    }
}

/// One signal read off a pixel, for the gradient-energy measure.
type Signal<'a> = &'a dyn Fn(&Pixel) -> Vec3A;

/// What the camera sees at each pixel's first hit.
struct Pixel {
    p: Vec3A,
    tube_normal: Vec3A,
    mass_normal: Vec3A,
    strand_colour: Vec3A,
    mass_colour: Vec3A,
}

struct View {
    width: usize,
    height: usize,
    pixels: Vec<Option<Pixel>>,
    /// Hits the field gave no normal or colour (no surface within its band,
    /// or a vanishing gradient): left out of every measure, and counted.
    unread: usize,
}

/// Pixels per world unit, and the region seen: both sheets and the clump.
const PIXELS_PER_UNIT: f32 = 200.0;
const REGION: [f32; 4] = [-1.1, 2.0, -1.1, 1.1]; // x0, x1, y0, y1

impl View {
    fn cast(scene: &Scene, strands: &[Strand], field: &Field) -> View {
        let width = ((REGION[1] - REGION[0]) * PIXELS_PER_UNIT) as usize;
        let height = ((REGION[3] - REGION[2]) * PIXELS_PER_UNIT) as usize;
        let mut pixels = Vec::with_capacity(width * height);
        let mut unread = 0;
        for j in 0..height {
            for i in 0..width {
                let x = REGION[0] + (i as f32 + 0.5) / PIXELS_PER_UNIT;
                let y = REGION[3] - (j as f32 + 0.5) / PIXELS_PER_UNIT;
                let ray = Ray::new(Vec3A::new(x, y, 5.0), -Vec3A::Z);
                let pixel = scene.world.intersect(&ray, 1e-4, 1e4).and_then(|hit| {
                    let p = hit.rec.p;
                    let field = field.normal(p).zip(field.colour(p));
                    unread += usize::from(field.is_none());
                    let (mass_normal, mass_colour) = field?;
                    Some(Pixel {
                        p,
                        tube_normal: hit.rec.normal,
                        mass_normal,
                        strand_colour: nearest_colour(strands, p),
                        mass_colour,
                    })
                });
                pixels.push(pixel);
            }
        }
        View {
            width,
            height,
            pixels,
            unread,
        }
    }

    fn write(&self, out: &Path) {
        let save = |name: &str, f: &dyn Fn(&Pixel) -> Vec3A| {
            let mut img = image::RgbImage::new(self.width as u32, self.height as u32);
            for (k, px) in self.pixels.iter().enumerate() {
                let c = px
                    .as_ref()
                    .map_or(Vec3A::ZERO, f)
                    .clamp(Vec3A::ZERO, Vec3A::ONE);
                img.put_pixel(
                    (k % self.width) as u32,
                    (k / self.width) as u32,
                    image::Rgb([
                        (c.x * 255.0) as u8,
                        (c.y * 255.0) as u8,
                        (c.z * 255.0) as u8,
                    ]),
                );
            }
            img.save(out.join(name)).expect("write image");
        };
        save("tube_normal.png", &|p| p.tube_normal * 0.5 + 0.5);
        save("mass_normal.png", &|p| p.mass_normal * 0.5 + 0.5);
        save("strand_colour.png", &|p| p.strand_colour);
        save("mass_colour.png", &|p| p.mass_colour);
        println!(
            "wrote tube_normal, mass_normal, strand_colour, mass_colour .png to {}",
            out.display()
        );
    }

    /// Mean squared difference between neighbouring pixels of `f`, over pairs
    /// both of whose hits are on the sheets, lie in `band` of `|x|`, and are
    /// three radii clear of the sheets' ends (where the mass curls round).
    fn energy(&self, f: Signal, band: (f32, f32), radius: f32) -> f32 {
        let on = |k: usize| -> Option<&Pixel> {
            let p = self.pixels[k].as_ref()?;
            (p.p.x < 1.1
                && (band.0..band.1).contains(&p.p.x.abs())
                && p.p.y.abs() < 1.0 - 3.0 * radius)
                .then_some(p)
        };
        let (mut sum, mut pairs) = (0.0f64, 0usize);
        for k in 0..self.pixels.len() {
            let Some(a) = on(k) else { continue };
            let right = (k % self.width + 1 < self.width).then_some(k + 1);
            let down = (k + self.width < self.pixels.len()).then_some(k + self.width);
            for b in [right, down].into_iter().flatten().filter_map(on) {
                sum += f64::from((f(a) - f(b)).length_squared());
                pairs += 1;
            }
        }
        (sum / pairs.max(1) as f64) as f32
    }

    fn report(&self, radius: f32) {
        let crossing = (0.0, 2.0 * radius);
        let away = (0.4, 1.0 - 3.0 * radius);
        println!("\ngradient energy (mean squared neighbour difference), sheets only:");
        println!(
            "  {:<14} {:>12} {:>12} {:>8}",
            "signal", "crossing", "away", "ratio"
        );
        let rows: [(&str, Signal); 4] = [
            ("tube normal", &|p| p.tube_normal),
            ("mass normal", &|p| p.mass_normal),
            ("strand colour", &|p| p.strand_colour),
            ("mass colour", &|p| p.mass_colour),
        ];
        if self.unread > 0 {
            println!(
                "  {} hits read no mass normal or colour (no surface within the band, or a \
                 vanishing gradient) and are left out",
                self.unread
            );
        }
        let mut energies = Vec::new();
        for (name, f) in rows {
            let (c, a) = (
                self.energy(f, crossing, radius),
                self.energy(f, away, radius),
            );
            let ratio = if a > 1e-9 {
                format!("{:.3}", c / a)
            } else {
                "-".into()
            };
            println!("  {name:<14} {c:>12.6} {a:>12.6} {ratio:>8}");
            energies.push((c, a));
        }
        let share = |m: f32, s: f32| {
            if s > 1e-9 {
                format!("{:.2}%", 100.0 * m / s)
            } else {
                "-".into()
            }
        };
        println!(
            "  mass / per-strand at the crossing: normal {}, colour {}; away: normal {}, colour {}",
            share(energies[1].0, energies[0].0),
            share(energies[3].0, energies[2].0),
            share(energies[1].1, energies[0].1),
            share(energies[3].1, energies[2].1),
        );
        for (name, region) in [("sheets", (f32::MIN, 1.1)), ("clump", (1.3, f32::MAX))] {
            let hits: Vec<&Pixel> = self
                .pixels
                .iter()
                .flatten()
                .filter(|p| (region.0..region.1).contains(&p.p.x))
                .collect();
            let away = hits.iter().filter(|p| p.mass_normal.z < 0.0).count();
            println!(
                "  {name}: {} hits, {:.1}% with the mass normal facing away from the camera",
                hits.len(),
                100.0 * away as f32 / hits.len().max(1) as f32
            );
        }
        // The clump: the field's normal at the strands on its silhouette and
        // on its axis, seen from above (the axis is the top of the clump).
        let clump: Vec<&Pixel> = self
            .pixels
            .iter()
            .flatten()
            .filter(|p| p.p.x > 1.3)
            .collect();
        if !clump.is_empty() {
            let edge = clump
                .iter()
                .filter(|p| (p.p.x - 1.6).abs() > 0.2)
                .map(|p| p.mass_normal.x.abs())
                .sum::<f32>()
                / clump
                    .iter()
                    .filter(|p| (p.p.x - 1.6).abs() > 0.2)
                    .count()
                    .max(1) as f32;
            let top = clump
                .iter()
                .filter(|p| (p.p.x - 1.6).abs() < 0.03)
                .map(|p| p.mass_normal.z)
                .sum::<f32>()
                / clump
                    .iter()
                    .filter(|p| (p.p.x - 1.6).abs() < 0.03)
                    .count()
                    .max(1) as f32;
            println!("\nclump: mean |n.x| at its sides {edge:.3}, mean n.z over its axis {top:.3}");
        }
    }
}

/// The colour of the strand nearest `p`.
fn nearest_colour(strands: &[Strand], p: Vec3A) -> Vec3A {
    let mut best = (f32::INFINITY, Vec3A::ZERO);
    for s in strands {
        for seg in s.points.windows(2) {
            let (a, b) = (seg[0], seg[1]);
            let t = ((p - a).dot(b - a) / (b - a).length_squared()).clamp(0.0, 1.0);
            let d = (a + (b - a) * t - p).length_squared();
            if d < best.0 {
                best = (d, s.colour);
            }
        }
    }
    best.1
}

/// A fixed, seedless hash to `[-1, 1)` (splitmix64's finaliser): the same
/// jitter on every run.
fn hash_unit(a: u64, b: u64, c: u64, lane: u64) -> f32 {
    let mut z = a
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(b.wrapping_mul(0xBF58_476D_1CE4_E5B9))
        .wrapping_add(c.wrapping_mul(0x94D0_49BB_1331_11EB))
        .wrapping_add(lane);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 40) as f32 / (1u64 << 23) as f32 - 1.0
}

/// Task 1.3: five frames, each displacing every strand point by up to
/// `amount` world units in each axis, independently per frame — a simulation's
/// jitter does not scale with the bake's radius — and the field's normal read
/// at frame 0's hit points and compared frame to frame.
fn jitter_report(
    strands: &[Strand],
    view: &View,
    radius: f32,
    threshold: f32,
    voxel: f32,
    smooth: bool,
) {
    let points: Vec<Vec3A> = view
        .pixels
        .iter()
        .flatten()
        .map(|p| p.p)
        .step_by(7)
        .collect();
    println!(
        "\nmass normal across 5 jittered frames ({} points; angle between consecutive frames):",
        points.len()
    );
    println!(
        "  {:<8} {:<10} {:>10} {:>10} {:>10}",
        "region", "jitter", "mean °", "p95 °", "max °"
    );
    // A fifth, a half and a whole of the sample's 0.04 strand spacing.
    for amount in [0.008f32, 0.02, 0.04] {
        let mut previous: Option<Vec<Option<Vec3A>>> = None;
        let mut angles: [Vec<f32>; 2] = [Vec::new(), Vec::new()];
        for frame in 0..5u64 {
            let moved: Vec<Strand> = strands
                .iter()
                .enumerate()
                .map(|(si, s)| Strand {
                    points: s
                        .points
                        .iter()
                        .enumerate()
                        .map(|(pi, p)| {
                            let j = |lane| hash_unit(si as u64, pi as u64, frame, lane);
                            *p + Vec3A::new(j(0), j(1), j(2)) * amount
                        })
                        .collect(),
                    width: s.width,
                    colour: s.colour,
                })
                .collect();
            let field = Field::bake(&moved, radius, threshold, voxel, smooth);
            let normals: Vec<Option<Vec3A>> = points.iter().map(|p| field.normal(*p)).collect();
            if let Some(prev) = &previous {
                for ((a, b), p) in prev.iter().zip(&normals).zip(&points) {
                    if let (Some(a), Some(b)) = (a, b) {
                        let region = usize::from(p.x > 1.3);
                        angles[region].push(a.dot(*b).clamp(-1.0, 1.0).acos().to_degrees());
                    }
                }
            }
            previous = Some(normals);
        }
        for (name, mut angles) in ["sheets", "clump"].into_iter().zip(angles) {
            angles.sort_by(f32::total_cmp);
            let mean = angles.iter().sum::<f32>() / angles.len().max(1) as f32;
            let p95 = angles.get(angles.len() * 95 / 100).copied().unwrap_or(0.0);
            let max = angles.last().copied().unwrap_or(0.0);
            println!("  {name:<8} {amount:<10} {mean:>10.2} {p95:>10.2} {max:>10.2}");
        }
    }
}
