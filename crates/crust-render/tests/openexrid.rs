//! The OpenEXRId mattes and the identity / primvar channels the CLI writes
//! for `samples/openexrid.usda`, read back from the files: each matte is a
//! deep scanline EXR in OpenEXRId's version-3 layout, decoded as its readers
//! decode it (the over form undone, the names unpacked), whose coverage sums
//! to the beauty's alpha and whose colour sums to the beauty; and the flat
//! ids and primvar agree with the matte on pixels one prim covers.
//!
//! `scripts/openexrid_oracle.py` checks the same files with OpenEXR and
//! OpenEXRId's own C++ reader.

use base64::Engine;
use crust_core::identity::murmur3_32;
use exr::image::deep::DeepChannels;
use exr::meta::attribute::AttributeValue;
use exr::prelude::{FlatSamples, ReadChannels, ReadLayers, Text, read};
use std::collections::{BTreeSet, HashMap};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

const W: usize = 160;
const H: usize = 90;

fn sample() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples/openexrid.usda")
        .canonicalize()
        .expect("samples/openexrid.usda")
}

/// Renders the sample at 4 spp in a fresh directory, where its products'
/// relative names land.
fn render() -> PathBuf {
    let dir = std::env::temp_dir().join("crust_openexrid_cli");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let status = Command::new(env!("CARGO_BIN_EXE_crust"))
        .args(["render", "-i"])
        .arg(sample())
        .args(["-s", "4", "-l", "error"])
        .current_dir(&dir)
        .status()
        .expect("run crust");
    assert!(status.success(), "crust render failed: {status}");
    dir
}

/// The flat product's channels, top-down.
fn flat(path: &Path) -> HashMap<String, FlatSamples> {
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes()
        .from_file(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    image
        .layer_data
        .channel_data
        .list
        .into_iter()
        .map(|c| (c.name.to_string(), c.sample_data))
        .collect()
}

fn f32s(samples: &FlatSamples) -> Vec<f32> {
    (0..samples.len())
        .map(|i| samples.value_by_flat_index(i).to_f32())
        .collect()
}

/// One decoded matte pixel sample: its name, `(r, g, b, a)` as OpenEXRId's
/// readers give them back (the over form undone), and its depth.
#[derive(Debug, Clone)]
struct Sample {
    name: String,
    value: [f32; 4],
    z: f32,
}

/// A matte read back: per pixel (top-down), its samples, front to back.
struct Matte {
    pixels: Vec<Vec<Sample>>,
    names: Vec<String>,
}

impl Matte {
    fn load(path: &Path) -> Matte {
        let image = read()
            .deep_data()
            .first_valid_layer()
            .from_file(path)
            .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
        let layer = &image.layer_data;
        assert_eq!(layer.size, (W, H).into());
        assert_eq!(layer.encoding.compression, exr::prelude::Compression::ZIP1);
        let attr = |name: &str| layer.attributes.other.get(&Text::from(name)).cloned();
        assert_eq!(attr("EXRIdVersion"), Some(AttributeValue::I32(3)));
        let Some(AttributeValue::Text(packed)) = attr("EXRIdNames") else {
            panic!("{}: no EXRIdNames", path.display());
        };
        // Version 3, as OpenEXRId's Nuke plug-in reads it: base64, zlib, one
        // name per line.
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(packed.to_string())
            .expect("base64");
        let mut text = String::new();
        flate2::read::ZlibDecoder::new(&bytes[..])
            .read_to_string(&mut text)
            .expect("zlib");
        let names: Vec<String> = text.split('\n').map(str::to_owned).collect();

        let deep: &DeepChannels = &layer.channel_data;
        let channel = |name: &str| deep.channel_by_name(name).map(|c| &c.sample_data);
        let ids = match channel("Id") {
            Some(FlatSamples::U32(v)) => v.clone(),
            other => panic!("Id is no UINT channel: {other:?}"),
        };
        let z = match channel("Z") {
            Some(FlatSamples::F32(v)) => v.clone(),
            other => panic!("Z is no FLOAT channel: {other:?}"),
        };
        let half = |name: &str| match channel(name) {
            Some(FlatSamples::F16(v)) => Some(v.iter().map(|h| h.to_f32()).collect::<Vec<_>>()),
            None => None,
            other => panic!("{name} is no HALF channel: {other:?}"),
        };
        let a = half("A").expect("an A slice");
        let rgb = [half("R"), half("G"), half("B")];
        let pixels = (0..W * H)
            .map(|p| {
                // `openexrid::Mask::read`: each sample times the transparency
                // of the samples in front of it.
                let mut prev_alpha = 0.0f32;
                deep.sample_range_by_index(p)
                    .map(|s| {
                        let over = [
                            rgb[0].as_ref().map_or(0.0, |c| c[s]),
                            rgb[1].as_ref().map_or(0.0, |c| c[s]),
                            rgb[2].as_ref().map_or(0.0, |c| c[s]),
                            a[s],
                        ];
                        let value = over.map(|x| (1.0 - prev_alpha) * x);
                        prev_alpha += (1.0 - prev_alpha) * a[s];
                        Sample {
                            name: names[ids[s] as usize].clone(),
                            value,
                            z: z[s],
                        }
                    })
                    .collect()
            })
            .collect();
        Matte { pixels, names }
    }
}

#[test]
fn mattes_cover_the_beauty_and_ids_agree_with_them() {
    let dir = render();
    let image = flat(&dir.join("renders/openexrid.exr"));
    let (r, g, b, alpha) = (
        f32s(&image["R"]),
        f32s(&image["G"]),
        f32s(&image["B"]),
        f32s(&image["A"]),
    );
    let prim_ids = match &image["primId"] {
        FlatSamples::U32(v) => v.clone(),
        other => panic!("primId is no UINT channel: {other:?}"),
    };
    let elements = match &image["elementId"] {
        FlatSamples::U32(v) => v.clone(),
        other => panic!("elementId is no UINT channel: {other:?}"),
    };
    let display = [
        f32s(&image["displayColor.R"]),
        f32s(&image["displayColor.G"]),
        f32s(&image["displayColor.B"]),
    ];

    let objects = Matte::load(&dir.join("renders/openexrid_objects.exr"));
    let materials = Matte::load(&dir.join("renders/openexrid_materials.exr"));
    let assets = Matte::load(&dir.join("renders/openexrid_assets.exr"));
    assert_eq!(
        objects.names,
        [
            "/World/Ball",
            "/World/Chair/Back",
            "/World/Chair/Legs",
            "/World/Chair/Seat",
            "/World/Floor",
            "/World/Pebbles",
        ]
    );
    assert_eq!(
        materials.names,
        [
            "/World/Looks/Red",
            "/World/Looks/Stone",
            "/World/Looks/Wood"
        ]
    );
    assert_eq!(
        assets.names,
        [
            "/World/Ball",
            "/World/Chair",
            "/World/Floor",
            "/World/Pebbles"
        ]
    );

    let mut single = 0;
    for p in 0..W * H {
        for matte in [&objects, &materials, &assets] {
            let samples = &matte.pixels[p];
            // Coverage: the share of the beauty's filter weight each name
            // took; together, the alpha (the sky is no name).
            let coverage: f32 = samples.iter().map(|s| s.value[3]).sum();
            assert!(
                (coverage - alpha[p]).abs() < 2e-3,
                "pixel {p}: coverage {coverage} against alpha {}",
                alpha[p]
            );
            assert!(samples.windows(2).all(|w| w[0].z <= w[1].z), "pixel {p}");
        }
        // The objects matte carries colour: where geometry covers the pixel
        // whole, its samples add up to the beauty.
        if alpha[p] > 0.9999 {
            for (c, want) in [r[p], g[p], b[p]].into_iter().enumerate() {
                let got: f32 = objects.pixels[p].iter().map(|s| s.value[c]).sum();
                assert!(
                    (got - want).abs() <= 1e-2 * want.abs().max(1.0),
                    "pixel {p}, channel {c}: {got} against {want}"
                );
            }
        }
        // Where one prim covers the pixel whole, the flat ids and the
        // primvar are that prim's.
        if let [only] = &objects.pixels[p][..]
            && only.value[3] > 0.999
        {
            single += 1;
            assert_eq!(prim_ids[p], murmur3_32(only.name.as_bytes()), "pixel {p}");
            let colour = [display[0][p], display[1][p], display[2][p]];
            let (want, element): ([f32; 3], Option<u32>) = match only.name.as_str() {
                "/World/Ball" => ([0.8, 0.1, 0.1], None),
                "/World/Chair/Seat" => ([0.55, 0.35, 0.2], Some(6)),
                "/World/Chair/Back" => ([0.5, 0.3, 0.18], Some(6)),
                "/World/Chair/Legs" => ([0.2, 0.15, 0.1], Some(4)),
                "/World/Pebbles" => ([0.45, 0.45, 0.42], None),
                // Uniform per face, filtered: on the faces' shared edge a
                // blend of the two colours, by the share of samples on each.
                "/World/Floor" => {
                    let (c0, c1) = ([0.35, 0.35, 0.3], [0.3, 0.32, 0.35]);
                    let t = ((c0[0] - colour[0]) / (c0[0] - c1[0])).clamp(0.0, 1.0);
                    (
                        std::array::from_fn(|k| c0[k] + t * (c1[k] - c0[k])),
                        Some(2),
                    )
                }
                other => panic!("pixel {p}: {other}"),
            };
            assert!(
                colour.iter().zip(want).all(|(a, b)| (a - b).abs() < 1e-5),
                "pixel {p} ({}): {colour:?}",
                only.name
            );
            match element {
                // Spheres have no authored faces.
                None => assert_eq!(elements[p], u32::MAX, "pixel {p}"),
                Some(faces) => assert!(elements[p] < faces, "pixel {p}: {}", elements[p]),
            }
        }
    }
    assert!(single > 2000, "{single} pixels covered by one prim");
    let seen: BTreeSet<&str> = objects
        .pixels
        .iter()
        .flatten()
        .map(|s| s.name.as_str())
        .collect();
    assert_eq!(seen.len(), objects.names.len(), "{seen:?}");
}
