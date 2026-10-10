#!/usr/bin/env python3
"""Regenerate the elliptic-integral reference values the disk light's
spherical-ellipse sampler is checked against.

`crates/crust-core/src/light/ellipse.rs` implements Carlson's symmetric forms
`R_F` and `R_J` by the duplication algorithm, and the incomplete elliptic
integral of the third kind `Π(n; φ | m)` from them (Guillén et al. 2017,
§4.2). This evaluates the same three functions with mpmath at 50 digits, an
independent implementation, and writes them to
`crates/crust-core/tests/data/ellipse_oracle.txt`, which the module's tests
replay to a relative 1e-12.

The `(n, m, φ)` cases are the ones eq. 20 produces: for semi-axes
`1 > a ≥ b > 0` of a spherical ellipse, `n = (a² − b²) / (a² (1 − b²))` and
`m = a² n`, with `φ` anywhere in `[0, π/2]`. The edges are included: the
circle (`n = m = 0`), the near edge-on disk (`n → 1`) and the complete
integral (`φ = π/2`).

CI never runs this: the fixture is committed. Rerun it only to change the cases.

Requirements:
  pip install mpmath

Usage:
  scripts/ellipse_oracle.py [--out PATH] [--seed S]
"""

import argparse
import math
import os
import random

import mpmath as mp

mp.mp.dps = 50


def cases(rng):
    """(a, b, phi) triples covering eq. 20's range and its edges."""
    out = []
    edges_ab = [
        (0.5, 0.5),  # circle: n = m = 0
        (0.9, 0.9),
        (0.1, 0.1),
        (0.5, 0.5 * 1e-4),  # near edge-on: n -> 1
        (0.99, 0.99 * 1e-3),
        (0.999, 0.5),  # a -> 1: near the disk plane
        (0.9999, 0.9),
        (1e-3, 0.9e-3),  # tiny, far ellipse
        (0.7071, 0.7),  # near circular
    ]
    edges_phi = [0.0, 1e-6, 0.3, math.pi / 4, 1.2, math.pi / 2 - 1e-6, math.pi / 2]
    for a, b in edges_ab:
        for phi in edges_phi:
            out.append((a, b, phi))
    for _ in range(200):
        a = rng.uniform(1e-3, 0.9999)
        b = a * rng.choice([rng.uniform(1e-4, 1.0), rng.uniform(0.9, 1.0), rng.uniform(1e-4, 0.05)])
        phi = rng.uniform(0.0, math.pi / 2)
        out.append((a, b, phi))
    return out


def main():
    p = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    p.add_argument(
        "--out",
        default=os.path.join(
            os.path.dirname(__file__), "..", "crates", "crust-core", "tests", "data", "ellipse_oracle.txt"
        ),
    )
    p.add_argument("--seed", type=int, default=2017)
    args = p.parse_args()
    rng = random.Random(args.seed)

    lines = [
        "# Elliptic integrals for crust-core's light/ellipse.rs, from mpmath at 50 digits.",
        "# Regenerate with scripts/ellipse_oracle.py. One case per line:",
        "#   rf x y z value",
        "#   rj x y z p value",
        "#   pi n phi m value      (Π(n; φ | m) = ∫₀^φ dθ / ((1 − n sin²θ) √(1 − m sin²θ)))",
    ]
    for a, b, phi in cases(rng):
        a2, b2 = a * a, b * b
        n = (a2 - b2) / (a2 * (1.0 - b2))
        m = a2 * n
        s, c = math.sin(phi), math.cos(phi)
        x, y, z = c * c, 1.0 - m * s * s, 1.0
        pp = 1.0 - n * s * s
        # Carlson's R_F needs at most one zero argument; x = 0 at φ = π/2 is fine.
        rf = mp.elliprf(x, y, z)
        rj = mp.elliprj(x, y, z, pp)
        pi = mp.ellippi(n, phi, m)
        lines.append(f"rf {x!r} {y!r} {z!r} {mp.nstr(rf, 20)}")
        lines.append(f"rj {x!r} {y!r} {z!r} {pp!r} {mp.nstr(rj, 20)}")
        lines.append(f"pi {n!r} {phi!r} {m!r} {mp.nstr(pi, 20)}")
    with open(args.out, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"wrote {len(lines) - 5} cases to {os.path.normpath(args.out)}")


if __name__ == "__main__":
    main()
