//! `neural` light selection's frozen product: the trained network and the
//! per-position pick tables [`LightList`](crate::LightList) reads at every pick and every MIS
//! weight. The pre-pass that trains it walks the world, so it lives with the
//! integrator (`tracer/light_visibility.rs`, which also holds the design
//! notes).

use std::cell::RefCell;
use std::sync::atomic::AtomicU64;

use crust_nn::{MAX_WIDTH, Network, sigmoid};

use crate::Vec3A;
use crate::light::learned::{DEFENSIVE, defensive_table};

/// The most lights `neural` supports (design D7): the output layer is one
/// unit per light, and past this its cost outgrows the go/no-go gate. ALab
/// has 47.
pub const MAX_LIGHTS: usize = 64;
const _: () = assert!(MAX_LIGHTS <= MAX_WIDTH);

/// Below this `Σ πV̂`, every light is predicted hidden: the ratios of
/// near-zero predictions are noise, and the power table answers instead.
const HIDDEN: f64 = 1e-6;

/// Distinguishes caches in the thread-local memo, so that no renderer reads
/// another's.
pub static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// The trained network and what it is read with. Built by the pre-pass
/// (`tracer/light_visibility.rs`), frozen, and read by
/// [`LightList`](crate::LightList) at every pick and every MIS weight.
#[derive(Debug)]
pub struct VisibilityCache {
    pub id: u64,
    /// The receivers' robust bounds; the unit cube's origin is `lo`, and
    /// every axis is scaled by the longest one's extent, so the encoding's
    /// cells stay cubic.
    pub lo: Vec3A,
    pub hi: Vec3A,
    pub inv_extent: f32,
    pub net: Network<f32>,
    /// The power table the predictions weigh, and which lights are live.
    pub power: Vec<f32>,
    pub live: Vec<bool>,
    pub n_live: usize,
    /// For the debug line and the tests.
    pub receivers: usize,
    pub trained: usize,
    pub loss: f64,
}

/// The last table a thread computed, keyed by the cache and the exact bits
/// of the position (design D6).
struct Memo {
    id: u64,
    key: [u32; 3],
    /// Whether the network answered there; `false` is the power table.
    some: bool,
    pmf: [f32; MAX_LIGHTS],
    cdf: [f32; MAX_LIGHTS],
}

thread_local! {
    static MEMO: RefCell<Memo> = const {
        RefCell::new(Memo {
            id: 0,
            key: [0; 3],
            some: false,
            pmf: [0.0; MAX_LIGHTS],
            cdf: [0.0; MAX_LIGHTS],
        })
    };
}

impl VisibilityCache {
    /// Lights the network predicts for.
    pub fn lights(&self) -> usize {
        self.power.len()
    }

    /// `p` in the network's unit cube, or `None` outside the trained bounds.
    #[inline]
    fn unit(&self, p: Vec3A) -> Option<[f32; 3]> {
        let inside = p.cmpge(self.lo).all() && p.cmple(self.hi).all();
        inside.then(|| ((p - self.lo) * self.inv_extent).to_array())
    }

    /// Each light's predicted visibility from `p`, or `None` outside the
    /// trained bounds.
    pub fn visibility(&self, p: Vec3A) -> Option<Vec<f32>> {
        let x = self.unit(p)?;
        let mut z = [0.0f32; MAX_LIGHTS];
        let n = self.lights();
        self.net.infer(x, &mut z[..n]);
        Some(z[..n].iter().map(|&z| sigmoid(z)).collect())
    }

    /// The pick distribution at `p` into `cdf` and `pmf` (design D2), or
    /// `false` where the power table answers instead: outside the bounds, or
    /// where every light is predicted hidden. A pure function of `p`'s bits.
    #[inline]
    fn compute(&self, p: Vec3A, cdf: &mut [f32], pmf: &mut [f32]) -> bool {
        let Some(x) = self.unit(p) else {
            return false;
        };
        let n = self.lights();
        let mut z = [0.0f32; MAX_LIGHTS];
        self.net.infer(x, &mut z[..n]);
        let mut w = [0.0f64; MAX_LIGHTS];
        let mut total = 0.0f64;
        for ((w, &z), &pi) in w.iter_mut().zip(&z[..n]).zip(&self.power) {
            *w = pi as f64 * sigmoid(z) as f64;
            total += *w;
        }
        if !(total > HIDDEN && total.is_finite()) {
            return false;
        }
        defensive_table(&w[..n], total, &self.live, self.n_live, DEFENSIVE, cdf, pmf);
        true
    }

    /// Calls `f` with the `(pmf, cdf)` NEE picks with at `p`, or with `None`
    /// where the power table answers. Served from this thread's memo when the
    /// last call was at the same `p`'s bits — the bounce side's lookup at
    /// `prev.pos` right after NEE's at that vertex — and recomputed, to the
    /// same bits, otherwise.
    #[inline]
    pub fn with_table<T>(&self, p: Vec3A, f: impl FnOnce(Option<(&[f32], &[f32])>) -> T) -> T {
        let key = p.to_array().map(f32::to_bits);
        let n = self.lights();
        MEMO.with(|memo| {
            let mut memo = memo.borrow_mut();
            if memo.id != self.id || memo.key != key {
                let Memo { pmf, cdf, .. } = &mut *memo;
                let some = self.compute(p, &mut cdf[..n], &mut pmf[..n]);
                memo.some = some;
                memo.id = self.id;
                memo.key = key;
            }
            f(memo.some.then(|| (&memo.pmf[..n], &memo.cdf[..n])))
        })
    }

    /// [`VisibilityCache::with_table`] without the memo, into the caller's
    /// arrays: for the tests that pin the memo to it, and the bench.
    pub fn table_uncached(&self, p: Vec3A) -> Option<(Vec<f32>, Vec<f32>)> {
        let n = self.lights();
        let (mut pmf, mut cdf) = (vec![0.0; n], vec![0.0; n]);
        self.compute(p, &mut cdf, &mut pmf).then_some((pmf, cdf))
    }
}
