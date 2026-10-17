//! ID mattes: per pixel, one sample per name the pixel's camera samples hit
//! first, holding that name's coverage — and, for a coloured matte, its
//! share of the beauty — and its mean depth. What an OpenEXRId file stores
//! (a `deepRaster` product; `crust-render`'s `exrid.rs` writes it).
//!
//! A sample's coverage is the beauty's own filter weight, so a matte is
//! filtered exactly as every filtered AOV is: per pixel, the coverage of the
//! names plus that of what no name covers (the background, a volume) sums
//! to one. Cutouts were passed before the first hit was recorded, so a
//! cutout leaf adds nothing and the surface behind it what it shows; a thin
//! wall is a hit, as for every data AOV.
//!
//! A unit keeps, per pixel, a short list searched by name — never a
//! full-frame map. A pixel can hold at most [`MAX_NAMES_PER_PIXEL`] names;
//! past that, a new name's samples go to one spill entry ([`SPILL`], written
//! as [`SPILL_NAME`]), so coverage still sums to one where a pixel sees more
//! objects than that.

use glam::Vec3A;

use crate::aov::MatteKey;

/// The most names one pixel of a matte holds, the spill included.
pub const MAX_NAMES_PER_PIXEL: usize = 64;

/// The name index of the spill entry ([`SPILL_NAME`]).
pub const SPILL: u32 = u32::MAX - 1;

/// What the spill entry is called in a matte's name table. Not a path (a
/// prim name cannot hold `:`), so it never collides with one.
pub const SPILL_NAME: &str = "crust:spill";

/// One name's running sums over a unit pixel's samples.
#[derive(Debug, Clone, Copy)]
struct Entry {
    name: u32,
    /// `Σ w·(r, g, b, 1)`: the weighted radiance (coloured mattes only) and
    /// the weight — the coverage once normalised.
    sum: [f32; 4],
    /// `Σ |w|·z` and `Σ |w|`: the depth averaged by the weights' sizes (a
    /// signed filter's lobes would cancel a plain weighted mean).
    z: f32,
    zw: f32,
    /// `Σ z` and the count: the mean when every weight is zero.
    zn: f32,
    n: u32,
}

/// One matte's per-pixel entries over a work unit.
#[derive(Debug, Clone)]
pub(crate) struct UnitMatte {
    key: MatteKey,
    pixels: Vec<Vec<Entry>>,
}

impl UnitMatte {
    pub(crate) fn new(key: MatteKey, pixels: usize) -> Self {
        UnitMatte {
            key,
            pixels: vec![Vec::new(); pixels],
        }
    }

    pub(crate) fn key(&self) -> MatteKey {
        self.key
    }

    /// Folds one camera sample whose first hit names `name`, at camera depth
    /// `depth`, into pixel `p`. `weight` is the beauty's filter weight,
    /// `radiance` the sample's (unweighted) light.
    pub(crate) fn add(&mut self, p: usize, name: u32, depth: f32, weight: f32, radiance: Vec3A) {
        let list = &mut self.pixels[p];
        let at = match list.iter().position(|e| e.name == name) {
            Some(i) => i,
            None if list.len() + 1 < MAX_NAMES_PER_PIXEL => {
                list.push(Entry::new(name));
                list.len() - 1
            }
            // Full: the last slot is the spill's.
            None => match list.iter().position(|e| e.name == SPILL) {
                Some(i) => i,
                None => {
                    list.push(Entry::new(SPILL));
                    list.len() - 1
                }
            },
        };
        let e = &mut list[at];
        if self.key.color {
            e.sum[0] += weight * radiance.x;
            e.sum[1] += weight * radiance.y;
            e.sum[2] += weight * radiance.z;
        }
        e.sum[3] += weight;
        e.z += weight.abs() * depth;
        e.zw += weight.abs();
        e.zn += depth;
        e.n += 1;
    }
}

impl Entry {
    fn new(name: u32) -> Self {
        Entry {
            name,
            sum: [0.0; 4],
            z: 0.0,
            zw: 0.0,
            zn: 0.0,
            n: 0,
        }
    }

    fn depth(&self) -> f32 {
        if self.zw > 0.0 {
            self.z / self.zw
        } else {
            self.zn / self.n.max(1) as f32
        }
    }
}

/// One sample of a matte pixel: a name, its share of the pixel —
/// `(r, g, b, coverage)`, normalised by the pixel's filter weights as the
/// beauty is; `r, g, b` are 0 in a matte without colour — and its depth.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatteSample {
    /// The name's index in the world's identity table for the matte's key
    /// ([`crate::identity::Identity::names`]), or [`SPILL`].
    pub name: u32,
    pub value: [f32; 4],
    /// Camera depth, the weighted mean over the name's samples.
    pub z: f32,
}

/// One matte over a film's pixels: each pixel's samples, sorted front to
/// back (by depth, then name), stored one pixel after another in the order
/// the pixels were gathered.
#[derive(Debug, Clone)]
pub(crate) struct FilmMatte {
    key: MatteKey,
    /// Per pixel (the film's indexing), its samples' start and count.
    spans: Vec<(u32, u32)>,
    samples: Vec<MatteSample>,
}

impl FilmMatte {
    pub(crate) fn new(key: MatteKey, pixels: usize) -> Self {
        FilmMatte {
            key,
            spans: vec![(0, 0); pixels],
            samples: Vec::new(),
        }
    }

    pub(crate) fn key(&self) -> MatteKey {
        self.key
    }

    /// The samples of film pixel `q`.
    pub(crate) fn pixel(&self, q: usize) -> &[MatteSample] {
        let (start, n) = self.spans[q];
        &self.samples[start as usize..(start + n) as usize]
    }

    /// Stores unit pixel `p` as film pixel `q`, normalised as the beauty's
    /// estimate is: by the weights' sum, or by the sample count where the
    /// weights cancel to nothing. A pixel that took no sample stays empty.
    pub(crate) fn store(
        &mut self,
        unit: &UnitMatte,
        p: usize,
        q: usize,
        weight_sum: f32,
        taken: u32,
    ) {
        if taken == 0 {
            self.spans[q] = (0, 0);
            return;
        }
        let norm = if weight_sum > 0.0 {
            weight_sum
        } else {
            taken as f32
        };
        let mut out: Vec<MatteSample> = unit.pixels[p]
            .iter()
            .map(|e| MatteSample {
                name: e.name,
                value: e.sum.map(|s| s / norm),
                z: e.depth(),
            })
            .collect();
        sort_front_to_back(&mut out);
        self.put(q, &out);
    }

    fn put(&mut self, q: usize, samples: &[MatteSample]) {
        self.spans[q] = (self.samples.len() as u32, samples.len() as u32);
        self.samples.extend_from_slice(samples);
    }

    /// Blends the mattes of independent passes with the beauty's per-pass
    /// shares (`weights[k] / total`, as [`crate::AovFilm`] blends its
    /// planes): each name's share is the same linear combination of passes
    /// as the beauty, and its depth the mean of the passes' weighted by the
    /// share of coverage each gave it.
    pub(crate) fn blend(films: &[&FilmMatte], weights: &[f64], total: f64) -> FilmMatte {
        let first = films.first().expect("at least one pass");
        let mut out = FilmMatte::new(first.key, first.spans.len());
        let mut merged: Vec<(MatteSample, f64, f64)> = Vec::new();
        for q in 0..first.spans.len() {
            merged.clear();
            for (film, w) in films.iter().zip(weights) {
                let share = *w / total;
                if share == 0.0 {
                    continue;
                }
                for s in film.pixel(q) {
                    let zw = share * f64::from(s.value[3].abs());
                    let at = match merged.iter().position(|(m, _, _)| m.name == s.name) {
                        Some(i) => i,
                        None => {
                            merged.push((
                                MatteSample {
                                    name: s.name,
                                    value: [0.0; 4],
                                    z: 0.0,
                                },
                                0.0,
                                0.0,
                            ));
                            merged.len() - 1
                        }
                    };
                    let (m, z, w) = &mut merged[at];
                    for (d, v) in m.value.iter_mut().zip(s.value) {
                        *d += v * share as f32;
                    }
                    *z += zw * f64::from(s.z);
                    *w += zw;
                    // The plain mean, while no pass gave the name coverage.
                    if *w == 0.0 {
                        m.z = s.z;
                    }
                }
            }
            let mut samples: Vec<MatteSample> = merged
                .iter()
                .map(|(m, z, w)| MatteSample {
                    z: if *w > 0.0 { (*z / *w) as f32 } else { m.z },
                    ..*m
                })
                .collect();
            sort_front_to_back(&mut samples);
            out.put(q, &samples);
        }
        out
    }

    /// Multiplies a coloured matte's radiance by `scale` (the camera's
    /// exposure); coverage and depth are not light.
    pub(crate) fn apply_exposure(&mut self, scale: f32) {
        if self.key.color {
            for s in &mut self.samples {
                for c in &mut s.value[..3] {
                    *c *= scale;
                }
            }
        }
    }
}

/// Front to back, ties by name: the order a deep pixel's samples are
/// written in, the same whichever order the pixel's names were met.
fn sort_front_to_back(samples: &mut [MatteSample]) {
    samples.sort_by(|a, b| a.z.total_cmp(&b.z).then(a.name.cmp(&b.name)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aov::NameKey;

    const KEY: MatteKey = MatteKey {
        names: NameKey::Prim,
        color: true,
    };

    #[test]
    fn coverage_is_the_filter_weight_share_and_samples_sort_by_depth() {
        let mut unit = UnitMatte::new(KEY, 1);
        unit.add(0, 7, 5.0, 1.0, Vec3A::new(1.0, 0.0, 0.0));
        unit.add(0, 3, 2.0, 2.0, Vec3A::new(0.0, 1.0, 0.0));
        unit.add(0, 7, 7.0, 1.0, Vec3A::new(1.0, 0.0, 0.0));
        let mut film = FilmMatte::new(KEY, 1);
        // A fourth sample of weight 1 escaped: the background's quarter.
        film.store(&unit, 0, 0, 5.0, 4);
        let px = film.pixel(0);
        assert_eq!(px.len(), 2);
        assert_eq!(px[0].name, 3);
        assert_eq!(px[0].value, [0.0, 0.4, 0.0, 0.4]);
        assert_eq!(px[0].z, 2.0);
        assert_eq!(px[1].name, 7);
        assert_eq!(px[1].value, [0.4, 0.0, 0.0, 0.4]);
        assert_eq!(px[1].z, 6.0);
    }

    #[test]
    fn a_pixel_past_its_names_spills_and_still_sums_to_one() {
        let mut unit = UnitMatte::new(KEY, 1);
        let n = MAX_NAMES_PER_PIXEL as u32 + 10;
        for name in 0..n {
            unit.add(0, name, 1.0 + name as f32, 1.0, Vec3A::ONE);
        }
        let mut film = FilmMatte::new(KEY, 1);
        film.store(&unit, 0, 0, n as f32, n);
        let px = film.pixel(0);
        assert_eq!(px.len(), MAX_NAMES_PER_PIXEL);
        let spill = px.iter().find(|s| s.name == SPILL).expect("a spill");
        // The 63 first names have one sample each; the rest went to the
        // spill.
        assert!((spill.value[3] * n as f32 - (n - 63) as f32).abs() < 1e-4);
        let total: f32 = px.iter().map(|s| s.value[3]).sum();
        assert!((total - 1.0).abs() < 1e-5, "{total}");
    }

    #[test]
    fn passes_blend_by_share_and_exposure_scales_colour_only() {
        let mut a = UnitMatte::new(KEY, 1);
        a.add(0, 1, 2.0, 1.0, Vec3A::ONE);
        let mut b = UnitMatte::new(KEY, 1);
        b.add(0, 1, 4.0, 1.0, Vec3A::splat(3.0));
        b.add(0, 2, 1.0, 1.0, Vec3A::ONE);
        let (mut fa, mut fb) = (FilmMatte::new(KEY, 1), FilmMatte::new(KEY, 1));
        fa.store(&a, 0, 0, 1.0, 1);
        fb.store(&b, 0, 0, 2.0, 2);
        let mut blended = FilmMatte::blend(&[&fa, &fb], &[1.0, 3.0], 4.0);
        let px = blended.pixel(0);
        assert_eq!(px[0].name, 2);
        assert_eq!(px[0].value[3], 0.375);
        assert_eq!(px[1].name, 1);
        assert_eq!(px[1].value[3], 0.25 + 0.375);
        assert_eq!(px[1].value[0], 0.25 + 0.75 * 1.5);
        // Depth weighted by the coverage each pass gave the name.
        let z = (0.25 * 2.0 + 0.375 * 4.0) / (0.25 + 0.375);
        assert!((px[1].z - z).abs() < 1e-6);
        blended.apply_exposure(2.0);
        assert_eq!(blended.pixel(0)[0].value, [0.75, 0.75, 0.75, 0.375]);
    }
}
