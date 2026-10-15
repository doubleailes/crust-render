## Why

Two opt-in strategies were built as experiments, measured, and lost at equal time
everywhere they were measured. They still cost code, a crate, tests and CI time.

- **`neural` light selection** (`neural-visibility-light-selection`) lost to `learned`
  by 1.5–5.3× and to `power` by 1.4–1.9× on every checked-in sample. It also lost on
  ALab, the scene it was built for: 0.79× `learned` on direct lighting and 0.69× on
  the full image (`docs/light_sampling.md` §3.14). Its design record's verdict was
  "retire".
- **`CRUST_DISK_SAMPLING=ellipse`** (`solid-angle-disk-and-tube-lights`) won only for
  a disk about as wide as its height over a diffuse floor. It lost on every glossy
  floor and on small disks, and cost 2–4× per frame (§3.13). It was off by default.

## What Changes

- Remove `LightSelection::Neural`, `light_visibility.rs`, the `crust-nn` crate, the
  `nvc_bench` example and `tests/neural_selection.rs`. The `learned` pre-pass walk
  loses the delivered-light labels only `neural` read; its contributions were the
  same bits either way.
- Remove `CRUST_DISK_SAMPLING`, `DiskSampling`, `light/ellipse.rs`,
  `scripts/ellipse_oracle.py` and its data. `AffineShape::with_sampling` takes the
  tube strategy alone.
- `--light-selection neural` is now rejected. `crust:lightSelection = "neural"` warns
  once and picks by `power`, as any unknown value does.
- No default changes: every render that does not opt in to either strategy is
  bit-identical.

## Capabilities

- `lighting`: the disk keeps area sampling only, and the four neural requirements go.
