//! The light BVH behind [`LightSelection::Tree`](super::LightSelection::Tree):
//! a binary tree over the finite lights, walked from a shading point by an
//! importance bound at each node (Conty Estevez & Kulla 2018, in pbrt-v4's
//! `BVHLightSampler` form).
//!
//! Three things make it one strategy for both halves of MIS:
//! - [`LightBounds::importance`] is one plain-`f32` function, so NEE's walk
//!   and the bounce side's re-walk compute the same bits at every node;
//! - [`LightTree::sample`] and [`LightTree::pmf`] multiply the same per-level
//!   ratios in the same order (root to leaf), the second along the light's
//!   stored bit trail;
//! - the walk is a monotone map from `u` to a leaf, so the stratified samples
//!   that pick a light stay one contiguous slice of the pick dimension.
//!
//! The lighting design record ("Light tree") has the reasoning and the
//! measurements.

use std::f32::consts::PI;

use glam::{Quat, Vec3, Vec3A};

/// The largest `f32` below one: where a remapped `u` is clamped, so that
/// rounding never carries it to one.
const ONE_MINUS_EPSILON: f32 = 1.0 - f32::EPSILON / 2.0;

/// Where light is received: a point, the normal of the surface there and
/// whether that surface is opaque. What every position-dependent selection
/// reads ([`LightList::pick_index_at`](super::LightList::pick_index_at) and
/// its bounce-side twins), so that both MIS halves are handed one value.
///
/// A receiver with no normal (`n` zero) is a volume scatter, or a caller
/// that knows only a position: the selection ignores the surface terms
/// there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Receiver {
    pub p: Vec3A,
    /// The surface normal, facing the side the ray arrived from
    /// (`HitRecord::normal`); zero for none. Stored unaligned, which makes a
    /// receiver 32 bytes rather than 48: the integrator carries one per
    /// path vertex, and moving the aligned form cost `power`, which never
    /// reads it, a measurable share of cornellbox's instructions.
    pub n: Vec3,
    /// Whether the surface reflects only ([`crate::Material::transmits`] is
    /// false): then no light below its horizon can reach it.
    pub opaque: bool,
}

impl Receiver {
    /// A receiver at `p` with no surface: a volume scatter.
    #[inline]
    pub fn at(p: Vec3A) -> Self {
        Self {
            p,
            n: Vec3::ZERO,
            opaque: false,
        }
    }

    /// A surface receiver at `p` with normal `n` (facing the side the ray
    /// arrived from).
    #[inline]
    pub fn surface(p: Vec3A, n: Vec3A, opaque: bool) -> Self {
        Self {
            p,
            n: n.into(),
            opaque,
        }
    }

    /// Whether `self` and `other` are the same bits — what NEE and the
    /// bounce side must agree on.
    pub fn same_bits(&self, other: &Self) -> bool {
        let bits = |v: [f32; 3]| v.map(f32::to_bits);
        bits(self.p.to_array()) == bits(other.p.to_array())
            && bits(self.n.to_array()) == bits(other.n.to_array())
            && self.opaque == other.opaque
    }
}

impl From<Vec3A> for Receiver {
    #[inline]
    fn from(p: Vec3A) -> Self {
        Self::at(p)
    }
}

/// A conservative bound on where a light (or a node of lights) emits from
/// and into which directions: what the tree's importance is computed from.
///
/// Every point the light emits from lies in `[min, max]`, and every direction
/// it emits into lies within `θo + θe` of `w` (or of `±w` when
/// `two_sided`): `θo` bounds the spread of the emitting normals and `θe` the
/// spread of emission about each normal. `phi` is the flux, the light's
/// [`Light::power`](super::Light::power).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LightBounds {
    pub min: Vec3A,
    pub max: Vec3A,
    pub w: Vec3A,
    pub phi: f32,
    pub cos_theta_o: f32,
    pub cos_theta_e: f32,
    pub two_sided: bool,
}

impl LightBounds {
    /// The box's centre.
    #[inline]
    pub fn centroid(&self) -> Vec3A {
        0.5 * (self.min + self.max)
    }

    /// The union of two bounds: the box around both, the cone around both
    /// normal cones, the wider emission spread and the summed flux.
    pub fn union(&self, other: &Self) -> Self {
        let (w, cos_theta_o) = cone_union((self.w, self.cos_theta_o), (other.w, other.cos_theta_o));
        Self {
            min: self.min.min(other.min),
            max: self.max.max(other.max),
            w,
            phi: self.phi + other.phi,
            cos_theta_o,
            cos_theta_e: self.cos_theta_e.min(other.cos_theta_e),
            two_sided: self.two_sided || other.two_sided,
        }
    }

    /// An upper bound, up to a constant, on what the lights inside these
    /// bounds can deliver to `r` (pbrt-v4's `LightBounds::Importance`): the
    /// flux over the squared distance, times the cosine at the light of the
    /// most favourable emission direction toward `r`, times the cosine at the
    /// receiver of the most favourable direction to the box. Zero only where
    /// no light inside can reach `r`: outside every emission cone, or wholly
    /// below an opaque receiver's horizon.
    ///
    /// Plain `f32` in a fixed order, no `mul_add`, and one function for every
    /// caller: NEE and the bounce side must get the same bits.
    #[inline]
    pub fn importance(&self, r: &Receiver) -> f32 {
        let pc = self.centroid();
        let half_diagonal = 0.5 * (self.max - self.min).length();
        // pbrt-v4 clamps by the half diagonal (a length, against a squared
        // distance): a receiver inside the box is not divided by ~0.
        let d2 =
            r.p.distance_squared(pc)
                .max(half_diagonal)
                .max(f32::MIN_POSITIVE);
        // From the light toward the receiver.
        let wi = (r.p - pc).normalize_or_zero();
        let mut cos_w = self.w.dot(wi);
        if self.two_sided {
            cos_w = cos_w.abs();
        }
        let sin_w = safe_sqrt(1.0 - cos_w * cos_w);
        let cos_b = subtended_cos(pc, half_diagonal, r.p);
        let sin_b = safe_sqrt(1.0 - cos_b * cos_b);
        // θ' = max(0, θw − θo − θb): the smallest angle any emitter in the
        // box can present toward the receiver.
        let sin_o = safe_sqrt(1.0 - self.cos_theta_o * self.cos_theta_o);
        let cos_x = cos_sub_clamped(sin_w, cos_w, sin_o, self.cos_theta_o);
        let sin_x = sin_sub_clamped(sin_w, cos_w, sin_o, self.cos_theta_o);
        let cos_p = cos_sub_clamped(sin_x, cos_x, sin_b, cos_b);
        if cos_p <= self.cos_theta_e {
            return 0.0;
        }
        let mut importance = self.phi * cos_p / d2;
        if r.n != Vec3::ZERO {
            let n = Vec3A::from(r.n);
            // The receiver's angle to the light, reduced by the box's
            // subtended angle. Signed at an opaque receiver, so a box wholly
            // below its horizon gets zero; unsigned where it may transmit.
            let cos_i = if r.opaque {
                n.dot(-wi)
            } else {
                n.dot(wi).abs()
            };
            let sin_i = safe_sqrt(1.0 - cos_i * cos_i);
            importance *= cos_sub_clamped(sin_i, cos_i, sin_b, cos_b);
        }
        importance.max(0.0)
    }

    /// The build's cost measure of the directions these bounds emit into
    /// (pbrt-v4's `M_Ω`): the solid angle of the normal cone plus the
    /// cosine-weighted spread beyond it.
    fn orientation_measure(&self) -> f32 {
        let theta_o = self.cos_theta_o.clamp(-1.0, 1.0).acos();
        let theta_e = self.cos_theta_e.clamp(-1.0, 1.0).acos();
        let theta_w = (theta_o + theta_e).min(PI);
        let sin_o = safe_sqrt(1.0 - self.cos_theta_o * self.cos_theta_o);
        2.0 * PI * (1.0 - self.cos_theta_o)
            + PI / 2.0
                * (2.0 * theta_w * sin_o - (theta_o - 2.0 * theta_w).cos() - 2.0 * theta_o * sin_o
                    + self.cos_theta_o)
    }

    fn surface_area(&self) -> f32 {
        let d = (self.max - self.min).max(Vec3A::ZERO);
        2.0 * (d.x * d.y + d.y * d.z + d.z * d.x)
    }
}

#[inline]
fn safe_sqrt(x: f32) -> f32 {
    x.max(0.0).sqrt()
}

/// `cos(max(0, a − b))` from the sines and cosines of `a` and `b`.
#[inline]
fn cos_sub_clamped(sin_a: f32, cos_a: f32, sin_b: f32, cos_b: f32) -> f32 {
    if cos_a > cos_b {
        1.0
    } else {
        cos_a * cos_b + sin_a * sin_b
    }
}

/// `sin(max(0, a − b))` from the sines and cosines of `a` and `b`.
#[inline]
fn sin_sub_clamped(sin_a: f32, cos_a: f32, sin_b: f32, cos_b: f32) -> f32 {
    if cos_a > cos_b {
        0.0
    } else {
        sin_a * cos_b - cos_a * sin_b
    }
}

/// The cosine of the half-angle of the cone from `p` that holds the sphere
/// of `radius` about `center` (the box's bounding sphere): −1, every
/// direction, from inside it.
#[inline]
fn subtended_cos(center: Vec3A, radius: f32, p: Vec3A) -> f32 {
    let d2 = p.distance_squared(center);
    let r2 = radius * radius;
    if d2 < r2 {
        return -1.0;
    }
    safe_sqrt(1.0 - r2 / d2)
}

/// The smallest cone around two cones `(axis, cos half-angle)`, as pbrt-v4's
/// `Union(DirectionCone, DirectionCone)`. Build-time only.
fn cone_union(a: (Vec3A, f32), b: (Vec3A, f32)) -> (Vec3A, f32) {
    let theta_a = a.1.clamp(-1.0, 1.0).acos();
    let theta_b = b.1.clamp(-1.0, 1.0).acos();
    let theta_d = a.0.dot(b.0).clamp(-1.0, 1.0).acos();
    if (theta_d + theta_b).min(PI) <= theta_a {
        return a;
    }
    if (theta_d + theta_a).min(PI) <= theta_b {
        return b;
    }
    let theta_o = 0.5 * (theta_a + theta_d + theta_b);
    if theta_o >= PI {
        return (a.0, -1.0);
    }
    let axis = a.0.cross(b.0);
    if axis.length_squared() == 0.0 {
        return (a.0, -1.0);
    }
    let theta_r = theta_o - theta_a;
    let w = Quat::from_axis_angle(axis.normalize().into(), theta_r) * glam::Vec3::from(a.0);
    (Vec3A::from(w).normalize(), theta_o.cos())
}

/// One node of the tree. An interior node's first child is the next node
/// (depth-first order) and its second is `second_or_light`; a leaf holds one
/// light, `second_or_light` being its index into the light list.
#[derive(Clone, Debug)]
struct Node {
    bounds: LightBounds,
    second_or_light: u32,
    leaf: bool,
}

/// Past this depth the build stops searching for the cheapest split and
/// halves the lights instead, which bounds a degenerate tree's depth by the
/// 64 bits of a trail.
const SAOH_MAX_DEPTH: u32 = 40;

/// The build's buckets per axis (pbrt-v4's 12).
const BUCKETS: usize = 12;

/// The light BVH: a flat, depth-first array of nodes, and for each light of
/// the list its leaf's bit trail (bit `d` set: the second child at depth
/// `d`).
#[derive(Clone, Debug)]
pub struct LightTree {
    nodes: Vec<Node>,
    /// Per light-list index, its bit trail; `None` for a light not in the
    /// tree (at infinity, or emitting nothing).
    trails: Vec<Option<u64>>,
}

impl LightTree {
    /// Builds the tree over `lights`: `(light-list index, bounds)` pairs, in
    /// list order, for a list of `count` lights. A pure function of its
    /// input, so a scene always builds the same tree. `None` with fewer than
    /// two lights, where there is nothing to choose.
    pub fn build(count: usize, lights: &[(usize, LightBounds)]) -> Option<Self> {
        if lights.len() < 2 {
            return None;
        }
        let mut tree = Self {
            nodes: Vec::with_capacity(2 * lights.len() - 1),
            trails: vec![None; count],
        };
        let mut items = lights.to_vec();
        tree.build_node(&mut items, 0, 0);
        Some(tree)
    }

    /// Appends the subtree over `items` at `depth`, whose root's trail is
    /// `trail`, and returns its bounds.
    fn build_node(
        &mut self,
        items: &mut [(usize, LightBounds)],
        depth: u32,
        trail: u64,
    ) -> LightBounds {
        assert!(
            depth < 64,
            "a light tree deeper than a bit trail can address"
        );
        if let [(light, bounds)] = items {
            self.trails[*light] = Some(trail);
            self.nodes.push(Node {
                bounds: *bounds,
                second_or_light: *light as u32,
                leaf: true,
            });
            return *bounds;
        }
        let mid = if depth < SAOH_MAX_DEPTH {
            saoh_split(items)
        } else {
            None
        }
        .unwrap_or(items.len() / 2);
        let at = self.nodes.len();
        // A placeholder, filled once both children are known.
        self.nodes.push(Node {
            bounds: items[0].1,
            second_or_light: 0,
            leaf: false,
        });
        let (first, second) = items.split_at_mut(mid);
        let a = self.build_node(first, depth + 1, trail);
        let second_at = self.nodes.len();
        let b = self.build_node(second, depth + 1, trail | (1u64 << depth));
        let bounds = a.union(&b);
        self.nodes[at] = Node {
            bounds,
            second_or_light: second_at as u32,
            leaf: false,
        };
        bounds
    }

    /// The number of nodes, for the debug line and the tests.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// The tree's depth: the longest root-to-leaf path, in edges.
    pub fn depth(&self) -> usize {
        fn walk(nodes: &[Node], at: usize) -> usize {
            let node = &nodes[at];
            if node.leaf {
                0
            } else {
                1 + walk(nodes, at + 1).max(walk(nodes, node.second_or_light as usize))
            }
        }
        walk(&self.nodes, 0)
    }

    /// Whether light-list index `light` is in the tree.
    pub fn contains(&self, light: usize) -> bool {
        self.trails.get(light).is_some_and(Option::is_some)
    }

    /// Picks a light for receiver `r` from one `[0, 1)` sample `u`, walking
    /// from the root and choosing each child in proportion to its
    /// importance: the light-list index, with the probability the walk
    /// reached it. `None` where every child of some node on the way has zero
    /// importance — no light below it can reach `r`.
    #[inline]
    pub fn sample(&self, r: &Receiver, mut u: f32) -> Option<(usize, f32)> {
        let mut at = 0;
        let mut pmf = 1.0f32;
        loop {
            let node = &self.nodes[at];
            if node.leaf {
                return Some((node.second_or_light as usize, pmf));
            }
            let second = node.second_or_light as usize;
            let c0 = self.nodes[at + 1].bounds.importance(r);
            let c1 = self.nodes[second].bounds.importance(r);
            let sum = c0 + c1;
            if sum.is_nan() || sum <= 0.0 {
                return None;
            }
            let p0 = c0 / sum;
            // Monotone in `u`: the first child takes `[0, p0)`, and `u` is
            // stretched back over `[0, 1)` within the child it chose.
            if u < p0 {
                pmf *= c0 / sum;
                u = (u / p0).min(ONE_MINUS_EPSILON);
                at += 1;
            } else {
                pmf *= c1 / sum;
                u = ((u - p0) / (1.0 - p0)).min(ONE_MINUS_EPSILON);
                at = second;
            }
        }
    }

    /// The probability [`LightTree::sample`] at `r` picks light-list index
    /// `light`: the same ratios, multiplied in the same order, along the
    /// light's bit trail. Zero for a light not in the tree, or one below a
    /// node whose importance at `r` is zero.
    #[inline]
    pub fn pmf(&self, r: &Receiver, light: usize) -> f32 {
        let Some(Some(mut trail)) = self.trails.get(light).copied() else {
            return 0.0;
        };
        let mut at = 0;
        let mut pmf = 1.0f32;
        loop {
            let node = &self.nodes[at];
            if node.leaf {
                debug_assert_eq!(node.second_or_light as usize, light);
                return pmf;
            }
            let second = node.second_or_light as usize;
            let c0 = self.nodes[at + 1].bounds.importance(r);
            let c1 = self.nodes[second].bounds.importance(r);
            let sum = c0 + c1;
            if sum.is_nan() || sum <= 0.0 {
                return 0.0;
            }
            if trail & 1 == 0 {
                pmf *= c0 / sum;
                at += 1;
            } else {
                pmf *= c1 / sum;
                at = second;
            }
            trail >>= 1;
        }
    }
}

/// The cheapest split of `items` by the binned surface-area-orientation
/// heuristic (Conty Estevez & Kulla 2018, as pbrt-v4 builds it): `items`
/// reordered, stably, so the first `mid` go to the first child, and `mid`.
/// `None` when no split separates anything (every centroid in one place).
/// Ties go to the lower axis, then the lower bucket.
fn saoh_split(items: &mut [(usize, LightBounds)]) -> Option<usize> {
    let (mut bmin, mut bmax) = (Vec3A::INFINITY, Vec3A::NEG_INFINITY);
    let (mut cmin, mut cmax) = (Vec3A::INFINITY, Vec3A::NEG_INFINITY);
    for (_, b) in items.iter() {
        bmin = bmin.min(b.min);
        bmax = bmax.max(b.max);
        cmin = cmin.min(b.centroid());
        cmax = cmax.max(b.centroid());
    }
    let diagonal = bmax - bmin;
    let max_extent = diagonal.max_element();
    let bucket_of = |c: Vec3A, dim: usize| -> usize {
        let t = (c[dim] - cmin[dim]) / (cmax[dim] - cmin[dim]);
        ((BUCKETS as f32 * t) as usize).min(BUCKETS - 1)
    };
    let mut best: Option<(f32, usize, usize)> = None;
    for dim in 0..3 {
        if cmax[dim] == cmin[dim] {
            continue;
        }
        let mut buckets: [Option<LightBounds>; BUCKETS] = [None; BUCKETS];
        for (_, b) in items.iter() {
            let k = bucket_of(b.centroid(), dim);
            buckets[k] = Some(buckets[k].map_or(*b, |acc| acc.union(b)));
        }
        let kr = max_extent / diagonal[dim];
        let cost_of = |b: Option<LightBounds>| {
            b.map_or(0.0, |b| {
                b.phi * b.orientation_measure() * kr * b.surface_area()
            })
        };
        let union_of = |range: &[Option<LightBounds>]| {
            range
                .iter()
                .flatten()
                .fold(None, |acc: Option<LightBounds>, b| {
                    Some(acc.map_or(*b, |a| a.union(b)))
                })
        };
        for split in 0..BUCKETS - 1 {
            let below = union_of(&buckets[..=split]);
            let above = union_of(&buckets[split + 1..]);
            if below.is_none() || above.is_none() {
                continue;
            }
            let cost = cost_of(below) + cost_of(above);
            if cost > 0.0 && best.is_none_or(|(c, _, _)| cost < c) {
                best = Some((cost, dim, split));
            }
        }
    }
    let (_, dim, split) = best?;
    // A stable partition: the lights keep their list order on each side.
    let (below, above): (Vec<_>, Vec<_>) = items
        .iter()
        .copied()
        .partition(|(_, b)| bucket_of(b.centroid(), dim) <= split);
    let mid = below.len();
    if mid == 0 || mid == items.len() {
        return None;
    }
    for (slot, item) in items.iter_mut().zip(below.into_iter().chain(above)) {
        *slot = item;
    }
    Some(mid)
}

#[cfg(test)]
mod tests;
