#!/usr/bin/env python3
"""Record what Arnold 7.5's auto-tx writes, and how Arnold reads a `.tx`, as
the reference crust's `.tx` convention is checked against.

Arnold's `kick` renders tiny scenes with `texture_auto_generate_tx on`, each
binding one small source under one colour space, through an `image` node or
through Arnold's USD procedural. `oiiotool --info -v` then reads the header
of every `.tx` Arnold wrote. The script also measures two behaviours a file
header cannot show:

- which `.tx` Arnold rebuilds (`maketx -u` semantics): a file whose
  `Software` string or modification time was changed is rendered again, and
  the log says whether Arnold regenerated it;
- how Arnold reads a `.tx` bound directly: a constant texture rendered through
  a `flat` shader with the camera inside the sphere, so every pixel is the
  texture value.

Everything goes to `crates/crust-assets/tests/data/arnold_tx/`:
`cases.json` (names, file types, headers, the rebuild and direct-binding
tables), the sources, and the `.tx` files Arnold wrote beside them. CI never
runs this: the fixtures are committed, so `cargo test` needs no Arnold. Rerun
it after adding a case; `cases.json` is identical from run to run (absolute
paths become `<dir>`, `DateTime` is dropped), so its diff shows exactly what
moved.

The OCIO config is crust's own (`ocio://cg-config-v4.0.0_aces-v2.0_ocio-v2.5`)
and the rendering space ACEScg, which is what makes Arnold's names comparable
to crust's.

Requirements:
  Arnold 7.5 (`kick`, `oiiotool`) in ARNOLD_BIN, default
  C:\\Program Files\\Arnold-7.5.0.0-windows\\bin
  Pillow (`pip install pillow`)

Usage:
  scripts/arnold_tx_oracle.py [--out DIR] [--keep DIR]
"""
import argparse
import json
import math
import os
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import zlib

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DEFAULT_OUT = os.path.join(ROOT, "crates", "crust-assets", "tests", "data", "arnold_tx")
DEFAULT_BIN = r"C:\Program Files\Arnold-7.5.0.0-windows\bin"
CONFIG = "ocio://cg-config-v4.0.0_aces-v2.0_ocio-v2.5"
TARGET = "ACEScg"
SRGB = "sRGB Encoded Rec.709 (sRGB)"
LIN709 = "Linear Rec.709 (sRGB)"

# Keys `oiiotool --info -v` prints that differ between two runs of the same
# conversion, so they would make `cases.json` change on every run.
VOLATILE = {"DateTime"}

# ---------------------------------------------------------------------------
# Sources. Small, deterministic, and each exercising one stage of maketx:
# odd sizes for the round-down levels, a cutout and an opaque alpha for
# `--opaque-detect` and `--unpremult`, a constant and a grey image for the
# constant-colour and monochrome detections, a NaN for `--fixnan box3`, and
# values above 1 for the float path.
# ---------------------------------------------------------------------------
W, H = 37, 23


def rgb_at(x, y):
    return ((x * 7 + y * 3) % 256, (x * x + y * 11) % 256, (x * y * 5 + 40) % 256)


def write_exr(path, w, h, channels, half=False):
    """An uncompressed scanline EXR of 32-bit floats (or `half`s): `channels`
    maps a channel name to a function of `(x, y)`."""

    def attr(name, kind, value):
        return name.encode() + b"\0" + kind.encode() + b"\0" + struct.pack("<i", len(value)) + value

    names = sorted(channels)
    pixel_type, size, code = (1, 2, "e") if half else (2, 4, "f")
    chlist = b"".join(n.encode() + b"\0" + struct.pack("<iB3xii", pixel_type, 0, 1, 1) for n in names) + b"\0"
    box = struct.pack("<iiii", 0, 0, w - 1, h - 1)
    header = (
        struct.pack("<ii", 20000630, 2)
        + attr("channels", "chlist", chlist)
        + attr("compression", "compression", b"\0")
        + attr("dataWindow", "box2i", box)
        + attr("displayWindow", "box2i", box)
        + attr("lineOrder", "lineOrder", b"\0")
        + attr("pixelAspectRatio", "float", struct.pack("<f", 1.0))
        + attr("screenWindowCenter", "v2f", struct.pack("<ff", 0.0, 0.0))
        + attr("screenWindowWidth", "float", struct.pack("<f", 1.0))
        + b"\0"
    )
    line_bytes = w * size * len(names)
    start = len(header) + 8 * h
    table = b"".join(struct.pack("<Q", start + y * (8 + line_bytes)) for y in range(h))
    lines = b""
    for y in range(h):
        lines += struct.pack("<ii", y, line_bytes)
        for n in names:
            lines += struct.pack(f"<{w}{code}", *(channels[n](x, y) for x in range(w)))
    with open(path, "wb") as f:
        f.write(header + table + lines)


def write_png16(path, w, h, rgb):
    """A 16-bit RGB PNG (which Pillow cannot write): `rgb(x, y)` gives three
    16-bit samples."""

    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body))

    raw = b"".join(b"\0" + b"".join(struct.pack(">3H", *rgb(x, y)) for x in range(w)) for y in range(h))
    png = (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 16, 2, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(raw, 9))
        + chunk(b"IEND", b"")
    )
    with open(path, "wb") as f:
        f.write(png)


def make_sources(d):
    """Writes every source into `d`; returns their file names."""
    img = Image.new("RGB", (W, H))
    img.putdata([rgb_at(x, y) for y in range(H) for x in range(W)])
    img.save(os.path.join(d, "grad.png"))

    tile = Image.new("RGB", (64, 64))
    tile.putdata([rgb_at(x * 3 + 1, y * 5 + 2) for y in range(64) for x in range(64)])
    tile.save(os.path.join(d, "tile.png"))

    cut = Image.new("RGBA", (W, H))
    cut.putdata([rgb_at(x, y) + (0 if x < 10 else min(255, (x + y) * 6),) for y in range(H) for x in range(W)])
    cut.save(os.path.join(d, "cut.png"))

    opaque = Image.new("RGBA", (W, H))
    opaque.putdata([rgb_at(x, y) + (255,) for y in range(H) for x in range(W)])
    opaque.save(os.path.join(d, "opaque.png"))

    Image.new("RGB", (W, H), (204, 102, 26)).save(os.path.join(d, "const.png"))
    # Larger than one tile, so `--constant-color-detect` has something to
    # shrink.
    Image.new("RGB", (200, 100), (204, 102, 26)).save(os.path.join(d, "const_big.png"))

    grey = Image.new("RGB", (W, H))
    grey.putdata([(rgb_at(x, y)[0],) * 3 for y in range(H) for x in range(W)])
    grey.save(os.path.join(d, "grey.png"))

    grey1 = Image.new("L", (W, H))
    grey1.putdata([rgb_at(x, y)[1] for y in range(H) for x in range(W)])
    grey1.save(os.path.join(d, "grey1.png"))

    deep = Image.new("I;16", (W, H))
    deep.putdata([(rgb_at(x, y)[0] * 257 + x) % 65536 for y in range(H) for x in range(W)])
    deep.save(os.path.join(d, "deep.png"))
    write_png16(os.path.join(d, "deep_rgb.png"), W, H, lambda x, y: tuple((c * 257 + x) % 65536 for c in rgb_at(x, y)))

    write_exr(
        os.path.join(d, "hdr.exr"),
        W,
        H,
        {
            "R": lambda x, y: (x + 1) * 0.25,
            "G": lambda x, y: (y + 1) * 0.125,
            "B": lambda x, y: 0.5 if (x + y) % 2 else 8.0,
        },
    )
    write_exr(
        os.path.join(d, "half.exr"),
        W,
        H,
        {
            "R": lambda x, y: (x + 1) * 0.25,
            "G": lambda x, y: (y + 1) * 0.125,
            "B": lambda x, y: 0.5 if (x + y) % 2 else 8.0,
        },
        half=True,
    )
    nan = float("nan")
    write_exr(
        os.path.join(d, "nan.exr"),
        W,
        H,
        {
            "R": lambda x, y: nan if (x, y) == (5, 7) else x / W,
            "G": lambda x, y: nan if (x, y) == (5, 7) else y / H,
            "B": lambda x, y: nan if (x, y) == (5, 7) else 0.25,
        },
    )
    return sorted(os.listdir(d))


# One `image` node per case: (case, source, color_space). The source is copied
# to `<case>.<ext>` so every case has its own stem, hence its own `.tx`.
IMAGE_CASES = [
    ("srgb_canon", "grad.png", SRGB),
    ("srgb_alias", "grad.png", "srgb_tx"),
    ("srgb_word", "grad.png", "sRGB"),
    ("srgb_texture", "grad.png", "srgb_texture"),
    ("auto_rgb", "grad.png", "auto"),
    ("auto_grey1", "grey1.png", "auto"),
    ("lin709", "grad.png", LIN709),
    ("ocio_Raw", "grad.png", "Raw"),
    ("word_raw", "grad.png", "raw"),
    ("target", "grad.png", TARGET),
    ("empty", "grad.png", ""),
    ("tile_srgb", "tile.png", SRGB),
    ("cut_srgb", "cut.png", SRGB),
    ("cut_lin709", "cut.png", LIN709),
    ("cut_empty", "cut.png", ""),
    ("opaque_srgb", "opaque.png", SRGB),
    ("const_srgb", "const.png", SRGB),
    ("const_big_srgb", "const_big.png", SRGB),
    ("const_big_empty", "const_big.png", ""),
    ("grey_srgb", "grey.png", SRGB),
    ("grey_empty", "grey.png", ""),
    ("grey1_srgb", "grey1.png", SRGB),
    # What `auto` makes of each pixel format.
    ("grey_auto", "grey.png", "auto"),
    ("cut_auto", "cut.png", "auto"),
    ("deep_auto", "deep.png", "auto"),
    ("deep_rgb_auto", "deep_rgb.png", "auto"),
    ("half_auto", "half.exr", "auto"),
    ("deep_lin709", "deep.png", LIN709),
    ("deep_srgb", "deep.png", SRGB),
    ("deep_empty", "deep.png", ""),
    ("deep_rgb_srgb", "deep_rgb.png", SRGB),
    ("deep_rgb_empty", "deep_rgb.png", ""),
    ("half_empty", "half.exr", ""),
    ("half_srgb", "half.exr", SRGB),
    ("hdr_empty", "hdr.exr", ""),
    ("hdr_lin709", "hdr.exr", LIN709),
    ("hdr_srgb", "hdr.exr", SRGB),
    ("hdr_auto", "hdr.exr", "auto"),
    ("hdr_raw", "hdr.exr", "raw"),
    ("nan_empty", "nan.exr", ""),
    ("nan_lin709", "nan.exr", LIN709),
]

# One USD stage per case: how Arnold's USD procedural hands a binding to its
# `image` node. (case, shader, source, colour space or None for unset, where).
# `where` is `input` (UsdUVTexture's `sourceColorSpace`) or `meta` (the
# `colorSpace` metadatum on `inputs:file`, which both renderers let win).
USD_CASES = [
    ("usd_uv_srgb", "UsdUVTexture", "grad.png", "sRGB", "input"),
    ("usd_uv_raw", "UsdUVTexture", "grad.png", "raw", "input"),
    ("usd_uv_auto", "UsdUVTexture", "grad.png", "auto", "input"),
    ("usd_uv_unset", "UsdUVTexture", "grad.png", None, "input"),
    ("usd_uv_auto_grey1", "UsdUVTexture", "grey1.png", "auto", "input"),
    ("usd_uv_meta_srgb_texture", "UsdUVTexture", "grad.png", "srgb_texture", "meta"),
    ("usd_uv_meta_canon", "UsdUVTexture", "grad.png", SRGB, "meta"),
    ("usd_mx_srgb_texture", "ND_image_color3", "grad.png", "srgb_texture", "meta"),
    ("usd_mx_lin_rec709", "ND_image_color3", "grad.png", "lin_rec709", "meta"),
    ("usd_mx_unset", "ND_image_color3", "grad.png", None, "meta"),
    ("usd_mx_unset_grey1", "ND_image_color3", "grey1.png", None, "meta"),
    # Non-colour images: does an unset colorspace convert a roughness or a
    # normal map too?
    ("usd_mx_float_unset", "ND_image_float", "grey1.png", None, "meta"),
    ("usd_mx_float_unset_rgb", "ND_image_float", "grad.png", None, "meta"),
    ("usd_mx_vector3_unset", "ND_image_vector3", "grad.png", None, "meta"),
]

# Where each MaterialX image node's output is connected on a standard_surface,
# and the output's USD type.
MX_OUTPUTS = {
    "ND_image_color3": ("color3f", "color3f inputs:base_color"),
    "ND_image_float": ("float", "float inputs:specular_roughness"),
    "ND_image_vector3": ("vector3f", "vector3f inputs:normal"),
}


class Arnold:
    def __init__(self, bindir):
        self.bindir = bindir
        exe = ".exe" if os.name == "nt" else ""
        self.kick_exe = os.path.join(bindir, "kick" + exe)
        self.oiiotool_exe = os.path.join(bindir, "oiiotool" + exe)
        for p in (self.kick_exe, self.oiiotool_exe):
            if not os.path.isfile(p):
                sys.exit(f"{p} not found: set ARNOLD_BIN to Arnold 7.5's bin directory")

    def kick(self, ass, cwd, check=True):
        """Renders `ass`; returns the `.tx` paths the log says were generated.

        With `check` off a failed render is not an error, and a `.tx` Arnold
        tried and failed to write counts as generated: on Windows a rebuild
        over an existing `.tx` fails ("Could not rename file"), because
        Arnold still holds the old file open, and aborts the render."""
        for attempt in range(2):
            out = subprocess.run(
                [self.kick_exe, "-i", ass, "-dw", "-dp", "-v", "2", "-nostdin"],
                cwd=cwd,
                capture_output=True,
                text=True,
            )
            # Arnold's auto-tx occasionally fails to open its own temporary
            # file on Windows (a scanner holding it, most likely); once more.
            if out.returncode == 0 or not check:
                break
        log = out.stdout + out.stderr
        if check and out.returncode != 0:
            sys.exit(f"kick failed on {ass}:\n{log}")
        made = re.findall(r"\[texturesys\] generating (.+?) file from ", log)
        if not check:
            made += re.findall(r"\[texturesys\] could not create (.+?)\.\n", log)
        return made

    def info(self, path):
        out = subprocess.run(
            [self.oiiotool_exe, "--info", "-v", path], capture_output=True, text=True, check=True
        ).stdout
        return parse_info(out)

    def stats(self, path):
        """Level 0's per-channel min, max and mean, decoded by OIIO (so by
        OpenEXR or libtiff): the reference crust's own decode is checked
        against."""
        out = subprocess.run(
            [self.oiiotool_exe, "--printstats", path], capture_output=True, text=True, check=True
        ).stdout
        get = lambda key: [float(v) for v in re.search(rf"Stats {key}: ([^(]+)", out).group(1).split()]
        return {"min": get("Min"), "max": get("Max"), "avg": get("Avg")}

    def pixel(self, exr, x, y):
        out = subprocess.run(
            [self.oiiotool_exe, "--dumpdata", exr], capture_output=True, text=True, check=True
        ).stdout
        m = re.search(rf"Pixel \({x}, {y}\): ([^\n]+)", out)
        return [float(v) for v in m.group(1).split()]


def parse_info(text):
    """`oiiotool --info -v` as a dict: the summary line, then `key: value`."""
    lines = text.splitlines()
    first = next(l for l in lines if " : " in l and "x" in l)
    m = re.search(r":\s+(\d+) x\s+(\d+), (\d+) channel, (\S+) (\S+)", first)
    info = {
        "size": [int(m.group(1)), int(m.group(2))],
        "channel_count": int(m.group(3)),
        "type": m.group(4),
        "format": m.group(5),
        "attributes": {},
    }
    for line in lines[lines.index(first) + 1 :]:
        key, _, value = line.strip().partition(": ")
        if key == "MIP-map levels":
            info["levels"] = [[int(v) for v in lv.split("x")] for lv in value.split()]
        elif key == "channel list":
            info["channels"] = [c.strip() for c in value.split(",")]
        elif key == "tile size":
            info["tile"] = [int(v) for v in value.split(" x ")]
        elif key and key not in VOLATILE:
            info["attributes"][key] = value
    info.setdefault("levels", [info["size"]])
    return info


def fwd(path):
    """A path as Arnold writes it: absolute, long names, forward slashes."""
    return os.path.realpath(path).replace("\\", "/")


def options(work, extra=""):
    return (
        f'options {{ outputs "RGBA RGBA f d" AA_samples 1 xres 4 yres 4 camera cam '
        f"color_manager ocio texture_auto_generate_tx on texture_accept_untiled on {extra}}}\n"
        f'color_manager_ocio {{ name ocio config "{CONFIG}" color_space_linear "{TARGET}" }}\n'
        # Inside the unit sphere, looking at its inside: every pixel is a
        # surface point, so a `flat` shader shows the texture in every pixel.
        f"persp_camera {{ name cam position 0 0 0.3 fov 20 }}\n"
        f'driver_exr {{ name d filename "{fwd(os.path.join(work, "out.exr"))}" color_space "{TARGET}" }}\n'
        f"box_filter {{ name f }}\n"
    )


def image_scene(work, filename, color_space):
    return options(work) + (
        f'image {{ name i filename "{fwd(filename)}" color_space "{color_space}" }}\n'
        f"flat {{ name s color i }}\n"
        f"sphere {{ name sph shader s radius 0.5 }}\n"
    )


def usd_stage(source, shader, space, where):
    """A stage binding `source` to one sphere through `shader`."""
    meta = f' ( colorSpace = "{space}" )' if where == "meta" and space is not None else ""
    if shader == "UsdUVTexture":
        scs = f'\n        token inputs:sourceColorSpace = "{space}"' if where == "input" and space is not None else ""
        material = f"""    def Material "m" {{
      token outputs:surface.connect = </World/m/pbs.outputs:surface>
      def Shader "pbs" {{
        uniform token info:id = "UsdPreviewSurface"
        color3f inputs:diffuseColor.connect = </World/m/tex.outputs:rgb>
        token outputs:surface
      }}
      def Shader "tex" {{
        uniform token info:id = "UsdUVTexture"
        asset inputs:file = @./{source}@{meta}{scs}
        float3 outputs:rgb
      }}
    }}"""
    else:
        out_type, target = MX_OUTPUTS[shader]
        material = f"""    def Material "m" {{
      token outputs:mtlx:surface.connect = </World/m/ss.outputs:out>
      def Shader "ss" {{
        uniform token info:id = "ND_standard_surface_surfaceshader"
        {target}.connect = </World/m/img.outputs:out>
        token outputs:out
      }}
      def Shader "img" {{
        uniform token info:id = "{shader}"
        asset inputs:file = @./{source}@{meta}
        {out_type} outputs:out
      }}
    }}"""
    return f"""#usda 1.0
(
    defaultPrim = "World"
    metersPerUnit = 1
    upAxis = "Y"
)
def Xform "World" {{
    def Sphere "s" ( prepend apiSchemas = ["MaterialBindingAPI"] ) {{
      double radius = 0.5
      rel material:binding = </World/m>
    }}
{material}
}}
"""


def normalise(text, work):
    """`text` with the work directory replaced by `<dir>`, both as Arnold
    spells it (forward slashes) and as Windows does."""
    for d in {fwd(work), os.path.realpath(work)}:
        text = text.replace(d, "<dir>")
    return text


def record(arnold, work, case, source, txs):
    """The JSON record of one case's `.tx` files."""
    src_mtime = os.stat(os.path.join(work, source)).st_mtime_ns
    out = []
    for tx in sorted(set(txs)):
        path = tx if os.path.isabs(tx) else os.path.join(work, tx)
        info = arnold.info(path)
        attrs = info.pop("attributes")
        software = attrs.pop("Software", None)
        history = attrs.pop("Exif:ImageHistory", None)
        out.append(
            {
                "tx": os.path.basename(path),
                **info,
                "level0_stats": arnold.stats(path),
                "attributes": attrs,
                # Quoted as oiiotool prints it: the outer quotes are the
                # printer's, everything between them is the attribute.
                "software": normalise(software[1:-1], work) if software else None,
                "image_history_equals_software": history == software,
                # OIIO sets the `.tx` time to the source's in whole seconds,
                # so a sub-second source time is truncated, never kept.
                "mtime_equals_source_seconds": os.stat(path).st_mtime_ns // 10**9 == src_mtime // 10**9,
                "mtime_whole_seconds": os.stat(path).st_mtime_ns % 10**9 == 0,
            }
        )
    return {"case": case, "source": source, "tx": out}


def run_image_cases(arnold, work):
    cases = []
    for case, source, space in IMAGE_CASES:
        ext = os.path.splitext(source)[1]
        name = case + ext
        shutil.copy2(os.path.join(work, "src", source), os.path.join(work, name))
        ass = os.path.join(work, f"{case}.ass")
        with open(ass, "w") as f:
            f.write(image_scene(work, os.path.join(work, name), space))
        txs = arnold.kick(ass, work)
        cases.append({"color_space": space, **record(arnold, work, case, name, txs)})
        print(f"  {case:>22}: {', '.join(t['tx'] for t in cases[-1]['tx']) or 'no .tx'}")
    return cases


def run_usd_cases(arnold, work):
    cases = []
    for case, shader, source, space, where in USD_CASES:
        name = case + ".png"
        shutil.copy2(os.path.join(work, "src", source), os.path.join(work, name))
        stage = os.path.join(work, f"{case}.usda")
        with open(stage, "w") as f:
            f.write(usd_stage(name, shader, space, where))
        ass = os.path.join(work, f"{case}.ass")
        with open(ass, "w") as f:
            f.write(options(work) + f'usd {{ name u filename "{fwd(stage)}" }}\nskydome_light {{ name sky }}\n')
        txs = arnold.kick(ass, work)
        rec = record(arnold, work, case, name, txs)
        cases.append({"shader": shader, "color_space": space, "authored_as": where, **rec})
        print(f"  {case:>22}: {', '.join(t['tx'] for t in rec['tx']) or 'no .tx'}")
    return cases


# ---------------------------------------------------------------------------
# What makes Arnold rebuild a `.tx`. Arnold's auto-tx runs maketx with `-u`,
# which skips a `.tx` whose modification time equals the source's and whose
# `Software` names the same command. Each probe edits Arnold's own file, puts
# its modification time back, renders again, and reads whether the log says
# the file was regenerated.
# ---------------------------------------------------------------------------


def tiff_set_software(path, text):
    """Points every IFD's `Software` (tag 305) at `text`, appended to the file."""
    with open(path, "rb") as f:
        data = bytearray(f.read())
    assert data[:4] == b"II*\0", "little-endian TIFF expected"
    value = text.encode() + b"\0"
    if len(data) % 2:
        data += b"\0"
    at = len(data)
    data += value
    ifd = struct.unpack_from("<I", data, 4)[0]
    found = False
    while ifd:
        (n,) = struct.unpack_from("<H", data, ifd)
        for e in range(n):
            entry = ifd + 2 + 12 * e
            if struct.unpack_from("<H", data, entry)[0] == 305:
                struct.pack_into("<HII", data, entry + 2, 2, len(value), at)
                found = True
        ifd = struct.unpack_from("<I", data, ifd + 2 + 12 * n)[0]
    assert found, f"{path} has no Software tag"
    with open(path, "wb") as f:
        f.write(data)


def exr_set_software(path, text):
    """Rewrites the `Software` header attribute of a single-part EXR, shifting
    the chunk offset table by the header's change in length."""
    with open(path, "rb") as f:
        data = f.read()
    pos, attrs = 8, []
    while data[pos] != 0:
        name_end = data.index(b"\0", pos)
        kind_end = data.index(b"\0", name_end + 1)
        (size,) = struct.unpack_from("<i", data, kind_end + 1)
        name = data[pos:name_end]
        value = data[kind_end + 5 : kind_end + 5 + size]
        attrs.append((name, data[name_end + 1 : kind_end], value))
        pos = kind_end + 5 + size
    table = pos + 1
    attrs = [(n, k, text.encode() if n == b"Software" else v) for n, k, v in attrs]
    header = data[:8] + b"".join(n + b"\0" + k + b"\0" + struct.pack("<i", len(v)) + v for n, k, v in attrs) + b"\0"
    delta = len(header) - table
    # The table runs until the first chunk it points at.
    offsets, at = [], table
    while not offsets or at < min(offsets):
        offsets.append(struct.unpack_from("<Q", data, at)[0])
        at += 8
    shifted = b"".join(struct.pack("<Q", o + delta) for o in offsets)
    with open(path, "wb") as f:
        f.write(header + shifted + data[at:])


def run_rebuild_probes(arnold, work):
    """Which edits to an Arnold `.tx` make Arnold rebuild it."""
    probes = []
    for case, space in (("u_exr", SRGB), ("u_tiff", LIN709)):
        name = case + ".png"
        shutil.copy2(os.path.join(work, "src", "grad.png"), os.path.join(work, name))
        ass = os.path.join(work, f"{case}.ass")
        with open(ass, "w") as f:
            f.write(image_scene(work, os.path.join(work, name), space))
        (tx,) = arnold.kick(ass, work)
        original = arnold.info(tx)["attributes"]["Software"][1:-1]
        prefix, sep, command = original.partition(" : ")
        assert sep, original
        set_software = exr_set_software if tx.endswith(".tx") and arnold.info(tx)["format"] == "openexr" else tiff_set_software
        src_stat = os.stat(os.path.join(work, name))

        def probe(label, edit):
            if edit is not None:
                edit()
            regenerated = bool(arnold.kick(ass, work, check=False))
            probes.append({"case": case, "probe": label, "regenerated": regenerated})
            print(f"  {case:>8} {label:>34}: {'rebuilt' if regenerated else 'kept'}")

        def restamp(ns):
            os.utime(tx, ns=(src_stat.st_atime_ns, ns))

        probe("unchanged", None)
        probe("prefix crust-render", lambda: (set_software(tx, "crust-render : " + command), restamp(src_stat.st_mtime_ns)))
        probe("prefix OpenImageIO-Arnold again", lambda: (set_software(tx, original), restamp(src_stat.st_mtime_ns)))
        probe("command line changed", lambda: (set_software(tx, prefix + " : " + command.replace("--fixnan box3", "--fixnan black")), restamp(src_stat.st_mtime_ns)))
        probe("one second newer than source", lambda: restamp(src_stat.st_mtime_ns + 1_000_000_000))
        probe("one second older than source", lambda: restamp(src_stat.st_mtime_ns - 1_000_000_000))
        probe("no Software", lambda: (set_software(tx, ""), restamp(src_stat.st_mtime_ns)))
    return probes


# ---------------------------------------------------------------------------
# How Arnold reads a `.tx` bound directly. A constant u8 sRGB source
# (0.8, 0.4, 0.1) through `flat`, so every pixel of the ACEScg output is the
# texture's value.
# ---------------------------------------------------------------------------


def run_direct_binding(arnold, work):
    d = os.path.join(work, "direct")
    os.makedirs(d)
    src = os.path.join(d, "conv.png")
    Image.new("RGB", (16, 16), (204, 102, 26)).save(src)
    plain_src = os.path.join(d, "plain.png")
    shutil.copy2(src, plain_src)

    def render(label, filename, space, auto_tx=True):
        ass = os.path.join(d, f"{label}.ass")
        extra = "" if auto_tx else "texture_auto_generate_tx off "
        scene = image_scene(d, filename, space).replace("texture_auto_generate_tx on ", extra)
        with open(ass, "w") as f:
            f.write(scene)
        arnold.kick(ass, d)
        return arnold.pixel(os.path.join(d, "out.exr"), 1, 1)[:3]

    rows = [{"binding": "source, sRGB", "value": render("source", src, SRGB)}]
    converted = os.path.join(d, f"conv_{SRGB}_{TARGET}.png.tx")
    renamed = os.path.join(d, "renamed.tx")
    shutil.copy2(converted, renamed)
    # A u8 `.tx` holding the source's own bytes, unconverted: what `raw`
    # writes for an 8-bit source.
    render("plain_make", plain_src, "")
    plain = os.path.join(d, "plain.tx")
    shutil.copy2(os.path.join(d, "plain_raw.png.tx"), plain)
    for label, filename, space in (
        ("converted .tx, bound sRGB", converted, SRGB),
        ("converted .tx, bound \"\"", converted, ""),
        ("converted .tx, bound auto", converted, "auto"),
        ("converted .tx, bound ACEScg", converted, TARGET),
        ("converted .tx renamed, bound sRGB", renamed, SRGB),
        ("u8 .tx unconverted, bound sRGB", plain, SRGB),
        ("u8 .tx unconverted, bound \"\"", plain, ""),
        ("u8 .tx unconverted, bound auto", plain, "auto"),
    ):
        key = re.sub(r"\W+", "_", label).strip("_")
        rows.append({"binding": label, "value": render(key, filename, space, auto_tx=False)})
    for r in rows:
        r["value"] = [round(v, 4) for v in r["value"]]
        print(f"  {r['binding']:>36}: {r['value']}")
    return rows


# ---------------------------------------------------------------------------
# Arnold reading crust's files (`--crust`). For each binding, Arnold converts
# and renders its own `.tx`; then that file is set aside, crust's `maketx`
# writes the same name from the same source path, and Arnold renders again.
# Arnold must keep crust's file (no regeneration in its log), and the two
# renders must agree (`crust diff`). A textured view: the camera looks at a
# plane of the texture through a `flat` shader, so every pixel is a lookup.
# ---------------------------------------------------------------------------
CRUST_CASES = [
    ("srgb_canon", "grad.png", SRGB),
    ("tile_srgb", "tile.png", SRGB),
    ("lin709", "grad.png", LIN709),
    ("ocio_Raw", "grad.png", "Raw"),
    ("word_raw", "grad.png", "raw"),
    ("empty", "grad.png", ""),
    ("auto_rgb", "grad.png", "auto"),
    ("cut_srgb", "cut.png", SRGB),
    ("grey_srgb", "grey.png", SRGB),
    ("const_big_srgb", "const_big.png", SRGB),
    ("deep_srgb", "deep_rgb.png", SRGB),
    ("hdr_srgb", "hdr.exr", SRGB),
    ("half_empty", "half.exr", ""),
]


def plane_scene(work, filename, color_space, out):
    return (
        f'options {{ outputs "RGBA RGBA f d" AA_samples 4 xres 64 yres 64 camera cam '
        f"color_manager ocio texture_auto_generate_tx on texture_accept_untiled on }}\n"
        f'color_manager_ocio {{ name ocio config "{CONFIG}" color_space_linear "{TARGET}" }}\n'
        f"ortho_camera {{ name cam position 0.5 0.5 1 look_at 0.5 0.5 0 "
        f"screen_window_min -0.5 -0.5 screen_window_max 0.5 0.5 }}\n"
        f'driver_exr {{ name d filename "{fwd(out)}" color_space "{TARGET}" }}\n'
        f"box_filter {{ name f }}\n"
        f'image {{ name i filename "{fwd(filename)}" color_space "{color_space}" }}\n'
        f"flat {{ name s color i }}\n"
        f"polymesh {{ name p nsides 1 1 UINT 4 vidxs 4 1 UINT 0 1 2 3 "
        f"vlist 4 1 VECTOR 0 0 0 1 0 0 1 1 0 0 1 0 "
        f"uvidxs 4 1 UINT 0 1 2 3 uvlist 4 1 VECTOR2 0 0 1 0 1 1 0 1 shader s }}\n"
    )


def run_arnold_reads_crust(arnold, work, crust_dir):
    exe = ".exe" if os.name == "nt" else ""
    crust = os.path.join(crust_dir, "crust" + exe)
    maketx = os.path.join(crust_dir, "examples", "maketx" + exe)
    for p in (crust, maketx):
        if not os.path.isfile(p):
            sys.exit(f"{p} not found: build crust (`cargo build --release -p crust-render "
                     f"--bin crust --example maketx`) or pass --crust")
    d = os.path.join(work, "crust")
    os.makedirs(d)
    rows = []
    for case, source, space in CRUST_CASES:
        ext = os.path.splitext(source)[1]
        src = os.path.join(d, case + ext)
        shutil.copy2(os.path.join(work, "src", source), src)
        ass = os.path.join(d, f"{case}.ass")

        def render(out):
            with open(ass, "w") as f:
                f.write(plane_scene(d, src, space, out))
            return arnold.kick(ass, d, check=False)

        theirs = os.path.join(d, f"{case}_arnold.exr")
        made = render(theirs)
        assert len(made) == 1, f"{case}: Arnold generated {made}"
        tx = made[0] if os.path.isabs(made[0]) else os.path.join(d, made[0])
        os.replace(tx, tx + ".arnold")
        # The binding as crust's `maketx` takes it: the same string, `auto`
        # included; an empty one is "nothing authored".
        out = subprocess.run([maketx, fwd(src), space], capture_output=True, text=True)
        if out.returncode != 0 or not os.path.isfile(tx):
            sys.exit(f"{case}: crust's maketx failed:\n{out.stdout}{out.stderr}")
        ours = os.path.join(d, f"{case}_crust.exr")
        regenerated = render(ours)
        diff = subprocess.run([crust, "diff", theirs, ours, "--json", "-"], capture_output=True, text=True)
        report = json.loads(diff.stdout)
        beauty = report.get("beauty") or {}
        rows.append(
            {
                "case": case,
                "color_space": space,
                "arnold_regenerated_crusts_file": bool(regenerated),
                "relmse": float(f"{beauty.get('relmse', 0.0):.3g}"),
                "max_abs": float(f"{beauty.get('max_abs', 0.0):.3g}"),
            }
        )
        r = rows[-1]
        print(f"  {case:>16}: regenerated {r['arnold_regenerated_crusts_file']}, relmse {r['relmse']}, max {r['max_abs']}")
    return rows


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=DEFAULT_OUT)
    ap.add_argument("--keep", help="work in DIR and keep it, instead of a temporary directory")
    ap.add_argument(
        "--crust",
        metavar="DIR",
        help="also have Arnold render crust's files: DIR holds crust and examples/maketx "
        "(e.g. target/release); writes arnold_reads_crust.json",
    )
    args = ap.parse_args()
    arnold = Arnold(os.environ.get("ARNOLD_BIN", DEFAULT_BIN))

    work = args.keep or tempfile.mkdtemp(prefix="arnold_tx_")
    if args.keep:
        shutil.rmtree(work, ignore_errors=True)
        os.makedirs(work)
    os.makedirs(os.path.join(work, "src"))
    sources = make_sources(os.path.join(work, "src"))

    print("image nodes:")
    image = run_image_cases(arnold, work)
    print("USD procedural:")
    usd = run_usd_cases(arnold, work)
    print("rebuild (maketx -u):")
    rebuild = run_rebuild_probes(arnold, work)
    print("direct binding (flat, ACEScg output):")
    direct = run_direct_binding(arnold, work)
    crust_rows = None
    if args.crust:
        print("Arnold reading crust's files:")
        crust_rows = run_arnold_reads_crust(arnold, work, args.crust)

    version = subprocess.run([arnold.kick_exe, "-av"], capture_output=True, text=True).stdout.strip()
    doc = {
        "arnold": version,
        "ocio_config": CONFIG,
        "target": TARGET,
        "sources": sources,
        "image": image,
        "usd": usd,
        "rebuild": rebuild,
        "direct_binding": direct,
    }

    # Emptied rather than removed: Windows cannot remove a directory that is
    # some process's working directory.
    os.makedirs(args.out, exist_ok=True)
    for old in os.listdir(args.out):
        os.remove(os.path.join(args.out, old))
    for case in image + usd:
        shutil.copy2(os.path.join(work, case["source"]), os.path.join(args.out, case["source"]))
        for t in case["tx"]:
            shutil.copy2(os.path.join(work, t["tx"]), os.path.join(args.out, t["tx"]))
    with open(os.path.join(args.out, "cases.json"), "w", newline="\n") as f:
        json.dump(doc, f, indent=1, sort_keys=False)
        f.write("\n")
    if crust_rows is not None:
        with open(os.path.join(args.out, "arnold_reads_crust.json"), "w", newline="\n") as f:
            json.dump({"arnold": version, "cases": crust_rows}, f, indent=1)
            f.write("\n")
    print(f"wrote {os.path.join(args.out, 'cases.json')}")
    if not args.keep:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
