import OpenImageIO as oiio
import numpy as np

W = H = 512
# Deterministic, high-frequency pattern so tile boundaries are obvious.
y, x = np.mgrid[0:H, 0:W]
r = ((x // 8) % 2 ^ (y // 8) % 2).astype(np.float32)
g = (x / (W - 1)).astype(np.float32)
b = (y / (H - 1)).astype(np.float32)
px = np.stack([r, g, b], axis=-1).astype(np.float32)

src = oiio.ImageBuf(oiio.ImageSpec(W, H, 3, "float"))
src.set_pixels(oiio.ROI(0, W, 0, H, 0, 1, 0, 3), px)
src.write("src.exr")

for name, cfg in [
    ("checker_u8.tx", {"format": "uint8"}),
    ("checker_f32.tx", {"format": "float"}),
    ("checker_u16_lzw.tx", {"format": "uint16", "compression": "lzw"}),
]:
    config = oiio.ImageSpec()
    config.attribute("maketx:filtername", "lanczos3")
    config.attribute("maketx:verbose", 0)
    if "compression" in cfg:
        config.attribute("compression", cfg["compression"])
    config.set_format(oiio.TypeDesc(cfg["format"]))
    ok = oiio.ImageBufAlgo.make_texture(oiio.MakeTxTexture, src, name, config)
    print(name, "written:", ok, oiio.geterror() or "")

# Report the structure OIIO itself sees.
for name in ("checker_u8.tx", "checker_f32.tx", "checker_u16_lzw.tx"):
    inp = oiio.ImageInput.open(name)
    print(f"\n=== {name} ===")
    lvl = 0
    while True:
        s = inp.spec()
        print(f"  mip {lvl}: {s.width}x{s.height} tile={s.tile_width}x{s.tile_height} "
              f"fmt={s.format} nchan={s.nchannels} compression={s.get_string_attribute('compression')} "
              f"planarconfig={s.get_string_attribute('tiff:planarconfig')}")
        if lvl == 0:
            for a in s.extra_attribs:
                print(f"      attr {a.name} = {str(a.value)[:70]}")
        if not inp.seek_subimage(0, lvl + 1):
            break
        lvl += 1
    print(f"  total mip levels: {lvl + 1}")
    inp.close()
