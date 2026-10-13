## Summary

<!-- What does this change do, and why? Link the issue it closes (`Closes #123`). -->

## Capability

<!-- Which design record(s) did you read and, if needed, update?
     openspec/specs/<capability>/design.md — e.g. rendering, materials, lighting,
     textures, aovs, usd-scene-import, intersection-kernel, cli, diagnostics, mcp-session.
     Behavioural changes go through openspec/changes/<name>/. -->

## Image impact

<!-- Pick one and give the evidence. Compare at `-s 16` with `--indirect-clamp 0`. -->

- [ ] Bit-identical — `scripts/check_images.sh check <dir>` passes
- [ ] Changed intentionally — `crust diff` relmse below, and the difference is noise
      (falls as 1/√N across spp), not bias
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
- [ ] No new `unsafe`, no new RNG outside `openqmc`, no asset decoding in `crust-core`
- [ ] Logging levels follow the rules (nothing per ray/pixel/sample; per-prim output is `DEBUG`;
      import WARNs use `warning!` with a `WarningCode`)
- [ ] User documentation in `site/` updated for any CLI flag, `CRUST_*` switch, `crust:*`
      attribute, design choice or limitation this touches
- [ ] A new `CRUST_*` switch has a `Config` field, a row in `docs/architecture.md`, and its
      "off" side is the old behaviour
