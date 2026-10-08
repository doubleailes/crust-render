//! An EXR read whole for comparison: every channel of every layer as `f32`,
//! and what the header records about how the file was rendered — `crust
//! diff`'s input, compared by [`crust_core::compare`].

use crate::AssetError;
use crust_core::compare::{Channel, Planes, Stamp};
use crust_core::stamp::{STAMP_PREFIX, StampValue};
use exr::meta::attribute::AttributeValue;
use exr::prelude::*;
use std::collections::BTreeMap;
use std::path::Path;

/// The file at `path` as [`Planes`]: each named channel of each layer keyed
/// `layer.channel` (the bare channel name in an unnamed layer), at the
/// largest resolution level, plus the header's `crust:*` attributes and its
/// `colorInteropID`. `source` is the path as given. A file that is missing,
/// truncated or not an EXR is an error, never a panic.
pub fn read_exr_planes(path: &Path) -> std::result::Result<Planes, AssetError> {
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .all_layers()
        .all_attributes()
        .from_file(path)
        .map_err(AssetError::exr(path))?;
    let (width, height) = image
        .layer_data
        .first()
        .map_or((0, 0), |l| (l.size.width(), l.size.height()));
    let mut channels = BTreeMap::new();
    let mut stamp = Stamp::default();
    for (i, layer) in image.layer_data.iter().enumerate() {
        let size = (layer.size.width(), layer.size.height());
        let prefix = layer
            .attributes
            .layer_name
            .as_ref()
            .map(|n| format!("{n}."))
            .unwrap_or_default();
        for channel in &layer.channel_data.list {
            let values = channel.sample_data.values_as_f32().collect();
            channels.insert(
                format!("{prefix}{}", channel.name),
                Channel { size, values },
            );
        }
        // crust writes one stamp per file (single-part); the first layer's
        // is the file's.
        if i == 0 {
            for (key, value) in &layer.attributes.other {
                let key = key.to_string();
                if key == "colorInteropID" {
                    if let AttributeValue::Text(t) = value {
                        stamp.color_interop_id = Some(t.to_string());
                    }
                } else if key.starts_with(STAMP_PREFIX)
                    && let Some(v) = stamp_value(value)
                {
                    stamp.attributes.insert(key, v);
                }
            }
        }
    }
    Ok(Planes {
        source: path.display().to_string(),
        width,
        height,
        channels,
        stamp,
    })
}

/// A header attribute as a stamp value, when it has one of the stamp's types.
fn stamp_value(value: &AttributeValue) -> Option<StampValue> {
    Some(match value {
        AttributeValue::I32(v) => StampValue::Int(*v),
        AttributeValue::IntVec2(Vec2(a, b)) => StampValue::Int2(*a, *b),
        AttributeValue::F32(v) => StampValue::Float(*v),
        AttributeValue::F64(v) => StampValue::Double(*v),
        AttributeValue::Text(t) => StampValue::Text(t.to_string()),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir()
            .join("crust_assets_exr_planes")
            .join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// A two-layer file with a stamp, written and read back.
    #[test]
    fn a_multi_layer_file_round_trips_its_channels_and_stamp() {
        let path = dir("round_trip").join("two.exr");
        let size = Vec2(3, 2);
        let channel = |name: &str, v: f32| {
            AnyChannel::new(
                name,
                FlatSamples::F32((0..6).map(|i| v + i as f32).collect()),
            )
        };
        // A multi-part file names every part.
        let mut beauty = LayerAttributes::named("beauty");
        beauty
            .other
            .insert(Text::from("crust:spp"), AttributeValue::I32(16));
        beauty.other.insert(
            Text::from("crust:sppTaken"),
            AttributeValue::IntVec2(Vec2(16, 16)),
        );
        beauty
            .other
            .insert(Text::from("crust:indirectClamp"), AttributeValue::F32(0.0));
        beauty
            .other
            .insert(Text::from("crust:frame"), AttributeValue::F64(10.5));
        beauty.other.insert(
            Text::from("crust:camera"),
            AttributeValue::Text(Text::from("/cam")),
        );
        beauty.other.insert(
            Text::from("colorInteropID"),
            AttributeValue::Text(Text::from("lin_rec709_scene")),
        );
        beauty
            .other
            .insert(Text::from("artist"), AttributeValue::Text(Text::from("x")));
        let rgb = Layer::new(
            size,
            beauty,
            Encoding::FAST_LOSSLESS,
            AnyChannels::sort(
                vec![channel("R", 0.0), channel("G", 10.0), channel("B", 20.0)]
                    .into_iter()
                    .collect(),
            ),
        );
        let depth = Layer::new(
            size,
            LayerAttributes::named("depth"),
            Encoding::FAST_LOSSLESS,
            AnyChannels::sort(std::iter::once(channel("Z", 5.0)).collect()),
        );
        Image::from_layers(
            ImageAttributes::new(IntegerBounds::from_dimensions(size)),
            vec![rgb, depth],
        )
        .write()
        .to_file(&path)
        .expect("written");

        let p = read_exr_planes(&path).expect("reads");
        assert_eq!((p.width, p.height), (3, 2));
        let names: Vec<&str> = p.channels.keys().map(String::as_str).collect();
        assert_eq!(names, ["beauty.B", "beauty.G", "beauty.R", "depth.Z"]);
        assert_eq!(
            p.channels["beauty.G"].values,
            [10.0, 11.0, 12.0, 13.0, 14.0, 15.0]
        );
        assert_eq!(p.channels["depth.Z"].size, (3, 2));
        let s = &p.stamp;
        assert_eq!(s.attributes["crust:spp"], StampValue::Int(16));
        assert_eq!(s.attributes["crust:sppTaken"], StampValue::Int2(16, 16));
        assert_eq!(s.attributes["crust:indirectClamp"], StampValue::Float(0.0));
        assert_eq!(s.attributes["crust:frame"], StampValue::Double(10.5));
        assert_eq!(
            s.attributes["crust:camera"],
            StampValue::Text("/cam".into())
        );
        assert_eq!(s.attributes.len(), 5, "only crust:* attributes");
        assert_eq!(s.color_interop_id.as_deref(), Some("lin_rec709_scene"));
    }

    #[test]
    fn a_missing_or_truncated_file_is_an_error() {
        let d = dir("bad");
        let missing = d.join("missing.exr");
        let err = read_exr_planes(&missing).expect_err("missing");
        assert_eq!(err.path(), missing);
        assert!(err.to_string().contains("missing.exr"), "{err}");

        let whole = d.join("whole.exr");
        let channels = SpecificChannels::rgb(|_: Vec2<usize>| (1.0f32, 0.5f32, 0.25f32));
        Image::from_channels((16, 16), channels)
            .write()
            .to_file(&whole)
            .unwrap();
        let bytes = std::fs::read(&whole).unwrap();
        let truncated = d.join("truncated.exr");
        std::fs::write(&truncated, &bytes[..bytes.len() / 2]).unwrap();
        assert!(read_exr_planes(&truncated).is_err());
        let not_exr = d.join("text.exr");
        std::fs::write(&not_exr, "not an image").unwrap();
        assert!(read_exr_planes(&not_exr).is_err());
        assert!(read_exr_planes(&whole).unwrap().stamp.attributes.is_empty());
    }
}
