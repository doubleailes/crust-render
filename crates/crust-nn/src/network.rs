//! [`Network`]: the encoding feeding a stack of dense layers.

use std::ops::Range;

use crate::{Dense, GridConfig, HashGrid, Real, bce_with_logits, relu, relu_backward};

/// Samples per chunk of a batch: the unit of [`Network::batch_gradient`]'s
/// parallel work, and of its reduction. Fixed, so the reduction's order is
/// a function of the batch alone.
pub const CHUNK: usize = 32;

/// The widest layer a network may have, the encoding and the output layer
/// included: [`Network::infer`] keeps its activations on the stack.
pub const MAX_WIDTH: usize = 64;

/// Half-width of the uniform the hash table starts in (Instant NGP's ±1e-4):
/// small enough that the untrained encoding says nearly nothing.
const GRID_INIT: f32 = 1e-4;

/// A network's shape: the encoding, the hidden layers' widths (ReLU after
/// each), and the outputs (logits).
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub grid: GridConfig,
    pub hidden: Vec<usize>,
    pub outputs: usize,
}

/// A [`HashGrid`] encoding of a point in the unit cube, feeding dense layers
/// with ReLU between them, ending in one logit per output.
///
/// Every parameter lives in one flat vector — the hash table, then each
/// layer's weights and biases — so a gradient, an optimiser state and a
/// finite-difference probe are all one indexing scheme, and
/// [`Network::groups`] splits it in two for the optimiser.
#[derive(Clone, Debug)]
pub struct Network<R: Real = f32> {
    grid: HashGrid,
    /// Each dense layer, with where its parameters start in `params`.
    layers: Vec<(Dense, usize)>,
    params: Vec<R>,
}

/// One chunk's share of a batch's gradient (see [`Network::chunk_gradient`]).
#[derive(Clone, Debug)]
pub struct Gradient<R: Real = f32> {
    /// `∂L/∂θ` for the dense layers' parameters, indexed from the start of
    /// [`Network::mlp_params`].
    pub mlp: Vec<R>,
    /// The hash-table entries the chunk's samples read, in the order they
    /// read them, each with its share of `∂L/∂θ`. Sparse: a sample reads
    /// `levels × 8 × features` entries of a table of up to a million.
    pub grid: Vec<(u32, R)>,
    /// The chunk's loss, summed over its samples and outputs, unscaled.
    pub loss: f64,
}

/// The map that runs a batch's chunks one after another — what
/// [`Network::batch_gradient`] takes when the caller has no thread pool.
pub fn sequential<R: Real>(
    chunks: usize,
    chunk: &(dyn Fn(usize) -> Gradient<R> + Sync),
) -> Vec<Gradient<R>> {
    (0..chunks).map(chunk).collect()
}

impl<R: Real> Network<R> {
    /// A network of `shape`, initialised from `rand`'s draws in `[0, 1)`:
    /// the hash table uniform in ±1e-4, each layer's weights Xavier-uniform
    /// (`±√(6 / (in + out))`), the biases zero. The draws are taken in
    /// parameter order, so the same draws give the same network, bit for bit.
    ///
    /// # Panics
    /// If a layer is empty or wider than [`MAX_WIDTH`].
    pub fn new(shape: &Shape, mut rand: impl FnMut() -> f32) -> Self {
        let grid = HashGrid::new(shape.grid);
        let widths = || {
            std::iter::once(grid.outputs())
                .chain(shape.hidden.iter().copied())
                .chain(std::iter::once(shape.outputs))
        };
        assert!(
            widths().all(|w| w > 0 && w <= MAX_WIDTH),
            "every layer holds 1 to {MAX_WIDTH} values"
        );
        let mut params: Vec<R> = (0..grid.params())
            .map(|_| R::from_f32((2.0 * rand() - 1.0) * GRID_INIT))
            .collect();
        let mut layers = Vec::new();
        let mut inputs = grid.outputs();
        for outputs in widths().skip(1) {
            let layer = Dense { inputs, outputs };
            layers.push((layer, params.len()));
            let a = (6.0 / (inputs + outputs) as f32).sqrt();
            params.extend((0..inputs * outputs).map(|_| R::from_f32((2.0 * rand() - 1.0) * a)));
            params.extend(std::iter::repeat_n(R::ZERO, outputs));
            inputs = outputs;
        }
        Network {
            grid,
            layers,
            params,
        }
    }

    /// Every parameter: the hash table, then the dense layers.
    pub fn params(&self) -> &[R] {
        &self.params
    }

    pub fn params_mut(&mut self) -> &mut [R] {
        &mut self.params
    }

    /// The hash table's parameters, the first group.
    pub fn grid_params(&self) -> Range<usize> {
        0..self.grid.params()
    }

    /// The dense layers' parameters, the second group.
    pub fn mlp_params(&self) -> Range<usize> {
        self.grid.params()..self.params.len()
    }

    /// The two groups with their learning rates, for [`crate::Adam::new`].
    pub fn groups(&self, grid_lr: f32, mlp_lr: f32) -> Vec<(Range<usize>, f32)> {
        vec![(self.grid_params(), grid_lr), (self.mlp_params(), mlp_lr)]
    }

    pub fn grid(&self) -> &HashGrid {
        &self.grid
    }

    /// Logits per input.
    pub fn outputs(&self) -> usize {
        self.layers.last().map_or(0, |(l, _)| l.outputs)
    }

    /// The same network in another scalar: [`grad_check`] runs on an `f64`
    /// copy of an `f32` network.
    pub fn cast<S: Real>(&self) -> Network<S> {
        Network {
            grid: self.grid.clone(),
            layers: self.layers.clone(),
            params: self
                .params
                .iter()
                .map(|v| S::from_f64(v.to_f64()))
                .collect(),
        }
    }

    /// The logits at `x` (in the unit cube) into `out`, [`Network::outputs`]
    /// long. No allocation: the activations live on the stack.
    #[inline]
    pub fn infer(&self, x: [f32; 3], out: &mut [R]) {
        let mut a = [R::ZERO; MAX_WIDTH];
        let mut b = [R::ZERO; MAX_WIDTH];
        let (mut cur, mut next) = (&mut a, &mut b);
        let mut width = self.grid.outputs();
        self.grid
            .forward(&self.params[self.grid_params()], x, &mut cur[..width]);
        let last = self.layers.len() - 1;
        for (k, &(layer, at)) in self.layers.iter().enumerate() {
            let p = &self.params[at..at + layer.params()];
            if k == last {
                layer.forward(p, &cur[..width], out);
            } else {
                layer.forward(p, &cur[..width], &mut next[..layer.outputs]);
                relu(&mut next[..layer.outputs]);
                std::mem::swap(&mut cur, &mut next);
                width = layer.outputs;
            }
        }
    }

    /// The mean binary cross-entropy of the logits at `xs` against the labels
    /// `ys` (`outputs` per input, input-major).
    pub fn loss(&self, xs: &[[f32; 3]], ys: &[f32]) -> f64 {
        let o = self.outputs();
        debug_assert_eq!(ys.len(), xs.len() * o);
        let mut z = [R::ZERO; MAX_WIDTH];
        let mut sum = 0.0f64;
        for (x, y) in xs.iter().zip(ys.chunks_exact(o)) {
            self.infer(*x, &mut z[..o]);
            for (&z, &y) in z.iter().zip(y) {
                sum += bce_with_logits(z, R::from_f32(y)).0.to_f64();
            }
        }
        sum / (xs.len() * o).max(1) as f64
    }

    /// The gradient of `scale ×` the summed loss over one chunk of samples,
    /// sample by sample in order. A pure function of the network and the
    /// chunk, so chunks can run on any thread.
    pub fn chunk_gradient(&self, xs: &[[f32; 3]], ys: &[f32], scale: R) -> Gradient<R> {
        let o = self.outputs();
        debug_assert_eq!(ys.len(), xs.len() * o);
        let e = self.grid.outputs();
        let mlp = self.mlp_params();
        let reads = self.grid.config().levels * 8 * self.grid.config().features;
        let mut g = Gradient {
            mlp: vec![R::ZERO; mlp.len()],
            grid: Vec::with_capacity(xs.len() * reads),
            loss: 0.0,
        };
        // Each layer's input, kept for its backward pass: the encoding, then
        // every hidden layer's output after its ReLU.
        let mut starts = Vec::with_capacity(self.layers.len());
        let mut total = 0;
        for &(layer, _) in &self.layers {
            starts.push(total);
            total += layer.inputs;
        }
        let mut acts = vec![R::ZERO; total];
        let mut logits = [R::ZERO; MAX_WIDTH];
        let mut d = [R::ZERO; MAX_WIDTH];
        let mut d_in = [R::ZERO; MAX_WIDTH];
        let last = self.layers.len() - 1;
        for (x, y) in xs.iter().zip(ys.chunks_exact(o)) {
            self.grid
                .forward(&self.params[self.grid_params()], *x, &mut acts[..e]);
            for (k, &(layer, at)) in self.layers.iter().enumerate() {
                let p = &self.params[at..at + layer.params()];
                let (input, rest) = acts[starts[k]..].split_at_mut(layer.inputs);
                if k == last {
                    layer.forward(p, input, &mut logits[..o]);
                } else {
                    layer.forward(p, input, &mut rest[..layer.outputs]);
                    relu(&mut rest[..layer.outputs]);
                }
            }
            for ((d, &z), &y) in d.iter_mut().zip(&logits[..o]).zip(y) {
                let (loss, dz) = bce_with_logits(z, R::from_f32(y));
                g.loss += loss.to_f64();
                *d = dz * scale;
            }
            for k in (0..self.layers.len()).rev() {
                let (layer, at) = self.layers[k];
                let input = &acts[starts[k]..starts[k] + layer.inputs];
                layer.backward(
                    &self.params[at..at + layer.params()],
                    input,
                    &d[..layer.outputs],
                    Some(&mut d_in[..layer.inputs]),
                    &mut g.mlp[at - mlp.start..at - mlp.start + layer.params()],
                );
                if k > 0 {
                    relu_backward(input, &mut d_in[..layer.inputs]);
                }
                std::mem::swap(&mut d, &mut d_in);
            }
            // `d` is now ∂L/∂(encoding).
            self.grid
                .backward(*x, &d[..e], |i, v| g.grid.push((i as u32, v)));
        }
        g
    }

    /// The gradient of the mean loss over a batch into `grad` (laid out like
    /// [`Network::params`]), returning the mean loss.
    ///
    /// The batch is cut into fixed [`CHUNK`]s, which `map(chunks, chunk)`
    /// evaluates — `chunk(c)` for every `c` in `0..chunks`, returned in that
    /// order, on whatever threads it likes ([`sequential`], or a rayon
    /// `into_par_iter().map(chunk).collect()`). The chunks are then summed
    /// in chunk order, so the gradient is bit-identical whoever ran them.
    pub fn batch_gradient<M>(&self, xs: &[[f32; 3]], ys: &[f32], grad: &mut [R], map: M) -> f64
    where
        M: FnOnce(usize, &(dyn Fn(usize) -> Gradient<R> + Sync)) -> Vec<Gradient<R>>,
    {
        let (n, o) = (xs.len(), self.outputs());
        assert_eq!(ys.len(), n * o, "one label per input per output");
        assert_eq!(grad.len(), self.params.len());
        grad.fill(R::ZERO);
        if n == 0 {
            return 0.0;
        }
        let scale = R::ONE / R::from_f64((n * o) as f64);
        let chunks = n.div_ceil(CHUNK);
        let chunk = |c: usize| {
            let r = c * CHUNK..((c + 1) * CHUNK).min(n);
            self.chunk_gradient(&xs[r.clone()], &ys[r.start * o..r.end * o], scale)
        };
        let parts = map(chunks, &chunk);
        assert_eq!(parts.len(), chunks, "the map returns every chunk");
        let mlp = self.mlp_params();
        let mut loss = 0.0f64;
        for part in &parts {
            for (g, &v) in grad[mlp.clone()].iter_mut().zip(&part.mlp) {
                *g += v;
            }
            for &(i, v) in &part.grid {
                grad[i as usize] += v;
            }
            loss += part.loss;
        }
        loss / (n * o) as f64
    }
}

/// Checks `net`'s gradient over `(xs, ys)` against central differences of
/// its mean loss, `(L(θ + h) − L(θ − h)) / 2h`, for every parameter. Returns
/// each group's relative error, `‖analytic − numeric‖ / max(‖analytic‖,
/// ‖numeric‖)` — the hash table's, then the dense layers'.
///
/// In `f64` ([`Network::cast`]): an `f32` loss rounds at about 1e-7 of
/// itself, which is most of a central difference at any `h` small enough to
/// be one.
pub fn grad_check(net: &Network<f64>, xs: &[[f32; 3]], ys: &[f32], h: f64) -> [f64; 2] {
    let n = net.params.len();
    let mut analytic = vec![0.0; n];
    net.batch_gradient(xs, ys, &mut analytic, sequential);
    let mut probe = net.clone();
    let numeric: Vec<f64> = (0..n)
        .map(|i| {
            let v = net.params[i];
            probe.params[i] = v + h;
            let up = probe.loss(xs, ys);
            probe.params[i] = v - h;
            let down = probe.loss(xs, ys);
            probe.params[i] = v;
            (up - down) / (2.0 * h)
        })
        .collect();
    let error = |r: Range<usize>| {
        let (mut diff, mut a, mut b) = (0.0f64, 0.0f64, 0.0f64);
        for i in r {
            diff += (analytic[i] - numeric[i]).powi(2);
            a += analytic[i].powi(2);
            b += numeric[i].powi(2);
        }
        diff.sqrt() / a.sqrt().max(b.sqrt()).max(f64::MIN_POSITIVE)
    };
    [error(net.grid_params()), error(net.mlp_params())]
}

/// Seeded draws in `[0, 1)` for the tests, from `openqmc::pcg::Rng` (the
/// repository's one source of test randomness), in the `FnMut() -> f32`
/// shape [`Network::new`] takes.
#[cfg(test)]
pub(crate) fn test_rand(seed: u32) -> impl FnMut() -> f32 {
    let mut rng = openqmc::pcg::Rng::new(seed);
    move || rng.next_f32()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shape(outputs: usize) -> Shape {
        Shape {
            // 4 and 8 cells: the first fits 2^8 vertices, the second is
            // hashed, so the check covers both kinds of level.
            grid: GridConfig {
                levels: 2,
                features: 2,
                log2_entries: 8,
                base: 4,
                growth: 2.0,
            },
            hidden: vec![8, 8],
            outputs,
        }
    }

    fn batch(
        n: usize,
        outputs: usize,
        rand: &mut impl FnMut() -> f32,
    ) -> (Vec<[f32; 3]>, Vec<f32>) {
        let xs = (0..n).map(|_| [rand(), rand(), rand()]).collect();
        let ys = (0..n * outputs).map(|_| rand()).collect();
        (xs, ys)
    }

    /// Every parameter group's gradient matches central differences, on
    /// random inputs, random fractional labels and random parameters.
    #[test]
    fn gradients_match_central_differences() {
        let mut rand = test_rand(1);
        let mut net = Network::<f64>::new(&shape(3), &mut rand);
        assert!(net.grid().is_dense(0) && !net.grid().is_dense(1));
        // Parameters of order one, not the initial ±1e-4 table and zero
        // biases: every unit active or not by a margin, so no ±h probe
        // crosses a ReLU's kink.
        for p in net.params_mut() {
            *p = (2.0 * rand() - 1.0) as f64;
        }
        let (xs, ys) = batch(16, 3, &mut rand);
        let [grid, mlp] = grad_check(&net, &xs, &ys, 1e-6);
        assert!(grid < 1e-3, "hash grid: relative error {grid}");
        assert!(mlp < 1e-3, "dense layers: relative error {mlp}");
    }

    /// The same draws give the same network, bit for bit; other draws
    /// another.
    #[test]
    fn same_draws_same_network() {
        let a = Network::<f32>::new(&shape(5), test_rand(9));
        let b = Network::<f32>::new(&shape(5), test_rand(9));
        let c = Network::<f32>::new(&shape(5), test_rand(10));
        let bits = |n: &Network<f32>| n.params().iter().map(|v| v.to_bits()).collect::<Vec<_>>();
        assert_eq!(bits(&a), bits(&b));
        assert_ne!(bits(&a), bits(&c));
        // Biases start at zero; weights inside the Xavier bound.
        let (layer, at) = a.layers[0];
        let w = &a.params()[at..at + layer.inputs * layer.outputs];
        let bound = (6.0 / (layer.inputs + layer.outputs) as f32).sqrt();
        assert!(w.iter().all(|v| v.abs() <= bound));
        assert!(a.params()[a.grid_params()].iter().all(|v| v.abs() <= 1e-4));
    }

    /// The bits of an inference and of an Adam step, pinned to constants so
    /// that every leg of `scripts/test_simd_matrix.sh -p crust-nn` must
    /// produce the same ones: the matrix runs this test once per codegen.
    /// No `exp` is involved, so the constants are the arithmetic's alone,
    /// not the C library's.
    #[test]
    fn bits_are_the_same_under_every_codegen() {
        let fnv = |h: u64, bits: u32| (h ^ bits as u64).wrapping_mul(0x100_0000_01b3);
        let mut rand = test_rand(21);
        let mut net = Network::<f32>::new(&shape(7), &mut rand);
        // Parameters of order one, so the table's entries matter.
        for p in net.params_mut() {
            *p = 2.0 * rand() - 1.0;
        }
        let mut h = 0xcbf2_9ce4_8422_2325u64;
        let mut z = [0.0f32; 7];
        for _ in 0..64 {
            net.infer([rand(), rand(), rand()], &mut z);
            h = z.iter().fold(h, |h, v| fnv(h, v.to_bits()));
        }
        let mut adam = crate::Adam::new(net.params().len(), net.groups(1e-2, 1e-3));
        let grad: Vec<f32> = (0..net.params().len()).map(|_| rand() - 0.5).collect();
        for _ in 0..3 {
            adam.step(net.params_mut(), &grad);
        }
        h = net.params().iter().fold(h, |h, v| fnv(h, v.to_bits()));
        assert_eq!(h, 0x8279_243d_5cb3_6d28, "{h:#x}");
    }

    /// Training's forward pass is inference's: the loss a batch reports is
    /// the loss of the logits `infer` gives.
    #[test]
    fn training_forward_is_inference() {
        let mut rand = test_rand(3);
        let net = Network::<f32>::new(&shape(4), &mut rand);
        let (xs, ys) = batch(CHUNK, 4, &mut rand);
        let mut grad = vec![0.0; net.params().len()];
        // One chunk, so both sum in the same order.
        let batch = net.batch_gradient(&xs, &ys, &mut grad, sequential);
        assert_eq!(batch.to_bits(), net.loss(&xs, &ys).to_bits());
    }

    /// A batch's gradient is bit-identical at 1 and at 8 rayon threads, and
    /// to the sequential map: chunks are summed in chunk order, whoever ran
    /// them.
    #[test]
    fn batch_gradient_is_thread_count_free() {
        let mut rand = test_rand(4);
        let net = Network::<f32>::new(&shape(6), &mut rand);
        // Seven chunks and a partial one.
        let (xs, ys) = batch(7 * CHUNK + 5, 6, &mut rand);
        let run = |threads: usize| {
            let pool = rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .expect("pool");
            let mut grad = vec![0.0f32; net.params().len()];
            let loss = pool.install(|| {
                net.batch_gradient(&xs, &ys, &mut grad, |n, chunk| {
                    use rayon::prelude::*;
                    (0..n).into_par_iter().map(chunk).collect()
                })
            });
            (
                grad.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                loss.to_bits(),
            )
        };
        let one = run(1);
        assert_eq!(one, run(8));
        let mut grad = vec![0.0f32; net.params().len()];
        let loss = net.batch_gradient(&xs, &ys, &mut grad, sequential);
        assert_eq!(
            one,
            (grad.iter().map(|v| v.to_bits()).collect(), loss.to_bits())
        );
        assert!(grad.iter().any(|&g| g != 0.0));
    }
}
