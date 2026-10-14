//! [`Adam`].

use std::ops::Range;

/// Adam (Kingma & Ba 2015) over a flat parameter vector, with a learning
/// rate per parameter group: a hash table learns at a rate its dense layers
/// would diverge at (Müller et al. 2022).
///
/// Dense: every parameter of a group moves every step, read or not. The
/// sparse ("lazy") variant would skip the table entries a batch did not
/// read, which saves little on a table of a million `f32`s and makes the
/// result depend on which batch read what.
#[derive(Clone, Debug)]
pub struct Adam {
    groups: Vec<(Range<usize>, f32)>,
    beta1: f32,
    beta2: f32,
    eps: f32,
    m: Vec<f32>,
    v: Vec<f32>,
    /// `β₁ᵗ` and `β₂ᵗ`, by repeated multiplication rather than `powi`.
    beta1_t: f64,
    beta2_t: f64,
    steps: u64,
}

/// `(β₁, β₂)`: tiny-cuda-nn's, which a hash encoding is usually trained with.
pub const BETAS: (f32, f32) = (0.9, 0.99);
/// tiny-cuda-nn's ε for a hash encoding. Small, because a table entry few
/// samples read has a tiny second moment, and a larger ε would freeze it.
pub const EPSILON: f32 = 1e-15;

impl Adam {
    /// An optimiser for `params` parameters, with [`BETAS`] and [`EPSILON`].
    /// `groups` are disjoint ranges of the parameters with their learning
    /// rates; a parameter in none never moves.
    pub fn new(params: usize, groups: Vec<(Range<usize>, f32)>) -> Self {
        Self::with(params, groups, BETAS, EPSILON)
    }

    pub fn with(
        params: usize,
        groups: Vec<(Range<usize>, f32)>,
        (beta1, beta2): (f32, f32),
        eps: f32,
    ) -> Self {
        assert!(
            groups
                .iter()
                .all(|(r, _)| r.start <= r.end && r.end <= params),
            "every group lies within the parameters"
        );
        Adam {
            groups,
            beta1,
            beta2,
            eps,
            m: vec![0.0; params],
            v: vec![0.0; params],
            beta1_t: 1.0,
            beta2_t: 1.0,
            steps: 0,
        }
    }

    /// Steps taken.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// One step down `grad`. Each parameter's update reads only its own
    /// state, so the result is the same in any order.
    pub fn step(&mut self, params: &mut [f32], grad: &[f32]) {
        assert_eq!(params.len(), self.m.len());
        assert_eq!(grad.len(), self.m.len());
        self.steps += 1;
        self.beta1_t *= self.beta1 as f64;
        self.beta2_t *= self.beta2 as f64;
        // The bias corrections folded into the rate and ε, once per step
        // (Kingma & Ba's "more efficient" order).
        let c1 = 1.0 - self.beta1_t;
        let c2 = (1.0 - self.beta2_t).sqrt();
        let eps = (self.eps as f64 * c2) as f32;
        let (b1, b2) = (self.beta1, self.beta2);
        let (k1, k2) = (1.0 - b1, 1.0 - b2);
        for (range, lr) in &self.groups {
            let lr = (*lr as f64 * c2 / c1) as f32;
            let r = range.clone();
            for (((p, m), v), &g) in params[r.clone()]
                .iter_mut()
                .zip(&mut self.m[r.clone()])
                .zip(&mut self.v[r.clone()])
                .zip(&grad[r])
            {
                *m = b1 * *m + k1 * g;
                *v = b2 * *v + k2 * (g * g);
                *p -= lr * *m / (v.sqrt() + eps);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::network::test_rand;
    use crate::{GridConfig, Network, Shape, sequential};

    /// A 1D step — a wall at x = 0.4, lit on its left — learned to a mean
    /// cross-entropy under 0.05 on points it never trained on, in a fixed
    /// number of steps.
    #[test]
    fn fits_a_wall() {
        let shape = Shape {
            grid: GridConfig {
                levels: 4,
                features: 2,
                log2_entries: 12,
                base: 4,
                growth: 2.0,
            },
            hidden: vec![16],
            outputs: 1,
        };
        let mut rand = test_rand(11);
        let mut net = Network::<f32>::new(&shape, &mut rand);
        let mut adam = Adam::new(net.params().len(), net.groups(1e-2, 1e-3));
        let mut sample = |n: usize| {
            let xs: Vec<[f32; 3]> = (0..n).map(|_| [rand(), rand(), rand()]).collect();
            let ys: Vec<f32> = xs.iter().map(|x| (x[0] < 0.4) as u32 as f32).collect();
            (xs, ys)
        };
        let (test_x, test_y) = sample(4096);
        let before = net.loss(&test_x, &test_y);
        let mut grad = vec![0.0; net.params().len()];
        for _ in 0..400 {
            let (xs, ys) = sample(256);
            net.batch_gradient(&xs, &ys, &mut grad, sequential);
            adam.step(net.params_mut(), &grad);
        }
        let after = net.loss(&test_x, &test_y);
        assert!(after < 0.05, "BCE {before} -> {after}");
        assert_eq!(adam.steps(), 400);
    }

    /// Hand-worked first step: with bias correction, Adam's first update is
    /// `lr · sign(g)` (up to ε), whatever the gradient's size.
    #[test]
    fn first_step_is_the_rate_times_the_sign() {
        let mut adam = Adam::new(3, vec![(0..2, 0.5), (2..3, 0.25)]);
        let mut p = [1.0f32, 1.0, 1.0];
        adam.step(&mut p, &[4.0, -1e-3, 2.0]);
        assert!((p[0] - 0.5).abs() < 1e-6, "{}", p[0]);
        assert!((p[1] - 1.5).abs() < 1e-6, "{}", p[1]);
        assert!((p[2] - 0.75).abs() < 1e-6, "{}", p[2]);
        // A parameter whose gradient has always been zero never moves.
        let mut adam = Adam::new(1, vec![(0..1, 1.0)]);
        let mut q = [3.0f32];
        for _ in 0..5 {
            adam.step(&mut q, &[0.0]);
        }
        assert_eq!(q[0], 3.0);
    }
}
