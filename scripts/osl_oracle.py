#!/usr/bin/env python3
"""Regenerate the OSL reference values `crust-mtlx` is checked against.

For every MaterialX standard-library node signature `crust-mtlx` compiles, this
builds small one-node documents, has MaterialX's own OSL code generator
(`genosl`) turn each into a shader, compiles it with `oslc` and runs it once
with `testshade`. The printed result is the value the MaterialX reference
implementation gives for those inputs. Every case is appended to
`crates/crust-mtlx/tests/data/osl_oracle.txt`, and
`crates/crust-mtlx/tests/osl_oracle.rs` replays it through `Compiler` +
`Program::eval`.

CI never runs this: the fixture is committed, so `cargo test` needs neither OSL
nor MaterialX. Rerun it after upgrading MaterialX, or after adding a node
signature to crust-mtlx (add its category to `CATEGORIES`).

Requirements:
  pip install materialx            # the genosl generator + stdlib .osl sources
  oslc and testshade on PATH       # OpenShadingLanguage >= 1.13, or set OSL_ROOT

OSL must be built with `-DUSE_FAST_MATH=0 -DOSL_BUILD_TESTS=1` (the second
builds `testshade`). The default build compiles OIIO's approximate `acos`,
`asin`, `pow`, ... into the runtime, off by ~1e-5 — too coarse for a
reference — and it cannot be switched off at run time. The script refuses to
run against such a build. OSL 1.13.9's exact path also needs four
`safe_asin` / `safe_acos` / `safe_log2` / `safe_log10` float calls qualified
with `OIIO::` (in `src/liboslexec/llvm_ops.cpp` and `src/include/OSL/dual.h`)
before it compiles; see `openspec/specs/materials/design.md` for the build.

Usage:
  scripts/osl_oracle.py [--cases N] [--out PATH] [--jobs J]
"""

import argparse
import concurrent.futures
import math
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile

import MaterialX as mx
import MaterialX.PyMaterialXGenOsl as mx_osl
import MaterialX.PyMaterialXGenShader as mx_gen

# Categories `crust-mtlx`'s `Compiler::compile_node` has an operator for,
# minus the ones that read something a one-shot `testshade` run cannot pin:
# `image` / `tiledimage` (a texture file) and `texcoord` / `normal` /
# `position` / `viewdirection` (the shading globals). `normalmap` stays: its
# frame is authored through its `normal` / `tangent` / `bitangent` inputs.
CATEGORIES = [
    "constant", "add", "subtract", "multiply", "divide", "power", "min", "max",
    "modulo", "absval", "ln", "exp", "sin", "cos", "asin", "acos", "sqrt",
    "sign", "floor", "ceil", "normalize", "luminance", "dotproduct", "mix",
    "clamp", "contrast", "remap", "invert", "smoothstep", "convert", "extract",
    "combine2", "combine3", "normalmap", "artistic_ior",
]

# The value types crust-mtlx models (`value::arity_of`). A signature using any
# other type (integer, boolean, matrix, string, closures) is not generated.
ARITY = {"float": 1, "vector2": 2, "vector3": 3, "color3": 3, "vector4": 4, "color4": 4}

# Every literal is drawn from here. Each has at most three decimals, so the
# `%f` text genosl prints into the shader parses to the same f32 that crust
# parses from the fixture: the two sides see bit-identical inputs.
POOL = ["0", "1", "-1", "0.5", "-0.5", "0.25", "2", "-3", "0.125", "1.5",
        "4", "-0.2", "0.7", "10", "0.9", "-0.875"]

# Orthonormal-ish shading frames for `normalmap`, `(N, T, B)` with `B = N x T`,
# all exact in three decimals. The second and third have a tangent that is not
# perpendicular to the normal, to exercise the Gram-Schmidt step.
FRAMES = [
    (("0", "0", "1"), ("1", "0", "0"), ("0", "1", "0")),
    (("0", "0.6", "0.8"), ("1", "0", "0.5"), ("0.3", "0.8", "-0.6")),
    (("0.8", "0", "0.6"), ("0.5", "1", "0"), ("-0.6", "0.3", "0.8")),
]


def osl_root_bin(name):
    root = os.environ.get("OSL_ROOT")
    if root:
        path = os.path.join(root, "bin", name)
        if os.path.exists(path):
            return path
    found = shutil.which(name)
    if not found:
        sys.exit(f"error: {name} not found (put it on PATH or set OSL_ROOT)")
    return found


def osl_shader_include(oslc):
    """The directory holding `stdosl.h`, beside the OSL install's `bin/`."""
    base = os.path.dirname(os.path.dirname(os.path.realpath(oslc)))
    for sub in ("share/OSL/shaders", "shaders"):
        path = os.path.join(base, sub)
        if os.path.exists(os.path.join(path, "stdosl.h")):
            return path
    sys.exit(f"error: no stdosl.h under {base}")


def check_exact_math(oslc, testshade):
    """Refuses an OSL built with fast math, whose `acos` is off by ~2e-5."""
    src = 'shader probe() { printf("ORACLE %.9g\\n", acos(0.5)); }\n'
    _, lanes, err = run_case((0, src, oslc, testshade, []))
    if err is not None:
        sys.exit(f"error: cannot run testshade: {err}")
    if abs(float(lanes[0]) - math.acos(0.5)) > 1e-6:
        sys.exit(f"error: this OSL computes acos(0.5) = {lanes[0]}: it was built with "
                 "fast math; rebuild it with -DUSE_FAST_MATH=0")


def signatures(lib):
    """`(nodedef, category, [(input, type, default)], [output types])`."""
    out = []
    for nd in lib.getNodeDefs():
        cat = nd.getNodeString()
        if cat not in CATEGORIES:
            continue
        ins = [(i.getName(), i.getType()) for i in nd.getInputs()]
        outs = [o.getType() for o in nd.getOutputs()]
        allowed = set(ARITY) | ({"integer"} if cat == "extract" else set())
        if any(t not in allowed for _, t in ins) or any(t not in ARITY for t in outs):
            continue
        out.append((nd.getName(), cat, ins, outs, [o.getName() for o in nd.getOutputs()]))
    return sorted(out)


def random_value(rng, cat, name, ty, arity_of_in):
    if ty == "integer":  # extract's index
        return str(rng.randrange(arity_of_in))
    lanes = [rng.choice(POOL) for _ in range(ARITY[ty])]
    if cat == "normalmap" and name == "in":
        # An encoded normal lives in [0, 1]; its z must stay positive for the
        # decoded vector to face out of the surface.
        lanes = [rng.choice(["0", "0.25", "0.5", "0.7", "0.9", "1"]) for _ in range(2)]
        lanes.append(rng.choice(["0.5", "0.7", "0.9", "1"]))
    if cat == "normalmap" and name == "scale":
        lanes = [rng.choice(["0", "0.5", "1", "2", "-1"]) for _ in range(ARITY[ty])]
    return ", ".join(lanes)


def cases_for(sig, n):
    """The unauthored-defaults case, then `n` seeded random ones."""
    nd, cat, ins, outs, out_names = sig
    rng = random.Random(nd)
    selections = out_names if len(outs) > 1 else [None]
    wide = max([ARITY.get(t, 1) for _, t in ins] or [1])
    frame_inputs = {"normal", "tangent", "bitangent"}
    result = []
    for sel in selections:
        for k in range(n + 1):
            vals = {}
            if cat == "normalmap":
                frame = FRAMES[k % len(FRAMES)]
                vals.update(zip(["normal", "tangent", "bitangent"], (", ".join(v) for v in frame)))
            if k > 0:
                for name, ty in ins:
                    if name in frame_inputs:
                        continue
                    # Leave roughly one input in five unauthored, so the
                    # defaults are checked in combination, not only all at once.
                    if rng.random() < 0.2:
                        continue
                    vals[name] = random_value(rng, cat, name, ty, wide)
            result.append((sel, [(name, ty, vals[name]) for name, ty in ins if name in vals]))
    return result


def build_doc(lib, sig, inputs, sel):
    nd, cat, ins, outs, out_names = sig
    doc = mx.createDocument()
    doc.importLibrary(lib)
    graph = doc.addNodeGraph("g")
    out_ty = outs[out_names.index(sel)] if sel else outs[0]
    node = graph.addNode(cat, "n", "multioutput" if len(outs) > 1 else outs[0])
    node.setNodeDefString(nd)
    for name, ty, text in inputs:
        if name in ("normal", "tangent", "bitangent"):
            # These declare a `defaultgeomprop`, which genosl binds in place
            # of an authored literal (it reads `N` / `dPdu` / `dPdv`), so the
            # frame is fed through a connection instead.
            src = graph.addNode("constant", f"frame_{name}", ty)
            src.setInputValue("value", text, ty)
            node.addInput(name, ty).setConnectedNode(src)
        else:
            node.setInputValue(name, text, ty)
    out = graph.addOutput("out", out_ty)
    out.setConnectedNode(node)
    if sel:
        out.setOutputString(sel)
    return doc, out, out_ty


def print_statement(ty):
    lanes = {
        "float": ["out"],
        "vector2": ["out.x", "out.y"],
        "vector3": ["out[0]", "out[1]", "out[2]"],
        "color3": ["out[0]", "out[1]", "out[2]"],
        "vector4": ["out.x", "out.y", "out.z", "out.w"],
        "color4": ["out.rgb[0]", "out.rgb[1]", "out.rgb[2]", "out.a"],
    }[ty]
    fmt = " ".join(["%.9g"] * len(lanes))
    return f'    printf("ORACLE {fmt}\\n", {", ".join(lanes)});\n'


def generate_source(lib, gen, search, sig, inputs, sel):
    doc, out, out_ty = build_doc(lib, sig, inputs, sel)
    ctx = mx_gen.GenContext(gen)
    ctx.registerSourceCodeSearchPath(search)
    shader = gen.generate("oracle", out, ctx)
    src = shader.getSourceCode(mx_gen.PIXEL_STAGE)
    # The shader body ends with the one assignment to `out`; print it there.
    end = src.rstrip().rfind("}")
    return src[:end] + print_statement(out_ty) + src[end:]


def run_case(job):
    idx, src, oslc, testshade, includes = job
    with tempfile.TemporaryDirectory() as tmp:
        osl = os.path.join(tmp, "oracle.osl")
        with open(osl, "w") as f:
            f.write(src)
        cmd = [oslc, "-q"] + [f"-I{p}" for p in includes] + ["-o", os.path.join(tmp, "oracle.oso"), osl]
        r = subprocess.run(cmd, capture_output=True, text=True)
        if r.returncode != 0:
            return idx, None, r.stderr.strip()
        r = subprocess.run([testshade, "-g", "1", "1", "oracle"], cwd=tmp,
                           capture_output=True, text=True)
        m = re.search(r"^ORACLE (.*)$", r.stdout, re.M)
        if not m:
            return idx, None, (r.stdout + r.stderr).strip()
        return idx, m.group(1).split(), None


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--cases", type=int, default=16, help="random cases per signature (default 16)")
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "..",
                                                  "crates/crust-mtlx/tests/data/osl_oracle.txt"))
    ap.add_argument("--jobs", type=int, default=os.cpu_count())
    args = ap.parse_args()

    oslc, testshade = osl_root_bin("oslc"), osl_root_bin("testshade")
    check_exact_math(oslc, testshade)
    search = mx.getDefaultDataSearchPath()
    lib = mx.createDocument()
    mx.loadLibraries(mx.getDefaultDataLibraryFolders(), search, lib)
    genosl_include = search.find("libraries/stdlib/genosl/include").asString()
    includes = [genosl_include, osl_shader_include(oslc)]
    gen = mx_osl.OslShaderGenerator.create()

    rows, jobs = [], []
    for sig in signatures(lib):
        for sel, inputs in cases_for(sig, args.cases):
            src = generate_source(lib, gen, search, sig, inputs, sel)
            jobs.append((len(rows), src, oslc, testshade, includes))
            rows.append((sig, sel, inputs))

    results = [None] * len(rows)
    failed = 0
    with concurrent.futures.ThreadPoolExecutor(max_workers=args.jobs) as pool:
        for idx, lanes, err in pool.map(run_case, jobs):
            if err is not None:
                failed += 1
                nd = rows[idx][0][0]
                print(f"warning: {nd} case {idx}: {err.splitlines()[-1] if err else 'no output'}",
                      file=sys.stderr)
            results[idx] = lanes

    osl_version = subprocess.run([oslc, "--help"], capture_output=True,
                                 text=True).stdout.splitlines()[0].split()[-1]
    with open(args.out, "w") as f:
        f.write("# Generated by scripts/osl_oracle.py; do not edit by hand.\n")
        f.write(f"# MaterialX {mx.__version__} genosl, OSL {osl_version} (exact math), "
                f"{args.cases} random cases per signature.\n")
        f.write("# nodedef\tcategory\toutput type\toutput\tinputs (name:type=lanes;...)\texpected lanes\n")
        for (sig, sel, inputs), lanes in zip(rows, results):
            if lanes is None:
                continue
            nd, cat, ins, outs, out_names = sig
            out_ty = outs[out_names.index(sel)] if sel else outs[0]
            ins_text = ";".join(f"{n}:{t}={v.replace(' ', '')}" for n, t, v in inputs) or "-"
            f.write("\t".join([nd, cat, out_ty, sel or "-", ins_text, " ".join(lanes)]) + "\n")
    print(f"{len(rows) - failed} cases from {len(set(r[0][0] for r in rows))} signatures "
          f"written to {os.path.relpath(args.out)}; {failed} failed")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
