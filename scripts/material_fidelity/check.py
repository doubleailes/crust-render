#!/usr/bin/env python3
"""Gate a `run.py` results.json against a recorded per-material PSNR baseline.

The suite scores crust against an external reference, so an absolute threshold
would fail most materials for gaps that are known (see
docs/material_fidelity.md). What a change must not do is move a material *away*
from the reference. This is Goldeneye's expected-failure idea turned into
numbers: the baseline records where every material stands, and the check fails
only on a material that got worse than it by more than `--tolerance` dB, or that
rendered before and errors now.

    check.py results.json                       # compare with baseline.json beside this script
    check.py results.json --update              # accept: rewrite the baseline from this run
    check.py results.json --baseline other.json --tolerance 1.0

Exit status: 0 clean, 1 regressions, 2 usage / missing baseline. A PSNR of
`null` means the image matched the reference exactly and ranks above any value.
"""
import argparse
import json
import math
import subprocess
import sys
from pathlib import Path

import suite

DEFAULT_BASELINE = suite.HERE / "baseline.json"


def value(p):
    return math.inf if p is None else p


def crust_revision():
    try:
        return subprocess.run(["git", "-C", str(suite.REPO), "rev-parse", "--short", "HEAD"],
                              capture_output=True, text=True, check=True).stdout.strip()
    except (OSError, subprocess.CalledProcessError):
        return None


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("results", type=Path)
    ap.add_argument("--baseline", type=Path, default=DEFAULT_BASELINE)
    ap.add_argument("--tolerance", type=float, default=0.5, help="dB a material may drop before it fails")
    ap.add_argument("--update", action="store_true", help="rewrite the baseline from these results")
    ap.add_argument("--suite-rev", help="material-fidelity revision, recorded with --update")
    args = ap.parse_args()

    results = json.loads(args.results.read_text())
    run_meta_path = args.results.with_name("run.json")
    run_meta = json.loads(run_meta_path.read_text()) if run_meta_path.exists() else {}
    current = {r["material"]: r for r in results}

    if args.update:
        scored = {m: r["psnr"] for m, r in sorted(current.items()) if r.get("status") != "error" and "psnr" in r}
        errors = sorted(m for m, r in current.items() if r.get("status") == "error")
        # Rendered, but the suite ships no reference to score them against.
        unscored = sorted(set(current) - set(scored) - set(errors))
        meta = {**run_meta, "crust": crust_revision()}
        if args.suite_rev:
            meta["suite"] = args.suite_rev
        args.baseline.write_text(json.dumps(
            {"meta": meta, "psnr": scored, "errors": errors, "unscored": unscored}, indent=1) + "\n")
        print(f"baseline: {len(scored)} scored, {len(unscored)} unscored, {len(errors)} errors -> {args.baseline}")
        return 0

    if not args.baseline.exists():
        print(f"no baseline at {args.baseline}; record one with --update", file=sys.stderr)
        return 2
    base = json.loads(args.baseline.read_text())
    for k in ("spp", "min_spp", "max_depth", "env_rotate", "self_shadow", "samples"):
        if k in base["meta"] and k in run_meta and base["meta"][k] != run_meta[k]:
            print(f"warning: baseline {k}={base['meta'][k]!r}, this run {k}={run_meta[k]!r}; "
                  "the comparison is not like for like", file=sys.stderr)

    regressed, broke, improved, fixed, new = [], [], [], [], []
    for m, r in sorted(current.items()):
        if r.get("status") == "skipped":
            continue
        errored = r.get("status") == "error"
        if m in base["psnr"]:
            if errored:
                broke.append((m, r.get("error")))
            elif "psnr" in r:
                d = value(r["psnr"]) - value(base["psnr"][m])
                if d < -args.tolerance:
                    regressed.append((m, base["psnr"][m], r["psnr"], d))
                elif d > args.tolerance:
                    improved.append((m, base["psnr"][m], r["psnr"], d))
        elif m in base.get("errors", []):
            if not errored:
                fixed.append(m)
        elif m in base.get("unscored", []) and not errored:
            continue
        elif not errored:
            new.append(m)
        else:
            broke.append((m, r.get("error")))
    missing = sorted(set(base["psnr"]) - set(current))

    def table(title, rows):
        if not rows:
            return
        print(f"\n{title} ({len(rows)}):")
        for m, b, c, d in sorted(rows, key=lambda x: x[3]):
            print(f"  {d:+7.2f} dB  {b!s:>7} -> {c!s:>7}  {m}")

    table("REGRESSED", regressed)
    if broke:
        print(f"\nERRORS ({len(broke)}):")
        for m, e in broke:
            print(f"  {m}: {e}")
    table("improved", improved)
    if fixed:
        print(f"\nnow render ({len(fixed)}): " + ", ".join(fixed))
    if new:
        print(f"\nnot in the baseline ({len(new)}): " + ", ".join(new))
    compared = len(current) - len(new)
    print(f"\n{compared} compared with {args.baseline.name} (crust {base['meta'].get('crust')}), "
          f"tolerance {args.tolerance} dB: {len(regressed)} regressed, {len(broke)} errors, "
          f"{len(improved)} improved" + (f"; {len(missing)} baseline materials not in this run" if missing else ""))
    return 1 if regressed or broke else 0


if __name__ == "__main__":
    sys.exit(main())
