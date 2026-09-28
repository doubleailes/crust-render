# Third-party material

Crust Render is MIT-licensed (`LICENSE`). The files below carry data or code
derived from other projects, under the licences quoted here. Crate
dependencies are not listed; each carries its own licence in its crate.

## BSDL (Open Shading Language)

- **Used in:** `crates/crust-core/src/material/closure/bsdl_tables.rs`, the
  rough-dielectric reflection throughput table (`mtx::DielectricReflFront`) the
  MaterialX closure evaluator uses for `layer` throughput.
- **Source:** BSDL's `genluts.cpp`, run by
  `scripts/tables/bsdl_luts_to_rust.py`'s recipe on the copy of BSDL vendored in
  NVIDIA's OpenUSD `typhoon/main` branch at commit `70c45e8`
  (`pxr/imaging/plugin/hdEmbree/renderer/materials/BSDL`). Its output matches
  the table Typhoon ships bit for bit.
- **Licence:** BSD-3-Clause.

```
Copyright (c) 2009-present Contributors to the Open Shading Language project.
All Rights Reserved.

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are met:

1. Redistributions of source code must retain the above copyright notice, this
   list of conditions and the following disclaimer.

2. Redistributions in binary form must reproduce the above copyright notice,
   this list of conditions and the following disclaimer in the documentation
   and/or other materials provided with the distribution.

3. Neither the name of the copyright holder nor the names of its
   contributors may be used to endorse or promote products derived from
   this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO, THE
IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR CONTRIBUTORS BE LIABLE
FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL, EXEMPLARY, OR CONSEQUENTIAL
DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER
CAUSED AND ON ANY THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY,
OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.
```

## MaterialX

- **Used in:** `crates/crust-core/src/material/closure/mx.rs` — ports of
  MaterialX 1.39's GLSL BSDF library (`libraries/pbrlib/genglsl/lib/
  mx_microfacet*.glsl`: the GGX directional-albedo fit, Turquin energy
  compensation, the dielectric / conductor / Hoffman–Schlick and Airy thin-film
  Fresnel models, Oren–Nayar / EON / Burley diffuse and Imageworks sheen with
  their albedo fits). Also the surface-shader builders in
  `crates/crust-mtlx/src/surface.rs`, which reproduce the structure of
  `libraries/bxdf/{open_pbr_surface,standard_surface,gltf_pbr}.mtlx`, and the
  nodedef defaults they carry.
- **Source:** <https://github.com/AcademySoftwareFoundation/MaterialX>, 1.39.
- **Licence:** Apache License 2.0
  (<https://www.apache.org/licenses/LICENSE-2.0>).
  Copyright Contributors to the MaterialX Project.
