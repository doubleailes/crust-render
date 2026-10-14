//! [`Dense`] layers, their activations, and the loss.

use crate::Real;

/// Outputs [`Dense::forward`] accumulates at once: two SSE registers of `f32`.
const BLOCK: usize = 8;
/// The width [`Dense::forward`] has its own instantiation for.
const WIDE: usize = 32;

/// [`Dense::forward`] for `N` outputs: the whole accumulator in registers.
#[inline(always)]
fn forward_fixed<R: Real, const N: usize>(w: &[R], b: &[R], x: &[R], y: &mut [R]) {
    let mut acc: [R; N] = b.try_into().expect("N biases");
    for (&xi, row) in x.iter().zip(w.as_chunks::<N>().0) {
        for (a, &wij) in acc.iter_mut().zip(row) {
            *a += xi * wij;
        }
    }
    y.copy_from_slice(&acc);
}

/// A fully connected layer, `y = W·x + b`. It holds only its shape: the
/// parameters live in the network's flat vector, `W` first, input-major
/// (`inputs` rows of `outputs` weights), then `b`.
///
/// Input-major so that the forward pass is a sum of scaled rows: each output
/// accumulates its terms in input order, which is a fixed order, yet the
/// inner loop runs across outputs and vectorises without reassociating
/// anything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dense {
    pub inputs: usize,
    pub outputs: usize,
}

impl Dense {
    /// Parameters: the weights, then the biases.
    pub fn params(&self) -> usize {
        self.inputs * self.outputs + self.outputs
    }

    /// `y = W·x + b`, each `y[j]` summed from `b[j]` in input order.
    ///
    /// The terms of each output, and their order, are the definition's
    /// whatever the path, so are its bits; the paths differ only in what the
    /// compiler can keep in registers. A layer [`WIDE`] outputs wide (the
    /// visibility network's hidden layers) has its own instantiation, whose
    /// accumulator lives in registers across the inputs and whose rows need
    /// no bounds check; any other width runs [`BLOCK`] outputs at a time,
    /// then the rest. Summed over the whole row at once instead, every input
    /// loaded and stored every output: most of an inference's instructions.
    #[inline]
    pub fn forward<R: Real>(&self, params: &[R], x: &[R], y: &mut [R]) {
        debug_assert_eq!(params.len(), self.params());
        debug_assert_eq!(x.len(), self.inputs);
        debug_assert_eq!(y.len(), self.outputs);
        let (w, b) = params.split_at(self.inputs * self.outputs);
        if self.outputs == WIDE {
            return forward_fixed::<R, WIDE>(w, b, x, y);
        }
        let out = self.outputs;
        let mut j0 = 0;
        while j0 + BLOCK <= out {
            let mut acc: [R; BLOCK] = b[j0..j0 + BLOCK].try_into().expect("a block");
            for (&xi, row) in x.iter().zip(w.chunks_exact(out)) {
                for (a, &wij) in acc.iter_mut().zip(&row[j0..j0 + BLOCK]) {
                    *a += xi * wij;
                }
            }
            y[j0..j0 + BLOCK].copy_from_slice(&acc);
            j0 += BLOCK;
        }
        // The last `out % BLOCK` outputs.
        if j0 < out {
            let tail = &mut y[j0..];
            tail.copy_from_slice(&b[j0..]);
            for (&xi, row) in x.iter().zip(w.chunks_exact(out)) {
                for (yj, &wij) in tail.iter_mut().zip(&row[j0..]) {
                    *yj += xi * wij;
                }
            }
        }
    }

    /// Adds `∂L/∂W` and `∂L/∂b` into `grad` (laid out like `params`), given
    /// the layer's input `x` and `dy = ∂L/∂y`, and writes `∂L/∂x` into `dx`
    /// when asked for it. Each `dx[i]` is summed in output order.
    pub fn backward<R: Real>(
        &self,
        params: &[R],
        x: &[R],
        dy: &[R],
        dx: Option<&mut [R]>,
        grad: &mut [R],
    ) {
        debug_assert_eq!(params.len(), self.params());
        debug_assert_eq!(grad.len(), self.params());
        let n = self.inputs * self.outputs;
        let (gw, gb) = grad.split_at_mut(n);
        for (g, &d) in gb.iter_mut().zip(dy) {
            *g += d;
        }
        for (&xi, row) in x.iter().zip(gw.chunks_exact_mut(self.outputs)) {
            for (g, &d) in row.iter_mut().zip(dy) {
                *g += xi * d;
            }
        }
        if let Some(dx) = dx {
            for (dxi, row) in dx.iter_mut().zip(params[..n].chunks_exact(self.outputs)) {
                let mut s = R::ZERO;
                for (&wij, &d) in row.iter().zip(dy) {
                    s += wij * d;
                }
                *dxi = s;
            }
        }
    }
}

/// `max(0, z)` in place.
#[inline]
pub fn relu<R: Real>(v: &mut [R]) {
    for z in v {
        if *z < R::ZERO {
            *z = R::ZERO;
        }
    }
}

/// ReLU's backward pass in place: `d` survives where the activation `y`
/// (the ReLU's *output*) is positive, and is zeroed where it is not.
#[inline]
pub fn relu_backward<R: Real>(y: &[R], d: &mut [R]) {
    for (d, &y) in d.iter_mut().zip(y) {
        if y <= R::ZERO {
            *d = R::ZERO;
        }
    }
}

/// The logistic function, computed on the side where `exp` cannot overflow.
#[inline]
pub fn sigmoid<R: Real>(z: R) -> R {
    if z >= R::ZERO {
        R::ONE / (R::ONE + (-z).exp())
    } else {
        let e = z.exp();
        e / (R::ONE + e)
    }
}

/// Binary cross-entropy of a logit `z` against a label `y ∈ [0, 1]`, as
/// `(loss, ∂loss/∂z)`. The label may be a fraction: a visibility measured
/// over several samples.
///
/// `max(z, 0) − z·y + ln(1 + e^−|z|)`, the form that neither overflows nor
/// loses the loss to cancellation at large `|z|`; its derivative is
/// `σ(z) − y`.
#[inline]
pub fn bce_with_logits<R: Real>(z: R, y: R) -> (R, R) {
    let pos = if z > R::ZERO { z } else { R::ZERO };
    let loss = pos - z * y + (-z.abs()).exp().ln_1p();
    (loss, sigmoid(z) - y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 2→2 layer against values worked by hand.
    #[test]
    fn two_by_two_by_hand() {
        let layer = Dense {
            inputs: 2,
            outputs: 2,
        };
        // W = [[1, 2], [3, 4]] input-major: x0 feeds (1, 2), x1 feeds (3, 4).
        // b = (0.5, -1).
        let params = [1.0f32, 2.0, 3.0, 4.0, 0.5, -1.0];
        let x = [2.0f32, -1.0];
        let mut y = [0.0f32; 2];
        layer.forward(&params, &x, &mut y);
        // y0 = 0.5 + 2·1 + (−1)·3 = −0.5; y1 = −1 + 2·2 + (−1)·4 = −1.
        assert_eq!(y, [-0.5, -1.0]);

        let dy = [1.0f32, -2.0];
        let mut grad = [0.0f32; 6];
        let mut dx = [0.0f32; 2];
        layer.backward(&params, &x, &dy, Some(&mut dx), &mut grad);
        // ∂W[i][j] = x_i·dy_j; ∂b = dy; dx_i = Σ_j W[i][j]·dy_j.
        assert_eq!(grad, [2.0, -4.0, -1.0, 2.0, 1.0, -2.0]);
        assert_eq!(dx, [1.0 - 4.0, 3.0 - 8.0]);

        // Backward accumulates.
        layer.backward(&params, &x, &dy, None, &mut grad);
        assert_eq!(grad, [4.0, -8.0, -2.0, 4.0, 2.0, -4.0]);
    }

    /// Every path of `forward` — blocks, a tail, the fixed 32-wide one —
    /// is the definition bit for bit: `b[j]` plus `x[i]·w[i][j]` in input
    /// order.
    #[test]
    fn every_width_is_the_definition() {
        let mut seed = 3u64;
        let mut rand = move || {
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            (seed >> 40) as f32 / (1u64 << 24) as f32 * 2.0 - 1.0
        };
        for &(inputs, outputs) in &[
            (3, 1),
            (16, 7),
            (16, 8),
            (5, 9),
            (16, 32),
            (32, 32),
            (7, 33),
            (32, 64),
        ] {
            let layer = Dense { inputs, outputs };
            let params: Vec<f32> = (0..layer.params()).map(|_| rand()).collect();
            let x: Vec<f32> = (0..inputs).map(|_| rand()).collect();
            let mut y = vec![0.0f32; outputs];
            layer.forward(&params, &x, &mut y);
            for j in 0..outputs {
                let mut want = params[inputs * outputs + j];
                for i in 0..inputs {
                    want += x[i] * params[i * outputs + j];
                }
                assert_eq!(
                    y[j].to_bits(),
                    want.to_bits(),
                    "{inputs}→{outputs}, output {j}"
                );
            }
        }
    }

    #[test]
    fn relu_and_its_backward() {
        let mut v = [-1.0f32, 0.0, 2.0];
        relu(&mut v);
        assert_eq!(v, [0.0, 0.0, 2.0]);
        let mut d = [5.0f32, 5.0, 5.0];
        relu_backward(&v, &mut d);
        assert_eq!(d, [0.0, 0.0, 5.0]);
    }

    #[test]
    fn sigmoid_by_hand_and_at_the_extremes() {
        assert_eq!(sigmoid(0.0f32), 0.5);
        let e = 1.0f64.exp();
        assert!((sigmoid(1.0f64) - e / (1.0 + e)).abs() < 1e-15);
        assert!((sigmoid(-1.0f64) - 1.0 / (1.0 + e)).abs() < 1e-15);
        assert_eq!(sigmoid(200.0f32), 1.0);
        assert_eq!(sigmoid(-200.0f32), 0.0);
        assert!(!sigmoid(-1e30f32).is_nan());
    }

    #[test]
    fn bce_by_hand() {
        // At z = 0: ln 2 whatever the label, gradient 0.5 − y.
        let (l, g) = bce_with_logits(0.0f64, 1.0);
        assert!((l - 2.0f64.ln()).abs() < 1e-15);
        assert_eq!(g, -0.5);
        // z = 2, y = 0.25: 2 − 0.5 + ln(1 + e^−2).
        let (l, g) = bce_with_logits(2.0f64, 0.25);
        assert!((l - (1.5 + (1.0 + (-2.0f64).exp()).ln())).abs() < 1e-15);
        assert!((g - (sigmoid(2.0) - 0.25)).abs() < 1e-15);
        // Equals the textbook −y ln σ − (1 − y) ln(1 − σ).
        for &(z, y) in &[(-3.0f64, 0.0), (0.7, 1.0), (1.3, 0.4)] {
            let s = sigmoid(z);
            let textbook = -y * s.ln() - (1.0 - y) * (1.0 - s).ln();
            assert!((bce_with_logits(z, y).0 - textbook).abs() < 1e-12);
        }
        // No overflow at a huge logit.
        assert_eq!(bce_with_logits(1e4f32, 1.0).0, 0.0);
        assert_eq!(bce_with_logits(1e4f32, 0.0).0, 1e4);
    }
}
