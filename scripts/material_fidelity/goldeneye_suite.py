#!/usr/bin/env python3
"""Export the Material Fidelity suite as a Goldeneye project, with crust as its renderer.

Goldeneye (https://github.com/anderslanglands/goldeneye) is the pytest-based USD
render regression runner behind Typhoon, NVIDIA's OpenUSD reference renderer,
whose materials conformance suite grew out of this same Material Fidelity suite.
It collects USD fixtures below a `goldeneye-suite.toml`, renders each one with a
configured command, compares it with a reference image by FLIP and writes a
browsable HTML report (`goldeneye view`) with EXR inspection.

This writes, under `--out`:

    pytest.ini                      makes --out the pytest / Goldeneye project root
    goldeneye.crust.toml            renderer profile: `goldeneye_suite.py render {usd_path} {output_path}`
    material-fidelity/
      goldeneye-suite.toml          suite: output {path}.exr, reference {path}.png, FLIP threshold
      _assets/shaderball.usda       the suite's ShaderBall.glb, converted once (glb_to_usda.py)
      nodes/absval.usda ...         one fixture per material, the same shot layer run.py renders
      reference/nodes/absval.png    materialx-glsl.avif, decoded (FLIP reads PNG, not AVIF)

Fixtures reference the suite checkout by relative path and carry a
`RenderProduct` named `{path}.exr`, so a Hydra renderer driven by `usdrender
--outputRoot {suite_output_root}` (Goldeneye's default Typhoon command) writes
where Goldeneye looks, and the same fixtures can be run through Typhoon for a
side-by-side. The `crust:*` attributes are crust's; other renderers ignore them.

Then, with Goldeneye installed (`fidelity.sh goldeneye` installs it into the venv):

    cd <out> && pytest material-fidelity                 # every material, crust profile
    cd <out> && pytest material-fidelity -k absval
    cd <out> && goldeneye view                           # the HTML report

A first run fails wherever crust's known gaps are (docs/material_fidelity.md).
`--expect-failures <out>/_output/run-NNNN/goldeneye-report.json` records each of
those as a per-case expected failure, naming the nodes crust logged as
unsupported, so later runs fail only on a material that newly regresses.

Usage: goldeneye_suite.py --suite /path/to/material-fidelity --out DIR
                          [--materials SEL ...] [--spp N] [--flip-threshold T]
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

from PIL import Image

import suite

SUITE_NAME = "material-fidelity"


def toml_str(s):
    return json.dumps(str(s))


def render(argv):
    """The profile's render command: crust, then the EXR rewritten for FLIP.

    crust-render neither creates the output's directory (usdrender does; Goldeneye
    expects `{output_path}` below a fresh `_output/run-NNNN`) nor writes scanline
    EXRs, and FLIP's EXR reader (tinyexr) crashes on crust's tiled ones. So the
    image is rewritten as a scanline EXR with the same pixels.
    """
    ap = argparse.ArgumentParser(prog="goldeneye_suite.py render")
    ap.add_argument("--binary", type=Path, required=True)
    ap.add_argument("--threads", type=int)
    ap.add_argument("usd", type=Path)
    ap.add_argument("output", type=Path)
    args = ap.parse_args(argv)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ, **({"RAYON_NUM_THREADS": str(args.threads)} if args.threads else {}))
    tiled = args.output.with_name(args.output.stem + ".tiled.exr")
    proc = subprocess.run([str(args.binary), "-i", str(args.usd), "-o", str(tiled), "-l", "warn"], env=env)
    if proc.returncode != 0:
        return proc.returncode
    suite.write_exr_rgb(args.output, suite.read_exr_rgb(tiled))
    tiled.unlink()
    tiled.with_suffix(".png").unlink(missing_ok=True)
    return 0


EXPECTED_MARKER = "# goldeneye_suite.py --expect-failures"
UNSUPPORTED = re.compile(r"no operator for node type\(s\) (.+?) \u2014")


def expect_failures(sdir, report_path):
    """Goldeneye's way of carrying known gaps: `<fixture>.goldeneye.toml` with a
    per-renderer `expected-failure`. A later run then fails only on a case that
    passed here, and reports each recorded one as `expected-failure`."""
    marked = cleared = 0
    for case in json.loads(Path(report_path).read_text()):
        if case.get("suite") != SUITE_NAME or case.get("renderer") != suite.RENDERER_NAME:
            continue
        conf = (sdir / case["relative_path"]).with_suffix(".goldeneye.toml")
        ours = not conf.exists() or conf.read_text().startswith(EXPECTED_MARKER)
        if not ours:
            continue
        status = case.get("status", "")
        if status == "expected-failure":
            status = case.get("expected_failure_status") or status
        if not status.startswith("failed"):
            if conf.exists():
                conf.unlink()
                cleared += 1
            continue
        reason = status
        if case.get("flip_mean") is not None:
            reason = f"mean FLIP {case['flip_mean']:.4f} > {case.get('flip_threshold')}"
        missing = sorted({n.strip() for m in UNSUPPORTED.finditer(case.get("renderer_output") or "")
                          for n in m.group(1).split(",")})
        if missing:
            reason += "; crust-mtlx has no operator for " + ", ".join(missing)
        conf.write_text(f"{EXPECTED_MARKER} {Path(report_path).parent.name}\n"
                        f"[test.expected-failure]\n{suite.RENDERER_NAME} = {toml_str(reason)}\n")
        marked += 1
    print(f"expected failures: {marked} recorded, {cleared} cleared (from {report_path})")


def main():
    if sys.argv[1:2] == ["render"]:
        return render(sys.argv[2:])
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--suite", required=True, type=Path, help="material-fidelity checkout")
    ap.add_argument("--out", required=True, type=Path, help="Goldeneye project directory to write")
    ap.add_argument("--materials", action="append", default=[], help="leaf-directory selector (substring or re:...)")
    ap.add_argument("--spp", type=int, default=64)
    ap.add_argument("--min-spp", type=int, default=32)
    ap.add_argument("--max-depth", type=int, default=12)
    ap.add_argument("--self-shadow", action="store_true")
    # Mean FLIP a case may reach and still pass. Goldeneye's own default (0.04) is a
    # regression tolerance between two runs of one renderer; against another
    # renderer's reference crust needs more.
    ap.add_argument("--flip-threshold", type=float, default=0.1)
    ap.add_argument("--binary", type=Path, default=suite.REPO / "target/release/crust-render")
    ap.add_argument("--threads", type=int, help="RAYON_NUM_THREADS for each render (default: all)")
    ap.add_argument("--clean", action="store_true", help="remove previously exported fixtures first")
    ap.add_argument("--expect-failures", type=Path, metavar="REPORT",
                    help="a run's goldeneye-report.json: record each failing case as an expected "
                         "failure for crust (and drop the record of each passing one)")
    args = ap.parse_args()

    samples = suite.samples_root(args.suite)
    root = samples / "materials"
    proj = args.out
    sdir = proj / SUITE_NAME
    if args.clean and sdir.exists():
        shutil.rmtree(sdir)
    (sdir / "_assets").mkdir(parents=True, exist_ok=True)
    ball = sdir / "_assets/shaderball.usda"
    if not ball.exists():
        subprocess.run([sys.executable, str(suite.HERE / "glb_to_usda.py"),
                        str(samples / "viewer/ShaderBall.glb"), str(ball)], check=True)
    hdr = samples / "viewer/san_giuseppe_bridge_2k.hdr"

    (proj / "pytest.ini").write_text(
        "[pytest]\n"
        "# Goldeneye collects the USD fixtures below goldeneye-suite.toml.\n"
        "norecursedirs = _output reference _assets .git\n")
    command = [sys.executable, str(Path(__file__).resolve()), "render", "--binary", str(args.binary.resolve())]
    if args.threads:
        command += ["--threads", str(args.threads)]
    command += ["{usd_path}", "{output_path}"]
    (proj / "goldeneye.crust.toml").write_text(
        "# Written by crust-render/scripts/material_fidelity/goldeneye_suite.py\n"
        "[goldeneye]\n"
        f"name = {toml_str('crust / Material Fidelity')}\n\n"
        "[render]\n"
        'renderer = "crust"\n\n'
        "[renderers.crust]\n"
        "command = [" + ", ".join(toml_str(c) for c in command) + "]\n")
    (sdir / "goldeneye-suite.toml").write_text(
        "[suite]\n"
        f"name = {toml_str(SUITE_NAME)}\n\n"
        "[render]\n"
        'output_pattern = "{path}.exr"\n\n'
        "[reference]\n"
        'dir = "reference"\n'
        'pattern = "{path}.png"\n'
        "# 57 of the suite's materials ship no materialx-glsl reference.\n"
        'missing = "allow"\n\n'
        "[comparison]\n"
        f"default_flip_threshold = {args.flip_threshold}\n")

    written = refs = 0
    for mtlx in suite.find_materials(root, args.materials):
        name = suite.surface_material_name(mtlx)
        if name is None:
            print(f"skipped {mtlx.relative_to(root)}: no <surfacematerial>", file=sys.stderr)
            continue
        rel = mtlx.parent.relative_to(root)          # nodes/absval
        fixture = sdir / rel.with_suffix(".usda")   # nodes/absval.usda
        fixture.parent.mkdir(parents=True, exist_ok=True)
        fixture.write_text(suite.shot_layer(
            mtlx=mtlx, material=name, ball=ball, hdr=hdr, spp=args.spp, min_spp=args.min_spp,
            max_depth=args.max_depth, product=f"{rel.as_posix()}.exr",
            self_shadow=args.self_shadow, layer_dir=fixture.parent))
        written += 1
        ref = suite.renderer_image(mtlx.parent, suite.REFERENCE_NAME)
        if ref is not None:
            out = sdir / "reference" / rel.with_suffix(".png")
            out.parent.mkdir(parents=True, exist_ok=True)
            if not out.exists() or out.stat().st_mtime < ref.stat().st_mtime:
                Image.open(ref).convert("RGB").save(out)
            refs += 1
    print(f"{written} fixtures, {refs} references -> {sdir}")
    if args.expect_failures:
        expect_failures(sdir, args.expect_failures)
    print(f"run: cd {proj} && pytest {SUITE_NAME}   (renderer profile: goldeneye.crust.toml)")


if __name__ == "__main__":
    sys.exit(main())
