#!/usr/bin/env python3
"""Summarise a `run.py` results.json as Markdown.

Reports crust's PSNR against `materialx-glsl` per suite group, beside the
suite's own published renderers on the same materials, and — statically, from
the documents themselves — which node categories the suite uses that
`crust-mtlx` has no operator for. The static half is needed because the
compiler degrades an unknown node to a constant and stops there, never reaching
the graph upstream of it, so the render log alone undercounts missing nodes.

Usage: summarize.py --suite /path/to/material-fidelity results.json > report.md
"""
import argparse
import json
import re
import statistics
from collections import Counter, defaultdict
from pathlib import Path

import suite

REPO = suite.REPO
# Elements that are document structure, not nodes.
STRUCTURAL = {"materialx", "nodegraph", "input", "output", "nodedef", "implementation",
              "look", "materialassign", "collection", "typedef", "token", "variant",
              "variantset", "variantassign", "geominfo", "geomprop", "backdrop", "member",
              "propertyset", "propertysetassign", "property", "visibility", "unit",
              "unittypedef", "unitdef", "attributedef", "targetdef"}
# Handled by `bsdf::flatten` (and, for the surface nodes, `surface.rs`) rather
# than the pattern compiler.
CLOSURE_NODES = {"surfacematerial", "surface", "layer", "mix", "add", "multiply",
                 "oren_nayar_diffuse_bsdf", "diffuse_bsdf", "burley_diffuse_bsdf",
                 "dielectric_bsdf", "generalized_schlick_bsdf", "thin_film_bsdf",
                 "conductor_bsdf", "sheen_bsdf", "subsurface_bsdf", "translucent_bsdf",
                 "uniform_edf", "generalized_schlick_edf", "anisotropic_vdf",
                 "absorption_vdf", "standard_surface", "open_pbr_surface", "gltf_pbr"}
SURFACES = ("standard_surface", "open_pbr_surface", "gltf_pbr")


def supported_categories():
    """The category arms of `Compiler`'s dispatch in crust-mtlx/src/eval/compiler.rs."""
    src = (REPO / "crates/crust-mtlx/src/eval/compiler.rs").read_text()
    start = src.index("match node.category.as_str()")
    end = src.index("other =>", start)
    arms = re.findall(r'^\s*((?:"[a-z0-9_]+"\s*\|?\s*)+)=>', src[start:end], re.M)
    return {c for a in arms for c in re.findall(r'"([a-z0-9_]+)"', a)} | CLOSURE_NODES


def group_of(material):
    parts = material.split("/")
    if parts[0] in ("nodes", "ai_authored"):
        return parts[0]
    return f"{parts[0]}/{parts[1]}"


def surface_of(text):
    for s in SURFACES:
        if f"<{s}" in text:
            return s
    return "other"


def stats(values):
    v = [x for x in values if x is not None]
    if not v:
        return "-", "-"
    return f"{statistics.mean(v):.2f}", f"{statistics.median(v):.2f}"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--suite", required=True, type=Path)
    ap.add_argument("results", type=Path)
    args = ap.parse_args()
    root = suite.materials_root(args.suite)
    results = json.loads(args.results.read_text())
    supported = supported_categories()

    by_group = defaultdict(list)
    by_surface = defaultdict(list)
    missing_nodes = Counter()
    missing_examples = {}
    errors = []
    others = sorted({o for r in results for o in r.get("others", {})})
    for r in results:
        if r.get("status") == "error":
            errors.append(r)
        text = (root / r["material"]).read_text(errors="replace")
        r["surface"] = surface_of(text)
        by_group[group_of(r["material"])].append(r)
        by_surface[r["surface"]].append(r)
        cats = set(re.findall(r"<([a-zA-Z_][a-zA-Z0-9_]*)[\s/>]", text)) - STRUCTURAL
        for c in sorted(cats - supported):
            missing_nodes[c] += 1
            missing_examples.setdefault(c, r["material"].split("/")[-2])

    scored = [r for r in results if r.get("psnr") is not None]
    out = []
    lossless = [r["psnr_lossless"] for r in scored if r.get("psnr_lossless") is not None]
    out.append(f"Materials: {len(results)}; scored against `{suite.REFERENCE_NAME}`: {len(scored)}; "
               f"render errors: {len(errors)}.\n")
    if lossless:
        out.append(f"Mean PSNR before AVIF encoding: {statistics.mean(lossless):.2f} dB "
                   f"(the `crust` columns score the AVIF, as the suite scores every renderer).\n")
    head = "| group | n | crust mean | crust median | " + " | ".join(f"{o} mean" for o in others) + " |"
    out.append(head)
    out.append("|" + "---|" * (4 + len(others)))
    for name, rows in [("**all**", results)] + sorted(by_group.items()) + \
                      [(f"surface: `{k}`", v) for k, v in sorted(by_surface.items())]:
        rows = [r for r in rows if r.get("psnr") is not None]
        mean, med = stats(r["psnr"] for r in rows)
        cells = [stats(r.get("others", {}).get(o) for r in rows)[0] for o in others]
        out.append(f"| {name} | {len(rows)} | {mean} | {med} | " + " | ".join(cells) + " |")

    ranked = sorted(scored, key=lambda r: r["psnr"])
    out.append("\nLowest crust PSNR:\n")
    out.append("| material | crust | " + " | ".join(others) + " |")
    out.append("|" + "---|" * (2 + len(others)))
    for r in ranked[:10]:
        out.append(f"| `{r['material'].rsplit('/', 1)[0]}` | {r['psnr']} | " +
                   " | ".join(str(r.get("others", {}).get(o, "-")) for o in others) + " |")
    out.append("\nHighest crust PSNR:\n")
    out.append("| material | crust | " + " | ".join(others) + " |")
    out.append("|" + "---|" * (2 + len(others)))
    for r in ranked[::-1][:10]:
        out.append(f"| `{r['material'].rsplit('/', 1)[0]}` | {r['psnr']} | " +
                   " | ".join(str(r.get("others", {}).get(o, "-")) for o in others) + " |")

    logged = Counter(u for r in results for u in r.get("unsupported", []))
    out.append("\nUnsupported node types crust logged at load (materials affected):\n")
    out.append(", ".join(f"`{k}` ({v})" for k, v in logged.most_common()) or "none")
    out.append("\nNode categories the suite's documents use that `crust-mtlx` has no operator for "
               "(static scan; materials affected, one example):\n")
    out.append("| node | materials | example |")
    out.append("|---|---|---|")
    for c, n in missing_nodes.most_common():
        out.append(f"| `{c}` | {n} | `{missing_examples[c]}` |")
    if errors:
        out.append("\nRender errors:\n")
        for r in errors:
            out.append(f"- `{r['material']}`: {r.get('error')}")
    print("\n".join(out))


if __name__ == "__main__":
    main()
