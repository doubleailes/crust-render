use crate::common::random;
use rand::seq::SliceRandom;
use rand::thread_rng;

/// Generate a 2D CMJ sample grid
pub fn generate_cmj_2d(samples_per_side: usize) -> Vec<(f32, f32)> {
    let n = samples_per_side;
    let n_f32 = n as f32;

    let mut xs: Vec<usize> = (0..n).collect();
    let mut ys: Vec<usize> = (0..n).collect();

    // Shuffle for jittering
    xs.shuffle(&mut thread_rng());
    ys.shuffle(&mut thread_rng());

    let mut samples = Vec::with_capacity(n * n);

    for j in 0..n {
        for i in 0..n {
            let x = (i as f32 + (j as f32 + random()) / n_f32) / n_f32;
            let y = (ys[i] as f32 + (xs[j] as f32 + random()) / n_f32) / n_f32;
            samples.push((x, y));
        }
    }

    samples
}
