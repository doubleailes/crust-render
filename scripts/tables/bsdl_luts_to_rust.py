#!/usr/bin/env python3
"""Regenerate BSDL's baked energy tables as Rust `static`s.

crust's MaterialX BSDF leaves are BSDL's `mtx::` lobes (OSL's `src/libbsdl`,
BSD-3-Clause, Copyright Contributors to the Open Shading Language project;
see `THIRD-PARTY.md`). They read four of the tables BSDL's `genluts` bakes,
each the missing energy `1 − E(cosθo)` of a lobe averaged over 128² QMC
samples:

- `spi::MiniMicrofacetGGX` (`SPI/microfacet_tools_luts.h`): single-scattering
  white GGX, the `E_ms` of Turquin's compensation on every reflection-only
  microfacet lobe;
- `mtx::DielectricReflFront` (`MTX/bsdf_dielectric_reflfront_luts.h`): a
  dielectric that only reflects, the layering filter of a reflection-only
  `dielectric_bsdf` or `generalized_schlick_bsdf`;
- `mtx::DielectricBothFront` / `DielectricBothBack`
  (`MTX/bsdf_dielectric_both{front,back}_luts.h`): a dielectric that reflects
  and refracts, seen from outside / inside, whose loss the refracting lobes
  renormalise away.

`genluts` also bakes `ContyKullaSheen` and `ZeltnerBurleySheen`; `mtx::SheenLobe`
reads neither (its albedo is MaterialX's rational fit in `conty_kulla` mode and
the LTC table's `R` in `zeltner` mode, the table crust already ships as
`material/ltc_sheen_table.rs`), so they are not converted.

This fetches OSL and Imath at the pinned commits, builds `genluts`, runs it
(about a minute on four cores) and writes the tables:

    scripts/tables/bsdl_luts_to_rust.py \\
        > crates/crust-core/src/material/closure/bsdl_tables.rs

The baked values are deterministic (hashed QMC seeds): rerunning it reproduces
the committed file bit for bit, and `DielectricReflFront` is bit for bit the
table crust first took from NVIDIA Typhoon's vendored BSDL.

Requirements: git, cmake, a C++17 compiler (`c++` on PATH, or set CXX), numpy,
and network access to github.com. `--headers DIR` converts an existing
`genluts` output directory instead (it holds `SPI/` and `MTX/`); `--keep DIR`
builds there and keeps it.
"""
import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile

import numpy as np

OSL_URL = "https://github.com/AcademySoftwareFoundation/OpenShadingLanguage.git"
OSL_COMMIT = "cc6f9b12fd3c98646fe83576c8c09f533a1ab6ce"
IMATH_URL = "https://github.com/AcademySoftwareFoundation/Imath.git"
IMATH_COMMIT = "c0396a055a01bc537d32f435aee11a9b7ed6f0b5"  # tag v3.1.12

# (Rust name, genluts header, BSDL type, Nf, Nr, Nc, cosine spacing)
TABLES = [
    ("GGX", "SPI/microfacet_tools_luts.h", "spi::MiniMicrofacetGGX", 1, 16, 16, "squared"),
    ("DIELECTRIC_REFL_FRONT", "MTX/bsdf_dielectric_reflfront_luts.h", "mtx::DielectricReflFront", 32, 16, 16, "linear"),
    ("DIELECTRIC_BOTH_FRONT", "MTX/bsdf_dielectric_bothfront_luts.h", "mtx::DielectricBothFront", 32, 16, 16, "linear"),
    ("DIELECTRIC_BOTH_BACK", "MTX/bsdf_dielectric_bothback_luts.h", "mtx::DielectricBothBack", 32, 16, 16, "linear"),
]


def fetch(url, commit, dest, sparse=None):
    os.makedirs(dest)
    run = lambda *a: subprocess.run(["git", *a], cwd=dest, check=True, capture_output=True)
    run("init", "-q")
    run("remote", "add", "origin", url)
    if sparse:
        run("sparse-checkout", "set", *sparse)
    run("fetch", "-q", "--depth", "1", "--filter=blob:none", "origin", commit)
    run("checkout", "-q", "FETCH_HEAD")


def verify(dest, commit):
    """Refuse a source tree that is not exactly the pinned commit."""
    git = lambda *a: subprocess.run(["git", *a], cwd=dest, check=True, capture_output=True, text=True).stdout.strip()
    head = git("rev-parse", "HEAD")
    if head != commit or git("status", "--porcelain"):
        sys.exit(f"{dest} is not a clean checkout of the pinned {commit}; remove it or use another --keep directory")


def bake(work):
    """Fetch and build at the pins, run `genluts`, return its output directory."""
    osl = os.path.join(work, "osl")
    imath = os.path.join(work, "imath")
    if not os.path.isdir(osl):
        fetch(OSL_URL, OSL_COMMIT, osl, sparse=["src/libbsdl"])
    if not os.path.isdir(imath):
        fetch(IMATH_URL, IMATH_COMMIT, imath)
    verify(osl, OSL_COMMIT)
    verify(imath, IMATH_COMMIT)
    prefix = os.path.join(work, "imath-install")
    if not os.path.isdir(prefix):
        build = os.path.join(work, "imath-build")
        quiet = dict(check=True, capture_output=True)
        subprocess.run(["cmake", "-S", imath, "-B", build, f"-DCMAKE_INSTALL_PREFIX={prefix}",
                        "-DBUILD_TESTING=OFF", "-DPYTHON=OFF"], **quiet)
        subprocess.run(["cmake", "--build", build, "-j"], **quiet)
        subprocess.run(["cmake", "--install", build], **quiet)
    bsdl = os.path.join(osl, "src", "libbsdl")
    genluts = os.path.join(work, "genluts")
    cxx = os.environ.get("CXX", "c++")
    subprocess.run([cxx, "-std=c++17", "-O2", "-DNDEBUG", f"-I{bsdl}/include",
                    f"-I{prefix}/include/Imath", f"-I{prefix}/include",
                    os.path.join(bsdl, "src", "genluts.cpp"), "-pthread", "-o", genluts], check=True)
    out = os.path.join(work, "luts")
    for d in ("SPI", "MTX"):
        os.makedirs(os.path.join(out, d), exist_ok=True)
    subprocess.run([genluts, out], check=True, stdout=sys.stderr)
    return out


def values(path, n):
    text = open(path).read()
    body = text[text.index("energy = {{"):]
    vals = [float(v) for v in re.findall(r"-?\d+\.\d*(?:e-?\d+)?(?=f)", body)]
    if len(vals) != n:
        raise SystemExit(f"{path}: expected {n} values, found {len(vals)}")
    return vals


def literal(v):
    """The shortest literal that round-trips to the same `f32` bits."""
    return np.format_float_positional(np.float32(v), unique=True, trim="0")


def emit(headers):
    out = [
        "//! BSDL's baked energy tables: the missing energy `1 − E(cosθo)` of the",
        "//! lobes crust's MaterialX leaves are ported from.",
        "//!",
        "//! Generated by `scripts/tables/bsdl_luts_to_rust.py` from BSDL's `genluts`",
        f"//! (OSL `src/libbsdl` at {OSL_COMMIT[:7]}). BSDL is BSD-3-Clause, Copyright",
        "//! Contributors to the Open Shading Language project; see `THIRD-PARTY.md`.",
        "//! Do not edit by hand.",
        "//!",
        "//! Layout `[ior][roughness][cosθo]` (`[roughness][cosθo]` for [`GGX`]): IOR",
        "//! index `√((η − 1.001)/(5 − 1.001))` on 32 steps, BSDL roughness (`√α`) on",
        "//! 16 steps over [0, 1], and 16 cosines — `i/15` for the dielectric tables,",
        "//! `(i/15)²` for [`GGX`], the first floored at 1e-6.",
        "",
        "pub(super) const NF: usize = 32;",
        "pub(super) const NR: usize = 16;",
        "pub(super) const NC: usize = 16;",
    ]
    for name, header, bsdl_type, nf, nr, nc, spacing in TABLES:
        vals = values(os.path.join(headers, header), nf * nr * nc)
        out += [
            "",
            f"/// `{bsdl_type}`, cosines spaced {spacing}ly.",
            "#[rustfmt::skip]",
            f"pub(super) static {name}: [f32; {nf * nr * nc}] = [",
        ]
        for row in range(nf * nr):
            cells = vals[row * nc:(row + 1) * nc]
            out.append("    " + " ".join(f"{literal(v)}," for v in cells))
        out.append("];")
    print("\n".join(out))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--headers", help="convert this genluts output directory instead of baking")
    ap.add_argument("--keep", help="build in this directory and keep it")
    args = ap.parse_args()
    if args.headers:
        emit(args.headers)
        return
    work = args.keep or tempfile.mkdtemp(prefix="bsdl_luts.")
    try:
        emit(bake(work))
    finally:
        if not args.keep:
            shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
