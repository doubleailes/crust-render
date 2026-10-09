//! Coded import warnings raised through the file loader: what the host
//! explains (the cause, once per file) and what the import counts (every
//! reference), seen on `Scene::warnings`.

use crust_assets::FileAssets;
use crust_core::{Scene, UsdImportOptions, WarningCode, WarningKind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("crust_asset_warnings_{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// Keeps every WARN message logged while it is the default subscriber.
struct Warnings(Arc<Mutex<Vec<String>>>);

struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl tracing::Subscriber for Warnings {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() == tracing::Level::WARN {
            let mut message = Message(String::new());
            event.record(&mut message);
            self.0.lock().unwrap().push(message.0);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

/// Loads `path` through a fresh `FileAssets`, with the WARN lines it logged.
fn load(path: &Path) -> (Scene, Vec<String>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let scene = tracing::subscriber::with_default(Warnings(lines.clone()), || {
        Scene::from_usd_with_options(path, &FileAssets::new(), &UsdImportOptions::default())
            .expect("stage loads")
    });
    let lines = lines.lock().unwrap().clone();
    (scene, lines)
}

/// A preview material at `/World/Looks/{name}` whose diffuse colour reads
/// `file`, and a sphere under `/World/Geo` bound to it. Grouped, so the stage
/// has too few top-level prims to stream.
fn textured(names: &[&str], file: &str) -> String {
    let looks: String = names
        .iter()
        .map(|name| {
            format!(
                r#"        def Material "{name}"
        {{
            token outputs:surface.connect = </World/Looks/{name}/Surface.outputs:surface>
            def Shader "Surface"
            {{
                uniform token info:id = "UsdPreviewSurface"
                color3f inputs:diffuseColor.connect = </World/Looks/{name}/Map.outputs:rgb>
                token outputs:surface
            }}
            def Shader "Map"
            {{
                uniform token info:id = "UsdUVTexture"
                asset inputs:file = @{file}@
                float3 outputs:rgb
            }}
        }}
"#
            )
        })
        .collect();
    let geo: String = names
        .iter()
        .enumerate()
        .map(|(i, name)| {
            format!(
                r#"        def Sphere "ball_{name}" (prepend apiSchemas = ["MaterialBindingAPI"])
        {{
            double3 xformOp:translate = ({x}, 0, 0)
            uniform token[] xformOpOrder = ["xformOp:translate"]
            rel material:binding = </World/Looks/{name}>
        }}
"#,
                x = 3 * i
            )
        })
        .collect();
    format!(
        "    def Scope \"Looks\"\n    {{\n{looks}    }}\n    def Xform \"Geo\"\n    {{\n{geo}    }}\n"
    )
}

fn stage(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("stage.usda");
    std::fs::write(
        &path,
        format!(
            "#usda 1.0\n(\n    defaultPrim = \"World\"\n)\n\ndef Xform \"World\"\n{{\n    def Camera \"cam\"\n    {{\n    }}\n{body}}}\n"
        ),
    )
    .expect("write stage");
    path
}

/// The Cornell box binds `/scene/Looks/initialShadingGroup`, which it never
/// defines: that one fallback is all it warns about — its dome map loads.
#[test]
fn the_cornell_box_warns_only_about_its_unbound_material() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/cornellbox.usda");
    let (scene, _) = load(&path);
    let codes: Vec<_> = scene.warnings.iter().map(|w| w.code).collect();
    assert_eq!(
        codes,
        [WarningCode::MaterialFallbackDefault],
        "{:#?}",
        scene.warnings
    );
    assert_eq!(
        scene.warnings[0].prims,
        ["/scene/Looks/initialShadingGroup"]
    );
}

/// A stage that authors nothing questionable raises nothing.
#[test]
fn a_clean_stage_has_no_warnings() {
    let dir = scratch("clean");
    let (scene, lines) = load(&stage(&dir, ""));
    assert!(scene.warnings.is_empty(), "{:#?}", scene.warnings);
    assert!(lines.is_empty(), "{lines:#?}");
}

/// Three materials referencing one missing file: one record counting the
/// three references, each material named, and the file logged once.
#[test]
fn a_missing_texture_shared_by_three_materials() {
    let dir = scratch("missing");
    let body = textured(&["A", "B", "C"], "missing_albedo.png");
    let (scene, lines) = load(&stage(&dir, &body));
    let records: Vec<_> = scene
        .warnings
        .iter()
        .filter(|w| w.code == WarningCode::TextureUnreadable)
        .collect();
    assert_eq!(records.len(), 1, "{:#?}", scene.warnings);
    let r = records[0];
    assert_eq!(r.kind, WarningKind::Skipped);
    assert_eq!(r.count, 3);
    let mut prims = r.prims.clone();
    prims.sort();
    assert_eq!(
        prims,
        ["/World/Looks/A", "/World/Looks/B", "/World/Looks/C"]
    );
    assert!(
        r.message.contains("missing_albedo.png"),
        "the loader's cause is the message: {}",
        r.message
    );
    let naming: Vec<_> = lines
        .iter()
        .filter(|l| l.contains("missing_albedo.png"))
        .collect();
    assert_eq!(naming.len(), 1, "{lines:#?}");
    assert!(
        naming[0].starts_with("[texture.unreadable] "),
        "{}",
        naming[0]
    );
}

/// A UDIM set with a tile that does not decode records the skipped tile.
#[test]
fn a_udim_set_with_a_bad_tile() {
    let dir = scratch("udim");
    image::RgbImage::from_pixel(4, 4, image::Rgb([255, 0, 0]))
        .save(dir.join("albedo.1001.png"))
        .expect("png");
    std::fs::write(dir.join("albedo.1002.png"), b"not a png").expect("bad tile");
    let (scene, _) = load(&stage(&dir, &textured(&["Tiled"], "albedo.<UDIM>.png")));
    let codes: Vec<_> = scene.warnings.iter().map(|w| w.code).collect();
    assert_eq!(
        codes,
        [WarningCode::TextureUdimTileMissing],
        "{:#?}",
        scene.warnings
    );
    assert_eq!(scene.warnings[0].count, 1);
}
