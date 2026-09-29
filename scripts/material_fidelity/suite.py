"""What `run.py`, `summarize.py`, `check.py` and `goldeneye_suite.py` share about the
Material Fidelity suite: where its files are, how it scores an image, and the shot
layer that puts one of its materials on the shader ball.

    https://github.com/bhouston/material-fidelity

The suite's layout moved once: the samples were `submodules/material-samples` with
`<renderer>.png` images, and are now `submodules/mtlx-sample-library` with
`<renderer>.avif` images (AVIF quality 90, 4:4:4, `RENDER_AVIF_OPTIONS` in its
`packages/core/src/references.ts`). Both are read; the current one wins.
"""
import math
import os
import re
from pathlib import Path

import numpy as np
from PIL import Image

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
RENDERER_NAME = "crust"
REFERENCE_NAME = "materialx-glsl"
SIZE = 512
FOV_DEG = 45.0
FOCAL = 50.0
APERTURE = 2.0 * FOCAL * math.tan(math.radians(FOV_DEG) / 2.0)
# The suite's own encoder settings for every `<renderer>.avif`.
AVIF_OPTIONS = {"quality": 90, "subsampling": "4:4:4"}
IMAGE_EXTS = (".avif", ".png")
SAMPLE_DIRS = ("mtlx-sample-library", "material-samples")
# MaterialXView's lat-long puts the seam where crust (USD: -Z at u = 0.5) puts the
# centre. Fitted on the background of the reference images: 180 scores 34 dB there,
# 177 and 183 score 11-12 dB.
ENV_ROTATE = 180.0


def samples_root(suite):
    """The suite's sample library: `<suite>/submodules/<one of SAMPLE_DIRS>`."""
    for d in SAMPLE_DIRS:
        p = Path(suite) / "submodules" / d
        if (p / "materials").is_dir():
            return p
    raise SystemExit(f"no sample library under {suite}/submodules "
                     f"(looked for {', '.join(SAMPLE_DIRS)}); run `fidelity.sh init`")


def materials_root(suite):
    return samples_root(suite) / "materials"


def renderer_image(material_dir, renderer):
    """`<renderer>.avif`, else the older `<renderer>.png`, else None."""
    for ext in IMAGE_EXTS:
        p = Path(material_dir) / f"{renderer}{ext}"
        if p.exists():
            return p
    return None


def find_materials(root, selectors):
    """The suite's `--materials` semantics: substring or `re:` on the leaf directory."""
    files = sorted(Path(root).rglob("*.mtlx"))
    if not selectors:
        return files
    out = []
    for f in files:
        leaf = f.parent.name
        for s in selectors:
            if (re.search(s[3:], leaf) if s.startswith("re:") else s in leaf):
                out.append(f)
                break
    return out


def surface_material_name(mtlx):
    text = Path(mtlx).read_text(errors="replace")
    m = re.search(r"<surfacematerial\b[^>]*\bname\s*=\s*\"([^\"]+)\"", text)
    return m.group(1) if m else None


def load_rgb8(path):
    return np.asarray(Image.open(path).convert("RGBA"), dtype=np.uint8)[..., :3]


def psnr(src, ref):
    """`calculatePsnr` in the suite's packages/core/src/metrics.ts, exactly."""
    d = src.astype(np.float64) - ref.astype(np.float64)
    sse = float(np.sum(d * d))
    if sse == 0.0:
        return None
    return round(20.0 * math.log10(255.0 / math.sqrt(sse / d.size)), 3)


def linear_to_srgb8(rgb):
    """The suite's output transform: no tone mapping, clamp, sRGB OETF, 8 bits."""
    rgb = np.nan_to_num(np.asarray(rgb, dtype=np.float32), nan=0.0, posinf=1.0, neginf=0.0)
    c = np.clip(rgb, 0.0, 1.0)
    srgb = np.where(c <= 0.0031308, 12.92 * c, 1.055 * np.power(c, 1.0 / 2.4) - 0.055)
    return np.clip(np.round(srgb * 255.0), 0, 255).astype(np.uint8)


def srgb8_to_linear(rgb8):
    c = np.asarray(rgb8, dtype=np.float32) / 255.0
    return np.where(c <= 0.04045, c / 12.92, np.power((c + 0.055) / 1.055, 2.4)).astype(np.float32)


def read_exr_rgb(path):
    import OpenEXR

    with OpenEXR.File(str(path)) as f:
        ch = f.channels()
        if "RGB" in ch:
            return np.asarray(ch["RGB"].pixels, dtype=np.float32)
        return np.stack([np.asarray(ch[c].pixels, dtype=np.float32) for c in "RGB"], axis=-1)


def write_exr_rgb(path, rgb):
    import OpenEXR

    header = {"compression": OpenEXR.ZIP_COMPRESSION, "type": OpenEXR.scanlineimage}
    with OpenEXR.File(header, {"RGB": np.ascontiguousarray(rgb, dtype=np.float32)}) as f:
        f.write(str(path))


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
        rel products = </Render/Product>
        int2 resolution = ({size}, {size})
        int crust:samplesPerPixel = {spp}
        int crust:minSamplesPerPixel = {min_spp}
        int crust:maxDepth = {max_depth}
        float crust:indirectClamp = 0
    }}

    def RenderProduct "Product"
    {{
        rel camera = </World/Cam>
        token productName = "{product}"
        int2 resolution = ({size}, {size})
        rel orderedVars = </Render/Vars/color>
    }}

    def Scope "Vars"
    {{
        def RenderVar "color"
        {{
            uniform token dataType = "color3f"
            uniform string sourceName = "color"
        }}
    }}
}}
"""


def asset_path(target, layer_dir):
    """An asset path for `target` as authored in a layer in `layer_dir`: relative when
    `layer_dir` is given (the layer can move with its assets), absolute otherwise."""
    target = Path(target).resolve()
    if layer_dir is None:
        return str(target)
    rel = os.path.relpath(target, Path(layer_dir).resolve())
    return rel if rel.startswith("..") else f"./{rel}"


def shot_layer(*, mtlx, material, ball, hdr, spp, min_spp, max_depth, product,
               self_shadow=False, env_rotate=ENV_ROTATE, layer_dir=None):
    """The shot layer for one material. `crust:*` attributes are crust's; any other
    Hydra renderer reads the same camera, dome, binding and `RenderProduct`, and
    ignores them (so it will let the ball shadow itself)."""
    return SHOT.format(
        focal=FOCAL,
        aperture=f"{APERTURE:.6f}",
        hdr=asset_path(hdr, layer_dir),
        env_rotate=env_rotate,
        mtlx=asset_path(mtlx, layer_dir),
        material=material,
        ball=asset_path(ball, layer_dir),
        # camera + indirect, no shadow rays: the suite's renderers cast no shadows
        ray_mask=7 if self_shadow else 5,
        size=SIZE,
        spp=spp,
        min_spp=min(spp, min_spp),
        max_depth=max_depth,
        product=product,
    )
