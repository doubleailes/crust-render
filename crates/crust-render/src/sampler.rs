use rand::seq::SliceRandom;
use utils::random;

/// Generate a 2D CMJ sample grid
pub fn generate_cmj_2d(samples_per_side: usize) -> Vec<(f32, f32)> {
    let n = samples_per_side;
    let n_f32 = n as f32;

    let mut xs: Vec<usize> = (0..n).collect();
    let mut ys: Vec<usize> = (0..n).collect();

    // Shuffle for jittering
    xs.shuffle(&mut rand::rng());
    ys.shuffle(&mut rand::rng());

    let mut samples = Vec::with_capacity(n * n);

    for j in 0..n {
        for i in 0..n {
            let x = (i as f32 + (j as f32 + random()) / n_f32) / n_f32;
            let y = (j as f32 + (i as f32 + random()) / n_f32) / n_f32;
            samples.push((x, y));
        }
    }

    samples
}
