//! Crust Render's tiny-network kit: what a neural visibility cache needs to
//! learn a function of position on the CPU, and nothing else.
//!
//! - [`HashGrid`]: a multi-resolution hash encoding (Müller et al. 2022),
//!   trilinear, with a backward pass that scatters into its table.
//! - [`Dense`]: a fully connected layer, forward and backward; [`relu`] and
//!   [`sigmoid`]; the [`bce_with_logits`] loss.
//! - [`Network`]: the encoding feeding a stack of dense layers, with
//!   [`grad_check`] to test its gradients by central differences.
//! - [`Adam`], with a learning rate per parameter group.
//!
//! Like `crust-rt` and `crust-mtlx`, it knows nothing of the renderer, has no
//! dependencies and no `unsafe`. Everything is bit-deterministic: plain
//! arithmetic in a fixed loop order with no `mul_add`, so a result is the same
//! under every target-feature set (`scripts/test_simd_matrix.sh -p crust-nn`).
//! Two things are the caller's: the random numbers that initialise a network
//! ([`Network::new`] takes a `FnMut() -> f32`, so the renderer can feed it
//! `openqmc`), and the parallelism of a batch ([`Network::batch_gradient`]
//! takes the map that runs its chunks, and sums them in chunk order whatever
//! ran them).
//!
//! ```
//! use crust_nn::{Adam, GridConfig, Network, Shape};
//!
//! let shape = Shape {
//!     grid: GridConfig { levels: 2, features: 2, log2_entries: 8, base: 4, growth: 2.0 },
//!     hidden: vec![8],
//!     outputs: 1,
//! };
//! let mut seed = 1u32;
//! let mut rand = move || {
//!     seed = seed.wrapping_mul(747_796_405).wrapping_add(2_891_336_453);
//!     (seed >> 8) as f32 / (1u32 << 24) as f32
//! };
//! let mut net = Network::new(&shape, &mut rand);
//! let mut adam = Adam::new(net.params().len(), net.groups(1e-2, 1e-3));
//! // Left of x = 0.5 is lit, right of it is not.
//! let xs: Vec<[f32; 3]> = (0..64).map(|i| [i as f32 / 63.0, 0.5, 0.5]).collect();
//! let ys: Vec<f32> = xs.iter().map(|x| if x[0] < 0.5 { 1.0 } else { 0.0 }).collect();
//! let mut grad = vec![0.0; net.params().len()];
//! let first = net.loss(&xs, &ys);
//! for _ in 0..200 {
//!     net.batch_gradient(&xs, &ys, &mut grad, crust_nn::sequential);
//!     adam.step(net.params_mut(), &grad);
//! }
//! assert!(net.loss(&xs, &ys) < first);
//! ```

#![forbid(unsafe_code)]

mod adam;
mod dense;
mod hash_grid;
mod network;
mod real;

pub use adam::Adam;
pub use dense::{Dense, bce_with_logits, relu, relu_backward, sigmoid};
pub use hash_grid::{GridConfig, HashGrid};
pub use network::{CHUNK, Gradient, MAX_WIDTH, Network, Shape, grad_check, sequential};
pub use real::Real;
