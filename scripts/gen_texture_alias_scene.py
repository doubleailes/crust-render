#!/usr/bin/env python3
"""Generate the texture-minification scene the checked-in samples lack.

Every sample that binds a texture *magnifies* it -- samples/materialx_basic.usda
puts five 64x64 images on four coplanar quads filling a 640x360 frame, so each
texel covers about three pixels and no mip level above 0 is ever selected.
Nothing in the repository exercises the opposite case, which is the one that
aliases: a high-frequency chart minified far below one texel per pixel.

So this emits the textbook case -- a checkerboard plane receding to the
horizon, viewed from near grazing. Near the camera the board is magnified
(bilinear territory, unchanged by any of this); towards the horizon a single
pixel covers hundreds of checks, which under point sampling is fixed-pattern
noise that no amount of spp removes, because the error is in the signal rather
than in the estimator.

That distinction is the measurement. Aliasing does not converge, so the honest
test is not "is the 16 spp image different" but "does the 16 spp image agree
with a high-spp reference *of itself*". Render a reference at high spp, then
compare a 16 spp render against it with cones on and off:

    python3 scripts/gen_texture_alias_scene.py /tmp/alias
    B=target/release/crust-render
    $B -i /tmp/alias/alias.usda -o /tmp/alias/ref.exr   -s 1024 -l error
    $B -i /tmp/alias/alias.usda -o /tmp/alias/mip.exr   -s 16   -l error
    CRUST_RAY_CONES=0 $B -i /tmp/alias/alias.usda -o /tmp/alias/flat.exr -s 16 -l error
    target/release/examples/exr_diff /tmp/alias/ref.exr /tmp/alias/mip.exr
    target/release/examples/exr_diff /tmp/alias/ref.exr /tmp/alias/flat.exr

Both references must be rendered with the *same* switch setting they are being
compared under -- a filtered render converges to a different (and correct)
image than an unfiltered one, so cross-comparing measures the bias rather than
the noise. `--measure` does all of this and prints the table.

Usage: gen_texture_alias_scene.py [outdir]
The scene and its texture are deliberately NOT checked in -- they are
generated, and the checker is 4 MB.
"""

import os
import struct
import subprocess
import sys
import zlib

TEX = 1024  # checker texture edge, texels (--size overrides)
CHECK = 8  # texels per check square
REPEAT = 64  # how many times the chart tiles across the plane
HALF = 400.0  # plane half-extent in world units
UDIM = 1  # how many UDIM tiles across (--udim); 1 means a single image


def write_checker_png(path, tex=None, phase=0):
    """A tex x tex black/white checkerboard, 8-bit RGB, written by hand.

    By hand because the repository's Python has no image library and adding
    one to run a generator would be a poor trade. A PNG is a signature, an
    IHDR, zlib-compressed scanlines each prefixed by a filter byte, and an
    IEND -- and filter 0 (None) on a two-colour image compresses to almost
    nothing anyway.
    """

    def chunk(tag, data):
        out = struct.pack(">I", len(data)) + tag + data
        return out + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    tex = tex or TEX
    raw = bytearray()
    for y in range(tex):
        raw.append(0)  # filter: None
        row = bytearray()
        for x in range(tex):
            on = ((x // CHECK) + (y // CHECK) + phase) % 2 == 0
            v = 245 if on else 10
            # A per-tile tint in the other two channels, so a chart that reads
            # the wrong UDIM tile is visible rather than merely plausible.
            row += bytes((v, (v + phase * 37) % 256, (v + phase * 91) % 256))
        raw += row
    png = b"\x89PNG\r\n\x1a\n"
    png += chunk(b"IHDR", struct.pack(">IIBBBBB", tex, tex, 8, 2, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(bytes(raw), 9))
    png += chunk(b"IEND", b"")
    with open(path, "wb") as f:
        f.write(png)


MTLX = """<?xml version="1.0"?>
<materialx version="1.38">
  <!-- Deliberately a plain image into base_color: the point of the scene is
       the texture lookup, so nothing else in the graph should be able to
       explain a difference between two renders of it. -->
  <nodegraph name="checker_ng">
    <tiledimage name="checker_tex" type="color3">
      <input name="file" type="filename" value="{chart}" colorspace="srgb_texture" />
      <input name="uvtiling" type="vector2" value="{repeat}, {repeat}" />
    </tiledimage>
    <output name="out" type="color3" nodename="checker_tex" />
  </nodegraph>

  <oren_nayar_diffuse_bsdf name="checker_diffuse" type="BSDF">
    <input name="color" type="color3" nodegraph="checker_ng" output="out" />
    <input name="roughness" type="float" value="0.0" />
  </oren_nayar_diffuse_bsdf>

  <surface name="checker_surface" type="surfaceshader">
    <input name="bsdf" type="BSDF" nodename="checker_diffuse" />
  </surface>

  <surfacematerial name="mtlx_checker" type="material">
    <input name="surfaceshader" type="surfaceshader" nodename="checker_surface" />
  </surfacematerial>
</materialx>
"""


USDA = """#usda 1.0
(
    upAxis = "Y"
    metersPerUnit = 1
    defaultPrim = "World"
)

def Xform "World"
{{
    def Scope "Looks"
    {{
        def Material "Checker" (
            prepend references = @alias.mtlx@</MaterialX/Materials/mtlx_checker>
        )
        {{
        }}
    }}

    # A ground plane seen from near grazing, so one image spans the full range
    # from magnified (foreground) to minified far past one texel per pixel
    # (horizon). Single placement, so it takes the bake path and carries a
    # tangent frame as well as a density.
    def Mesh "Ground" (
        prepend apiSchemas = ["MaterialBindingAPI"]
    )
    {{
        int[] faceVertexCounts = [4]
        int[] faceVertexIndices = [0, 1, 2, 3]
        point3f[] points = [
            (-{half}, 0, {half}), ({half}, 0, {half}),
            ({half}, 0, -{half}), (-{half}, 0, -{half})
        ]
        texCoord2f[] primvars:st = [(0, 0), ({umax}, 0), ({umax}, 1), (0, 1)] (
            interpolation = "faceVarying"
        )
        rel material:binding = </World/Looks/Checker>
    }}

    # A uniform dome, so the shading is the albedo and nothing else: any
    # difference between two renders of this scene is the texture lookup.
    def DomeLight "Sky"
    {{
        color3f inputs:color = (1, 1, 1)
        float inputs:intensity = 1.0
    }}

    def Camera "ShotCam"
    {{
        float focalLength = 35
        float horizontalAperture = 36
        float focusDistance = 20
        float fStop = 0
        double3 xformOp:translate = (0, 1.6, 18)
        float3 xformOp:rotateXYZ = (-4.5, 0, 0)
        uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateXYZ"]
    }}
}}

def Scope "Render"
{{
    def RenderSettings "settings"
    {{
        int2 resolution = (640, 360)
        int crust:samplesPerPixel = 16
        int crust:maxDepth = 2
        # Adaptive sampling off: this scene is measured against a reference of
        # itself, and a per-pixel sample budget that varies with the image
        # would make the two renders disagree for a reason that is not the
        # texture.
        float crust:varianceThreshold = 0.0
    }}
}}
"""


REF_SPP = 1024
TEST_SPP = 16


def render(scene, out, spp, env=None):
    e = dict(os.environ)
    e.update(env or {})
    subprocess.run(
        ["target/release/crust-render", "-i", scene, "-o", out,
         "-s", str(spp), "-l", "error"],
        check=True,
        env=e,
    )


def diff(a, b):
    """The `rmse:` and `mean abs diff:` lines exr_diff prints, as floats."""
    out = subprocess.run(
        ["target/release/examples/exr_diff", a, b],
        check=True,
        capture_output=True,
        text=True,
    ).stdout
    got = {}
    for line in out.splitlines():
        for key in ("rmse", "mean abs diff"):
            if line.startswith(key + ":"):
                got[key] = float(line.split(":", 1)[1])
    return got


def measure(outdir):
    """Render both sides against references of themselves and report.

    Each side is compared against a high-spp reference rendered **under the
    same switch**, because filtering changes the image a render converges to,
    not just the noise around it. Comparing filtered against unfiltered would
    measure the bias between two different correct answers; comparing each
    against itself measures what is actually claimed — that point-sampling a
    minified texture produces error that does not converge away.
    """
    scene = os.path.join(outdir, "alias.usda")
    off = {"CRUST_RAY_CONES": "0"}
    jobs = [
        ("filtered", None, "ref_mip.exr", "test_mip.exr"),
        ("point-sampled", off, "ref_flat.exr", "test_flat.exr"),
    ]
    results = {}
    for name, env, ref, test in jobs:
        ref, test = os.path.join(outdir, ref), os.path.join(outdir, test)
        print(f"rendering {name} reference at {REF_SPP} spp ...", flush=True)
        render(scene, ref, REF_SPP, env)
        print(f"rendering {name} test at {TEST_SPP} spp ...", flush=True)
        render(scene, test, TEST_SPP, env)
        results[name] = diff(ref, test)

    print(f"\nError at {TEST_SPP} spp against a {REF_SPP} spp reference "
          f"of the same configuration:\n")
    print(f"  {'':<16}{'rmse':>12}{'mean abs':>12}")
    for name, r in results.items():
        print(f"  {name:<16}{r['rmse']:>12.5f}{r['mean abs diff']:>12.5f}")
    a = results["point-sampled"]["rmse"]
    b = results["filtered"]["rmse"]
    if b > 0:
        print(f"\n  filtering lowers the RMSE by {a / b:.2f}x")


def main():
    flags = {a.split("=")[0]: a.split("=", 1)[-1] for a in sys.argv[1:] if a.startswith("--")}
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    outdir = args[0] if args else "/tmp/alias"
    tex = int(flags.get("--size", TEX))
    udim = max(1, min(10, int(flags.get("--udim", UDIM))))
    os.makedirs(outdir, exist_ok=True)

    # `--size` and `--udim` exist to build a working set preloading genuinely
    # cannot hold. The default 1024 checker is 3 MB and fits in anything; 8
    # UDIM tiles at 4096 is 400 MB of authored texture against a cache budget
    # the operator chooses, which is the comparison the streaming path is for.
    if udim > 1:
        for i in range(udim):
            write_checker_png(
                os.path.join(outdir, f"checker.{1001 + i}.png"), tex, phase=i
            )
        chart = "checker.<UDIM>.png"
    else:
        write_checker_png(os.path.join(outdir, "checker.png"), tex)
        chart = "checker.png"

    with open(os.path.join(outdir, "alias.mtlx"), "w") as f:
        # With a UDIM set the chart itself already spans the tiles (`st` runs
        # 0..udim), so the tiling must be 1 — multiplying the two sends `u`
        # past the 10x10 UDIM grid, where every lookup reads black. That is
        # exactly what happened the first time, and an empty texture-cache
        # block in `--stats` is what caught it.
        f.write(MTLX.format(repeat=REPEAT if udim == 1 else 1, chart=chart))
    with open(os.path.join(outdir, "alias.usda"), "w") as f:
        f.write(USDA.format(half=HALF, umax=udim))
    authored = udim * tex * tex * 3
    print(f"wrote {outdir}/alias.usda, alias.mtlx, {chart}")
    print(f"  {tex}x{tex} checker, {CHECK}-texel squares, {udim} UDIM tile(s), "
          f"{authored / (1024 * 1024):.0f} MiB authored")
    if "--measure" in sys.argv:
        measure(outdir)
    else:
        print("  pass --measure to render both sides and print the error table")


if __name__ == "__main__":
    main()
