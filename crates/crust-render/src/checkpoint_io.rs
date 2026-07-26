//! Resumable checkpoint EXRs — the CLI side of checkpoint/resume.
//!
//! The engine hands over raw accumulation state
//! ([`crust_core::CheckpointState`]); this module round-trips it through a
//! multi-channel EXR so an interrupted render can resume **bit-exactly**:
//! `R`/`G`/`B` hold the raw radiance *sums* (not means — the file looks
//! `count`-times bright in a viewer, but f32 sums are what round-trip
//! exactly), `crust.oddR/G/B` the odd-sample sums, and `crust.count` the
//! per-pixel sample counts (exact in f32 up to 2^24 samples). Resume
//! metadata travels as EXR header attributes. Writes go to a sibling
//! temporary file first and are renamed into place, so a checkpoint file is
//! never observed half-written.

use crust_core::{CHECKPOINT_VERSION, CheckpointState, Vec3A};
use exr::prelude::*;
use std::path::Path;

const ATTR_VERSION: &str = "crustCheckpointVersion";
const ATTR_NEXT_SAMPLE: &str = "crustNextSample";
const ATTR_FINGERPRINT: &str = "crustFingerprint";

/// Extract one channel plane in film order (film row 0 is the image
/// bottom; EXR rows run top-down, so rows are flipped symmetrically here
/// and in [`read_checkpoint`]).
fn plane(width: usize, height: usize, f: impl Fn(usize) -> f32) -> FlatSamples {
    let mut v = Vec::with_capacity(width * height);
    for row in 0..height {
        let j = height - 1 - row;
        for i in 0..width {
            v.push(f(j * width + i));
        }
    }
    FlatSamples::F32(v)
}

pub fn write_checkpoint(state: &CheckpointState, path: &Path) -> std::result::Result<(), String> {
    let (w, h) = (state.width, state.height);
    if state.next_sample >= 1 << 24 {
        return Err(format!(
            "sample counts of {} exceed 2^24 and would not round-trip exactly through f32",
            state.next_sample
        ));
    }
    let channels: Vec<AnyChannel<FlatSamples>> = vec![
        AnyChannel::new("R", plane(w, h, |i| state.sum[i].x)),
        AnyChannel::new("G", plane(w, h, |i| state.sum[i].y)),
        AnyChannel::new("B", plane(w, h, |i| state.sum[i].z)),
        AnyChannel::new("crust.oddR", plane(w, h, |i| state.odd_sum[i].x)),
        AnyChannel::new("crust.oddG", plane(w, h, |i| state.odd_sum[i].y)),
        AnyChannel::new("crust.oddB", plane(w, h, |i| state.odd_sum[i].z)),
        AnyChannel::new("crust.count", plane(w, h, |i| state.count[i] as f32)),
    ];
    let mut attrs = LayerAttributes::named("crust.checkpoint");
    attrs.other.insert(
        Text::from(ATTR_VERSION),
        AttributeValue::I32(CHECKPOINT_VERSION as i32),
    );
    attrs.other.insert(
        Text::from(ATTR_NEXT_SAMPLE),
        AttributeValue::I32(state.next_sample as i32),
    );
    attrs.other.insert(
        Text::from(ATTR_FINGERPRINT),
        AttributeValue::Text(Text::from(format!("{:016x}", state.fingerprint).as_str())),
    );
    let layer = Layer::new(
        (w, h),
        attrs,
        Encoding::SMALL_LOSSLESS,
        AnyChannels::sort(channels.into_iter().collect()),
    );
    // Atomic-enough: write a sibling temp file, then rename it into place.
    let tmp = path.with_extension("tmp");
    Image::from_layer(layer)
        .write()
        .to_file(&tmp)
        .map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("renaming into {}: {e}", path.display()))
}

pub fn read_checkpoint(path: &Path) -> std::result::Result<CheckpointState, String> {
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes()
        .from_file(path)
        .map_err(|e| format!("reading {}: {e}", path.display()))?;
    let layer = &image.layer_data;
    let (w, h) = (layer.size.x(), layer.size.y());

    let attr_i32 = |name: &str| -> std::result::Result<i32, String> {
        match layer.attributes.other.get(&Text::from(name)) {
            Some(AttributeValue::I32(v)) => Ok(*v),
            _ => Err(format!("not a crust checkpoint EXR: missing {name}")),
        }
    };
    let version = attr_i32(ATTR_VERSION)?;
    if version != CHECKPOINT_VERSION as i32 {
        return Err(format!(
            "checkpoint version {version} is not the supported version {CHECKPOINT_VERSION}"
        ));
    }
    let next_sample = attr_i32(ATTR_NEXT_SAMPLE)? as u32;
    let fingerprint = match layer.attributes.other.get(&Text::from(ATTR_FINGERPRINT)) {
        Some(AttributeValue::Text(t)) => u64::from_str_radix(&t.to_string(), 16)
            .map_err(|_| format!("malformed {ATTR_FINGERPRINT} attribute"))?,
        _ => return Err(format!("not a crust checkpoint EXR: missing {ATTR_FINGERPRINT}")),
    };

    let channel = |name: &str| -> std::result::Result<Vec<f32>, String> {
        layer
            .channel_data
            .list
            .iter()
            .find(|c| c.name.to_string() == name)
            .map(|c| c.sample_data.values_as_f32().collect())
            .ok_or_else(|| format!("checkpoint EXR is missing channel {name}"))
    };
    let (r, g, b) = (channel("R")?, channel("G")?, channel("B")?);
    let (or, og, ob) = (
        channel("crust.oddR")?,
        channel("crust.oddG")?,
        channel("crust.oddB")?,
    );
    let count_f = channel("crust.count")?;

    // Undo the row flip of `plane`.
    let unflip = |row: usize, i: usize| (h - 1 - row) * w + i;
    let mut sum = vec![Vec3A::ZERO; w * h];
    let mut odd_sum = vec![Vec3A::ZERO; w * h];
    let mut count = vec![0u32; w * h];
    for row in 0..h {
        for i in 0..w {
            let src = row * w + i;
            let dst = unflip(row, i);
            sum[dst] = Vec3A::new(r[src], g[src], b[src]);
            odd_sum[dst] = Vec3A::new(or[src], og[src], ob[src]);
            count[dst] = count_f[src] as u32;
        }
    }
    Ok(CheckpointState {
        width: w,
        height: h,
        sum,
        odd_sum,
        count,
        next_sample,
        fingerprint,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// EXR roundtrip is field-identical — f32 sums, counts, and metadata
    /// all survive exactly, which is what bit-exact resume rests on.
    #[test]
    fn roundtrip_is_exact() {
        let (w, h) = (13, 7);
        let mut state = CheckpointState {
            width: w,
            height: h,
            sum: Vec::new(),
            odd_sum: Vec::new(),
            count: Vec::new(),
            next_sample: 48,
            fingerprint: 0xdead_beef_cafe_f00d,
        };
        // Awkward float values (subnormals, exact-integer, irrational-ish).
        for i in 0..w * h {
            let f = i as f32;
            state.sum.push(Vec3A::new(
                f * 0.1 + 1e-39,
                (f + 1.0).sqrt(),
                f * 1234.5678,
            ));
            state.odd_sum.push(Vec3A::new(f * 0.05, f, 1.0 / (f + 1.0)));
            state.count.push(if i % 9 == 0 { 16 } else { 48 });
        }
        let dir = std::env::temp_dir().join("crust_checkpoint_io_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("roundtrip.checkpoint.exr");
        write_checkpoint(&state, &path).expect("write");
        let back = read_checkpoint(&path).expect("read");
        assert_eq!(back.width, state.width);
        assert_eq!(back.height, state.height);
        assert_eq!(back.next_sample, state.next_sample);
        assert_eq!(back.fingerprint, state.fingerprint);
        assert_eq!(back.count, state.count);
        assert_eq!(back.sum, state.sum);
        assert_eq!(back.odd_sum, state.odd_sum);
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_metadata_is_rejected() {
        let dir = std::env::temp_dir().join("crust_checkpoint_io_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("not_a_checkpoint.exr");
        // A plain RGB EXR without the crust attributes.
        write_rgb_file(&path, 4, 4, |x, y| (x as f32, y as f32, 0.0)).unwrap();
        let err = read_checkpoint(&path).unwrap_err();
        assert!(err.contains("missing"), "{err}");
        std::fs::remove_file(&path).ok();
    }
}
