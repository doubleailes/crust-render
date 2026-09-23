//! IES LM-63 photometric files → [`crust_core::IesProfile`].
//!
//! A port of the reader OpenUSD's hdEmbree vendors for `ShapingAPI`
//! (`pxrIES/ies.cpp`, itself Blender Cycles' `util/ies.cpp`, Apache-2.0), so
//! a profile decodes into the same table the UsdLux reference samples. The
//! parser is deliberately as forgiving as that one — the comment it carries
//! is worth repeating: "in practice, IES files are all over the place" — and
//! normalises every photometric type to type C spanning the full azimuth,
//! which is the one layout the evaluator in crust-core reads.
//!
//! Two departures, both fixes rather than choices:
//!
//! - The type A conversion walks its angles backwards with a `size_t` that
//!   can never go below zero, so upstream loops until it faults on the first
//!   type A file it meets. Here it terminates.
//! - The file is read as bytes and decoded lossily: LM-63 headers are free
//!   text and routinely Latin-1 (`°`), which a strict UTF-8 read refuses
//!   outright. Only the numeric block matters, and it is ASCII.

use crust_core::IesProfile;
use std::path::Path;

/// Reads and decodes an `.ies` file. `None` for an unreadable file or one
/// whose numeric block does not parse.
pub fn load_ies(path: &Path) -> Option<IesProfile> {
    let bytes = std::fs::read(path).ok()?;
    parse_ies(&String::from_utf8_lossy(&bytes))
}

/// Photometric types as LM-63 numbers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IesType {
    C = 1,
    B = 2,
    A = 3,
}

/// Decodes LM-63 text into a type-C profile over 0–360° of azimuth.
pub fn parse_ies(text: &str) -> Option<IesProfile> {
    let text = text.replace(',', " ");
    // The numeric block starts at the TILT line, which must begin a line.
    let tilt = if text.starts_with("TILT=") {
        0
    } else {
        text.find("\nTILT=")? + 1
    };
    let rest = &text[tilt..];
    let (numbers, include) = match rest.strip_prefix("TILT=INCLUDE") {
        Some(after) => (after, true),
        None => (&rest[rest.find('\n')?..], false),
    };
    // `f64::from_str` accepts `inf` and `NaN`; neither is a photometric
    // value, and either would reach every radiance the light emits.
    let mut tok = numbers
        .split_whitespace()
        .map(|t| t.parse::<f64>().ok().filter(|x| x.is_finite()));
    let mut next = || tok.next().flatten();

    if include {
        next()?; // lamp-to-luminaire geometry
        let n_tilt = next()? as usize;
        for _ in 0..2 * n_tilt {
            next()?;
        }
    }

    next()?; // number of lamps
    next()?; // lumens per lamp
    let mut factor = next()?; // candela multiplier
    let n_v = next()? as usize;
    let n_h = next()? as usize;
    let kind = match next()? as i64 {
        1 => IesType::C,
        2 => IesType::B,
        3 => IesType::A,
        _ => return None,
    };
    next()?; // units
    next()?; // width
    next()?; // length
    next()?; // height
    factor *= next()?; // ballast factor
    factor *= next()?; // ballast-lamp photometric factor
    next()?; // input watts
    // No candela → watt conversion: UsdLux is photometric, and hdEmbree
    // builds its reader with that conversion compiled out.

    // Each axis is bounded, and so is their product: the table is allocated
    // before a single value is read, so a header declaring 100 000 × 100 000
    // would ask for 40 GB. Real profiles are at most a few hundred angles on
    // each axis (181 × 361 = 65 341 at one-degree spacing over the sphere).
    if n_v == 0
        || n_h == 0
        || n_v > MAX_IES_ANGLES
        || n_h > MAX_IES_ANGLES
        || n_v.checked_mul(n_h).is_none_or(|n| n > MAX_IES_SAMPLES)
    {
        return None;
    }
    let mut v: Vec<f32> = (0..n_v)
        .map(|_| next().map(|x| x as f32))
        .collect::<Option<_>>()?;
    let mut h: Vec<f32> = (0..n_h)
        .map(|_| next().map(|x| x as f32))
        .collect::<Option<_>>()?;
    let mut intensity: Vec<Vec<f32>> = (0..n_h)
        .map(|_| {
            (0..n_v)
                .map(|_| next().map(|x| (factor * x) as f32))
                .collect::<Option<Vec<f32>>>()
        })
        .collect::<Option<_>>()?;

    match kind {
        IesType::A => process_type_a(&mut v, &mut h, &mut intensity),
        IesType::B => process_type_b(&mut v, &mut h, &mut intensity),
        IesType::C => process_type_c(&mut h, &mut intensity),
    }
    let rad = |a: Vec<f32>| a.into_iter().map(f32::to_radians).collect();
    IesProfile::new(rad(v), rad(h), intensity)
}

/// Most angles either axis of an IES table may declare.
const MAX_IES_ANGLES: usize = 100_000;
/// Most candela samples a table may declare — ~15× a one-degree full sphere.
const MAX_IES_SAMPLES: usize = 1_000_000;

fn angle_close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

/// Type B has a horizontal polar axis; like upstream, transpose it into the
/// type A/C frame (the user rotates the light), then mirror a quadrant or
/// shift a half to 0–180°.
fn process_type_b(v: &mut Vec<f32>, h: &mut Vec<f32>, intensity: &mut Vec<Vec<f32>>) {
    let transposed: Vec<Vec<f32>> = (0..v.len())
        .map(|i| (0..h.len()).map(|j| intensity[j][i]).collect())
        .collect();
    *intensity = transposed;
    std::mem::swap(h, v);

    if angle_close(h[0], 0.0) {
        let n = h.len();
        let mut new_h = Vec::with_capacity(2 * n - 1);
        let mut new_i = Vec::with_capacity(2 * n - 1);
        for i in (1..n).rev() {
            new_h.push(90.0 - h[i]);
            new_i.push(intensity[i].clone());
        }
        for i in 0..n {
            new_h.push(90.0 + h[i]);
            new_i.push(intensity[i].clone());
        }
        *h = new_h;
        *intensity = new_i;
    } else {
        h.iter_mut().for_each(|a| *a += 90.0);
    }

    if angle_close(v[0], 0.0) {
        let n = v.len();
        let mut new_v = Vec::with_capacity(2 * n - 1);
        for i in (1..n).rev() {
            new_v.push(90.0 - v[i]);
        }
        for &a in v.iter() {
            new_v.push(90.0 + a);
        }
        for row in intensity.iter_mut() {
            let mut new_row: Vec<f32> = (1..n).rev().map(|j| row[j]).collect();
            new_row.extend_from_slice(row);
            *row = new_row;
        }
        *v = new_v;
    } else {
        v.iter_mut().for_each(|a| *a += 90.0);
    }
}

/// Type A: vertical angles offset by 90°, and −90…90° of horizontal angle
/// mapped to 270…90° of type C — mirrored when the file gives only 0…90°.
fn process_type_a(v: &mut [f32], h: &mut Vec<f32>, intensity: &mut Vec<Vec<f32>>) {
    v.iter_mut().for_each(|a| *a += 90.0);
    let n = h.len();
    let mut new_h = Vec::with_capacity(2 * n);
    let mut new_i = Vec::with_capacity(2 * n);
    for i in (0..n).rev() {
        new_h.push(180.0 - h[i]);
        new_i.push(intensity[i].clone());
    }
    if angle_close(h[0], 0.0) {
        for i in 1..n {
            new_h.push(180.0 + h[i]);
            new_i.push(intensity[i].clone());
        }
    }
    *h = new_h;
    *intensity = new_i;
}

/// Type C: bring every symmetry the format allows out to the full circle.
fn process_type_c(h: &mut Vec<f32>, intensity: &mut Vec<Vec<f32>>) {
    if angle_close(h[0], 90.0) {
        // Stored 90–270°: rotate to the regular 0–180°.
        h.iter_mut().for_each(|a| *a -= 90.0);
    }
    if h.len() == 1 {
        // Rotationally symmetric.
        h[0] = 0.0;
        h.push(360.0);
        intensity.push(intensity[0].clone());
    }
    if angle_close(*h.last().unwrap(), 90.0) {
        // One quadrant: mirror to two (then to four, below).
        for i in (0..h.len() - 1).rev() {
            h.push(180.0 - h[i]);
            intensity.push(intensity[i].clone());
        }
    }
    if angle_close(*h.last().unwrap(), 180.0) {
        // One half: mirror to the full circle.
        for i in (0..h.len() - 1).rev() {
            h.push(360.0 - h[i]);
            intensity.push(intensity[i].clone());
        }
    }
    // Some files omit the 360° entry, which must equal 0°'s. Restore it when
    // the spacing makes the gap unambiguous.
    let n = h.len();
    if n >= 2 && angle_close(h[0], 0.0) && !angle_close(h[n - 1], 360.0) {
        let last_step = h[n - 1] - h[n - 2];
        let first_step = h[1] - h[0];
        let gap = 360.0 - h[n - 1];
        if angle_close(last_step, gap) || angle_close(first_step, gap) {
            h.push(360.0);
            intensity.push(intensity[0].clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    /// A rotationally symmetric downlight: 1000 cd at nadir falling to 0 at
    /// 90°, one horizontal angle. The header carries Latin-1 on purpose.
    const DOWNLIGHT: &[u8] = b"IESNA:LM-63-2002\n[TEST] 30\xb0 downlight\nTILT=NONE\n\
1 1000 1 4 1 1 2 0.1 0.1 0.0\n1 1 50\n0 30 60 90\n0\n1000 800 300 0\n";

    #[test]
    fn a_symmetric_type_c_file_covers_the_full_circle() {
        let p = parse_ies(&String::from_utf8_lossy(DOWNLIGHT)).expect("parses");
        // Any azimuth, nadir.
        for phi in [0.0, 1.0, 3.0, 6.0] {
            assert_eq!(p.eval(0.0, phi, 0.0), 1000.0);
        }
        let at45 = p.eval(45f32.to_radians(), 2.0, 0.0);
        assert!(
            (at45 - 550.0).abs() < 1e-2,
            "linear between 800 and 300: {at45}"
        );
        // Beyond the last vertical angle the table has nothing.
        assert_eq!(p.eval(120f32.to_radians(), 0.0, 0.0), 0.0);
        assert!(p.power() > 0.0);
    }

    #[test]
    fn multipliers_scale_the_candela_values() {
        // candela multiplier 2, ballast 0.5, ballast-lamp factor 3 → ×3.
        let text = "TILT=NONE\n1 1000 2 2 1 1 2 0 0 0\n0.5 3 50\n0 90\n0\n10 0\n";
        let p = parse_ies(text).expect("parses");
        assert_eq!(p.eval(0.0, 0.0, 0.0), 30.0);
    }

    #[test]
    fn tilt_include_blocks_are_skipped() {
        let text =
            "hdr\nTILT=INCLUDE\n1\n2\n0 90\n1 1\n1 1000 1 2 1 1 2 0 0 0\n1 1 50\n0 90\n0\n7 7\n";
        let p = parse_ies(text).expect("parses");
        assert_eq!(p.eval(0.1, 0.0, 0.0), 7.0);
    }

    /// One quadrant (0–90°) mirrors out to four: the value authored at 30°
    /// azimuth reappears at 150°, 210° and 330°.
    #[test]
    fn a_quadrant_mirrors_to_the_full_circle() {
        let text = "TILT=NONE\n1 1000 1 2 4 1 2 0 0 0\n1 1 50\n0 90\n0 30 60 90\n\
1 1\n2 2\n3 3\n4 4\n";
        let p = parse_ies(text).expect("parses");
        let at = |deg: f32| p.eval(0.1, deg.to_radians(), 0.0);
        for deg in [30.0, 150.0, 210.0, 330.0] {
            assert!((at(deg) - 2.0).abs() < 1e-3, "{deg}°: {}", at(deg));
        }
        assert!((at(90.0) - 4.0).abs() < 1e-3 && (at(270.0) - 4.0).abs() < 1e-3);
    }

    /// Upstream never returns from a type A file; this one must.
    #[test]
    fn type_a_terminates_and_maps_to_type_c_angles() {
        let text = "TILT=NONE\n1 1000 1 3 2 3 2 0 0 0\n1 1 50\n-90 0 90\n0 90\n\
5 6 7\n8 9 10\n";
        let p = parse_ies(text).expect("parses");
        // Vertical 0° (type A) is 90° (type C); horizontal 0° is 180°.
        let v = p.eval(0.5 * PI, PI, 0.0);
        assert!((v - 6.0).abs() < 1e-3, "{v}");
    }

    #[test]
    fn garbage_is_refused_not_misread() {
        assert!(parse_ies("").is_none());
        assert!(parse_ies("no tilt line here").is_none());
        assert!(
            parse_ies("TILT=NONE\n1 1000 1 4 1 9 2 0 0 0\n1 1 50\n").is_none(),
            "type 9"
        );
        assert!(
            parse_ies("TILT=NONE\n1 1000 1 4 1 1 2 0 0 0\n1 1 50\n0 30\n").is_none(),
            "truncated"
        );
        // Each axis within bounds, their product not: refused from the header.
        assert!(
            parse_ies("TILT=NONE\n1 1000 1 90000 90000 1 2 0 0 0\n1 1 50\n").is_none(),
            "oversized"
        );
        // A non-finite multiplier or candela value.
        assert!(parse_ies("TILT=NONE\n1 1000 inf 2 1 1 2 0 0 0\n1 1 50\n0 90\n0\n1 1\n").is_none());
        assert!(parse_ies("TILT=NONE\n1 1000 1 2 1 1 2 0 0 0\n1 1 50\n0 90\n0\n1 NaN\n").is_none());
        // …while the same file with finite values parses.
        assert!(parse_ies("TILT=NONE\n1 1000 1 2 1 1 2 0 0 0\n1 1 50\n0 90\n0\n1 1\n").is_some());
    }
}
