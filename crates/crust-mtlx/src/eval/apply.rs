//! The interpreter: one instruction's value, and the MaterialX node
//! implementations it calls.

use super::ShadeCtx;
use super::op::{BinOp, Op, UnOp};
use crate::value::Val;
use glam::Vec3A;

/// One instruction's value, given the slots computed before it.
///
/// Shared by [`Program::eval`] and the constant folder in
/// [`Program::optimize`], which is what makes a folded constant exactly the
/// value the interpreter would have produced.
#[inline(always)]
pub(super) fn apply(op: &Op, slots: &[Val], ctx: &ShadeCtx) -> Val {
    // Every operand index was emitted before this instruction, so the slot
    // exists. `get` rather than indexing keeps a malformed program from
    // panicking inside the integrator.
    let g = |i: u32| -> Val { slots.get(i as usize).copied().unwrap_or(Val::ZERO) };
    match op {
        Op::Const(v) => *v,
        Op::Texture {
            tex,
            fallback,
            scale,
            offset,
            arity,
            shift,
            coord,
        } => match tex {
            Some(t) => {
                let (u, v) = match coord {
                    Some(c) => {
                        let c = g(*c);
                        // A `float` coordinate is both axes, as MaterialX's
                        // implicit promotion to `vector2` makes it.
                        if c.arity == 1 {
                            (c.x(), c.x())
                        } else {
                            (c.v[0], c.v[1])
                        }
                    }
                    None => shifted_uv(ctx, *shift),
                };
                let u = u * scale[0] + offset[0];
                let v = v * scale[1] + offset[1];
                // `uvtiling` scales the coordinates, so it scales the
                // footprint with them: a texture tiled 10× is being
                // minified 10× and must read a coarser level to match.
                // The two axes are averaged because the width is one
                // isotropic number.
                let w = ctx.uv_width * 0.5 * (scale[0].abs() + scale[1].abs());
                let rgba = t.eval(u, v, w);
                Val {
                    v: rgba,
                    arity: *arity,
                }
            }
            None => *fallback,
        },
        Op::TexCoord { shift } => {
            let (u, v) = shifted_uv(ctx, *shift);
            Val::vec2(u, v)
        }
        Op::Normal => ctx.normal.into(),
        Op::ViewDirection => ctx.view.into(),
        Op::Position => ctx.position.into(),
        Op::Unary { op, a } => {
            let a = g(*a);
            match op {
                UnOp::Abs => a.map(f32::abs),
                // Guarded so a zero or negative operand — which the
                // teapot's Beer-Lambert chain can produce from a
                // black texel — yields a finite value instead of an
                // infinity that then poisons every downstream lane.
                // The guard is OSL's `safe_log`, which the MaterialX
                // reference runs: the operand is raised to the smallest
                // normal float, so `ln(0)` is `ln(f32::MIN_POSITIVE)`.
                UnOp::Ln => a.map(|x| x.max(f32::MIN_POSITIVE).ln()),
                UnOp::Exp => a.map(|x| x.clamp(-88.0, 88.0).exp()),
                UnOp::Sin => a.map(f32::sin),
                UnOp::Cos => a.map(f32::cos),
                UnOp::Asin => a.map(|x| x.clamp(-1.0, 1.0).asin()),
                UnOp::Acos => a.map(|x| x.clamp(-1.0, 1.0).acos()),
                UnOp::Sqrt => a.map(|x| x.max(0.0).sqrt()),
                // Not `f32::signum`, which answers ±1 for ±0: MaterialX's
                // `sign` of zero is zero.
                UnOp::Sign => a.map(|x| {
                    if x > 0.0 {
                        1.0
                    } else if x < 0.0 {
                        -1.0
                    } else {
                        x
                    }
                }),
                UnOp::Floor => a.map(f32::floor),
                UnOp::Ceil => a.map(f32::ceil),
                UnOp::Normalize => normalize(a),
            }
        }
        Op::Binary { op, a, b } => {
            let (a, b) = (g(*a), g(*b));
            match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                // A zero divisor is a real possibility in these
                // graphs (`1 / transmittance` with a black channel),
                // and an infinity survives every later multiply.
                BinOp::Div => a.zip(b, |x, y| if y.abs() > 1e-20 { x / y } else { 0.0 }),
                BinOp::Pow => a.zip(b, safe_pow),
                BinOp::Min => a.zip(b, f32::min),
                BinOp::Max => a.zip(b, f32::max),
                BinOp::Modulo => a.zip(b, floored_mod),
            }
        }
        // `bg·(1−m) + fg·m`. Written as two weighted terms rather
        // than `bg + (fg−bg)·m` so that a `float` mix against wider
        // operands broadcasts through `zip`'s promotion in both
        // terms alike.
        Op::Mix { fg, bg, m } => {
            let (fg, bg, m) = (g(*fg), g(*bg), g(*m));
            let inv = m.map(|x| 1.0 - x);
            bg * inv + fg * m
        }
        // `max(min(in, high), low)`, OSL's order: it only shows when
        // `low > high`, where `low` wins.
        Op::Clamp { a, low, high } => {
            let (a, lo, hi) = (g(*a), g(*low), g(*high));
            a.zip(hi, f32::min).zip(lo, f32::max)
        }
        Op::Contrast { a, amount, pivot } => {
            let (a, amt, piv) = (g(*a), g(*amount), g(*pivot));
            (a - piv) * amt + piv
        }
        Op::Remap {
            a,
            in_low,
            in_high,
            out_low,
            out_high,
        } => {
            let (a, il, ih, ol, oh) = (g(*a), g(*in_low), g(*in_high), g(*out_low), g(*out_high));
            let t = (a - il).zip(ih - il, |x, d| if d.abs() > 1e-20 { x / d } else { 0.0 });
            ol + (oh - ol) * t
        }
        Op::Invert { a, amount } => g(*amount) - g(*a),
        Op::Convert { a, arity } => convert(g(*a), *arity),
        Op::Extract { a, index } => Val::float(g(*a).v[(*index).min(3)]),
        Op::Combine3 { a, b, c } => Val::vec3(g(*a).x(), g(*b).x(), g(*c).x()),
        Op::Combine2 { a, b } => combine2(g(*a), g(*b)),
        Op::DotProduct { a, b } => dot(g(*a), g(*b)),
        Op::Luminance { a, coeffs } => {
            let a = g(*a);
            let l = a.rgb().dot(g(*coeffs).rgb());
            // The input's width: `color3` is the grey `(l, l, l)`, and
            // `color4` keeps its alpha.
            if a.arity == 4 {
                Val::vec4(l, l, l, a.v[3])
            } else {
                Val::float(l).broadcast_to(a.arity)
            }
        }
        Op::NormalMap { a, scale } => normal_map(g(*a), g(*scale), ctx).into(),
        Op::ArtisticIor {
            reflectivity,
            edge,
            extinction,
        } => {
            let (n, k) = artistic_ior(g(*reflectivity).rgb(), g(*edge).rgb());
            if *extinction { k.into() } else { n.into() }
        }
        Op::Smoothstep { a, low, high } => {
            let (a, lo, hi) = (g(*a), g(*low), g(*high));
            let n = a.arity.max(lo.arity).max(hi.arity);
            let (a, lo, hi) = (a.broadcast_to(n), lo.broadcast_to(n), hi.broadcast_to(n));
            Val {
                v: [0, 1, 2, 3].map(|i| smoothstep(a.v[i], lo.v[i], hi.v[i])),
                arity: n,
            }
        }
        Op::HsvAdjust { a, amount } => {
            let hsv = rgb_to_hsv(g(*a).rgb());
            let m = g(*amount).rgb();
            hsv_to_rgb(Vec3A::new(hsv.x + m.x, hsv.y * m.y, hsv.z * m.z)).into()
        }
        Op::HeightToNormal {
            xp,
            xm,
            yp,
            ym,
            scale,
        } => height_to_normal(
            g(*xp).x() - g(*xm).x(),
            g(*yp).x() - g(*ym).x(),
            g(*scale).x(),
        )
        .into(),
    }
}

/// The chart coordinates `shift` footprint widths away from the shading
/// point.
///
/// A zero shift returns `ctx.uv` untouched rather than adding `0 · width`,
/// which keeps every ordinary lookup bit-identical to what it was before
/// shifts existed (and to the JIT's inline texture path, which never sees a
/// shifted op). A shift over a zero footprint — no ray cone — lands back on
/// the shading point, so a derivative taken from shifted copies reads zero.
#[inline]
fn shifted_uv(ctx: &ShadeCtx, shift: [f32; 2]) -> (f32, f32) {
    if shift == [0.0, 0.0] {
        ctx.uv
    } else {
        (
            ctx.uv.0 + shift[0] * ctx.uv_width,
            ctx.uv.1 + shift[1] * ctx.uv_width,
        )
    }
}

/// MaterialX's OSL `mx_heighttonormal_vector3`, given the height's change
/// across one footprint along `u` (`du`) and `v` (`dv`).
///
/// The reference reads `dx = -Dx(in)`, `dy = Dy(in)`: screen-space
/// derivatives, i.e. the height's change across one pixel. The footprint is
/// this renderer's pixel (a ray cone's width in chart units), so the change
/// across it is the same quantity, with `Dx` running along `u`. Raster `y`
/// runs *down* while `v` runs up, so `Dy(in) = -dv` and both lateral
/// components come out as `-dh`: the normal of a surface raised by `in`.
/// That also makes the result resolution-dependent exactly as the
/// reference's is — a bump reads steeper the coarser the footprint.
fn height_to_normal(du: f32, dv: f32, scale: f32) -> Vec3A {
    let (dx, dy) = (-du, -dv);
    let dz = scale.max(1.0e-5) * (1.0 - dx * dx - dy * dy).max(1.0e-5).sqrt();
    Vec3A::new(dx, dy, dz).normalize_or(Vec3A::Z) * 0.5 + Vec3A::splat(0.5)
}

/// MaterialX's `mx_rgbtohsv` (Foley & van Dam, via OSL), transcribed.
pub(super) fn rgb_to_hsv(c: Vec3A) -> Vec3A {
    let (r, g, b) = (c.x, c.y, c.z);
    let min = r.min(g.min(b));
    let max = r.max(g.max(b));
    let delta = max - min;
    let s = if max > 0.0 { delta / max } else { 0.0 };
    let h = if s <= 0.0 {
        0.0
    } else {
        let h = if r >= max {
            (g - b) / delta
        } else if g >= max {
            2.0 + (b - r) / delta
        } else {
            4.0 + (r - g) / delta
        } * (1.0 / 6.0);
        if h < 0.0 { h + 1.0 } else { h }
    };
    Vec3A::new(h, s, max)
}

/// MaterialX's `mx_hsvtorgb`, transcribed. The hue wraps, so a hue shift
/// past 1 comes round again.
pub(super) fn hsv_to_rgb(hsv: Vec3A) -> Vec3A {
    let (h, s, v) = (hsv.x, hsv.y, hsv.z);
    if s < 0.0001 {
        return Vec3A::splat(v);
    }
    let h = 6.0 * (h - h.floor());
    // `h` is in [0, 6) up to rounding; a non-finite hue lands in the last
    // sextant rather than anywhere undefined.
    let hi = h.trunc();
    let f = h - hi;
    let p = v * (1.0 - s);
    let q = v * (1.0 - s * f);
    let t = v * (1.0 - s * (1.0 - f));
    match hi as i32 {
        0 => Vec3A::new(v, t, p),
        1 => Vec3A::new(q, v, p),
        2 => Vec3A::new(p, v, t),
        3 => Vec3A::new(p, q, v),
        4 => Vec3A::new(t, p, v),
        _ => Vec3A::new(v, p, q),
    }
}

/// MaterialX `normalize`, over the value's own lanes. A zero-length input is
/// returned unchanged rather than divided into NaNs. A `float` broadcasts to a
/// `vector3`, as it always did here (MaterialX has no `float` variant).
fn normalize(a: Val) -> Val {
    match a.arity {
        4 => {
            let v = glam::Vec4::from_array(a.v);
            let n = v.length();
            if n > 1e-20 {
                let [x, y, z, w] = (v / n).to_array();
                Val::vec4(x, y, z, w)
            } else {
                a
            }
        }
        arity => {
            // Lanes past a `vector2`'s are whatever the op that made it left
            // there, so they are zeroed before they can enter the length.
            let v = if arity == 2 {
                Vec3A::new(a.v[0], a.v[1], 0.0)
            } else {
                a.rgb()
            };
            let n = v.length();
            if n > 1e-20 {
                let r = v / n;
                if arity == 2 {
                    Val::vec2(r.x, r.y)
                } else {
                    r.into()
                }
            } else {
                a
            }
        }
    }
}

/// MaterialX `dotproduct` over the operands' lanes. The `vector3` sum is
/// glam's, as it always was; the others extend it. (A `float` operand's
/// lanes all hold its value, so reading them broadcasts it.)
fn dot(a: Val, b: Val) -> Val {
    let n = a.arity.max(b.arity);
    let lanes = |v: Val| match n {
        // A `vector2`'s third lane is not its own; see `normalize`.
        2 => Vec3A::new(v.v[0], v.v[1], 0.0),
        _ => v.rgb(),
    };
    let d = lanes(a).dot(lanes(b));
    Val::float(if n == 4 { d + a.v[3] * b.v[3] } else { d })
}

/// MaterialX `convert`. A `float` broadcasts. Widening a wider value fills
/// the new lanes the way the nodedefs do — zero, except that a `color4` /
/// `vector4` made from fewer lanes gets `1` in its last (an opaque alpha).
/// Narrowing keeps the leading lanes; to a `float`, the first.
fn convert(a: Val, arity: u8) -> Val {
    let arity = arity.clamp(1, 4);
    if a.arity == 1 {
        return a.with_arity(arity);
    }
    if arity == 1 {
        // Every lane of a `float` holds its value; see `Val::float`.
        return Val::float(a.v[0]);
    }
    let mut v = a.v;
    for (i, lane) in v.iter_mut().enumerate().skip(a.arity as usize) {
        *lane = if i == 3 { 1.0 } else { 0.0 };
    }
    Val { v, arity }
}

/// MaterialX `combine2`: the lanes of `a`, then of `b`. Covers every
/// signature — `(float, float)` → `vector2`, `(color3, float)` → `color4`,
/// `(vector3, float)` and `(vector2, vector2)` → `vector4`.
fn combine2(a: Val, b: Val) -> Val {
    let mut v = [0.0; 4];
    let (na, nb) = (a.arity as usize, b.arity as usize);
    v[..na].copy_from_slice(&a.v[..na]);
    let nb = nb.min(4 - na.min(4));
    v[na..na + nb].copy_from_slice(&b.v[..nb]);
    Val {
        v,
        arity: (na + nb) as u8,
    }
}

/// MaterialX's `modulo`, OSL's `mod`: floored, so the result takes the
/// divisor's sign (`-0.2 mod 1` is `0.8`) where Rust's `%` truncates, and a
/// zero divisor returns the dividend, as OSL's does.
///
/// OSL's `x − y·floor(x / y)` is kept wherever its quotient is finite, so
/// the rounding matches the reference (at `-1 mod -0.2` the quotient rounds
/// to 5 and the result to 0, a period away from the exact −0.19999999). Its
/// quotient overflows for some finite operands, though (`1 mod 1e-40` would
/// be `−inf`), and there the exact remainder `%` takes over, moved by one `y`
/// when its sign is the dividend's rather than the divisor's.
fn floored_mod(x: f32, y: f32) -> f32 {
    if y == 0.0 {
        return x;
    }
    let q = (x / y).floor();
    if q.is_finite() {
        return x - y * q;
    }
    let r = x % y;
    if r != 0.0 && (r < 0.0) != (y < 0.0) {
        r + y
    } else {
        r
    }
}

/// OSL's `pow` (OIIO `safe_pow`), which MaterialX's `power` is: `x^0` is one,
/// `0^y` zero, a negative base takes only integer exponents (zero otherwise),
/// and the result is clamped finite.
fn safe_pow(x: f32, y: f32) -> f32 {
    if y == 0.0 {
        return 1.0;
    }
    if x == 0.0 {
        return 0.0;
    }
    if x < 0.0 && y != y.floor() {
        return 0.0;
    }
    x.powf(y).clamp(-f32::MAX, f32::MAX)
}

/// OSL's `smoothstep(low, high, x)`, which MaterialX's is: zero below `low`,
/// one from `high` up, the Hermite ramp between. With `low >= high` the first
/// two tests decide every `x`, so no division by the empty interval happens.
fn smoothstep(x: f32, low: f32, high: f32) -> f32 {
    if x < low {
        0.0
    } else if x >= high {
        1.0
    } else {
        let t = (x - low) / (high - low);
        t * t * (3.0 - 2.0 * t)
    }
}

/// MaterialX `normalmap`: decode `[0,1]`-encoded tangent-space vector, scale
/// its lateral components, and rotate it into world space.
///
/// Falls back to the geometric normal when there is no tangent — the host
/// says when that happens (in crust, only baked single-placement geometry
/// carries one; see crust-core's `UvMap::tangents`). Returning the
/// geometric normal is the right degradation: a normal map's *mean* is the
/// surface normal, so the flat surface is the map's own zero.
fn normal_map(encoded: Val, scale: Val, ctx: &ShadeCtx) -> Vec3A {
    let v = encoded.rgb() * 2.0 - Vec3A::ONE;
    // `scale` is a `float` or, per axis, a `vector2`.
    let (sx, sy) = if scale.arity >= 2 {
        (scale.v[0], scale.v[1])
    } else {
        (scale.x(), scale.x())
    };
    let finite = |s: f32| if s.is_finite() { s } else { 1.0 };
    let local = Vec3A::new(v.x * finite(sx), v.y * finite(sy), v.z.max(1e-4));
    perturb_normal(local, ctx.normal, ctx.tangent)
}

/// Rotates a **decoded** tangent-space normal (`z` along `normal`) into world
/// space, against `tangent` re-orthogonalised to `normal`.
///
/// The half of [`normal_map`] that knows nothing about MaterialX's `[0,1]`
/// encoding, public so a host with its own decode — UsdPreviewSurface's
/// `normal` input arrives already in `[-1,1]`, its UsdUVTexture's
/// `scale`/`bias` having done the decode — rotates it identically. Returns
/// `normal` unchanged when `tangent` is zero (no chart frame) or parallel to
/// it, for the reason [`normal_map`] gives.
pub fn perturb_normal(local: Vec3A, normal: Vec3A, tangent: Vec3A) -> Vec3A {
    if tangent.length_squared() < 1e-20 {
        return normal;
    }
    let n = normal;
    // Re-orthogonalise: the stored tangent is the triangle's, while `n` may
    // already carry interpolated shading curvature, so the two need not be
    // perpendicular.
    let t = (tangent - n * n.dot(tangent)).normalize_or_zero();
    if t.length_squared() < 1e-20 {
        return n;
    }
    let b = n.cross(t);
    let world = t * local.x + b * local.y + n * local.z;
    if world.length_squared() > 1e-20 {
        world.normalize()
    } else {
        n
    }
}

/// Gulbrandsen's "Artist Friendly Metallic Fresnel": normal-incidence
/// reflectivity and grazing edge tint → complex IOR.
///
/// Implemented rather than short-circuited (the conductor lobe wants a
/// reflectivity back, which is what went in) because a graph may author `ior`
/// and `extinction` directly, and the round trip through
/// [`reflectivity_from_ior`] then handles both authorings with one path.
pub(super) fn artistic_ior(reflectivity: Vec3A, edge: Vec3A) -> (Vec3A, Vec3A) {
    let r = reflectivity.clamp(Vec3A::ZERO, Vec3A::splat(0.99));
    let rs = Vec3A::new(r.x.sqrt(), r.y.sqrt(), r.z.sqrt());
    let n_min = (Vec3A::ONE - r) / (Vec3A::ONE + r);
    let n_max = (Vec3A::ONE + rs) / (Vec3A::ONE - rs).max(Vec3A::splat(1e-6));
    // OSL's `mix(n_max, n_min, edge)`, as `x·(1 − t) + y·t`: an edge colour
    // of white — the default — selects `n_min` exactly, where `x + (y − x)·t`
    // would leave `n_max`'s rounding in it (n_max is ~70 for a bright metal).
    // Clamped, unlike the reference: an edge tint outside [0, 1] extrapolates
    // the IOR to nonsense, down to negative values.
    let e = edge.clamp(Vec3A::ZERO, Vec3A::ONE);
    let n = n_max * (Vec3A::ONE - e) + n_min * e;
    let np1 = n + Vec3A::ONE;
    let nm1 = n - Vec3A::ONE;
    let k2 =
        ((np1 * np1 * r - nm1 * nm1) / (Vec3A::ONE - r).max(Vec3A::splat(1e-6))).max(Vec3A::ZERO);
    (n, Vec3A::new(k2.x.sqrt(), k2.y.sqrt(), k2.z.sqrt()))
}

/// Normal-incidence reflectivity of a conductor with complex IOR `n + ik`.
///
/// The exact inverse of `artistic_ior` (the node's implementation above), which
/// is what lets a conductor lobe
/// be reduced to the one colour OpenPBR's metal lobe takes, whichever way the
/// graph authored it.
pub fn reflectivity_from_ior(n: Vec3A, k: Vec3A) -> Vec3A {
    let num = (n - Vec3A::ONE) * (n - Vec3A::ONE) + k * k;
    let den = ((n + Vec3A::ONE) * (n + Vec3A::ONE) + k * k).max(Vec3A::splat(1e-6));
    (num / den).clamp(Vec3A::ZERO, Vec3A::ONE)
}
