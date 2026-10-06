+++
title = "A physically-based path tracer for USD"

# The homepage contents
[extra]
lead = '<b>Crust Render</b> is a path tracer written in safe Rust. It renders <b>USD</b> scenes (<code>.usda</code>, <code>.usdc</code>, <code>.usdz</code>) with an OpenPBR übershader, MaterialX look-dev graphs, UsdLux lights, volumes and path guiding.'
url = "/docs/getting-started/introduction/"
url_button = "Get started"
repo_version = "GitHub v0.6.0"
repo_license = "Open-source MIT License."
repo_url = "https://github.com/doubleailes/crust-render"

[[extra.list]]
title = "USD only"
content = 'Camera, geometry, lights, materials and render settings all come from the USD stage, read by the pure-Rust <a href="https://github.com/mxpv/openusd">openusd</a> crate. There is no other scene format.'

[[extra.list]]
title = "One material"
content = 'An <b>OpenPBR</b> übershader covers diffuse, metal, glass, coat, fuzz, thin film, subsurface and emission. <b>MaterialX</b> graphs and <code>UsdPreviewSurface</code> are read as well.'

[[extra.list]]
title = "Production scenes"
content = 'Streaming USD import, instancing, <code>.tx</code> and Ptex texture streaming let it load assets such as the Disney Moana Island and ALab.'

[[extra.list]]
title = "Configurable"
content = 'Every setting lives on the stage as a <code>crust:*</code> attribute. Command-line flags override them for one render, and <code>CRUST_*</code> environment variables switch optimizations off for A/B tests.'

[[extra.list]]
title = "Measurable"
content = '<code>--stats</code> and <code>--profile</code> print per-phase timings, memory and a per-section render profile.'

[[extra.list]]
title = "🦀 Safe Rust"
content = 'Every crate is <code>forbid(unsafe_code)</code> except two small, audited exceptions. The kernel, BVH, materials and importer are all written from scratch.'
+++
