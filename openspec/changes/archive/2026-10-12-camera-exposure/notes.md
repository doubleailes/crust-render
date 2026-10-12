# Working notes

## 4.1 Goldens

`scripts/check_images.sh record` with a clean build of `784ce32` (this change's
proposal commit, no code change), then `check` with the implementation: all 37
checked-in samples `identical`. `kitchen` and `kitchen_instanced` report `NO GOLDEN`
because `samples/Kitchen_set` is not in the checkout, as before this change. No
sample authors a camera exposure, so the scale-1 skip leaves every image as it was.

## 4.2 Intel New Sponza (main), camera 1

`NewSponza_Main_USD_Yup_003.usda` through a lighting overlay (the dataset ships no
lights: a dome with its `kloppenheim_05_4k.hdr` and a distant sun), `--camera
/root/PhysCamera001` (`exposure = 4.5`), 16 spp, `--indirect-clamp 0`,
`CRUST_STREAM_IMPORT=0`, before (`784ce32`) and after:

- every pixel of the after image is exactly 22.6274 times the before image, in all
  three channels: 2^4.5 = 22.627417 in `f32`, as C++ computes it;
- the after EXR records `crust:exposureScale = 22.627417`; the before EXR has no
  key, which `crust diff` reads as 1 and warns about (`crust:exposureScale differs
  (1 vs 22.627417): every radiance channel differs by their ratio`);
- `crust check` reports `exposure_scale 22.627417`.

## Typhoon (hdEmbree, OpenUSD `typhoon/main` `70c45e8`)

Read before finishing the design (`design.md` D6): Typhoon multiplies the Color AOV's
RGB by `HdCamera::GetLinearExposureScale()` as each sample is written
(`renderer/aov/aovOutput.cpp`), feeds adaptive sampling the unexposed colour
(`renderer/renderer.cpp`), and gates it behind `enableExposureCompensation` (default
on). That setting was added to this change as task 1.4.
