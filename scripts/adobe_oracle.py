#!/usr/bin/env python3
"""Regenerate the Adobe OpenPBR reference values crust's native `OpenPBR` is
checked against.

This fetches Adobe's header-only OpenPBR BSDF reference
(github.com/adobe/openpbr-bsdf) and GLM at pinned commits, compiles
`scripts/adobe_oracle/probe.cpp` against them, and runs it on a fixed set of
cases: material parameters, a view direction and a few light directions.
For each case the probe reports what the reference gives:

- the emission toward the view;
- the directional albedo over a fixed 32 × 32 cosine-weighted grid on the
  view's side (the same grid the replay builds, so the two compute one
  quantity);
- the BSDF value times the cosine toward each light direction.

The cases and the answers go to `crates/crust-core/tests/data/adobe_oracle.txt`.
`crates/crust-core/tests/adobe_oracle.rs` replays them through crust's
`OpenPBR`. Where crust is known to differ, it names a deviation rule there.

CI never runs this: the fixture is committed, so `cargo test` needs neither a
C++ compiler nor Adobe's sources. Rerun it after adding cases or moving the
pins. The diff of the fixture then shows exactly which reference values moved.

Requirements:
  git, and network access to github.com
  a C++17 compiler (`c++` on PATH, or set CXX)

Usage:
  scripts/adobe_oracle.py [--out PATH] [--keep DIR]
"""
import argparse
import math
import os
import random
import shutil
import subprocess
import sys
import tempfile

ADOBE_URL = "https://github.com/adobe/openpbr-bsdf.git"
ADOBE_COMMIT = "c91aad1d1ce1693e803f039d7c92c2965c4eb013"
GLM_URL = "https://github.com/g-truc/glm.git"
GLM_COMMIT = "0af55ccecd98d4e5a8d1fad7de25ba429d60e863"  # tag 1.0.1

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DEFAULT_OUT = os.path.join(ROOT, "crates", "crust-core", "tests", "data", "adobe_oracle.txt")

SEED = 20261008
RANDOM_CASES = 160
LIGHTS_PER_CASE = 8


def fetch(url, commit, dest):
    os.makedirs(dest)
    run = lambda *a: subprocess.run(["git", *a], cwd=dest, check=True, capture_output=True)
    run("init", "-q")
    run("remote", "add", "origin", url)
    run("fetch", "-q", "--depth", "1", "origin", commit)
    run("checkout", "-q", "FETCH_HEAD")


def verify(dest, commit):
    """Refuse a source tree that is not exactly the pinned commit: the fixture
    header names the pins, so evaluating anything else would record a
    reference that was never run. A `--keep` directory is reused only if it
    passes."""
    git = lambda *a: subprocess.run(["git", *a], cwd=dest, check=True, capture_output=True, text=True).stdout.strip()
    head = git("rev-parse", "HEAD")
    dirty = git("status", "--porcelain")
    if head != commit or dirty:
        state = "with local changes" if head == commit else f"at {head}"
        sys.exit(f"{dest} is {state}, not the pinned {commit}; remove it or use another --keep directory")


def fmt(x):
    """Four decimals: the replay parses the same string, so both sides
    read one f32."""
    s = f"{x:.4f}"
    return "0.0000" if s == "-0.0000" else s


def colour(rng, lo=0.0, hi=1.0):
    return tuple(rng.uniform(lo, hi) for _ in range(3))


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


def random_inputs(rng):
    """Every input crust maps, drawn so each layer is present in some cases
    and absent (exactly 0) in others."""
    p = {}

    def maybe(prob, f):
        return f() if rng.random() < prob else 0.0

    p["base_weight"] = rng.choice([1.0, rng.uniform(0.0, 1.0)])
    p["base_color"] = colour(rng)
    p["base_diffuse_roughness"] = maybe(0.5, lambda: rng.uniform(0.0, 1.0))
    p["base_metalness"] = rng.choice([0.0, 0.0, 1.0, rng.uniform(0.0, 1.0)])
    p["specular_weight"] = rng.choice([1.0, 0.0, rng.uniform(0.0, 1.0)])
    p["specular_color"] = rng.choice([(1.0, 1.0, 1.0), colour(rng, 0.5)])
    p["specular_roughness"] = rng.uniform(0.0, 1.0)
    p["specular_ior"] = rng.uniform(1.1, 2.5)
    p["specular_roughness_anisotropy"] = maybe(0.15, lambda: rng.uniform(0.0, 1.0))
    p["transmission_weight"] = maybe(0.3, lambda: rng.choice([1.0, rng.uniform(0.0, 1.0)]))
    p["transmission_color"] = colour(rng, 0.3)
    p["transmission_depth"] = maybe(0.3, lambda: rng.uniform(0.1, 2.0))
    p["subsurface_weight"] = maybe(0.2, lambda: rng.uniform(0.0, 1.0))
    p["subsurface_color"] = colour(rng)
    p["fuzz_weight"] = maybe(0.4, lambda: rng.uniform(0.0, 1.0))
    p["fuzz_color"] = colour(rng)
    p["fuzz_roughness"] = rng.uniform(0.0, 1.0)
    p["coat_weight"] = maybe(0.35, lambda: rng.uniform(0.0, 1.0))
    p["coat_color"] = colour(rng, 0.4)
    p["coat_roughness"] = rng.uniform(0.0, 1.0)
    p["coat_roughness_anisotropy"] = maybe(0.1, lambda: rng.uniform(0.0, 1.0))
    p["coat_ior"] = rng.uniform(1.2, 2.0)
    p["coat_darkening"] = rng.choice([1.0, rng.uniform(0.0, 1.0)])
    p["thin_film_weight"] = maybe(0.15, lambda: rng.uniform(0.0, 1.0))
    p["thin_film_thickness"] = rng.uniform(0.1, 1.0)
    p["thin_film_ior"] = rng.uniform(1.2, 2.0)
    p["emission_luminance"] = maybe(0.2, lambda: rng.uniform(0.0, 4.0))
    p["emission_color"] = colour(rng)
    p["geometry_thin_walled"] = 1 if rng.random() < 0.15 else 0
    return p


def corner_cases():
    """Hand-picked cases: each layer on its own, at its extremes. The fuzz
    ones are the ones `adobe_oracle.rs` requires to match within 1e-4
    (`fuzz_cases_match_to_float_precision`)."""
    black = {"base_weight": 0.0, "specular_weight": 0.0}
    cases = []
    for r in (0.0, 0.05, 0.1, 0.3, 0.5, 0.8, 1.0):
        for c in (0.05, 0.25, 0.5, 1.0):
            cases.append((f"fuzz-r{r}-c{c}", {**black, "fuzz_weight": 1.0, "fuzz_roughness": r}, view_at(c)))
    for w in (0.25, 1.0):
        cases.append((
            f"fuzz-tinted-w{w}",
            {**black, "fuzz_weight": w, "fuzz_color": (0.9, 0.4, 0.1), "fuzz_roughness": 0.6},
            view_at(0.4),
        ))
    for c in (0.1, 0.5, 1.0):
        cases.append((
            f"fuzz-over-diffuse-c{c}",
            {"specular_weight": 0.0, "base_color": (0.7, 0.5, 0.3), "fuzz_weight": 0.8, "fuzz_roughness": 0.4},
            view_at(c),
        ))
        cases.append((
            f"fuzz-emission-c{c}",
            {**black, "fuzz_weight": 1.0, "fuzz_roughness": 0.7, "emission_luminance": 2.0},
            view_at(c),
        ))
        cases.append((
            f"fuzz-over-coat-c{c}",
            {**black, "coat_weight": 1.0, "coat_roughness": 0.2, "fuzz_weight": 0.7, "fuzz_roughness": 0.9},
            view_at(c),
        ))
    cases.append(("fuzz-inside", {**black, "fuzz_weight": 1.0, "fuzz_roughness": 0.5}, view_at(-0.6)))
    # One case per known gap with nothing else present, so each deviation
    # rule in `adobe_oracle.rs` has a case it alone explains.
    cases.append(("black", black, view_at(0.6)))
    cases.append(("diffuse", {"specular_weight": 0.0}, view_at(0.6)))
    cases.append(("diffuse-rough", {"specular_weight": 0.0, "base_diffuse_roughness": 1.0}, view_at(0.6)))
    cases.append(("dielectric", {}, view_at(0.6)))
    for r in (0.0, 0.3, 0.8):
        cases.append((f"dielectric-only-r{r}", {"base_weight": 0.0, "specular_roughness": r}, view_at(0.6)))
    for r in (0.1, 0.6):
        cases.append((f"metal-r{r}", {"base_metalness": 1.0, "specular_roughness": r}, view_at(0.6)))
    for r in (0.0, 0.3, 0.8):
        cases.append((f"coat-only-r{r}", {**black, "coat_weight": 1.0, "coat_roughness": r}, view_at(0.6)))
    # The coat under the fuzz-over-coat cases, without the fuzz: their
    # difference from Adobe is the coat's unless these match.
    for c in (0.1, 0.5, 1.0):
        cases.append((f"coat-only-c{c}", {**black, "coat_weight": 1.0, "coat_roughness": 0.2}, view_at(c)))
    cases.append(("coat", {"coat_weight": 1.0, "coat_roughness": 0.3}, view_at(0.6)))
    cases.append(("emission", {**black, "emission_luminance": 3.0}, view_at(0.5)))
    cases.append(("emission-inside", {**black, "emission_luminance": 3.0}, view_at(-0.5)))
    cases.append(("emission-coat", {"emission_luminance": 3.0, "coat_weight": 1.0, "coat_color": (0.8, 0.6, 0.4)}, view_at(0.5)))
    cases.append(("subsurface-only", {"specular_weight": 0.0, "subsurface_weight": 1.0}, view_at(0.6)))
    cases.append(("transmission-only", {"specular_weight": 0.0, "transmission_weight": 1.0}, view_at(0.6)))
    cases.append(("glass", {"transmission_weight": 1.0, "specular_roughness": 0.3}, view_at(0.6)))
    cases.append(("thin-film", {"base_weight": 0.0, "thin_film_weight": 1.0}, view_at(0.6)))
    cases.append(("anisotropic", {"base_weight": 0.0, "specular_roughness_anisotropy": 0.8}, view_at(0.6)))
    cases.append(("thin-walled", {**black, "geometry_thin_walled": 1}, view_at(0.6)))
    cases.append(("thin-walled-diffuse", {"specular_weight": 0.0, "geometry_thin_walled": 1}, view_at(0.6)))
    cases.append((
        "thin-film-without-specular",
        {"specular_weight": 0.0, "base_color": (0.86, 0.72, 0.04), "thin_film_weight": 0.34, "thin_film_thickness": 0.13},
        view_at(0.6),
    ))
    return cases


def line(case_id, inputs, view, lights):
    toks = [case_id]
    for k, v in inputs.items():
        if isinstance(v, tuple):
            toks.append(f"{k}={','.join(fmt(c) for c in v)}")
        elif k == "geometry_thin_walled":
            toks.append(f"{k}={int(v)}")
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
    for case_id, inputs, view in corner_cases():
        out.append(line(case_id, inputs, view, [sphere(corner_rng) for _ in range(LIGHTS_PER_CASE)]))
    for i in range(RANDOM_CASES):
        inputs = random_inputs(rng)
        cos_v = rng.uniform(0.05, 1.0) * (-1.0 if rng.random() < 0.1 else 1.0)
        view = view_at(cos_v, rng.uniform(0.0, 2.0 * math.pi))
        out.append(line(f"random-{i}", inputs, view, [sphere(rng) for _ in range(LIGHTS_PER_CASE)]))
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=DEFAULT_OUT)
    ap.add_argument("--keep", help="build in this directory and keep it")
    args = ap.parse_args()

    work = args.keep or tempfile.mkdtemp(prefix="adobe_oracle.")
    try:
        adobe = os.path.join(work, "openpbr-bsdf")
        glm = os.path.join(work, "glm")
        if not os.path.isdir(adobe):
            fetch(ADOBE_URL, ADOBE_COMMIT, adobe)
        if not os.path.isdir(glm):
            fetch(GLM_URL, GLM_COMMIT, glm)
        verify(adobe, ADOBE_COMMIT)
        verify(glm, GLM_COMMIT)
        probe = os.path.join(work, "probe")
        cxx = os.environ.get("CXX", "c++")
        subprocess.run(
            [cxx, "-std=c++17", "-O2", "-ffp-contract=off", f"-I{glm}", f"-I{adobe}",
             os.path.join(HERE, "adobe_oracle", "probe.cpp"), "-o", probe],
            check=True,
        )

        inputs = cases()
        result = subprocess.run([probe], input="\n".join(inputs) + "\n", capture_output=True, text=True, check=True)
        answers = result.stdout.strip().split("\n")
        if len(answers) != len(inputs):
            sys.exit(f"probe answered {len(answers)} of {len(inputs)} cases")

        with open(args.out, "w") as f:
            f.write("# Adobe OpenPBR reference values. Generated by scripts/adobe_oracle.py; do not edit.\n")
            f.write(f"# adobe/openpbr-bsdf {ADOBE_COMMIT}\n")
            f.write(f"# g-truc/glm {GLM_COMMIT}\n")
            f.write(f"# {len(inputs)} cases, {LIGHTS_PER_CASE} light directions each.\n")
            f.write("# <id> <inputs> | <view> | <lights> || <emission> | <albedo> | <values>\n")
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
