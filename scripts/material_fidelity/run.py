#!/usr/bin/env python3
"""Run Ben Houston's Material Fidelity suite through crust.

    https://github.com/bhouston/material-fidelity

For every `.mtlx` under `<samples>/materials`, writes a shot layer that follows
the suite's reference setup (`README.md` § Reference Renderer Setup), renders
it with crust, writes `crust.png` beside the material (the suite's
`<renderer-name>.png` convention, so its viewer can show it), and scores it with
the suite's own metric: PSNR over 8-bit RGB against `materialx-glsl.png`
(`packages/core/src/metrics.ts`, reproduced exactly in `psnr()`).

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
import math
import os
import re
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

import numpy as np
import OpenEXR
from PIL import Image

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
RENDERER_NAME = "crust"
REFERENCE_NAME = "materialx-glsl"
SIZE = 512
FOV_DEG = 45.0
FOCAL = 50.0
APERTURE = 2.0 * FOCAL * math.tan(math.radians(FOV_DEG) / 2.0)

ANSI = re.compile(r"\x1b\[[0-9;]*m")
UNSUPPORTED = re.compile(r"no operator for node type\(s\) (.+?) \u2014")

SHOT = """#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
    metersPerUnit = 1
)

def Xform "World"
{{
    def Camera "Cam"
    {{
        float focalLength = {focal}
        float horizontalAperture = {aperture}
        float verticalAperture = {aperture}
        float2 clippingRange = (0.05, 1000)
        double3 xformOp:translate = (0, 0, 5)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }}

    def DomeLight "Env"
    {{
        float inputs:intensity = 1
        asset inputs:texture:file = @{hdr}@
        token inputs:texture:format = "latlong"
        float xformOp:rotateY = {env_rotate}
        uniform token[] xformOpOrder = ["xformOp:rotateY"]
    }}

    def Scope "Looks"
    {{
        def Material "M" (
            prepend references = @{mtlx}@</MaterialX/Materials/{material}>
        )
        {{
        }}
    }}

    def "Ball" (
        prepend references = @{ball}@
    )
    {{
        over "Preview_Mesh" (prepend apiSchemas = ["MaterialBindingAPI"])
        {{
            rel material:binding = </World/Looks/M>
            int crust:rayMask = {ray_mask}
        }}
        over "Calibration_Mesh" (prepend apiSchemas = ["MaterialBindingAPI"])
        {{
            rel material:binding = </World/Looks/M>
            int crust:rayMask = {ray_mask}
        }}
    }}
}}

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        rel camera = </World/Cam>
        int2 resolution = ({size}, {size})
        int crust:samplesPerPixel = {spp}
        int crust:minSamplesPerPixel = {min_spp}
        int crust:maxDepth = {max_depth}
        float crust:indirectClamp = 0
    }}
}}
"""


def find_materials(root, selectors):
    files = sorted(root.rglob("*.mtlx"))
    if not selectors:
        return files
    out = []
    for f in files:
        leaf = f.parent.name
        for s in selectors:
            if s.startswith("re:"):
                if re.search(s[3:], leaf):
                    out.append(f)
                    break
            elif s in leaf:
                out.append(f)
                break
    return out


def surface_material_name(mtlx):
    text = mtlx.read_text(errors="replace")
    m = re.search(r"<surfacematerial\b[^>]*\bname\s*=\s*\"([^\"]+)\"", text)
    return m.group(1) if m else None


def exr_to_srgb8(path):
    with OpenEXR.File(str(path)) as f:
        ch = f.channels()
        if "RGB" in ch:
            rgb = np.asarray(ch["RGB"].pixels, dtype=np.float32)
        else:
            rgb = np.stack([np.asarray(ch[c].pixels, dtype=np.float32) for c in "RGB"], axis=-1)
    rgb = np.nan_to_num(rgb, nan=0.0, posinf=1.0, neginf=0.0)
    c = np.clip(rgb, 0.0, 1.0)
    srgb = np.where(c <= 0.0031308, 12.92 * c, 1.055 * np.power(c, 1.0 / 2.4) - 0.055)
    return np.clip(np.round(srgb * 255.0), 0, 255).astype(np.uint8)


def load_rgb8(path):
    return np.asarray(Image.open(path).convert("RGBA"), dtype=np.uint8)[..., :3]


def psnr(src, ref):
    """`calculatePsnr` in the suite's packages/core/src/metrics.ts, exactly."""
    d = src.astype(np.float64) - ref.astype(np.float64)
    sse = float(np.sum(d * d))
    if sse == 0.0:
        return None
    return round(20.0 * math.log10(255.0 / math.sqrt(sse / d.size)), 3)


def render_one(mtlx, args, shots, binary):
    out_png = mtlx.parent / f"{RENDERER_NAME}.png"
    report = {"material": str(mtlx.relative_to(args.materials_root)), "png": str(out_png)}
    ref_png = mtlx.parent / f"{REFERENCE_NAME}.png"
    if args.skip_existing and out_png.exists():
        report["status"] = "skipped"
    else:
        name = surface_material_name(mtlx)
        if name is None:
            report.update(status="error", error="no <surfacematerial> in document")
            return report
        stem = str(mtlx.relative_to(args.materials_root)).replace("/", "__")[: -len(".mtlx")]
        shot = shots / f"{stem}.usda"
        exr = shots / f"{stem}.exr"
        shot.write_text(
            SHOT.format(
                focal=FOCAL,
                aperture=f"{APERTURE:.6f}",
                hdr=args.hdr,
                env_rotate=args.env_rotate,
                mtlx=mtlx.resolve(),
                material=name,
                ball=args.ball,
                ray_mask=5 if args.no_self_shadow else 7,
                size=SIZE,
                spp=args.spp,
                min_spp=min(args.spp, args.min_spp),
                max_depth=args.max_depth,
            )
        )
        env = dict(os.environ, RAYON_NUM_THREADS=str(args.threads))
        t0 = time.perf_counter()
        proc = subprocess.run(
            [binary, "-i", str(shot), "-o", str(exr), "-l", "warn"],
            capture_output=True, text=True, env=env, timeout=args.timeout,
        )
        report["seconds"] = round(time.perf_counter() - t0, 2)
        log = ANSI.sub("", proc.stdout + proc.stderr)
        report["log"] = [l for l in log.splitlines() if l.strip()][-40:]
        unsupported = sorted({n.strip() for m in UNSUPPORTED.finditer(log) for n in m.group(1).split(",")})
        if unsupported:
            report["unsupported"] = unsupported
        if proc.returncode != 0 or not exr.exists():
            report.update(status="error", error=f"crust exited {proc.returncode}")
            return report
        Image.fromarray(exr_to_srgb8(exr), "RGB").save(out_png)
        exr.unlink()
        exr.with_suffix(".png").unlink(missing_ok=True)
        report["status"] = "rendered"
    if ref_png.exists() and out_png.exists():
        report["psnr"] = psnr(load_rgb8(out_png), load_rgb8(ref_png))
        for other in args.compare:
            o = mtlx.parent / f"{other}.png"
            if o.exists():
                report.setdefault("others", {})[other] = psnr(load_rgb8(o), load_rgb8(ref_png))
    return report


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--suite", required=True, type=Path, help="material-fidelity checkout")
    ap.add_argument("--materials", action="append", default=[], help="leaf-directory selector (substring or re:...)")
    ap.add_argument("--spp", type=int, default=64)
    ap.add_argument("--min-spp", type=int, default=32)
    ap.add_argument("--max-depth", type=int, default=12)
    # MaterialXView's lat-long puts the seam where crust (USD: -Z at u = 0.5)
    # puts the centre. Fitted on the background of the reference images: 180
    # scores 34 dB there, 177 and 183 score 11-12 dB.
    ap.add_argument("--env-rotate", type=float, default=180.0, help="dome rotateY in degrees")
    ap.add_argument("--self-shadow", dest="no_self_shadow", action="store_false",
                    help="let the ball shadow itself (the suite's renderers do not)")
    ap.add_argument("--jobs", type=int, default=1, help="materials rendered at once")
    ap.add_argument("--threads", type=int, default=os.cpu_count(), help="RAYON_NUM_THREADS per render")
    ap.add_argument("--timeout", type=float, default=900)
    ap.add_argument("--skip-existing", action="store_true")
    ap.add_argument("--compare", action="append", default=["blender-nodes", "blender-new", "threejs-new"],
                    help="also report these renderers' published PSNR")
    ap.add_argument("--binary", type=Path, default=REPO / "target/release/crust-render")
    ap.add_argument("--out", type=Path, default=Path("fidelity-out"), help="shots, shader ball, results.json")
    args = ap.parse_args()

    samples = args.suite / "submodules/material-samples"
    args.materials_root = samples / "materials"
    args.hdr = (samples / "viewer/san_giuseppe_bridge_2k.hdr").resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    shots = args.out / "shots"
    shots.mkdir(exist_ok=True)
    ball = args.out / "shaderball.usda"
    if not ball.exists():
        subprocess.run([sys.executable, str(HERE / "glb_to_usda.py"),
                        str(samples / "viewer/ShaderBall.glb"), str(ball)], check=True)
    args.ball = ball.resolve()

    materials = find_materials(args.materials_root, args.materials)
    print(f"{len(materials)} materials, {args.spp} spp, jobs {args.jobs} x {args.threads} threads", flush=True)
    results_path = args.out / "results.json"
    results = {}
    if results_path.exists() and args.skip_existing:
        results = {r["material"]: r for r in json.loads(results_path.read_text())}
    t0 = time.perf_counter()
    done = 0

    def job(m):
        return render_one(m, args, shots, str(args.binary))

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for r in pool.map(job, materials):
            done += 1
            prev = results.get(r["material"], {})
            if r.get("status") == "skipped":
                r = {**prev, **{k: v for k, v in r.items() if k != "status"}}
                r.setdefault("status", "rendered")
            results[r["material"]] = r
            print(f"[{done}/{len(materials)}] {r['status']:8} psnr={r.get('psnr')!s:>7} "
                  f"{r.get('seconds', '-')!s:>6}s {r['material']}", flush=True)
            if done % 20 == 0:
                results_path.write_text(json.dumps(list(results.values()), indent=1))
    results_path.write_text(json.dumps(list(results.values()), indent=1))
    print(f"done in {time.perf_counter() - t0:.0f}s -> {results_path}")


if __name__ == "__main__":
    main()
