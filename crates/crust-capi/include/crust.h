/* crust.h — the C ABI of Crust Render, for render-delegate hosts (hdCrust).
 *
 * Hand-written and kept in lockstep with crates/crust-capi/src/ by three
 * guards: the C smoke test compiles this header with -Wall -Wextra -Werror
 * and exercises every exported symbol; POD struct sizes are pinned by
 * paired static_asserts here and in Rust; and every extern "C" function in
 * Rust carries its C declaration in the doc comment directly above it.
 *
 * Conventions, once:
 *  - Every fallible function returns CrustStatus; out-parameters come last.
 *  - Framebuffer and AOV reads are BOTTOM-UP: row 0 is the BOTTOM scanline
 *    (OpenGL / Hydra render-buffer convention, and crust's native layout),
 *    indexed y*width + x. Flip yourself if you want display order.
 *  - Dome-light textures are the one asymmetric exception: equirect
 *    lat-long pixels with row 0 at the TOP (+Y pole), the texture-space
 *    convention environment maps are authored in.
 *  - Matrices are column-major, column-vector (OpenGL) convention. A USD
 *    GfMatrix4d (row-major, row-vector) passes element-for-element with NO
 *    transpose: transposing the storage and transposing the convention
 *    cancel out.
 *  - Thread safety: CrustScene and CrustRenderer are externally
 *    synchronized — at most one thread inside any function on a given
 *    handle at a time; distinct handles are independent. CrustStopToken is
 *    the ONE cross-thread type: its functions may be called from any thread
 *    at any time, including while a step on a renderer holding a clone of
 *    the token is in flight — that is its purpose. crust_renderer_step
 *    blocks and parallelizes internally.
 *  - All *_destroy functions accept NULL as a no-op.
 */
#ifndef CRUST_H
#define CRUST_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* ---- version ---------------------------------------------------------- */

/* Bumped on any ABI-incompatible change to this header. */
#define CRUST_API_VERSION 1u

/* The API version the library was built against; check == CRUST_API_VERSION
 * at startup. */
uint32_t crust_api_version(void);

/* The crust-render release (semver) behind the library. */
void crust_library_version(uint32_t* major, uint32_t* minor, uint32_t* patch);

/* ---- status ------------------------------------------------------------ */

typedef enum CrustStatus {
  CRUST_OK = 0,
  CRUST_ERROR_NULL_ARGUMENT = 1,    /* a required pointer was NULL          */
  CRUST_ERROR_INVALID_ARGUMENT = 2, /* zero/oversized count, non-finite
                                     * float, malformed enum or texture     */
  CRUST_ERROR_INVALID_CAMERA = 3,   /* non-invertible matrices, orthographic
                                     * projection, bad focus distance       */
  CRUST_ERROR_BAD_STATE = 4,        /* builder used after commit, or commit
                                     * without camera/settings              */
  CRUST_ERROR_BUFFER_TOO_SMALL = 5, /* read capacity < width*height pixels  */
} CrustStatus;

/* Static string for logging; never NULL. Thread-safe. */
const char* crust_status_string(CrustStatus status);

/* ---- opaque handles ----------------------------------------------------- */

typedef struct CrustScene CrustScene;         /* scene builder              */
typedef struct CrustRenderer CrustRenderer;   /* committed, steppable render */
typedef struct CrustStopToken CrustStopToken; /* cross-thread cancellation  */
typedef struct CrustGeoCache CrustGeoCache;   /* cross-rebuild prototype cache */

/* ---- stop token --------------------------------------------------------- */

CrustStopToken* crust_stop_token_create(void);
/* Idempotent; a stopped token is PERMANENT (there is no un-stop). Create a
 * fresh token per committed scene. */
void crust_stop_token_stop(CrustStopToken* token);
bool crust_stop_token_is_stopped(const CrustStopToken* token);
void crust_stop_token_destroy(CrustStopToken* token);

/* ---- geometry cache -------------------------------------------------------
 * What makes edits cheap: a mesh's triangles (and their acceleration
 * structure) are committed once and cached across scene rebuilds, keyed by
 * an opaque caller-chosen key (e.g. a prim path hash) plus a content
 * version the caller bumps when the geometry itself changes. A rebuild
 * whose meshes all hit the cache pays only the top-level build over
 * instance bounds. Thread-safe (internally synchronized), like the stop
 * token; prototypes still referenced by live renderers survive removal. */

CrustGeoCache* crust_geo_cache_create(void);
bool crust_geo_cache_contains(const CrustGeoCache* cache, uint64_t key,
                              uint32_t version);
/* Call when a prim is deleted so its prototype can be reclaimed. */
void crust_geo_cache_remove(CrustGeoCache* cache, uint64_t key);
void crust_geo_cache_clear(CrustGeoCache* cache);
void crust_geo_cache_destroy(CrustGeoCache* cache);

/* ---- material ----------------------------------------------------------- */

/* A portable subset of the OpenPBR übershader (crust's single surface
 * shader). Unset lobes keep OpenPBR defaults. All colors linear RGB. */
typedef struct CrustMaterial {
  float base_color[3];
  float metalness;          /* 0 dielectric .. 1 metal                     */
  float roughness;          /* specular/microfacet roughness, 0..1         */
  float ior;                /* dielectric IOR (default 1.5)                */
  float transmission;       /* 0 opaque .. 1 fully transmissive (glass)    */
  float opacity;            /* geometric presence, 0..1                    */
  float emission_color[3];
  float emission_luminance; /* nits; 0 = not emissive                      */
  int32_t thin_walled;      /* nonzero: thin sheet, no interior            */
  int32_t reserved_;        /* keep zeroed                                 */
} CrustMaterial;

#if !defined(__cplusplus)
_Static_assert(sizeof(CrustMaterial) == 56, "CrustMaterial ABI drift");
#else
static_assert(sizeof(CrustMaterial) == 56, "CrustMaterial ABI drift");
#endif

/* Fill with defaults (grey diffuse: the OpenPBR default surface). */
void crust_material_default(CrustMaterial* out);

/* ---- render settings ----------------------------------------------------- */

typedef struct CrustRenderSettings {
  uint32_t width, height;         /* pixels; 1..=65536 each                */
  uint32_t samples_per_pixel;     /* full budget; >= 1                     */
  uint32_t max_depth;             /* path length bound; >= 1               */
  uint32_t min_samples_per_pixel; /* adaptive-stop floor                   */
  float variance_threshold;       /* relative std-error target; 0 disables
                                   * adaptive early stop                   */
  uint32_t frame;                 /* seeds the sampler (animation frame)   */
} CrustRenderSettings;

#if !defined(__cplusplus)
_Static_assert(sizeof(CrustRenderSettings) == 28, "CrustRenderSettings ABI drift");
#else
static_assert(sizeof(CrustRenderSettings) == 28, "CrustRenderSettings ABI drift");
#endif

/* 640x360, 64 spp, depth 8, adaptive off. */
void crust_render_settings_default(CrustRenderSettings* out);

/* ---- scene builder -------------------------------------------------------
 * Single-threaded. After a successful commit the builder is SPENT: further
 * add/set calls return CRUST_ERROR_BAD_STATE; the handle itself must still
 * be destroyed. Rebuilds (any scene edit) mean: destroy the renderer,
 * build a fresh scene, commit again — crust has no incremental edits yet. */

CrustScene* crust_scene_create(void);
void crust_scene_destroy(CrustScene* scene);

/* Triangles only — Hydra hosts triangulate with HdMeshUtil, which also
 * yields the triangle->authored-face mapping picking wants.
 *   positions:  3 floats per vertex, world space.
 *   tri_indices: 3 uint32 per triangle. Out-of-range indices are skipped
 *                by the kernel (they cannot crash it), but are a modeling
 *                error.
 *   normals_or_null: optional per-vertex shading normals, 3 floats per
 *                vertex, exactly vertex_count of them.
 * out_geom_id receives the id the id-AOV reports for this mesh. */
CrustStatus crust_scene_add_mesh(CrustScene* scene,
                                 const float* positions, size_t vertex_count,
                                 const uint32_t* tri_indices, size_t triangle_count,
                                 const float* normals_or_null,
                                 const CrustMaterial* material,
                                 uint32_t* out_geom_id);

CrustStatus crust_scene_add_sphere(CrustScene* scene,
                                   const float center[3], float radius,
                                   const CrustMaterial* material,
                                   uint32_t* out_geom_id);

/* An OBJECT-SPACE mesh placed by xform (column-major doubles, same
 * convention as the camera matrices — a GfMatrix4d passes untransposed),
 * with its triangles cached in `cache` under (key, version):
 *  - on a cache HIT the vertex arrays are never read and may be NULL —
 *    check crust_geo_cache_contains first to skip marshalling entirely;
 *  - on a MISS the arrays are required (as in crust_scene_add_mesh, but
 *    object space; normals transform correctly through the placement) and
 *    the committed prototype is stored for every later rebuild.
 * The placement must be invertible: a singular xform (e.g. zero scale, the
 * common "hide this" idiom) returns CRUST_ERROR_INVALID_ARGUMENT and the
 * caller skips the placement. Each call adds one placement and returns its
 * geom_id; N placements of one prototype share the cached triangles. */
CrustStatus crust_scene_add_instance(CrustScene* scene, CrustGeoCache* cache,
                                     uint64_t key, uint32_t version,
                                     const float* positions_or_null,
                                     size_t vertex_count,
                                     const uint32_t* tri_indices_or_null,
                                     size_t triangle_count,
                                     const float* normals_or_null,
                                     const double xform[16],
                                     const CrustMaterial* material,
                                     uint32_t* out_geom_id);

/* Lights. Geometry-backed lights (sphere, rect) follow crust's engine
 * convention internally: their emissive geometry is hidden from camera
 * rays (a light in frame does not show its source) but visible to shadow
 * and indirect rays. radiance is linear RGB, W·sr⁻¹·m⁻². */
CrustStatus crust_scene_add_sphere_light(CrustScene* scene,
                                         const float center[3], float radius,
                                         const float radiance[3]);

/* Rect area light spanning origin -> origin+edge_u -> origin+edge_u+edge_v
 * -> origin+edge_v, emitting along normalize(edge_u x edge_v) only. */
CrustStatus crust_scene_add_rect_light(CrustScene* scene,
                                       const float origin[3],
                                       const float edge_u[3],
                                       const float edge_v[3],
                                       const float radiance[3]);

/* direction: from the light toward the scene (world space). irradiance:
 * linear RGB W·m⁻² on a surface facing the light. angle_deg: the source's
 * angular DIAMETER (0.53 = the sun); tiny angles are widened to a floor
 * rather than made singular. */
CrustStatus crust_scene_add_distant_light(CrustScene* scene,
                                          const float direction[3],
                                          const float irradiance[3],
                                          float angle_deg);

/* Infinite environment dome; replaces the built-in sky gradient.
 *   tint: linear RGB multiplier (the whole radiance when untextured).
 *   pixels_or_null: optional equirect lat-long RGB f32 texture,
 *     tex_width*tex_height*3 floats, ROW 0 AT THE TOP (+Y pole) — the
 *     texture convention, NOT the framebuffer one.
 *   rotation: 3x3 column-major dome-to-world rotation ({1,0,0,0,1,0,0,0,1}
 *     for identity). A dome is at infinity: translation/scale are
 *     meaningless and not accepted. */
CrustStatus crust_scene_add_dome_light(CrustScene* scene,
                                       const float tint[3],
                                       uint32_t tex_width, uint32_t tex_height,
                                       const float* pixels_or_null,
                                       const float rotation[9]);

/* view: world-to-view. proj: any perspective projection (GL [-1,1] z,
 * DirectX [0,1] z, reverse-Z, off-axis all accepted; orthographic is
 * rejected with CRUST_ERROR_INVALID_CAMERA). Both column-major
 * column-vector — pass GfMatrix4d::data() straight through, no transpose.
 * aperture: lens diameter in world units, 0 = pinhole. focus_distance:
 * distance to the focal plane, > 0. */
CrustStatus crust_scene_set_camera(CrustScene* scene,
                                   const double view[16], const double proj[16],
                                   float aperture, float focus_distance);

CrustStatus crust_scene_set_render_settings(CrustScene* scene,
                                            const CrustRenderSettings* settings);

/* Builds the acceleration structure and produces the steppable renderer.
 * Camera and settings must have been set. token_or_null is CLONED — the
 * caller keeps ownership and may stop/destroy it independently; NULL means
 * this render cannot be cancelled mid-step. On success the builder is
 * spent (see above). */
CrustStatus crust_scene_commit(CrustScene* scene,
                               const CrustStopToken* token_or_null,
                               CrustRenderer** out_renderer);

/* ---- renderer ------------------------------------------------------------ */

typedef enum CrustStepStatus {
  CRUST_STEP_IN_PROGRESS = 0, /* chunk done, budget remains               */
  CRUST_STEP_STOPPED = 1,     /* token fired mid-chunk; image coherent;
                               * stepping later resumes exactly there      */
  CRUST_STEP_COMPLETE = 2,    /* full budget rendered; step is a no-op    */
} CrustStepStatus;

/* Advance the render by (up to) spp more samples per pixel. Blocking;
 * honors the commit-time stop token at row/tile granularity. Driving this
 * once per Hydra Execute call yields progressive refinement: read the
 * framebuffer after each step. A render stepped to completion is
 * bit-identical to the same scene rendered in one shot, whatever the chunk
 * sizes. */
CrustStatus crust_renderer_step(CrustRenderer* renderer, uint32_t spp,
                                CrustStepStatus* out_status,
                                uint32_t* out_spp_done);

bool crust_renderer_is_converged(const CrustRenderer* renderer);
uint32_t crust_renderer_spp_done(const CrustRenderer* renderer);
void crust_renderer_get_dimensions(const CrustRenderer* renderer,
                                   uint32_t* width, uint32_t* height);

/* All reads: bottom-up rows (see header comment), capacity in PIXELS,
 * capacity >= width*height or CRUST_ERROR_BUFFER_TOO_SMALL. Valid at any
 * time: black before the first step, the coherent partial image after a
 * stopped one. The first AOV read (any of the four) runs one primary-hit
 * probe pass and caches all four planes; later reads are copies. */

/* 4 floats per pixel, linear RGBA. Alpha is primary-hit coverage (1 hit /
 * 0 miss), point-sampled, not filtered. */
CrustStatus crust_renderer_read_color(CrustRenderer* renderer,
                                      float* rgba, size_t capacity_px);

/* 1 float per pixel: camera-forward hit distance; +INFINITY on miss.
 * Convert to your own depth convention with your projection matrix. */
CrustStatus crust_renderer_read_aov_depth(CrustRenderer* renderer,
                                          float* depth, size_t capacity_px);

/* 3 floats per pixel: outward world-space normal; (0,0,0) on miss. */
CrustStatus crust_renderer_read_aov_normal(CrustRenderer* renderer,
                                           float* xyz, size_t capacity_px);

/* 2 uint32 per pixel: [geom_id, prim_id] of the primary hit; UINT32_MAX on
 * miss. geom_id matches the ids returned at scene build time. */
CrustStatus crust_renderer_read_aov_id(CrustRenderer* renderer,
                                       uint32_t* geom_prim, size_t capacity_px);

/* 1 float per pixel: 1.0 hit / 0.0 miss. */
CrustStatus crust_renderer_read_aov_alpha(CrustRenderer* renderer,
                                          float* alpha, size_t capacity_px);

/* ---- in-place edits --------------------------------------------------------
 * Both restart sampling from zero (a film cannot survive a camera or
 * resolution change) WITHOUT touching the world or its acceleration
 * structure — this is the cheap path for viewport orbits. Reads made after
 * an edit see the restarted render. A stopped token is permanent, so pass
 * a fresh token to keep the restarted render cancellable; NULL keeps the
 * current one (stopped or not). */

CrustStatus crust_renderer_update_camera(CrustRenderer* renderer,
                                         const double view[16],
                                         const double proj[16],
                                         float aperture, float focus_distance,
                                         const CrustStopToken* token_or_null);

CrustStatus crust_renderer_update_settings(CrustRenderer* renderer,
                                           const CrustRenderSettings* settings,
                                           const CrustStopToken* token_or_null);

void crust_renderer_destroy(CrustRenderer* renderer);

#ifdef __cplusplus
} /* extern "C" */
#endif

#endif /* CRUST_H */
