# Design

## Vertices, not bounces

The walk in `tracer/path.rs` (`trace_path`) decrements `remaining` once per shaded
vertex. At the cap, the last bounce ray still collects the emission of the surface it
hits (the BSDF-sampled twin of that vertex's NEE) but never a light at infinity. So
depth `N` is direct lighting plus `N - 1` bounces of indirect light; `docs/light_sampling.md` already relies on `crust:maxDepth = 1` meaning one
light sample at the camera vertex.

## Refuse, do not clamp

0 is refused rather than clamped to 1: an author writing 0 most likely meant "direct
lighting only" from a bounce-counting renderer, but could as well have meant "no
indirect" or a typo; the default with a warning is what every other out-of-domain
`crust:*` setting does, and the warning names the value that would have meant direct
lighting. `RenderSettings::with_max_depth(0)` stays accepted in the API (a black frame
is a well-defined result there); only the authored attribute is checked.
