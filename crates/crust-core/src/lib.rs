//! The engine as one library: renderer, integrator, materials, lights,
//! volumes, path guiding and USD import. It holds no code of its own: it
//! re-exports the six crates the engine is built from, each a layer that
//! imports only from the ones below it (`docs/architecture.md` § Layering),
//! so applications, the CLI and the integration tests keep naming everything
//! `crust_core::…`.
//!
//! | crate | layer |
//! |-------|-------|
//! | `crust-base` | configuration, names, warnings, reports, statistics, colour, textures, errors, rays |
//! | `crust-shading` | materials, light path expressions |
//! | `crust-world` | the `World` over the kernel, lights and their selection |
//! | `crust-integrator` | `Renderer`, the path tracer, AOVs, filters, samplers, guiding, volumes, the camera |
//! | `crust-usd` | USD import, the procedural scene, in-place updates |
//! | `crust-tools` | the diagnostic, the light path inspector, `crust diff`, the stamp, `crust check` |
#![forbid(unsafe_code)]

pub use crust_base::*;
pub use crust_integrator::*;
pub use crust_shading::*;
pub use crust_tools::*;
pub use crust_usd::*;
pub use crust_world::*;
