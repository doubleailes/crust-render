//! Adam (Kingma & Ba 2015) over one parameter block. The trainer keeps two,
//! one for the hash grid and one for the MLP, with Instant NGP's split of
//! learning rates. Each parameter's update reads only its own state, so a
//! block may be stepped in parallel slices without changing a bit.

use rayon::prelude::*;

pub(crate) struct Adam {
    m: Vec<f32>,
    v: Vec<f32>,
    lr: f32,
    beta1: f32,
    beta2: f32,
    eps: f32,
    t: i32,
}

/// Parameters per parallel slice of a step: large enough that a block of a
/// few thousand MLP weights stays on one thread.
const SLICE: usize = 1 << 14;

impl Adam {
    pub(crate) fn new(n: usize, lr: f32) -> Self {
        Adam {
            m: vec![0.0; n],
            v: vec![0.0; n],
            lr,
            beta1: 0.9,
            beta2: 0.99,
            eps: 1e-10,
            t: 0,
        }
    }

    /// One update of `params` by `grads`.
    pub(crate) fn step(&mut self, params: &mut [f32], grads: &[f32]) {
        self.t += 1;
        let (b1, b2, lr, eps) = (self.beta1, self.beta2, self.lr, self.eps);
        let bc1 = 1.0 - b1.powi(self.t);
        let bc2 = 1.0 - b2.powi(self.t);
        params
            .par_chunks_mut(SLICE)
            .zip(grads.par_chunks(SLICE))
            .zip(self.m.par_chunks_mut(SLICE))
            .zip(self.v.par_chunks_mut(SLICE))
            .for_each(|(((p, g), m), v)| {
                for i in 0..p.len() {
                    m[i] = b1 * m[i] + (1.0 - b1) * g[i];
                    v[i] = b2 * v[i] + (1.0 - b2) * g[i] * g[i];
                    p[i] -= lr * (m[i] / bc1) / ((v[i] / bc2).sqrt() + eps);
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Logistic regression onto a step at x = 0.3: Adam drives the binary
    /// cross-entropy of `sigmoid(w·x + b)` below a fixed bound.
    #[test]
    fn fits_a_step_function() {
        let xs: Vec<f32> = (0..200).map(|i| i as f32 / 199.0).collect();
        let label = |x: f32| if x > 0.3 { 1.0 } else { 0.0 };
        let mut p = [0.0f32, 0.0];
        let mut adam = Adam::new(2, 0.1);
        let loss = |p: &[f32; 2]| {
            xs.iter()
                .map(|&x| {
                    let q = super::super::mlp::sigmoid(p[0] * x + p[1]).clamp(1e-7, 1.0 - 1e-7);
                    let y = label(x);
                    -(y * q.ln() + (1.0 - y) * (1.0 - q).ln())
                })
                .sum::<f32>()
                / xs.len() as f32
        };
        let start = loss(&p);
        for _ in 0..3000 {
            let mut g = [0.0f32; 2];
            for &x in &xs {
                let d = super::super::mlp::sigmoid(p[0] * x + p[1]) - label(x);
                g[0] += d * x / xs.len() as f32;
                g[1] += d / xs.len() as f32;
            }
            adam.step(&mut p, &g);
        }
        let end = loss(&p);
        assert!(end < 0.05, "loss {start} -> {end}");
        assert!(
            (-p[1] / p[0] - 0.3).abs() < 0.02,
            "step at {}",
            -p[1] / p[0]
        );
    }
}
