//! The 68 → 32 → 32 → 1 multilayer perceptron: ReLU hidden layers and a
//! single logit, read through a sigmoid.
//!
//! Plain `f32`, written so every sum has one order whatever the codegen:
//! the hidden layers loop over inputs on the outside and over the 32 outputs
//! on the inside, so each output is one sequential sum and a vectoriser can
//! only widen *across* outputs, never reassociate *within* one. Weights are
//! stored input-major for exactly that loop.

/// Inputs: S = 4 points × the grid's 16 features, the unit direction and the
/// segment's relative length.
pub(crate) const IN: usize = 68;
/// Hidden width.
pub(crate) const H: usize = 32;

const W1: usize = 0;
const B1: usize = W1 + IN * H;
const W2: usize = B1 + H;
const B2: usize = W2 + H * H;
const W3: usize = B2 + H;
const B3: usize = W3 + H;
/// Total parameters (3 297).
pub(crate) const PARAMS: usize = B3 + 1;

/// The hidden activations a forward pass leaves for its backward pass.
pub(crate) struct Acts {
    h1: [f32; H],
    h2: [f32; H],
}

/// `y[j] = b[j] + Σ_i x[i]·w[i·H + j]`, summed in `i` order.
#[inline]
fn layer<const N: usize>(w: &[f32], b: &[f32], x: &[f32; N]) -> [f32; H] {
    let mut y = [0f32; H];
    y.copy_from_slice(&b[..H]);
    for (i, &xi) in x.iter().enumerate() {
        let row = &w[i * H..(i + 1) * H];
        for j in 0..H {
            y[j] += xi * row[j];
        }
    }
    y
}

#[inline]
fn relu(mut y: [f32; H]) -> [f32; H] {
    for v in &mut y {
        *v = v.max(0.0);
    }
    y
}

/// The logit for input `x`, and the activations for [`backward`].
#[inline]
pub(crate) fn forward(p: &[f32], x: &[f32; IN]) -> (f32, Acts) {
    let h1 = relu(layer(&p[W1..B1], &p[B1..W2], x));
    let h2 = relu(layer(&p[W2..B2], &p[B2..W3], &h1));
    let mut z = p[B3];
    for j in 0..H {
        z += h2[j] * p[W3 + j];
    }
    (z, Acts { h1, h2 })
}

/// The logit alone, for queries.
#[inline]
pub(crate) fn logit(p: &[f32], x: &[f32; IN]) -> f32 {
    forward(p, x).0
}

/// Accumulates into `grad` the gradient of a loss whose derivative by the
/// logit is `dz`, and writes the gradient by the input into `dx`.
pub(crate) fn backward(
    p: &[f32],
    x: &[f32; IN],
    a: &Acts,
    dz: f32,
    grad: &mut [f32],
    dx: &mut [f32; IN],
) {
    // Output layer.
    let mut dh2 = [0f32; H];
    for j in 0..H {
        grad[W3 + j] += dz * a.h2[j];
        dh2[j] = if a.h2[j] > 0.0 { dz * p[W3 + j] } else { 0.0 };
    }
    grad[B3] += dz;
    // Second hidden layer.
    let mut dh1 = [0f32; H];
    for i in 0..H {
        let row = &p[W2 + i * H..W2 + (i + 1) * H];
        let g = &mut grad[W2 + i * H..W2 + (i + 1) * H];
        let mut s = 0f32;
        for j in 0..H {
            g[j] += a.h1[i] * dh2[j];
            s += row[j] * dh2[j];
        }
        dh1[i] = if a.h1[i] > 0.0 { s } else { 0.0 };
    }
    for j in 0..H {
        grad[B2 + j] += dh2[j];
    }
    // First hidden layer.
    for i in 0..IN {
        let row = &p[W1 + i * H..W1 + (i + 1) * H];
        let g = &mut grad[W1 + i * H..W1 + (i + 1) * H];
        let mut s = 0f32;
        for j in 0..H {
            g[j] += x[i] * dh1[j];
            s += row[j] * dh1[j];
        }
        dx[i] = s;
    }
    for j in 0..H {
        grad[B1 + j] += dh1[j];
    }
}

/// He-uniform weights, zero biases: `uniform()` is called once per weight,
/// in parameter order.
pub(crate) fn init(mut uniform: impl FnMut() -> f32) -> Vec<f32> {
    let mut p = vec![0f32; PARAMS];
    let mut fill = |range: std::ops::Range<usize>, fan_in: usize| {
        let bound = (6.0 / fan_in as f32).sqrt();
        for v in &mut p[range] {
            *v = (2.0 * uniform() - 1.0) * bound;
        }
    };
    fill(W1..B1, IN);
    fill(W2..B2, H);
    fill(W3..B3, H);
    p
}

#[inline]
pub(crate) fn sigmoid(z: f32) -> f32 {
    1.0 / (1.0 + (-z).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(seed: u32) -> Vec<f32> {
        let mut rng = openqmc::pcg::Rng::new(seed);
        let mut p = init(|| rng.next_f32());
        // Non-zero biases, so their gradients are exercised too.
        for k in (B1..W2).chain(B2..W3) {
            p[k] = 0.05 * ((k % 7) as f32 - 3.0);
        }
        p
    }

    fn input(seed: u32) -> [f32; IN] {
        let mut rng = openqmc::pcg::Rng::new(seed);
        std::array::from_fn(|_| 2.0 * rng.next_f32() - 1.0)
    }

    #[test]
    fn backward_matches_finite_differences() {
        let mut p = params(5);
        let x = input(9);
        let (_, a) = forward(&p, &x);
        let mut grad = vec![0f32; PARAMS];
        let mut dx = [0f32; IN];
        backward(&p, &x, &a, 1.0, &mut grad, &mut dx);
        let h = 1e-3f32;
        // A spread of parameters across every block, and every input.
        for k in (0..PARAMS).step_by(37).chain([B1, B2, W3, B3]) {
            let v = p[k];
            p[k] = v + h;
            let up = logit(&p, &x) as f64;
            p[k] = v - h;
            let down = logit(&p, &x) as f64;
            p[k] = v;
            let fd = ((up - down) / (2.0 * h as f64)) as f32;
            assert!(
                (fd - grad[k]).abs() < 2e-2,
                "param {k}: {} vs {fd}",
                grad[k]
            );
        }
        let mut xm = x;
        for i in 0..IN {
            xm[i] = x[i] + h;
            let up = logit(&p, &xm) as f64;
            xm[i] = x[i] - h;
            let down = logit(&p, &xm) as f64;
            xm[i] = x[i];
            let fd = ((up - down) / (2.0 * h as f64)) as f32;
            assert!((fd - dx[i]).abs() < 2e-2, "input {i}: {} vs {fd}", dx[i]);
        }
    }

    /// The forward pass's bits, pinned: `scripts/test_simd_matrix.sh` runs
    /// this under every codegen, which must all land on the same logit.
    #[test]
    fn forward_is_pinned_across_codegens() {
        let p = params(21);
        let x = input(23);
        let z = logit(&p, &x);
        // Recomputed by a scalar reference in the same summation order.
        let mut h1 = [0f32; H];
        for j in 0..H {
            let mut s = p[B1 + j];
            for i in 0..IN {
                s += x[i] * p[W1 + i * H + j];
            }
            h1[j] = s.max(0.0);
        }
        let mut h2 = [0f32; H];
        for j in 0..H {
            let mut s = p[B2 + j];
            for i in 0..H {
                s += h1[i] * p[W2 + i * H + j];
            }
            h2[j] = s.max(0.0);
        }
        let mut r = p[B3];
        for j in 0..H {
            r += h2[j] * p[W3 + j];
        }
        assert_eq!(z.to_bits(), r.to_bits());
        // And the bits themselves, so two codegens cannot agree with their
        // own reference while disagreeing with each other.
        assert_eq!(z.to_bits(), 0x400a_6898, "logit {z}");
    }
}
