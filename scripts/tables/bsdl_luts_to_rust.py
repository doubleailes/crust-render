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

and one table BSDL does not bake, built by the same `genluts` from a copy with
one struct added (the pinned checkout itself is never modified):

- `mtx::DielectricReflBack` (`MTX/bsdf_dielectric_reflback_luts.h`): the
  reflection-only dielectric seen from inside, `DielectricReflFront` with
  `DielectricFresnel::from_table_index(fresnel_index, true)`, on a cosine axis
  aligned to each row's critical angle (`REFL_BACK_KNEE` below). Its lobe
  totally reflects past the critical angle, which `DielectricReflFront`
  (BSDL's filter on both faces) knows nothing of; crust's back-face filter
  reads this one so that a reflection-only top over a refracting base cannot
  return more than it receives (`openspec/specs/materials/design.md`,
  "MaterialX transmission").

`genluts` also bakes `ContyKullaSheen` and `ZeltnerBurleySheen`; `mtx::SheenLobe`
reads neither (its albedo is MaterialX's rational fit in `conty_kulla` mode and
the LTC table's `R` in `zeltner` mode, the table crust already ships as
`material/ltc_sheen_table.rs`), so they are not converted.

This fetches OSL and Imath at the pinned commits, builds `genluts`, runs it
(about a minute on four cores) and writes the tables:

    scripts/tables/bsdl_luts_to_rust.py \\
        > crates/crust-core/src/material/closure/bsdl_tables.rs

The baked values are deterministic (QMC seeds hashed from each table entry's
Fresnel and roughness indices alone): rerunning it reproduces the committed
file bit for bit, baking `DielectricReflBack` after them moves no value of the
other four, and `DielectricReflFront` is bit for bit the table crust first
took from NVIDIA Typhoon's vendored BSDL.

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
    ("DIELECTRIC_REFL_BACK", "MTX/bsdf_dielectric_reflback_luts.h", "mtx::DielectricReflBack", 32, 16, 16, "linear"),
    ("DIELECTRIC_BOTH_FRONT", "MTX/bsdf_dielectric_bothfront_luts.h", "mtx::DielectricBothFront", 32, 16, 16, "linear"),
    ("DIELECTRIC_BOTH_BACK", "MTX/bsdf_dielectric_bothback_luts.h", "mtx::DielectricBothBack", 32, 16, 16, "linear"),
]

# crust's addition to `genluts`: `DielectricReflFront` from the back side,
# baked by `genluts`' own `compute_E` on a Snell-aligned cosine axis. Below
# the critical angle the lobe totally reflects and its filter is 0; above it
# rises from 0. That edge moves with the IOR, so on BSDL's fixed `i/15`
# cosines bilinear interpolation would straddle it (and let a smooth top pass
# up to half its base past the critical angle); here every row puts its own
# critical cosine at column `REFL_BACK_KNEE`, columns `0..=KNEE` spanning
# `[0, c_crit]` and `KNEE..=15` spanning `[c_crit, 1]`, each quadratically
# denser toward the knee. `bsdl::refl_back_axis` is the inverse; the two must
# agree.
REFL_BACK_KNEE = 7
REFL_BACK_CODE = """
BSDL_ENTER_NAMESPACE
namespace mtx {
struct DielectricReflBack : public DielectricBSDF<DielectricFresnel> {
    explicit BSDL_INLINE_METHOD DielectricReflBack(float cosNO,
                                                   float roughness_index,
                                                   float fresnel_index)
        : DielectricBSDF<DielectricFresnel>(
              GGXDist(roughness_index, 0),
              DielectricFresnel::from_table_index(fresnel_index, true), cosNO,
              roughness_index, false, 1)
    {
    }
    static const char* lut_header()
    {
        return "MTX/bsdf_dielectric_reflback_luts.h";
    }
    static const char* struct_name() { return "DielectricReflBack"; }
    static Energy& get_energy()
    {
        static Energy energy = {};
        return energy;
    }
};
}  // namespace mtx
BSDL_LEAVE_NAMESPACE
"""
REFL_BACK_BAKE = """
// crust: `mtx::DielectricReflBack` on the Snell-aligned axis.
static float
reflback_cosine(int c, float fresnel_index)
{
    using BSDF          = mtx::DielectricReflBack;
    constexpr int KNEE  = %(knee)d;
    const float eta     = mtx::DielectricFresnel::from_table_index(fresnel_index, true).eta;
    const float c_crit  = eta < 1 ? sqrtf(1 - eta * eta) : 0.0f;
    // Quadratically denser toward the knee from both sides: just past the
    // critical angle a slightly rough lobe goes from letting a third through
    // to letting nothing through within a few degrees.
    float cos;
    if (c <= KNEE) {
        const float t = 1 - float(c) / KNEE;
        cos           = c_crit * (1 - t * t);
    } else {
        const float t = float(c - KNEE) / (BSDF::Nc - 1 - KNEE);
        cos           = c_crit + (1 - c_crit) * t * t;
    }
    return std::max(cos, 1e-6f);
}

static void
bake_reflback(const std::string& output_dir)
{
    using BSDF     = mtx::DielectricReflBack;
    float* storedE = BSDF::get_energy().data;
    printf("Generating LUTs for %%s ...\\n", BSDF::struct_name());
    parallel_for(0, BSDF::Nf, [&](unsigned f) {
        const float fresnel_index = float(f) * (1.0f / std::max(1, BSDF::Nf - 1));
        for (int r = 0; r < BSDF::Nr; r++) {
            const float roughness_index = float(r) * (1.0f / (BSDF::Nr - 1));
            for (int c = 0; c < BSDF::Nc; c++) {
                const float cos = reflback_cosine(c, fresnel_index);
                const BSDF bsdf(cos, roughness_index, fresnel_index);
                storedE[f * BSDF::Nr * BSDF::Nc + r * BSDF::Nc + c]
                    = 1 - compute_E(cos, bsdf, f, r);
            }
        }
    });
    std::string out_file = output_dir + "/" + BSDF::lut_header();
    FILE* outf           = fopen(out_file.c_str(), "wb");
    if (!outf) {
        printf("Failed to open %%s for writing\\n", out_file.c_str());
        exit(-1);
    }
    fprintf(outf, "static Energy energy = {{\\n");
    for (int i = 0; i < BSDF::Nf * BSDF::Nr * BSDF::Nc; i++) {
        // As `bake_emiss_tables` writes them: an integer with a `.0`.
        if (storedE[i] == int(storedE[i]))
            fprintf(outf, " %%12d.0f,", int(storedE[i]));
        else
            fprintf(outf, " %%14.9gf,", storedE[i]);
        if ((i + 1) %% BSDF::Nc == 0)
            fprintf(outf, "\\n");
    }
    fprintf(outf, "}};\\n");
    fclose(outf);
    printf("Wrote LUTs to %%s\\n", out_file.c_str());
}

""" % {"knee": REFL_BACK_KNEE}


# Extra doc lines for a table, under its first.
NOTES = {
    "DIELECTRIC_REFL_BACK": [
        "///",
        "/// Not one of BSDL's tables: `DielectricReflFront` from the back side",
        "/// (`from_table_index(fresnel_index, true)`), baked by BSDL's `genluts`",
        "/// estimator with that struct added. Its cosine axis is not `i/15`: each",
        f"/// IOR row has its critical cosine at column {REFL_BACK_KNEE} (see `bsdl::refl_back_axis`",
        "/// and `bsdl_luts_to_rust.py`).",
    ],
}


def patched_genluts(src):
    """genluts.cpp with `mtx::DielectricReflBack` declared and baked."""
    text = open(src).read()
    anchors = [
        ("#include <BSDL/microfacet_tools_impl.h>\n",
         "#include <BSDL/microfacet_tools_impl.h>\n" + REFL_BACK_CODE),
        ("int\nmain(int argc, const char** argv)\n",
         REFL_BACK_BAKE + "int\nmain(int argc, const char** argv)\n"),
        ("        bsdf_list);\n\n    return 0;\n",
         "        bsdf_list);\n    bake_reflback(argv[1]);\n\n    return 0;\n"),
    ]
    for old, new in anchors:
        if text.count(old) != 1:
            sys.exit(f"{src}: expected exactly one {old.strip()!r} to patch")
        text = text.replace(old, new)
    return text


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
    source = os.path.join(work, "genluts_crust.cpp")
    with open(source, "w") as f:
        f.write(patched_genluts(os.path.join(bsdl, "src", "genluts.cpp")))
    cxx = os.environ.get("CXX", "c++")
    # `-I src` for genluts.cpp's own `#include "parallel.h"`.
    subprocess.run([cxx, "-std=c++17", "-O2", "-DNDEBUG", f"-I{bsdl}/include", f"-I{bsdl}/src",
                    f"-I{prefix}/include/Imath", f"-I{prefix}/include",
                    source, "-pthread", "-o", genluts], check=True)
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
        "//! `(i/15)²` for [`GGX`], the first floored at 1e-6, except",
        "//! [`DIELECTRIC_REFL_BACK`], whose cosines are aligned to each row's critical",
        "//! angle.",
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
            *NOTES.get(name, []),
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
