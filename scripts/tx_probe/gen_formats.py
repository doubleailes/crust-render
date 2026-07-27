"""Which container does OIIO pick for a .tx, and does bit depth ever change it?

Backs docs/oiio_tx_texture_evaluation.md section 1.4/1.5. Answer: the container comes
from `maketx:fileformatname` or the output extension only -- never from the data type.
`half` is silently promoted to `float` when writing TIFF unless `tiff:half` is set.
"""
import OpenImageIO as oiio
import numpy as np

W = H = 256
px = np.random.default_rng(3).random((H, W, 3), dtype=np.float32)


def src(fmt):
    b = oiio.ImageBuf(oiio.ImageSpec(W, H, 3, fmt))
    b.set_pixels(oiio.ROI(0, W, 0, H, 0, 1, 0, 3), px)
    return b


src("half").write("src_half.exr")

cases = []


def make(label, out, buf, **attrs):
    cfg = oiio.ImageSpec()
    fmt = attrs.pop("format", None)
    if fmt:
        cfg.set_format(oiio.TypeDesc(fmt))
    for k, v in attrs.items():
        cfg.attribute(k, v)
    ok = oiio.ImageBufAlgo.make_texture(oiio.MakeTxTexture, buf, out, cfg)
    cases.append((label, out, ok))


make("float source, defaults",     "fmt_f32.tx",      src("float"), format="float")
make("half source, defaults",      "fmt_from_half.tx", src("half"))
make("half explicitly requested",  "fmt_req_half.tx", src("half"), format="half")
make("half EXR read from disk",    "fmt_fromfile.tx", oiio.ImageBuf("src_half.exr"))
make("half + tiff:half=1",         "fmt_half_tiff.tx", src("half"), format="half", **{"tiff:half": 1})
make("fileformatname=openexr",     "fmt_exr.tx",      src("half"), format="half",
     **{"maketx:fileformatname": "openexr"})
make("half, .exr extension",       "fmt_tex.exr",     src("half"), format="half")

MAGIC = {
    b"II*\x00": "TIFF (classic)",
    b"MM\x00*": "TIFF (classic, BE)",
    b"II+\x00": "BigTIFF",
    b"MM\x00+": "BigTIFF (BE)",
    b"v/1\x01": "OpenEXR",
}

print(f"{'case':32} {'file':18} {'magic':20} {'reader':9} {'fmt':6} tile     levels")
for label, out, ok in cases:
    assert ok, f"{out}: {oiio.geterror()}"
    magic = open(out, "rb").read(4)
    i = oiio.ImageInput.open(out)
    s = i.spec()
    n = 0
    while i.seek_subimage(0, n + 1):
        n += 1
    print(f"{label:32} {out:18} {MAGIC.get(magic, magic.hex()):20} "
          f"{i.format_name():9} {str(s.format):6} {s.tile_width}x{s.tile_height:<4} {n + 1}")
    i.close()
