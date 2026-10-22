//! What OIIO's `maketx` does to an image on its way to a `.tx`, stage by
//! stage, in its order — the image half of Arnold 7.5's auto-tx (design
//! decision 3 of `tx-arnold-convention`). Each stage is checked against the
//! files Arnold wrote for the same sources (`tests/data/arnold_tx`).
//!
//! Everything works on `f32`, as `maketx` does: the source is read into
//! floats, every stage and every mip level is computed in floats, and the
//! samples are quantised to the file's type only as each level is written.
//! Building a level from the previous level's floats rather than from its
//! quantised bytes is what reproduces Arnold's levels exactly.

use crust_core::color::Conversion;

/// A sample type, of a source and of the `.tx` written from it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleType {
    U8,
    U16,
    Half,
    Float,
}

/// An image as `maketx` holds it between stages: interleaved `f32` samples,
/// each channel named — `R G B`, `R G B A`, `Y` or `Y A`.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub channels: Vec<&'static str>,
    pub pixels: Vec<f32>,
}

impl Image {
    pub fn nchannels(&self) -> usize {
        self.channels.len()
    }

    /// Which channel is alpha, if one is.
    pub fn alpha(&self) -> Option<usize> {
        self.channels.iter().position(|&c| c == "A")
    }

    /// The colour channels: all but alpha.
    pub fn colour(&self) -> usize {
        self.nchannels() - usize::from(self.alpha().is_some())
    }

    fn texels(&self) -> std::slice::ChunksExact<'_, f32> {
        self.pixels.chunks_exact(self.nchannels())
    }

    fn texels_mut(&mut self) -> std::slice::ChunksExactMut<'_, f32> {
        let n = self.nchannels();
        self.pixels.chunks_exact_mut(n)
    }
}

/// `--fixnan box3`: a non-finite sample becomes the mean of the finite
/// samples of its channel in the 3x3 neighbourhood around it, or 0 when none
/// is. Read from the image as it was, so a fixed sample never feeds another.
pub fn fix_nan_box3(img: &mut Image) {
    if img.pixels.iter().all(|v| v.is_finite()) {
        return;
    }
    let (w, h, n) = (img.width, img.height, img.nchannels());
    let src = img.pixels.clone();
    for y in 0..h {
        for x in 0..w {
            for c in 0..n {
                let o = (y * w + x) * n + c;
                if src[o].is_finite() {
                    continue;
                }
                let (mut sum, mut count) = (0.0f32, 0u32);
                for ny in y.saturating_sub(1)..(y + 2).min(h) {
                    for nx in x.saturating_sub(1)..(x + 2).min(w) {
                        let v = src[(ny * w + nx) * n + c];
                        if v.is_finite() {
                            sum += v;
                            count += 1;
                        }
                    }
                }
                img.pixels[o] = if count > 0 { sum / count as f32 } else { 0.0 };
            }
        }
    }
}

/// `--opaque-detect`: an alpha that is 1 everywhere is dropped. Whether it
/// was.
pub fn drop_opaque_alpha(img: &mut Image) -> bool {
    let Some(a) = img.alpha() else {
        return false;
    };
    if img.texels().any(|t| t[a] != 1.0) {
        return false;
    }
    let n = img.nchannels();
    img.pixels = img
        .pixels
        .chunks_exact(n)
        .flat_map(|t| {
            t.iter()
                .enumerate()
                .filter(|&(c, _)| c != a)
                .map(|(_, &v)| v)
        })
        .collect();
    img.channels.remove(a);
    true
}

/// `--constant-color-detect`: an image of one colour is shrunk to one tile
/// of it, `min(tile, w) x min(tile, h)`. The colour, when it was.
pub fn shrink_constant(img: &mut Image, tile: usize) -> Option<Vec<f32>> {
    let n = img.nchannels();
    let first = img.pixels[..n].to_vec();
    if img.texels().any(|t| t != first.as_slice()) {
        return None;
    }
    let (w, h) = (img.width.min(tile), img.height.min(tile));
    img.width = w;
    img.height = h;
    img.pixels = first.iter().copied().cycle().take(w * h * n).collect();
    Some(first)
}

/// `--monochrome-detect`: an RGB image whose three channels are equal at
/// every texel is stored as its one `Y` channel. Whether it was. An image
/// with alpha is left as it is.
pub fn to_monochrome(img: &mut Image) -> bool {
    if img.channels != ["R", "G", "B"] || img.texels().any(|t| t[0] != t[1] || t[1] != t[2]) {
        return false;
    }
    img.pixels = img.texels().map(|t| t[0]).collect();
    img.channels = vec!["Y"];
    true
}

/// The mean of every channel: what `oiio:AverageColor` starts from.
pub fn average(img: &Image) -> Vec<f32> {
    let n = img.nchannels();
    let mut sum = vec![0.0f64; n];
    for t in img.texels() {
        for (s, &v) in sum.iter_mut().zip(t) {
            *s += f64::from(v);
        }
    }
    let count = (img.width * img.height) as f64;
    sum.iter().map(|s| (s / count) as f32).collect()
}

/// `--unpremult`'s first half: colour divided by alpha, where alpha is not
/// zero.
pub fn unpremultiply(img: &mut Image) {
    let Some(a) = img.alpha() else { return };
    for t in img.texels_mut() {
        let alpha = t[a];
        if alpha != 0.0 && alpha != 1.0 {
            for (c, v) in t.iter_mut().enumerate() {
                if c != a {
                    *v /= alpha;
                }
            }
        }
    }
}

/// `--unpremult`'s second half: colour multiplied back by alpha.
pub fn premultiply(img: &mut Image) {
    let Some(a) = img.alpha() else { return };
    for t in img.texels_mut() {
        let alpha = t[a];
        for (c, v) in t.iter_mut().enumerate() {
            if c != a {
                *v *= alpha;
            }
        }
    }
}

/// `--colorconvert C T`: every texel's colour through `conversion`, alpha
/// untouched. A `Y` channel converts as the grey `(y, y, y)` it stands for
/// and keeps the result's first component.
pub fn convert(img: &mut Image, conversion: &Conversion) {
    let colour = img.colour();
    let mut rgb: Vec<f32> = img
        .texels()
        .flat_map(|t| match colour {
            1 => [t[0], t[0], t[0]],
            _ => [t[0], t[1], t[2]],
        })
        .collect();
    conversion.convert_rgb_slice(&mut rgb);
    for (t, c) in img.texels_mut().zip(rgb.as_chunks::<3>().0) {
        t[..colour].copy_from_slice(&c[..colour]);
    }
}

/// One colour — an `oiio:AverageColor` or `oiio:ConstantColor` — through
/// the same `--unpremult` / `--colorconvert` as the image, `channels`
/// naming its components.
pub fn convert_one(colour: &mut [f32], channels: &[&'static str], conversion: &Conversion) {
    let mut img = Image {
        width: 1,
        height: 1,
        channels: channels.to_vec(),
        pixels: colour.to_vec(),
    };
    unpremultiply(&mut img);
    convert(&mut img, conversion);
    premultiply(&mut img);
    colour.copy_from_slice(&img.pixels);
}

/// The next mip level: half the size, rounded down and at least 1, each
/// texel the bilinear sample of `img` at its own centre (in normalised
/// coordinates, edges clamped). That is OIIO's `maketx` box reduction: an
/// exact 2x2 mean on even axes and fractional weights on odd ones.
pub fn reduce(img: &Image) -> Image {
    let (w, h, n) = (img.width, img.height, img.nchannels());
    let (dw, dh) = ((w / 2).max(1), (h / 2).max(1));
    let mut pixels = vec![0.0f32; dw * dh * n];
    let taps = |dst: usize, len: usize, src: usize| {
        let s = (dst as f32 + 0.5) / len as f32 * src as f32 - 0.5;
        let s0 = s.floor();
        let f = s - s0;
        let clamp = |i: f32| (i.max(0.0) as usize).min(src - 1);
        (clamp(s0), clamp(s0 + 1.0), f)
    };
    for y in 0..dh {
        let (y0, y1, fy) = taps(y, dh, h);
        for x in 0..dw {
            let (x0, x1, fx) = taps(x, dw, w);
            let at = |xx: usize, yy: usize, c: usize| img.pixels[(yy * w + xx) * n + c];
            for c in 0..n {
                let top = at(x0, y0, c) * (1.0 - fx) + at(x1, y0, c) * fx;
                let bottom = at(x0, y1, c) * (1.0 - fx) + at(x1, y1, c) * fx;
                pixels[(y * dw + x) * n + c] = top * (1.0 - fy) + bottom * fy;
            }
        }
    }
    Image {
        width: dw,
        height: dh,
        channels: img.channels.clone(),
        pixels,
    }
}

/// Level 0 and every level below it, down to 1x1, each built from the one
/// above in floats.
pub fn mip_chain(level0: Image) -> Vec<Image> {
    let mut levels = vec![level0];
    loop {
        let last = levels.last().expect("level 0 always exists");
        if last.width <= 1 && last.height <= 1 {
            return levels;
        }
        let next = reduce(last);
        levels.push(next);
    }
}

/// A float sample as an 8-bit one: clamped to `[0, 1]` and rounded.
pub fn quantize_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// A float sample as a 16-bit one: clamped to `[0, 1]` and rounded.
pub fn quantize_u16(v: f32) -> u16 {
    (v.clamp(0.0, 1.0) * 65535.0 + 0.5) as u16
}

/// `v` as OIIO prints a float in an attribute: six significant digits,
/// trailing zeros dropped (`0.101961`, `4.75`, `25.5727`).
pub fn format_g6(v: f32) -> String {
    let v = f64::from(v);
    if v == 0.0 || !v.is_finite() {
        return if v.is_finite() {
            "0".into()
        } else {
            format!("{v}")
        };
    }
    let exp = v.abs().log10().floor() as i32;
    if !(-5..6).contains(&exp) {
        let s = format!("{:.5e}", v);
        let (mantissa, e) = s.split_once('e').expect("exponent");
        let mantissa = mantissa.trim_end_matches('0').trim_end_matches('.');
        let e: i32 = e.parse().expect("exponent");
        return format!("{mantissa}e{}{:02}", if e < 0 { '-' } else { '+' }, e.abs());
    }
    let decimals = (5 - exp).max(0) as usize;
    let s = format!("{v:.decimals$}");
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// Components joined as OIIO joins them in an attribute: `0.8,0.4,0.101961`.
pub fn format_colour(c: &[f32]) -> String {
    c.iter()
        .map(|&v| format_g6(v))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(w: usize, h: usize, channels: &[&'static str], pixels: Vec<f32>) -> Image {
        Image {
            width: w,
            height: h,
            channels: channels.to_vec(),
            pixels,
        }
    }

    #[test]
    fn a_nan_becomes_the_mean_of_its_finite_neighbours() {
        let mut i = img(
            3,
            2,
            &["Y"],
            vec![1.0, 2.0, 3.0, f32::NAN, 5.0, f32::INFINITY],
        );
        fix_nan_box3(&mut i);
        // (1 + 2 + 5) / 3, then (2 + 3 + 5) / 3: neither fixed value feeds
        // the other.
        assert_eq!(i.pixels, vec![1.0, 2.0, 3.0, 8.0 / 3.0, 5.0, 10.0 / 3.0]);
    }

    #[test]
    fn an_opaque_alpha_is_dropped_and_a_cutting_one_kept() {
        let mut i = img(
            2,
            1,
            &["R", "G", "B", "A"],
            vec![0.1, 0.2, 0.3, 1.0, 0.4, 0.5, 0.6, 1.0],
        );
        assert!(drop_opaque_alpha(&mut i));
        assert_eq!(
            (i.channels.clone(), i.pixels.clone()),
            (vec!["R", "G", "B"], vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6])
        );
        let mut i = img(1, 1, &["R", "G", "B", "A"], vec![0.1, 0.2, 0.3, 0.5]);
        assert!(!drop_opaque_alpha(&mut i));
    }

    #[test]
    fn a_constant_image_shrinks_to_a_tile_and_a_grey_one_to_y() {
        let mut i = img(100, 3, &["R", "G", "B"], [0.5f32, 0.5, 0.5].repeat(300));
        assert_eq!(shrink_constant(&mut i, 64), Some(vec![0.5, 0.5, 0.5]));
        assert_eq!((i.width, i.height), (64, 3));
        assert!(to_monochrome(&mut i));
        assert_eq!(i.channels, ["Y"]);
        assert_eq!(i.pixels.len(), 64 * 3);
    }

    #[test]
    fn an_even_level_is_the_2x2_mean_and_an_odd_one_rounds_down() {
        let i = img(2, 2, &["Y"], vec![1.0, 2.0, 3.0, 6.0]);
        assert_eq!(reduce(&i).pixels, vec![3.0]);
        let chain = mip_chain(img(37, 23, &["Y"], vec![0.25; 37 * 23]));
        let sizes: Vec<_> = chain.iter().map(|l| (l.width, l.height)).collect();
        assert_eq!(sizes, [(37, 23), (18, 11), (9, 5), (4, 2), (2, 1), (1, 1)]);
        assert!(chain.iter().all(|l| l.pixels.iter().all(|&v| v == 0.25)));
    }

    #[test]
    fn floats_print_as_oiio_prints_them() {
        for (v, s) in [
            (0.8f32, "0.8"),
            (26.0 / 255.0, "0.101961"),
            (4.25441, "4.25441"),
            (25.5727, "25.5727"),
            (1.5, "1.5"),
            (0.0, "0"),
            (0.563102, "0.563102"),
        ] {
            assert_eq!(format_g6(v), s);
        }
    }
}
