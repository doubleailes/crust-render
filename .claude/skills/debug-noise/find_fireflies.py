#!/usr/bin/env python3
"""List the firefly pixels of a crust render, and the light-paths region around each.

A firefly is an isolated outlier: a pixel much brighter than the median of its
3x3 neighbourhood. This reads the tone-mapped PNG crust writes beside its EXR
(tone mapping is monotonic, so the ranking holds even where it saturates) and
prints, for the strongest outliers, the image-space pixel and a `--region` of
at most 100 pixels around it for `crust light-paths`.

    find_fireflies.py crop.png --offset X0,Y0 [--top 5] [--window 10]

`--offset` is the region's top-left corner when the PNG came from a render
with `--region` (that PNG holds only the region); omit it for a full frame.
"""

import argparse
import sys

import numpy as np
from PIL import Image


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("png")
    ap.add_argument("--offset", default="0,0", help="X0,Y0 of the rendered region")
    ap.add_argument("--top", type=int, default=5, help="how many fireflies to list")
    ap.add_argument("--window", type=int, default=10, help="light-paths window side, <= 10")
    args = ap.parse_args()

    if not 1 <= args.window <= 10:
        sys.exit("--window must be 1..10: light-paths inspects at most 100 pixels")
    ox, oy = (int(v) for v in args.offset.split(","))

    img = np.asarray(Image.open(args.png).convert("RGB"), dtype=np.float64) / 255.0
    # Rec.709 weights, as crust's luminance; on display values this is a ranking only.
    luma = img @ np.array([0.2126, 0.7152, 0.0722])
    h, w = luma.shape

    padded = np.pad(luma, 1, mode="edge")
    stack = np.stack(
        [padded[dy : dy + h, dx : dx + w] for dy in range(3) for dx in range(3) if (dy, dx) != (1, 1)]
    )
    median = np.median(stack, axis=0)
    excess = luma - median  # how far above its neighbours a pixel stands
    ratio = (luma + 1e-3) / (median + 1e-3)

    order = np.argsort(excess, axis=None)[::-1]
    picked: list[tuple[int, int]] = []
    half = args.window // 2
    for flat in order:
        y, x = divmod(int(flat), w)
        if excess[y, x] <= 0.05 or ratio[y, x] < 1.5:
            break
        # One window per firefly: skip pixels inside a window already listed.
        if any(abs(x - px) <= half and abs(y - py) <= half for px, py in picked):
            continue
        picked.append((x, y))
        if len(picked) == args.top:
            break

    if not picked:
        print("no isolated outliers: the noise here is not fireflies (read the noise breakdown instead)")
        return 0

    print(f"{'pixel':>12}  {'luma':>5}  {'nbr median':>10}  light-paths region")
    for x, y in picked:
        gx, gy = x + ox, y + oy
        x0, y0 = gx - half, gy - half
        x0, y0 = max(x0, 0), max(y0, 0)
        region = f"{x0},{y0},{x0 + args.window},{y0 + args.window}"
        print(f"{gx:>5},{gy:<6}  {luma[y, x]:5.3f}  {median[y, x]:10.3f}  --region {region}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
