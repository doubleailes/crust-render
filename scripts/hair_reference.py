#!/usr/bin/env python3
"""Regenerate the reference values for MaterialX's three hair helper nodes.

`chiang_hair_roughness`, `chiang_hair_absorption_from_color` and
`deon_hair_absorption_from_melanin` cannot be pinned by `scripts/osl_oracle.py`:
in MaterialX 1.39 their genosl implementations are placeholders (`vector(1.0)`
and zeros), so OSL would hand back nonsense. The only complete implementation
in the library is genglsl's `mx_chiang_hair_bsdf.glsl`, and this script is a
float64 transcription of its three helper functions. Each case is written to
`crates/crust-mtlx/tests/data/hair_helpers.txt` in `osl_oracle.txt`'s format,
and `crates/crust-mtlx/tests/hair_helpers.rs` replays it through `Compiler` +
`Program::eval`.

Needs nothing but Python 3: the inputs are drawn from a fixed seed and are
multiples of 1/64, so they are exact in `f32` and the output is reproducible
byte for byte. Rerun it after a MaterialX upgrade changes the genglsl source.

Usage:
  scripts/hair_reference.py [--cases N] [--out PATH]
"""

import argparse
import math
import os
import random

MATERIALX = "MaterialX 1.39.5 genglsl (mx_chiang_hair_bsdf.glsl)"


def clamp(x, lo, hi):
    return min(max(x, lo), hi)


# --- the genglsl functions, transcribed -------------------------------------


def chiang_hair_roughness(longitudinal, azimuthal, scale_TT, scale_TRT):
    lr = clamp(longitudinal, 0.001, 1.0)
    ar = clamp(azimuthal, 0.001, 1.0)
    # longitudinal variance
    v = 0.726 * lr + 0.812 * lr * lr + 3.7 * lr**20
    v = v * v
    s = 0.265 * ar + 1.194 * ar * ar + 5.372 * ar**22
    return {
        "roughness_R": [v, s],
        "roughness_TT": [v * scale_TT * scale_TT, s],
        "roughness_TRT": [v * scale_TRT * scale_TRT, s],
    }


def chiang_hair_absorption_from_color(color, betaN):
    b2 = betaN * betaN
    b4 = b2 * b2
    b_fac = (
        5.969
        - (0.215 * betaN)
        + (2.532 * b2)
        - (10.73 * b2 * betaN)
        + (5.574 * b4)
        + (0.245 * b4 * betaN)
    )
    out = []
    for c in color:
        sigma = math.log(min(max(c, 0.001), 1.0)) / b_fac
        out.append(sigma * sigma)
    return out


def deon_hair_absorption_from_melanin(concentration, redness, eumelanin, pheomelanin):
    melanin = -math.log(max(1.0 - concentration, 0.0001))
    eu = melanin * (1.0 - redness)
    pheo = melanin * redness
    return [
        # `+ 0.0` turns the -0 a zero melanin gives into +0, as `max` in
        # GLSL may not.
        max(eu * -math.log(e) + pheo * -math.log(p), 0.0) + 0.0
        for e, p in zip(eumelanin, pheomelanin)
    ]


# --- the cases ---------------------------------------------------------------

# (nodedef, category, output type, inputs as (name, type, (lo, hi)))
ROUGHNESS = (
    "ND_chiang_hair_roughness",
    "chiang_hair_roughness",
    [
        ("longitudinal", "float", (-0.25, 1.25)),
        ("azimuthal", "float", (-0.25, 1.25)),
        ("scale_TT", "float", (0.0, 3.0)),
        ("scale_TRT", "float", (0.0, 3.0)),
    ],
)
FROM_COLOR = (
    "ND_chiang_hair_absorption_from_color",
    "chiang_hair_absorption_from_color",
    [
        ("color", "color3", (-0.25, 1.25)),
        ("azimuthal_roughness", "float", (0.0, 1.0)),
    ],
)
# The melanin colours go through `-log`, which the reference leaves at
# infinity for a channel <= 0; crust's `ln` guards that, so the cases stay
# strictly positive there, as every authored colour is.
FROM_MELANIN = (
    "ND_deon_hair_absorption_from_melanin",
    "deon_hair_absorption_from_melanin",
    [
        ("melanin_concentration", "float", (-0.25, 1.25)),
        ("melanin_redness", "float", (-0.25, 1.25)),
        ("eumelanin_color", "color3", (0.0625, 1.0)),
        ("pheomelanin_color", "color3", (0.0625, 1.0)),
    ],
)

DEFAULTS = {
    "longitudinal": [0.1],
    "azimuthal": [0.2],
    "scale_TT": [0.5],
    "scale_TRT": [2.0],
    "color": [1.0, 1.0, 1.0],
    "azimuthal_roughness": [0.2],
    "melanin_concentration": [0.25],
    "melanin_redness": [0.5],
    "eumelanin_color": [0.657704, 0.498077, 0.254107],
    "pheomelanin_color": [0.829444, 0.67032, 0.349938],
}

# Hand-picked cases: the clamps' edges, and the values the spec's scenarios
# name.
EDGES = {
    "ND_chiang_hair_roughness": [
        {"longitudinal": [0.0], "azimuthal": [0.0]},
        {"longitudinal": [1.0], "azimuthal": [1.0]},
        {"longitudinal": [1.5], "azimuthal": [-1.0]},
        {"longitudinal": [0.3], "azimuthal": [0.5]},
    ],
    "ND_chiang_hair_absorption_from_color": [
        {"color": [1.0, 1.0, 1.0]},
        {"color": [0.0, 0.0, 0.0]},
        {"color": [2.0, -1.0, 0.5]},
        {"color": [0.6, 0.4, 0.2], "azimuthal_roughness": [0.3]},
    ],
    "ND_deon_hair_absorption_from_melanin": [
        {"melanin_concentration": [0.0]},
        {"melanin_concentration": [0.9]},
        {"melanin_concentration": [1.0]},
        {"melanin_concentration": [0.5], "melanin_redness": [1.0]},
    ],
}

LANES = {"float": 1, "color3": 3, "vector2": 2, "vector3": 3}


def draw(rng, lo, hi):
    # Multiples of 1/64: exact in f32, so both sides read the same input.
    return round(rng.uniform(lo, hi) * 64.0) / 64.0


def evaluate(nodedef, values):
    v = {**DEFAULTS, **values}
    if nodedef == "ND_chiang_hair_roughness":
        return chiang_hair_roughness(
            v["longitudinal"][0], v["azimuthal"][0], v["scale_TT"][0], v["scale_TRT"][0]
        )
    if nodedef == "ND_chiang_hair_absorption_from_color":
        return {
            None: chiang_hair_absorption_from_color(v["color"], v["azimuthal_roughness"][0])
        }
    return {
        None: deon_hair_absorption_from_melanin(
            v["melanin_concentration"][0],
            v["melanin_redness"][0],
            v["eumelanin_color"],
            v["pheomelanin_color"],
        )
    }


def fmt(x):
    return "%.9g" % x


def rows(signature, rng, n):
    nodedef, category, inputs = signature
    cases = [{}] + EDGES[nodedef]
    for _ in range(n):
        cases.append(
            {
                name: [draw(rng, lo, hi) for _ in range(LANES[ty])]
                for name, ty, (lo, hi) in inputs
            }
        )
    types = {name: ty for name, ty, _ in inputs}
    out = []
    for case in cases:
        spec = (
            ";".join(
                "%s:%s=%s" % (name, types[name], ",".join(fmt(x) for x in lanes))
                for name, lanes in case.items()
            )
            or "-"
        )
        for output, lanes in evaluate(nodedef, case).items():
            ty = "vector2" if output else "vector3"
            out.append(
                "\t".join(
                    [nodedef, category, ty, output or "-", spec, " ".join(fmt(x) for x in lanes)]
                )
            )
    return out


def main():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    p = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    p.add_argument("--cases", type=int, default=16, help="random cases per signature")
    p.add_argument(
        "--out",
        default=os.path.join(root, "crates/crust-mtlx/tests/data/hair_helpers.txt"),
    )
    args = p.parse_args()

    rng = random.Random(1939)
    lines = [
        "# Generated by scripts/hair_reference.py; do not edit by hand.",
        "# %s, float64, %d random cases per signature, 3 signatures."
        % (MATERIALX, args.cases),
        "# nodedef\tcategory\toutput type\toutput\tinputs (name:type=lanes;...)\texpected lanes",
    ]
    for signature in (ROUGHNESS, FROM_COLOR, FROM_MELANIN):
        lines += rows(signature, rng, args.cases)
    with open(args.out, "w") as f:
        f.write("\n".join(lines) + "\n")
    print("wrote %d cases to %s" % (len(lines) - 3, args.out))


if __name__ == "__main__":
    main()
