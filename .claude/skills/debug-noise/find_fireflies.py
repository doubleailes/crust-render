#!/usr/bin/env python3
"""List the firefly pixels of a crust render, and the light-paths region around each.

A firefly is an isolated outlier: a pixel much brighter than the median of its
3x3 neighbourhood. This reads the tone-mapped PNG crust writes beside its EXR
(no EXR reader is assumed) and prints, for the strongest outliers, the
image-space pixel and a `--region` of at most 100 pixels around it for
`crust light-paths`.

The PNG is display-referred, clamped to [0, 1] and 8-bit: a firefly against a
darker surround still stands out, but one inside an area that already clips
does not. Such clipped areas are listed separately, as windows to inspect
directly: the PNG cannot rank what is inside them.

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
    # A ranking of display values only, never compared with crust's own luminance.
    luma = img @ np.array([0.2126, 0.7152, 0.0722])
    # A channel at the 8-bit ceiling has lost its linear value.
    clipped = img.max(axis=2) >= 254.5 / 255.0
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

    def region(gx: int, gy: int) -> str:
        x0, y0 = max(gx - half, 0), max(gy - half, 0)
        return f"{x0},{y0},{x0 + args.window},{y0 + args.window}"

    if picked:
        print(f"{'pixel':>12}  {'luma':>5}  {'nbr median':>10}  light-paths region")
        for x, y in picked:
            gx, gy = x + ox, y + oy
            print(f"{gx:>5},{gy:<6}  {luma[y, x]:5.3f}  {median[y, x]:10.3f}  --region {region(gx, gy)}")
    else:
        print("no isolated outliers against an unclipped surround")

    # Clipped pixels whose neighbours clip too: a firefly there is invisible to
    # the ranking above. One window per cluster, largest clusters first.
    nbr_clipped = np.stack(
        [np.pad(clipped, 1)[dy : dy + h, dx : dx + w] for dy in range(3) for dx in range(3) if (dy, dx) != (1, 1)]
    ).sum(axis=0)
    hidden = clipped & (nbr_clipped >= 4)
    ys, xs = np.nonzero(hidden)
    if len(xs):
        windows: list[tuple[int, int, int]] = []
        for x, y in zip(xs.tolist(), ys.tolist()):
            for i, (cx, cy, n) in enumerate(windows):
                if abs(x - cx) <= half and abs(y - cy) <= half:
                    windows[i] = (cx, cy, n + 1)
                    break
            else:
                windows.append((x, y, 1))
        windows.sort(key=lambda t: -t[2])
        print(f"\n{len(xs)} pixels clip with clipped neighbours: the PNG cannot rank fireflies there.")
        print("Inspect these windows directly (largest first):")
        for cx, cy, n in windows[: args.top]:
            print(f"  {n:>5} clipped px  --region {region(cx + ox, cy + oy)}")
    elif not picked:
        print("the noise here is not fireflies: read the diagnostic's noise breakdown instead")
    return 0


if __name__ == "__main__":
    sys.exit(main())
