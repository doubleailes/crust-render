## Summary

<!-- What does this change do, and why? Link the issue it closes (`Closes #123`). -->

## Capability

<!-- Which design record(s) did you read and, if needed, update?
     openspec/specs/<capability>/design.md — e.g. rendering, materials, lighting,
     textures, aovs, usd-scene-import, intersection-kernel, cli, diagnostics, mcp-session.
     Behavioural changes go through openspec/changes/<name>/. -->

## Image impact

<!-- Pick one and give the evidence below. -->

- [ ] Bit-identical — goldens recorded from the base branch with `scripts/check_images.sh record <dir>`,
      then `scripts/check_images.sh check <dir>` on this branch reports `identical` for every scene
      (a `NO GOLDEN` line is skipped, not checked; the script renders at `-s 16` with the same
      settings on both sides, so no clamp override is needed for bit identity)
- [ ] Changed intentionally — paste the `crust diff` relmse against the base branch at two or more
      sample counts (e.g. 16, 64, 256), rendered with `--indirect-clamp 0` on both sides; the
      difference is noise only if relmse falls as 1/√N as spp grows, bias if it plateaus
- [ ] Not applicable (docs, tooling, CI)

## Performance

<!-- Only for changes that claim a speed or memory effect. Sequential timings lie:
     use `scripts/bench_ab.sh` (report min *and* mean), or callgrind instruction
     counts for steps under ~5%. -->

## Checklist

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace --no-fail-fast`
- [ ] Both halves of any pair in `docs/architecture.md` § Invariants changed together
      (NEE ↔ bounce weight, `Tri4` ↔ scalar, JIT ↔ interpreter, streamed ↔ preloaded, …)
- [ ] No new `unsafe`, no new RNG outside `openqmc`, no asset decoding in the engine crates
- [ ] Logging levels follow the rules (nothing per ray/pixel/sample; per-prim output is `DEBUG`;
      import WARNs use `warning!` with a `WarningCode`)
- [ ] User documentation in `site/` updated for any CLI flag, `CRUST_*` switch, `crust:*`
      attribute, design choice or limitation this touches
- [ ] A new `CRUST_*` switch has a `Config` field, a row in `docs/architecture.md`, and its
      "off" side is the old behaviour
