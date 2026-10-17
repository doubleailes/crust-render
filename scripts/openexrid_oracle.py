#!/usr/bin/env python3
"""Check crust's OpenEXRId mattes with OpenEXR and OpenEXRId themselves.

crust writes a `deepRaster` RenderProduct as an OpenEXRId matte (file
version 3) through the `exr` crate's deep writer
(`crates/crust-render/src/exrid.rs`). The Rust tests read those files back
with the same crate; this script reads them with the C++ libraries the
compositing tools use:

- it downloads micromamba from conda.anaconda.org and creates a throwaway
  environment holding conda-forge's `openexr`, pinned to a version and build;
- it fetches OpenEXRId at a pinned commit and compiles its reader
  (`openexrid/Mask.cpp`, which reads a file through OpenEXR's
  `DeepScanLineInputFile` and undoes the deep "over" form) with
  `scripts/openexrid_oracle/probe.cpp`. One change is made to it first:
  written for OpenEXR 2, it turns the sample counts it read into offsets in
  the frame buffer's sample-count slice, which OpenEXR 3's `readPixels`
  writes again line by line, and it then reads past its arrays — on
  OpenEXRId's own sample file as on crust's. The patch points that slice at
  scratch storage before the pixels are read ([`patch_mask`]);
- it renders `samples/openexrid.usda` with a crust binary and runs the probe
  on each matte against the flat beauty of the same render: per pixel the
  coverage of the matte's samples must sum to the beauty's alpha, and a
  coloured matte's samples to the beauty where the alpha is one, every id
  must have a name (decoded as OpenEXRId's Nuke plug-in decodes version 3:
  base64, zlib, one name per line).

The probe also prints how many names `Mask::read` itself finds: it splits
the table on NUL, where the Nuke plug-in splits it on newlines, so it sees
one. That disagreement is OpenEXRId's own (`openspec/specs/aovs/design.md`,
"OpenEXRId as built"); crust follows the Nuke plug-in.

CI never runs this. Rerun it after touching the matte writer or moving the
`exr` dependency; it prints one line per matte and exits non-zero on the first
mismatch.

Requirements:
  network access to conda.anaconda.org and github.com
  a C++17 compiler (`c++` on PATH, or set CXX)
  a crust binary (`cargo build --release -p crust-render`), or --crust PATH
  about 300 MB of disk for the environment

Usage:
  scripts/openexrid_oracle.py [--crust PATH] [--keep DIR]
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
OPENEXR = "3.5.2"
OPENEXR_BUILD = "hff7ccaf_1"
OPENEXRID = "https://github.com/MercenariesEngineering/openexrid.git"
OPENEXRID_COMMIT = "1ec6dff08714876f1a6f15added25d26f49270ac"

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SAMPLE = os.path.join(ROOT, "samples", "openexrid.usda")
FLAT = "renders/openexrid.exr"
MATTES = [
    "renders/openexrid_objects.exr",
    "renders/openexrid_materials.exr",
    "renders/openexrid_assets.exr",
]


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
             f"openexr=={OPENEXR}={OPENEXR_BUILD}", "zlib"],
            check=True,
            env={**os.environ, "MAMBA_ROOT_PREFIX": os.path.join(work, "root")},
        )
    return env


def fetch_openexrid(work):
    """OpenEXRId's sources at the pinned commit."""
    src = os.path.join(work, "openexrid")
    if not os.path.isdir(src):
        subprocess.run(["git", "clone", "-q", "--filter=blob:none", OPENEXRID, src], check=True)
    subprocess.run(["git", "-C", src, "checkout", "-q", OPENEXRID_COMMIT], check=True)
    return src


def patch_mask(src, work):
    """`Mask.cpp` with the sample-count slice moved to scratch storage
    before the pixels are read (see the module documentation)."""
    with open(os.path.join(src, "openexrid", "Mask.cpp")) as f:
        text = f.read()
    anchor = "\t// Resize the samples\n"
    if text.count(anchor) != 1:
        sys.exit("openexrid/Mask.cpp no longer has the anchor the patch needs")
    scratch = (
        "\t// OpenEXR 3 writes each line's counts into the sample count slice\n"
        "\t// as readPixels reads it: point the slice at scratch storage now\n"
        "\t// that the counts are offsets.\n"
        "\tstd::vector<uint32_t> scratch (_Width*_Height+1, 0);\n"
        "\tframeBuffer.insertSampleCountSlice (Imf::Slice (UINT,\n"
        "\t\t(char *) (&scratch[0]), sizeof (uint32_t), sizeof (uint32_t)*_Width));\n"
        "\tfile.setFrameBuffer(frameBuffer);\n\n"
    )
    patched = os.path.join(work, "Mask.cpp")
    with open(patched, "w") as f:
        f.write(text.replace(anchor, scratch + anchor))
    return patched


def build_probe(env, src, work):
    probe = os.path.join(work, "probe")
    lib = os.path.join(env, "lib")
    include = os.path.join(env, "include")
    cxx = os.environ.get("CXX", "c++")
    sources = [
        os.path.join(HERE, "openexrid_oracle", "probe.cpp"),
        patch_mask(src, work),
        os.path.join(src, "openexrid", "b64.cpp"),
        os.path.join(src, "openexrid", "zlib.cpp"),
    ]
    subprocess.run(
        [cxx, "-std=c++17", "-O1", "-o", probe, *sources,
         f"-I{os.path.join(src, 'openexrid')}",
         f"-I{os.path.join(include, 'OpenEXR')}", f"-I{os.path.join(include, 'Imath')}",
         f"-I{include}", f"-L{lib}", f"-Wl,-rpath,{lib}",
         "-lOpenEXR", "-lOpenEXRCore", "-lIex", "-lIlmThread", "-lImath", "-lz"],
        check=True,
    )
    return probe


def main():
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("--crust", default=os.path.join(ROOT, "target", "release", "crust"),
                        help="the crust binary to render with")
    parser.add_argument("--keep", help="reuse (or create) the work directory here")
    args = parser.parse_args()
    work = args.keep or tempfile.mkdtemp(prefix="openexrid_oracle_")
    os.makedirs(work, exist_ok=True)
    try:
        env = bootstrap(work)
        src = fetch_openexrid(work)
        probe = build_probe(env, src, work)
        out = os.path.join(work, "render")
        shutil.rmtree(out, ignore_errors=True)
        os.makedirs(out)
        subprocess.run([os.path.abspath(args.crust), "render", "-i", SAMPLE, "-s", "16", "-l", "error"],
                       cwd=out, check=True)
        failed = False
        for matte in MATTES:
            r = subprocess.run([probe, os.path.join(out, matte), os.path.join(out, FLAT)],
                               capture_output=True, text=True)
            sys.stdout.write(r.stdout)
            sys.stderr.write(r.stderr)
            failed |= r.returncode != 0
        return 1 if failed else 0
    finally:
        if not args.keep:
            shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main())
