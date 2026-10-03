//! Writing a render's `RenderProduct`s: one single-part, scanline,
//! ZIP16-compressed EXR per product, one layer of named channels per var.
//!
//! Channel names follow OpenEXR's `<layer>.<component>` convention and the
//! ASWF Color Interop rule that only colour gets `R/G/B`: colour vars are
//! `<layer>.R/.G/.B[/.A]`, vectors `<layer>.X/.Y/.Z`, UVs `<layer>.U/.V`, and
//! a scalar is one channel named after its layer. The product's first beauty
//! var is written bare (`R/G/B[/A]`) so every viewer shows it as the image.
//!
//! Scanline, not the `exr` crate's default tiling: tinyexr crashes on crust's
//! tiled files (`docs/material_fidelity.md`). The no-products render does not
//! come through here at all — it keeps `write_rgb_file`, byte for byte.

use crust_core::{AovFilm, AovProduct, AovVar, Buffer, ChannelKind, Precision};
use exr::meta::attribute::AttributeValue;
use exr::prelude::{
    AnyChannel, AnyChannels, Blocks, Compression, Encoding, FlatSamples, Image, Layer,
    LayerAttributes, LineOrder, Text, WritableImage, f16,
};
use std::io;
use std::path::Path;
use tracing::warn;

/// The colour space crust renders in, as the ASWF Color Interop ID the
/// colour channels are tagged with (`docs/color_management.md`).
const COLOR_INTEROP_ID: &str = "lin_rec709_scene";

/// The channel names `var` writes, one per plane `AovFilm::var_channels`
/// returns. `bare` makes the layer prefix empty — the product's beauty.
pub fn channel_names(var: &AovVar, bare: bool) -> Vec<String> {
    let layer = match &var.channel_prefix {
        Some(prefix) => prefix.clone(),
        None if bare => String::new(),
        None => var.name.clone(),
    };
    let components: &[&str] = match var.source.channel_kind() {
        ChannelKind::Color if var.with_alpha() => &["R", "G", "B", "A"],
        ChannelKind::Color => &["R", "G", "B"],
        ChannelKind::Vector => &["X", "Y", "Z"],
        ChannelKind::Uv => &["U", "V"],
        ChannelKind::Scalar => {
            return vec![if layer.is_empty() {
                var.name.clone()
            } else {
                layer
            }];
        }
    };
    components
        .iter()
        .map(|c| {
            if layer.is_empty() {
                (*c).to_owned()
            } else {
                format!("{layer}.{c}")
            }
        })
        .collect()
}

/// `plane` as `precision` samples. A UINT channel holds integers; a negative
/// one (the `-1` "no ID" clear value) is written as its two's-complement
/// bit pattern.
fn samples(plane: Vec<f32>, precision: Precision) -> FlatSamples {
    match precision {
        Precision::Half => FlatSamples::F16(plane.into_iter().map(f16::from_f32).collect()),
        Precision::Float => FlatSamples::F32(plane),
        Precision::Uint => FlatSamples::U32(plane.into_iter().map(|v| v as i32 as u32).collect()),
    }
}

/// The layout of a product's file: its channels' names, in var order, with
/// the var each comes from. A var whose names collide with an earlier one's
/// is refused (warned) rather than written over it.
pub fn product_channels(product: &AovProduct) -> Vec<(&AovVar, Vec<String>)> {
    let beauty = product.beauty().map(|v| v.prim_path.as_str());
    let mut taken: Vec<String> = Vec::new();
    let mut out = Vec::new();
    for var in &product.vars {
        let bare = Some(var.prim_path.as_str()) == beauty;
        let names = channel_names(var, bare);
        if let Some(clash) = names.iter().find(|n| taken.contains(n)) {
            warn!(
                "{}: channel {clash:?} is already written by another var of {}; skipped",
                var.prim_path, product.prim_path
            );
            continue;
        }
        taken.extend(names.iter().cloned());
        out.push((var, names));
    }
    out
}

/// Writes `product` to `path`, creating its parent directories. Returns the
/// channel names written, in file (alphabetical) order.
pub fn write_product(
    path: &Path,
    product: &AovProduct,
    beauty: &Buffer,
    film: &AovFilm,
) -> io::Result<Vec<String>> {
    let (width, height) = film.dimensions();
    let mut channels = Vec::new();
    for (var, names) in product_channels(product) {
        for (name, plane) in names.iter().zip(film.var_channels(beauty, var)) {
            let name = Text::new_or_none(name).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("channel name {name:?} is not valid in an EXR"),
                )
            })?;
            channels.push(AnyChannel::new(name, samples(plane, var.precision)));
        }
    }
    if channels.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no channel to write",
        ));
    }
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    // Sorted, because EXR stores channels alphabetically and a reader finds
    // them by name — an unsorted list is a malformed file.
    let channels = AnyChannels::sort(channels.into_iter().collect());
    let names = channels.list.iter().map(|c| c.name.to_string()).collect();

    let mut attributes = LayerAttributes {
        software_name: Text::new_or_none(concat!("crust-render ", env!("CARGO_PKG_VERSION"))),
        ..LayerAttributes::default()
    };
    let mut text = vec![("colorInteropID", COLOR_INTEROP_ID)];
    for (key, value) in &product.attributes {
        match key.as_str() {
            // Standard attributes `exr` exposes as typed fields.
            "comments" | "comment" => attributes.comments = Text::new_or_none(value),
            "owner" => attributes.owner = Text::new_or_none(value),
            k if exr::meta::header::standard_names::ALL.contains(&k.as_bytes()) => {
                warn!(
                    "{}: driver:parameters {k:?} names a standard EXR attribute crust sets \
                     itself; not copied",
                    product.prim_path
                );
            }
            k => text.push((k, value)),
        }
    }
    for (key, value) in text {
        if let (Some(k), Some(v)) = (Text::new_or_none(key), Text::new_or_none(value)) {
            attributes.other.insert(k, AttributeValue::Text(v));
        }
    }

    let layer = Layer::new(
        (width, height),
        attributes,
        Encoding {
            compression: Compression::ZIP16,
            blocks: Blocks::ScanLines,
            line_order: LineOrder::Increasing,
        },
        channels,
    );
    Image::from_layer(layer)
        .write()
        .to_file(path)
        .map_err(|e| io::Error::other(format!("{}: {e}", path.display())))?;
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crust_core::{Accumulation, AovSource};
    use exr::prelude::{ReadChannels, ReadLayers, read};

    fn var(name: &str, source: AovSource) -> AovVar {
        AovVar {
            prim_path: format!("/Render/Vars/{name}"),
            name: name.to_owned(),
            channel_prefix: None,
            source,
            components: source.components(),
            precision: Precision::Float,
            accumulation: Accumulation::Filtered,
            clear: source.default_clear(),
        }
    }

    fn product(vars: Vec<AovVar>) -> AovProduct {
        AovProduct {
            prim_path: "/Render/p".into(),
            name: "p.exr".into(),
            vars,
            attributes: vec![
                ("artist".into(), "someone".into()),
                ("comments".into(), "a note".into()),
            ],
        }
    }

    fn names(p: &AovProduct) -> Vec<String> {
        product_channels(p)
            .into_iter()
            .flat_map(|(_, names)| names)
            .collect()
    }

    #[test]
    fn channels_follow_the_layer_dot_component_convention() {
        let mut beauty = var("beauty", AovSource::Color);
        beauty.components = 4;
        let p = product(vec![
            beauty,
            var("Z", AovSource::Depth),
            var("N", AovSource::Normal),
            var("st", AovSource::St),
            var("diffuse", AovSource::Color),
        ]);
        // The beauty bare, scalars named after their layer, data in X/Y/Z or
        // U/V, a second colour var prefixed.
        assert_eq!(
            names(&p),
            [
                "R",
                "G",
                "B",
                "A",
                "Z",
                "N.X",
                "N.Y",
                "N.Z",
                "st.U",
                "st.V",
                "diffuse.R",
                "diffuse.G",
                "diffuse.B"
            ]
        );
    }

    #[test]
    fn a_channel_prefix_replaces_the_layer_and_a_clash_is_refused() {
        let mut beauty = var("beauty", AovSource::Color);
        beauty.channel_prefix = Some("rgba".into());
        let mut p_world = var("P", AovSource::P);
        p_world.channel_prefix = Some("Pw".into());
        let clash = var("Z", AovSource::Depth);
        let p = product(vec![beauty, p_world, var("Z", AovSource::Depth), clash]);
        assert_eq!(
            names(&p),
            ["rgba.R", "rgba.G", "rgba.B", "Pw.X", "Pw.Y", "Pw.Z", "Z"]
        );
    }

    #[test]
    fn a_product_is_written_scanline_zip_with_its_header() {
        let (w, h) = (3, 2);
        let mut beauty = Buffer::new(w, h);
        beauty.set_pixel(0, h - 1, crust_core::Vec3A::new(1.0, 2.0, 3.0));
        let film = crust_core::AovFilm::empty(w, h);
        let dir = std::env::temp_dir().join("crust_render_products_test/nested");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("p.exr");
        // A beauty-only product comes out of an empty film; the parent
        // directories do not exist yet.
        let p = product(vec![var("beauty", AovSource::Color)]);
        let written = write_product(&path, &p, &beauty, &film).expect("written");
        assert_eq!(written, ["B", "G", "R"]);
        let image = read()
            .no_deep_data()
            .largest_resolution_level()
            .all_channels()
            .first_valid_layer()
            .all_attributes()
            .from_file(&path)
            .expect("reads back");
        let layer = &image.layer_data;
        assert_eq!(layer.encoding.blocks, Blocks::ScanLines);
        assert_eq!(layer.encoding.compression, Compression::ZIP16);
        assert_eq!(
            layer.attributes.other.get(&Text::from("colorInteropID")),
            Some(&AttributeValue::Text(Text::from(COLOR_INTEROP_ID)))
        );
        assert_eq!(
            layer.attributes.other.get(&Text::from("artist")),
            Some(&AttributeValue::Text(Text::from("someone")))
        );
        assert_eq!(layer.attributes.comments, Some(Text::from("a note")));
        // Top-down rows: the buffer's top row (y = h - 1) is the file's first.
        let r = &layer.channel_data.list[2];
        assert_eq!(r.name, Text::from("R"));
        assert_eq!(r.sample_data.value_by_flat_index(0).to_f32(), 1.0);
    }

    #[test]
    fn samples_take_the_requested_precision() {
        assert!(matches!(
            samples(vec![0.5], Precision::Half),
            FlatSamples::F16(v) if v == [f16::from_f32(0.5)]
        ));
        assert!(matches!(
            samples(vec![0.5], Precision::Float),
            FlatSamples::F32(v) if v == [0.5]
        ));
        // `-1` ("no ID") is the all-ones bit pattern.
        assert!(matches!(
            samples(vec![-1.0, 7.0], Precision::Uint),
            FlatSamples::U32(v) if v == [0xFFFF_FFFF, 7]
        ));
    }
}
