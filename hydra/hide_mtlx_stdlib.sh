# Sourced by `pixi run` (pixi.toml [activation]). The openusd package's
# usdMtlx bakes the MaterialX standard library path, PXR_MATERIALX_STDLIB_DIR,
# with the length of conda-build's 255-character placeholder prefix; conda
# shortens the text on install and pads it with NULs, so the std::string is
# `$CONDA_PREFIX/libraries` followed by ~167 NULs. Walking it, `opendir` stops
# at the first NUL and reopens `libraries` while the path grows behind it,
# until the stack overflows: any stage referencing a .mtlx (materialx_basic)
# segfaults on open. Without that directory the baked path is skipped, and
# usdMtlx falls back to the copy in its plugin resources
# (lib/usd/usdMtlx/resources/libraries), which Storm reads as well.
if [ -d "$CONDA_PREFIX/libraries" ]; then
    mv "$CONDA_PREFIX/libraries" "$CONDA_PREFIX/libraries.baked-path-hidden" 2>/dev/null || true
fi
