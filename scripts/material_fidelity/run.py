#!/usr/bin/env python3
"""Run Ben Houston's Material Fidelity suite through crust.

    https://github.com/bhouston/material-fidelity

For every `.mtlx` under `<samples>/materials`, writes a shot layer that follows
the suite's reference setup (`README.md` § Reference Renderer Setup), renders
it with crust and scores it with the suite's own metric: PSNR over 8-bit RGB
against `materialx-glsl.avif` (`packages/core/src/metrics.ts`, reproduced
exactly in `suite.psnr()`).

Into each material directory it writes what the suite's own renderers write, so
the suite's viewer and `pnpm cli metrics` treat crust as one more renderer:
`crust.avif` (encoded as the suite encodes every render: AVIF quality 90, 4:4:4),
`crust.json` (the render report: status and log) and a `crust` entry merged into
`metrics.json`. `--no-write-suite` keeps the checkout untouched.

`psnr` scores the AVIF, as the suite scores every other renderer;
`psnr_lossless` scores the same pixels before encoding, so the codec's share of
the gap is visible.

Scene contract, as the suite states it:
  camera   perspective, vertical FOV 45, near 0.05, eye (0,0,5) looking at the origin
  model    ShaderBall.glb centred, bounding-sphere radius 2 (see glb_to_usda.py)
  lighting IBL from san_giuseppe_bridge_2k.hdr, visible as the background,
           no direct light; the ball casts no shadows (the Cycles renderer
           turns its shadow visibility off, the rasterisers have no shadow map)
  output   512x512, no tone mapping, sRGB encoding

Usage:
  run.py --suite /path/to/material-fidelity [--materials SEL ...] [--spp N]
         [--jobs N] [--skip-existing] [--out DIR]
"""
import argparse
import json
import os
import re
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from PIL import Image

import suite

ANSI = re.compile(r"\x1b\[[0-9;]*m")
UNSUPPORTED = re.compile(r"no operator for node type\(s\) (.+?) —")
LEVEL = re.compile(r"\b(ERROR|WARN|INFO|DEBUG|TRACE)\b")
REPORT_LEVELS = {"ERROR": "error", "WARN": "warning", "INFO": "info", "DEBUG": "debug", "TRACE": "debug"}


def write_render_report(material_dir, status, log_lines, error=None):
    """`<renderer>.json`: the suite's `RenderResultReport` (packages/samples/src/render-report.ts)."""
    logs = []
    for line in log_lines:
        m = LEVEL.search(line)
        logs.append({"level": REPORT_LEVELS[m.group(1)] if m else "info",
                     "source": "renderer", "message": line})
    report = {"rendererName": suite.RENDERER_NAME, "status": status,
              "error": {"name": "Error", "message": error} if error else None, "logs": logs}
    (material_dir / f"{suite.RENDERER_NAME}.json").write_text(json.dumps(report, indent=2) + "\n")


def merge_metrics(material_dir, value):
    """Add crust to the suite's per-material `metrics.json`, keyed by renderer name."""
    path = material_dir / "metrics.json"
    try:
        metrics = json.loads(path.read_text())
    except (OSError, ValueError):
        metrics = {}
    metrics[suite.RENDERER_NAME] = {"psnr": value}
    path.write_text(json.dumps(metrics, indent=2) + "\n")


def render_one(mtlx, args, shots):
    mdir = mtlx.parent
    ref = suite.renderer_image(mdir, suite.REFERENCE_NAME)
    # Follow the reference's format, so an older PNG checkout gets `crust.png`.
    ext = ref.suffix if ref is not None else suite.IMAGE_EXTS[0]
    # One name per document, directory and file stem both: the shot, its EXR and,
    # under --no-write-suite, its image. (In the suite, `crust.avif` is per directory,
    # as every renderer's image is: the suite keeps one document per directory.)
    stem = str(mtlx.relative_to(args.materials_root).with_suffix("")).replace("/", "__")
    # `.crust` keeps it apart from the tone-mapped `<stem>.png` crust writes beside its EXR.
    out_img = (mdir / f"{suite.RENDERER_NAME}{ext}" if args.write_suite
               else shots / f"{stem}.{suite.RENDERER_NAME}{ext}")
    report = {"material": str(mtlx.relative_to(args.materials_root)), "image": str(out_img)}
    rgb8 = None
    if args.skip_existing and out_img.exists():
        report["status"] = "skipped"
    else:
        name = suite.surface_material_name(mtlx)
        if name is None:
            report.update(status="error", error="no <surfacematerial> in document")
            return report
        shot = shots / f"{stem}.usda"
        exr = shots / f"{stem}.exr"
        shot.write_text(suite.shot_layer(
            mtlx=mtlx, material=name, ball=args.ball, hdr=args.hdr, spp=args.spp,
            min_spp=args.min_spp, max_depth=args.max_depth, product=exr.name,
            self_shadow=args.self_shadow, env_rotate=args.env_rotate))
        env = dict(os.environ, RAYON_NUM_THREADS=str(args.threads))
        t0 = time.perf_counter()
        # A failure here is this material's, not the run's: raising would stop
        # `pool.map` before results.json is written.
        try:
            proc = subprocess.run(
                [str(args.binary), "render", "-i", str(shot), "-o", str(exr), "-l", "warn"],
                capture_output=True, text=True, env=env, timeout=args.timeout,
            )
        except subprocess.TimeoutExpired:
            report.update(status="error", error=f"timed out after {args.timeout:g}s",
                          seconds=round(time.perf_counter() - t0, 2))
            if args.write_suite:
                write_render_report(mdir, "failed", [], report["error"])
            return report
        except OSError as e:
            report.update(status="error", error=f"could not launch crust: {e}")
            return report
        report["seconds"] = round(time.perf_counter() - t0, 2)
        log = [l for l in ANSI.sub("", proc.stdout + proc.stderr).splitlines() if l.strip()]
        report["log"] = log[-40:]
        unsupported = sorted({n.strip() for l in log for m in UNSUPPORTED.finditer(l)
                              for n in m.group(1).split(",")})
        if unsupported:
            report["unsupported"] = unsupported
        if proc.returncode != 0 or not exr.exists():
            report.update(status="error", error=f"crust exited {proc.returncode}")
            if args.write_suite:
                write_render_report(mdir, "failed", log, report["error"])
            return report
        rgb8 = suite.linear_to_srgb8(suite.read_exr_rgb(exr))
        img = Image.fromarray(rgb8, "RGB")
        if ext == ".avif":
            img.save(out_img, **suite.AVIF_OPTIONS)
        else:
            img.save(out_img)
        if not args.keep_exr:
            exr.unlink()
        exr.with_suffix(".png").unlink(missing_ok=True)
        if args.write_suite:
            write_render_report(mdir, "success", log)
        report["status"] = "rendered"
    if ref is not None and out_img.exists():
        ref8 = suite.load_rgb8(ref)
        report["psnr"] = suite.psnr(suite.load_rgb8(out_img), ref8)
        if rgb8 is not None:
            report["psnr_lossless"] = suite.psnr(rgb8, ref8)
        if args.write_suite:
            merge_metrics(mdir, report["psnr"])
        for other in args.compare:
            o = suite.renderer_image(mdir, other)
            if o is not None:
                report.setdefault("others", {})[other] = suite.psnr(suite.load_rgb8(o), ref8)
    return report


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--suite", required=True, type=Path, help="material-fidelity checkout")
    ap.add_argument("--materials", action="append", default=[], help="leaf-directory selector (substring or re:...)")
    ap.add_argument("--spp", type=int, default=64)
    ap.add_argument("--min-spp", type=int, default=32)
    ap.add_argument("--max-depth", type=int, default=12)
    ap.add_argument("--env-rotate", type=float, default=suite.ENV_ROTATE, help="dome rotateY in degrees")
    ap.add_argument("--self-shadow", action="store_true",
                    help="let the ball shadow itself (the suite's renderers do not)")
    ap.add_argument("--jobs", type=int, default=1, help="materials rendered at once")
    ap.add_argument("--threads", type=int, default=os.cpu_count(), help="RAYON_NUM_THREADS per render")
    ap.add_argument("--timeout", type=float, default=900)
    ap.add_argument("--skip-existing", action="store_true", help="re-score existing crust images instead of rendering")
    ap.add_argument("--no-write-suite", dest="write_suite", action="store_false",
                    help="write images under --out, not crust.avif / crust.json / metrics.json in the suite")
    ap.add_argument("--keep-exr", action="store_true", help="keep each linear EXR beside its shot")
    ap.add_argument("--compare", action="append",
                    default=["materialx-osl", "blender-new", "blender-nodes", "threejs-new"],
                    help="also score these renderers' published images")
    ap.add_argument("--binary", type=Path, default=suite.REPO / "target/release/crust")
    ap.add_argument("--out", type=Path, default=Path("fidelity-out"), help="shots, shader ball, results.json")
    args = ap.parse_args()

    samples = suite.samples_root(args.suite)
    args.materials_root = samples / "materials"
    args.hdr = (samples / "viewer/san_giuseppe_bridge_2k.hdr").resolve()
    if not args.binary.exists():
        sys.exit(f"no crust binary at {args.binary}; run `cargo build --release`")
    args.out.mkdir(parents=True, exist_ok=True)
    shots = args.out / "shots"
    shots.mkdir(exist_ok=True)
    ball = args.out / "shaderball.usda"
    if not ball.exists():
        subprocess.run([sys.executable, str(suite.HERE / "glb_to_usda.py"),
                        str(samples / "viewer/ShaderBall.glb"), str(ball)], check=True)
    args.ball = ball.resolve()

    materials = suite.find_materials(args.materials_root, args.materials)
    print(f"{len(materials)} materials from {samples.name}, {args.spp} spp, "
          f"jobs {args.jobs} x {args.threads} threads", flush=True)
    results_path = args.out / "results.json"
    results = {}
    if results_path.exists() and args.skip_existing:
        results = {r["material"]: r for r in json.loads(results_path.read_text())}
    t0 = time.perf_counter()
    done = 0

    def save():
        meta = {"spp": args.spp, "min_spp": args.min_spp, "max_depth": args.max_depth,
                "env_rotate": args.env_rotate, "self_shadow": args.self_shadow,
                "samples": samples.name, "reference": suite.REFERENCE_NAME}
        (args.out / "run.json").write_text(json.dumps(meta, indent=1) + "\n")
        results_path.write_text(json.dumps(sorted(results.values(), key=lambda r: r["material"]), indent=1))

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for r in pool.map(lambda m: render_one(m, args, shots), materials):
            done += 1
            prev = results.get(r["material"], {})
            if r.get("status") == "skipped":
                r = {**prev, **{k: v for k, v in r.items() if k != "status"}}
                r["status"] = prev.get("status", "rendered")
            results[r["material"]] = r
            print(f"[{done}/{len(materials)}] {r['status']:8} psnr={r.get('psnr')!s:>7} "
                  f"{r.get('seconds', '-')!s:>6}s {r['material']}", flush=True)
            if done % 20 == 0:
                save()
    save()
    print(f"done in {time.perf_counter() - t0:.0f}s -> {results_path}")


if __name__ == "__main__":
    main()
