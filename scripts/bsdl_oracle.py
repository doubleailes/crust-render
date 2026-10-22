#!/usr/bin/env python3
"""Regenerate the BSDL reference values crust's MaterialX BSDF leaves are
checked against.

MaterialX's OSL backend renders every BSDF node as a lobe of BSDL (OSL's
`src/libbsdl`), the library OSL's `testrender` shades with. This fetches OSL
and Imath at pinned commits, bakes BSDL's energy tables with its `genluts`,
compiles `scripts/bsdl_oracle/probe.cpp` against them, and runs it on a fixed
set of cases: one leaf with its parameters, a view direction and a few light
directions. For each case the probe reports what BSDL gives:

- the directional albedo over a fixed 32 × 32 cosine-weighted grid on each
  side of the surface (the same grid the replay builds);
- the layering filter `filter_o(wo)`: what the lobe lets through to a layer's
  base (0 for an opaque lobe);
- `albedo_impl()`: the lobe's albedo estimate, which testrender picks lobes by;
- the BSDF value times the cosine toward each light direction.

The cases and the answers go to `crates/crust-core/tests/data/bsdl_oracle.txt`.
`crates/crust-core/tests/bsdl_oracle.rs` replays them through crust's
MaterialX closure evaluator. Where crust is known to differ, it names a
deviation rule there.

CI never runs this: the fixture is committed, so `cargo test` needs neither a
C++ compiler nor OSL's sources. Rerun it after adding cases or moving the
pins; the diff of the fixture then shows exactly which reference values moved.
The OSL pin is the one `scripts/tables/bsdl_luts_to_rust.py` bakes crust's
tables at; move both together.

Requirements:
  git, cmake, and network access to github.com
  a C++17 compiler (`c++` on PATH, or set CXX)

Usage:
  scripts/bsdl_oracle.py [--out PATH] [--keep DIR]
"""
import argparse
import math
import os
import random
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, os.path.join(HERE, "tables"))
import bsdl_luts_to_rust as luts  # noqa: E402  (the pins and the bake)

DEFAULT_OUT = os.path.join(ROOT, "crates", "crust-core", "tests", "data", "bsdl_oracle.txt")

SEED = 20261022
RANDOM_CASES = 160
LIGHTS_PER_CASE = 8

GOLD = ((0.18299, 0.42108, 1.37340), (3.42420, 2.34590, 1.77040))
ALUMINIUM = ((1.34560, 0.96521, 0.61722), (7.47460, 6.39950, 5.30310))
COPPER = ((0.27105, 0.67693, 1.31640), (3.60920, 2.62480, 2.29210))


def fmt(x):
    """Four decimals: the replay parses the same string, so both sides
    read one f32."""
    s = f"{x:.4f}"
    return "0.0000" if s == "-0.0000" else s


def unit(v):
    n = math.sqrt(sum(c * c for c in v))
    return tuple(c / n for c in v)


def sphere(rng):
    z = rng.uniform(-1.0, 1.0)
    phi = rng.uniform(0.0, 2.0 * math.pi)
    r = math.sqrt(max(0.0, 1.0 - z * z))
    return (r * math.cos(phi), r * math.sin(phi), z)


def view_at(cos_theta, phi=0.3):
    s = math.sqrt(max(0.0, 1.0 - cos_theta * cos_theta))
    return (s * math.cos(phi), s * math.sin(phi), cos_theta)


def alpha(r):
    """A perceptual roughness as the GGX alpha pair MaterialX authors."""
    return (r * r, r * r)


# Every leaf with all of its inputs, so neither side falls back on a default
# the other does not share.
def dielectric(mode, ior=1.5, rough=(0.04, 0.04), tint=(1.0, 1.0, 1.0), back=0, film=None):
    p = {"leaf": "dielectric", "mode": mode, "ior": ior, "roughness": rough, "tint": tint, "back": back}
    if film:
        p["thinfilm_thickness"], p["thinfilm_ior"] = film
    return p


def conductor(metal=GOLD, rough=(0.04, 0.04), film=None):
    p = {"leaf": "conductor", "ior": metal[0], "extinction": metal[1], "roughness": rough, "back": 0}
    if film:
        p["thinfilm_thickness"], p["thinfilm_ior"] = film
    return p


def schlick(mode, color0=(0.04, 0.04, 0.04), color90=(1.0, 1.0, 1.0), exponent=5.0, rough=(0.04, 0.04), back=0):
    return {"leaf": "schlick", "mode": mode, "color0": color0, "color90": color90, "exponent": exponent,
            "roughness": rough, "back": back}


def sheen(sheen_mode, roughness=0.3, color=(1.0, 1.0, 1.0), back=0):
    return {"leaf": "sheen", "sheen_mode": sheen_mode, "roughness": roughness, "color": color, "back": back}


def oren_nayar(roughness=0.5, color=(0.8, 0.8, 0.8), ec=0):
    return {"leaf": "oren_nayar", "roughness": roughness, "color": color, "energy_compensation": ec, "back": 0}


def burley(roughness=0.5, color=(0.8, 0.8, 0.8)):
    return {"leaf": "burley", "roughness": roughness, "color": color, "back": 0}


def translucent(color=(0.8, 0.8, 0.8)):
    return {"leaf": "translucent", "color": color, "back": 0}


def corner_cases():
    """Every leaf and mode on its own: each scatter mode of each microfacet
    leaf on both faces, the roughness and IOR extremes, anisotropy both
    ways, tints, and every sheen and diffuse model."""
    cases = []
    views = (0.9, 0.5, 0.15)
    for mode in ("R", "T", "RT"):
        for back in (0, 1):
            for r in (0.0, 0.2, 0.5, 1.0):
                for c in views:
                    cases.append((f"dielectric-{mode}-b{back}-r{r}-c{c}", dielectric(mode, rough=alpha(r), back=back), c))
            for ior in (1.001, 1.33, 2.4, 5.0):
                cases.append((f"dielectric-{mode}-b{back}-ior{ior}", dielectric(mode, ior=ior, rough=alpha(0.4), back=back), 0.6))
        cases.append((f"dielectric-{mode}-aniso-x", dielectric(mode, rough=(0.36, 0.04)), 0.6))
        cases.append((f"dielectric-{mode}-aniso-y", dielectric(mode, rough=(0.04, 0.36)), 0.6))
        cases.append((f"dielectric-{mode}-tinted", dielectric(mode, rough=alpha(0.3), tint=(0.9, 0.5, 0.2)), 0.6))
    cases.append(("dielectric-R-film", dielectric("R", rough=alpha(0.2), film=(500.0, 1.3)), 0.6))
    cases.append(("dielectric-RT-film", dielectric("RT", rough=alpha(0.2), film=(500.0, 1.3)), 0.6))
    for name, metal in (("gold", GOLD), ("aluminium", ALUMINIUM), ("copper", COPPER)):
        for r in (0.0, 0.2, 0.5, 1.0):
            for c in views:
                cases.append((f"conductor-{name}-r{r}-c{c}", conductor(metal, rough=alpha(r)), c))
    cases.append(("conductor-aniso-x", conductor(rough=(0.36, 0.04)), 0.6))
    cases.append(("conductor-aniso-y", conductor(rough=(0.04, 0.36)), 0.6))
    cases.append(("conductor-film", conductor(rough=alpha(0.2), film=(500.0, 1.3)), 0.6))
    for mode in ("R", "RT"):
        for back in (0, 1):
            for r in (0.0, 0.3, 0.8):
                for c in views:
                    cases.append((f"schlick-{mode}-b{back}-r{r}-c{c}", schlick(mode, rough=alpha(r), back=back), c))
        cases.append((f"schlick-{mode}-coloured", schlick(mode, color0=(0.9, 0.6, 0.2), color90=(1.0, 0.9, 0.8),
                                                          rough=alpha(0.3)), 0.6))
        cases.append((f"schlick-{mode}-exponent", schlick(mode, color0=(0.2, 0.2, 0.2), exponent=3.0,
                                                          rough=alpha(0.3)), 0.6))
    for sm in ("conty_kulla", "zeltner"):
        for r in (0.0, 0.1, 0.3, 0.6, 1.0):
            for c in views:
                cases.append((f"sheen-{sm}-r{r}-c{c}", sheen(sm, roughness=r), c))
        cases.append((f"sheen-{sm}-tinted", sheen(sm, roughness=0.4, color=(0.9, 0.4, 0.1)), 0.6))
        cases.append((f"sheen-{sm}-bright", sheen(sm, roughness=0.4, color=(2.0, 1.5, 1.0)), 0.6))
        cases.append((f"sheen-{sm}-back", sheen(sm, roughness=0.4, back=1), 0.6))
    for ec in (0, 1):
        for r in (0.0, 0.5, 1.0):
            for c in views:
                cases.append((f"oren-nayar-ec{ec}-r{r}-c{c}", oren_nayar(roughness=r, ec=ec), c))
        cases.append((f"oren-nayar-ec{ec}-coloured", oren_nayar(roughness=0.7, color=(0.9, 0.5, 0.1), ec=ec), 0.6))
    for r in (0.0, 0.5, 1.0):
        for c in views:
            cases.append((f"burley-r{r}-c{c}", burley(roughness=r), c))
    for c in views:
        cases.append((f"translucent-c{c}", translucent(color=(0.9, 0.6, 0.3)), c))
    return cases


def colour(rng, lo=0.0, hi=1.0):
    return tuple(rng.uniform(lo, hi) for _ in range(3))


def random_rough(rng):
    if rng.random() < 0.2:
        return (rng.uniform(0.001, 1.0), rng.uniform(0.001, 1.0))
    a = rng.uniform(0.0, 1.0) ** 2
    return (max(a, 1e-4), max(a, 1e-4))


def random_leaf(rng):
    kind = rng.choice(["dielectric", "dielectric", "conductor", "schlick", "sheen", "oren_nayar", "burley",
                       "translucent"])
    back = 1 if rng.random() < 0.25 else 0
    if kind == "dielectric":
        tint = rng.choice([(1.0, 1.0, 1.0), colour(rng, 0.3)])
        return dielectric(rng.choice(["R", "T", "RT"]), ior=rng.uniform(1.05, 3.0), rough=random_rough(rng),
                          tint=tint, back=back)
    if kind == "conductor":
        n, k = rng.choice([GOLD, ALUMINIUM, COPPER])
        return conductor((n, k), rough=random_rough(rng))
    if kind == "schlick":
        return schlick(rng.choice(["R", "RT"]), color0=colour(rng, 0.0, 0.5), color90=colour(rng, 0.7),
                       exponent=rng.uniform(2.0, 8.0), rough=random_rough(rng), back=back)
    if kind == "sheen":
        return sheen(rng.choice(["conty_kulla", "zeltner"]), roughness=rng.uniform(0.0, 1.0), color=colour(rng),
                     back=back)
    if kind == "oren_nayar":
        return oren_nayar(roughness=rng.uniform(0.0, 1.0), color=colour(rng), ec=rng.choice([0, 1]))
    if kind == "burley":
        return burley(roughness=rng.uniform(0.0, 1.0), color=colour(rng))
    return translucent(color=colour(rng))


def line(case_id, inputs, view, lights):
    toks = [case_id]
    for k, v in inputs.items():
        if isinstance(v, tuple):
            toks.append(f"{k}={','.join(fmt(c) for c in v)}")
        elif isinstance(v, str):
            toks.append(f"{k}={v}")
        elif isinstance(v, int):
            toks.append(f"{k}={v}")
        else:
            toks.append(f"{k}={fmt(v)}")
    v = " ".join(fmt(c) for c in unit(view))
    ls = " ".join(" ".join(fmt(c) for c in unit(l)) for l in lights)
    return f"{' '.join(toks)} | {v} | {ls}"


def cases():
    # Separate streams, so adding a corner case leaves every random case as
    # it was.
    corner_rng = random.Random(SEED + 1)
    rng = random.Random(SEED)
    out = []
    for case_id, inputs, cos_v in corner_cases():
        out.append(line(case_id, inputs, view_at(cos_v), [sphere(corner_rng) for _ in range(LIGHTS_PER_CASE)]))
    for i in range(RANDOM_CASES):
        inputs = random_leaf(rng)
        view = view_at(rng.uniform(0.05, 1.0), rng.uniform(0.0, 2.0 * math.pi))
        out.append(line(f"random-{i}", inputs, view, [sphere(rng) for _ in range(LIGHTS_PER_CASE)]))
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=DEFAULT_OUT)
    ap.add_argument("--keep", help="build in this directory and keep it")
    args = ap.parse_args()

    work = args.keep or tempfile.mkdtemp(prefix="bsdl_oracle.")
    try:
        # The probe includes BSDL's baked tables as `<BSDL/SPI/…>` and
        # `<BSDL/MTX/…>`: bake them where that resolves.
        baked = luts.bake(work)
        gen = os.path.join(work, "gen")
        if os.path.isdir(gen):
            shutil.rmtree(gen)
        os.makedirs(gen)
        shutil.copytree(baked, os.path.join(gen, "BSDL"))
        prefix = os.path.join(work, "imath-install")
        probe = os.path.join(work, "probe")
        cxx = os.environ.get("CXX", "c++")
        subprocess.run(
            [cxx, "-std=c++17", "-O2", "-ffp-contract=off", f"-I{gen}",
             f"-I{os.path.join(work, 'osl', 'src', 'libbsdl', 'include')}",
             f"-I{prefix}/include/Imath", f"-I{prefix}/include",
             os.path.join(HERE, "bsdl_oracle", "probe.cpp"), "-o", probe],
            check=True,
        )

        inputs = cases()
        result = subprocess.run([probe], input="\n".join(inputs) + "\n", capture_output=True, text=True, check=True)
        answers = result.stdout.strip().split("\n")
        if len(answers) != len(inputs):
            sys.exit(f"probe answered {len(answers)} of {len(inputs)} cases")

        with open(args.out, "w") as f:
            f.write("# BSDL reference values. Generated by scripts/bsdl_oracle.py; do not edit.\n")
            f.write(f"# AcademySoftwareFoundation/OpenShadingLanguage {luts.OSL_COMMIT} (src/libbsdl)\n")
            f.write(f"# AcademySoftwareFoundation/Imath {luts.IMATH_COMMIT}\n")
            f.write(f"# {len(inputs)} cases, {LIGHTS_PER_CASE} light directions each.\n")
            f.write("# <id> <inputs> | <view> | <lights> || <albedo> | <filter> | <albedo_impl> | <values>\n")
            for case, answer in zip(inputs, answers):
                case_id, rest = answer.split(" |", 1)
                if not case.startswith(case_id + " "):
                    sys.exit(f"probe answered {case_id} out of order")
                f.write(f"{case} || {rest.strip()}\n")
        print(f"wrote {len(inputs)} cases to {os.path.relpath(args.out, ROOT)}")
    finally:
        if not args.keep:
            shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
