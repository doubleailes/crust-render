#!/usr/bin/env python3
"""Regenerate the OpenUSD GeomUtil reference meshes crust's implicit-shape
tessellation is checked against.

crust tessellates `Cube`, `Plane`, `Cylinder`, `Cylinder_1`, `Cone`, `Capsule`
and `Capsule_1` prims into the meshes USD's own `pxr/imaging/geomUtil`
generators produce, the ones Storm draws in usdview
(`crates/crust-core/src/scene/usd_import/implicit.rs`). This script checks that
transcription against the real thing:

- it downloads micromamba from conda.anaconda.org and creates a throwaway
  environment holding conda-forge's `openusd`, pinned to a version and build;
- it compiles `scripts/geomutil_oracle/probe.cpp` against that environment's
  headers and libraries. The probe calls the generators' full signatures
  (`radiusBottom`, `radiusTop`, a frame matrix) with the frame from
  `UsdImagingGprimAdapter::GetImplicitBasis(axis)`;
- it runs the probe on a fixed list of cases and writes the topology, points
  and normals of each to `crates/crust-core/tests/data/geomutil_oracle.txt`.

`crates/crust-core/tests/geomutil_oracle.rs` replays every case through crust's
generators.

The Python bindings are not enough for this: conda-forge's wrap only the
original signatures (no `radiusBottom` / `radiusTop`, no frame), and the pip
`usd-core` wheel has no `GeomUtil` at all. Hence the C++ probe.

CI never runs this: the fixture is committed, so `cargo test` needs neither a
C++ compiler nor OpenUSD. Rerun it after adding cases or moving the pin; the
diff of the fixture then shows exactly which points moved.

Requirements:
  network access to conda.anaconda.org (micro.mamba.pm is not needed)
  a C++17 compiler (`c++` on PATH, or set CXX)
  about 2 GB of disk for the environment

Usage:
  scripts/geomutil_oracle.py [--out PATH] [--keep DIR]
"""
import argparse
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.request

CHANNEL = "https://conda.anaconda.org/conda-forge"
MICROMAMBA = "micromamba-2.9.0-0.tar.bz2"
PYTHON = "3.13"
OPENUSD = "26.08"
OPENUSD_BUILD = "py313h6963fe8_0"

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
DEFAULT_OUT = os.path.join(ROOT, "crates", "crust-core", "tests", "data", "geomutil_oracle.txt")

# The libraries the probe links, in link order. `usd_python` and libpython
# come along because conda-forge builds OpenUSD with Python support, so the
# Tf and Vt symbols the headers pull in reference them.
LIBS = [
    "usd_usdImaging", "usd_geomUtil", "usd_pxOsd", "usd_usdGeom",
    "usd_vt", "usd_gf", "usd_tf", "usd_arch", "usd_python", f"python{PYTHON}",
]


def cases():
    """The fixed case list: every family, every axis, the schema fallbacks,
    cones and truncated cones, unequal capsule radii in both directions,
    N in {3, 10, 16, 48}, M in {1, 4}, and a tiny and a huge radius."""
    out = []
    # Cube: the fallback, and a scaled one. No axis.
    out.append("cube-fallback cube 2")
    out.append("cube-size-0.5 cube 0.5")
    # Plane: the fallback (2 x 2, axis Z), every axis, unequal sides.
    out.append("plane-fallback plane 2 2 Z")
    for axis in "XYZ":
        out.append(f"plane-{axis} plane 3 1.5 {axis}")
    # Cylinder: the fallback at Storm's count (10), every axis.
    out.append("cylinder-fallback cylinder 10 1 1 2 Z")
    for n in (3, 16, 48):
        for axis in "XYZ":
            out.append(f"cylinder-n{n}-{axis} cylinder {n} 0.75 0.75 3 {axis}")
    # Cylinder_1: a cone (radiusTop 0), truncated cones both ways, a flat disc.
    out.append("cylinder1-cone cylinder 16 1 0 2 Z")
    out.append("cylinder1-truncated cylinder 16 1 0.4 2 X")
    out.append("cylinder1-flared cylinder 10 0.3 1.2 1.5 Y")
    out.append("cylinder1-flat cylinder 10 1 0.5 0 Z")
    # Cone: the fallback, every axis.
    out.append("cone-fallback cone 10 1 2 Z")
    for n in (3, 16, 48):
        for axis in "XYZ":
            out.append(f"cone-n{n}-{axis} cone {n} 0.6 1.8 {axis}")
    # Capsule: the fallback (radius 0.5, height 1) at Storm's counts, every
    # axis, M in {1, 4}.
    out.append("capsule-fallback capsule 10 4 0.5 0.5 1 Z")
    for n, m in ((3, 1), (16, 4), (48, 4)):
        for axis in "XYZ":
            out.append(f"capsule-n{n}-m{m}-{axis} capsule {n} {m} 0.4 0.4 2 {axis}")
    out.append("capsule-n16-m12 capsule 16 12 0.4 0.4 2 Z")
    # Capsule_1: unequal radii both ways, and a zero-height one.
    out.append("capsule1-narrowing capsule 16 4 1 0.25 2 Z")
    out.append("capsule1-widening capsule 16 4 0.2 0.8 1 X")
    out.append("capsule1-steep capsule 10 2 1 0.1 0.5 Y")
    out.append("capsule1-m1 capsule 10 1 1 0.5 2 Z")
    out.append("capsule1-flat capsule 10 4 1 0.5 0 Z")
    # Scale: a tiny and a huge radius.
    out.append("cylinder-tiny cylinder 16 1e-4 1e-4 3e-4 Z")
    out.append("cylinder-huge cylinder 16 1e4 1e4 5e3 Z")
    out.append("capsule-tiny capsule 16 4 1e-4 1e-4 2e-4 Y")
    out.append("capsule-huge capsule 16 4 1e4 1e4 2e4 X")
    return out


def bootstrap(work):
    """micromamba, then the pinned environment, both under `work`. Reused
    when already there (`--keep`)."""
    mamba = os.path.join(work, "mm", "bin", "micromamba")
    if not os.path.exists(mamba):
        os.makedirs(os.path.join(work, "mm"), exist_ok=True)
        archive = os.path.join(work, "mm", MICROMAMBA)
        urllib.request.urlretrieve(f"{CHANNEL}/linux-64/{MICROMAMBA}", archive)
        with tarfile.open(archive) as t:
            t.extract("bin/micromamba", os.path.join(work, "mm"), filter="data")
        os.chmod(mamba, 0o755)
    env = os.path.join(work, "env")
    if not os.path.isdir(env):
        subprocess.run(
            [mamba, "create", "-y", "-q", "-p", env, "-c", "conda-forge", "--override-channels",
             f"python={PYTHON}", f"openusd=={OPENUSD}={OPENUSD_BUILD}"],
            check=True,
            env={**os.environ, "MAMBA_ROOT_PREFIX": os.path.join(work, "root")},
        )
    verify(mamba, env, work)
    return env


def verify(mamba, env, work):
    """Refuse an environment that does not hold exactly the pinned build: the
    fixture header names the pin, so probing anything else would record a
    reference that was never run. A `--keep` directory is reused only if it
    passes."""
    listing = subprocess.run(
        [mamba, "list", "-p", env, "^openusd$"],
        check=True, capture_output=True, text=True,
        env={**os.environ, "MAMBA_ROOT_PREFIX": os.path.join(work, "root")},
    ).stdout
    if not any(line.split()[:3] == ["openusd", OPENUSD, OPENUSD_BUILD] for line in listing.splitlines() if line.strip()):
        sys.exit(f"{env} does not hold openusd {OPENUSD} {OPENUSD_BUILD}; remove it or use another --keep directory")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--out", default=DEFAULT_OUT)
    ap.add_argument("--keep", help="bootstrap and build in this directory and keep it")
    args = ap.parse_args()

    work = os.path.abspath(args.keep) if args.keep else tempfile.mkdtemp(prefix="geomutil_oracle.")
    try:
        env = bootstrap(work)
        probe = os.path.join(work, "probe")
        cxx = os.environ.get("CXX", "c++")
        subprocess.run(
            [cxx, "-std=c++17", "-O2", "-w",
             f"-I{env}/include", f"-I{env}/include/python{PYTHON}",
             os.path.join(HERE, "geomutil_oracle", "probe.cpp"), "-o", probe,
             f"-L{env}/lib", f"-Wl,-rpath,{env}/lib", *[f"-l{lib}" for lib in LIBS]],
            check=True,
        )

        inputs = cases()
        result = subprocess.run([probe], input="\n".join(inputs) + "\n", capture_output=True, text=True, check=True)
        answers = result.stdout.strip().split("\n")
        if len(answers) != len(inputs):
            sys.exit(f"probe answered {len(answers)} of {len(inputs)} cases")

        with open(args.out, "w") as f:
            f.write("# OpenUSD GeomUtil reference meshes. Generated by scripts/geomutil_oracle.py; do not edit.\n")
            f.write(f"# conda-forge openusd {OPENUSD} {OPENUSD_BUILD}\n")
            f.write(f"# {len(inputs)} cases. Points and normals as Storm writes them (float, through\n")
            f.write("# UsdImagingGprimAdapter::GetImplicitBasis(axis)); a face's corners are listed in order.\n")
            f.write("# <id> <family> <params> || <scheme> | <counts> | <indices> | <points> | <interpolation> | <normals>\n")
            for case, answer in zip(inputs, answers):
                case_id, rest = answer.split(" |", 1)
                if case.split(" ", 1)[0] != case_id:
                    sys.exit(f"probe answered {case_id} out of order")
                f.write(f"{case} || {rest.strip()}\n")
        print(f"wrote {len(inputs)} cases to {os.path.relpath(args.out, ROOT)}")
    finally:
        if not args.keep:
            shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
