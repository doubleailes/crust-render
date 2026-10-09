//! Watching and stopping `crust render` (the `cli` spec's "Interrupting a
//! render keeps its work" and "Checkpoint previews", and the `image-output`
//! spec's "EXRs record an interrupted render").
//!
//! Each render is cropped to a small region, so a debug build renders it in
//! seconds — or, for the one Ctrl-C stops, never would.

use exr::meta::attribute::AttributeValue;
use exr::prelude::{ReadChannels, ReadLayers, Text, read};
use std::path::{Path, PathBuf};
use std::process::Command;

fn sample(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
        .canonicalize()
        .unwrap_or_else(|e| panic!("samples/{name}: {e}"))
}

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_interrupt_cli").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir
}

/// An EXR's `crust:renderStatus`, if any, and every sample of every channel.
fn load(path: &Path) -> (Option<AttributeValue>, Vec<f32>) {
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes()
        .from_file(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let layer = &image.layer_data;
    let status = layer
        .attributes
        .other
        .get(&Text::from("crust:renderStatus"))
        .cloned();
    let samples = layer
        .channel_data
        .list
        .iter()
        .flat_map(|c| (0..layer.size.area()).map(|i| c.sample_data.value_by_flat_index(i).to_f32()))
        .collect();
    (status, samples)
}

/// Ctrl-C once the render has started stops it, writes the EXR and the PNG
/// of what it traced — the EXR saying it was interrupted, with no NaN — warns
/// with the samples reached, and exits 130.
#[cfg(unix)]
#[test]
fn ctrl_c_writes_the_partial_render_and_exits_130() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let dir = work_dir("ctrl_c");
    let mut child = Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(&dir)
        .arg("render")
        .arg("-i")
        .arg(sample("cornellbox.usda"))
        // Hours in a debug build, were it not stopped.
        .args(["-o", "out.exr", "-s", "1000000", "--region", "0,0,32,32"])
        .args(["-l", "info"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("run crust");
    let mut lines = BufReader::new(child.stdout.take().expect("piped")).lines();
    let mut log = Vec::new();
    // The banner comes once the render is under way: from it on, the first
    // Ctrl-C stops the render rather than the process.
    for line in lines.by_ref() {
        let line = line.expect("a log line");
        let started = line.contains("Rendering ");
        log.push(line);
        if started {
            break;
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    let kill = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("run kill");
    assert!(kill.success());
    log.extend(lines.map(|l| l.expect("a log line")));
    let status = child.wait().expect("crust exits");
    let log = log.join("\n");
    assert_eq!(status.code(), Some(130), "{log}");
    assert!(
        log.lines().any(|l| l.contains("WARN")
            && l.contains("Render interrupted")
            && l.contains("of 1000000 samples")),
        "no warning naming the interruption and the samples reached:\n{log}"
    );
    assert!(dir.join("out.png").is_file(), "no PNG:\n{log}");
    let (render_status, samples) = load(&dir.join("out.exr"));
    assert_eq!(
        render_status,
        Some(AttributeValue::Text(Text::from("interrupted")))
    );
    assert_eq!(samples.len(), 32 * 32 * 3);
    assert!(samples.iter().all(|v| !v.is_nan()), "a NaN in the EXR");
    assert!(samples.iter().any(|&v| v > 0.0), "nothing was traced");
}

/// A render that completes writes no `crust:renderStatus` at all.
#[test]
fn a_completed_render_writes_no_render_status() {
    let dir = work_dir("completed");
    let out = Command::new(env!("CARGO_BIN_EXE_crust"))
        .current_dir(&dir)
        .arg("render")
        .arg("-i")
        .arg(sample("cornellbox.usda"))
        .args([
            "-o",
            "out.exr",
            "-s",
            "2",
            "--region",
            "0,0,16,16",
            "-l",
            "error",
        ])
        .output()
        .expect("run crust");
    assert!(out.status.success(), "{out:?}");
    assert_eq!(load(&dir.join("out.exr")).0, None);
}

/// A channel's name and its samples' bits.
type Channel = (String, Vec<u32>);

/// An EXR's header attributes and its channels, each sample as bits: what
/// two files must share to be the same image. Not the bytes — the `exr`
/// crate stores compressed chunks in the order they finish, so two runs of
/// the same render already differ there.
fn content(path: &Path) -> (Vec<(String, String)>, Vec<Channel>) {
    let image = read()
        .no_deep_data()
        .largest_resolution_level()
        .all_channels()
        .first_valid_layer()
        .all_attributes()
        .from_file(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let layer = &image.layer_data;
    let mut attributes: Vec<(String, String)> = layer
        .attributes
        .other
        .iter()
        .map(|(k, v)| (k.to_string(), format!("{v:?}")))
        .collect();
    attributes.sort();
    attributes.push(("image".into(), format!("{:?}", image.attributes)));
    let channels = layer
        .channel_data
        .list
        .iter()
        .map(|c| {
            let bits = (0..layer.size.area())
                .map(|i| c.sample_data.value_by_flat_index(i).to_f32().to_bits())
                .collect();
            (c.name.to_string(), bits)
        })
        .collect();
    (attributes, channels)
}

/// `--checkpoint` rewrites the preview while the render runs, and the render
/// then ends with the files a render without it writes: the same PNG, byte
/// for byte, and the same EXR header and pixels.
#[test]
fn a_checkpointed_render_ends_with_the_files_of_one_without() {
    let dir = work_dir("checkpoint");
    let render = |name: &str, extra: &[&str]| -> String {
        let out = Command::new(env!("CARGO_BIN_EXE_crust"))
            .current_dir(&dir)
            .arg("render")
            .arg("-i")
            .arg(sample("cornellbox.usda"))
            .args([
                "-o",
                &format!("{name}.exr"),
                "-s",
                "64",
                "--region",
                "0,0,48,48",
            ])
            .args(["-l", "debug"])
            .args(extra)
            .output()
            .expect("run crust");
        let log = String::from_utf8_lossy(&out.stdout).into_owned();
        assert!(out.status.success(), "{log}");
        log
    };
    let plain = render("plain", &[]);
    assert!(!plain.contains("--checkpoint"), "{plain}");
    let watched = render("watched", &["--checkpoint", "0.02"]);
    assert!(
        watched.contains("--checkpoint: ") && watched.contains("watched.png rewritten"),
        "no preview was rewritten while the render ran:\n{watched}"
    );
    let png = |name: &str| {
        let path = dir.join(format!("{name}.png"));
        std::fs::read(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    };
    assert!(png("plain") == png("watched"), "the PNG differs");
    let (plain, watched) = (
        content(&dir.join("plain.exr")),
        content(&dir.join("watched.exr")),
    );
    assert_eq!(plain.0, watched.0, "the EXR headers differ");
    assert!(plain.1 == watched.1, "the EXR pixels differ");
    assert!(!dir.join("watched.png.partial").exists());
}
