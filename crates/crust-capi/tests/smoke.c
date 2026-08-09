/* The C half of the ABI guard: compiles the real crust.h with
 * -Wall -Wextra -Werror, links the real cdylib, and exercises every
 * exported symbol at least once. Mirrors tests/abi.rs: a lit quad,
 * stepped to completion twice, asserting nonzero deterministic pixels.
 * Run via scripts/test_capi_c.sh. */

#include <assert.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "crust.h"

#define W 32u
#define H 32u
#define SPP 8u
#define PX (W * H)

#define CHECK(expr)                                                          \
  do {                                                                       \
    CrustStatus status_ = (expr);                                            \
    if (status_ != CRUST_OK) {                                               \
      fprintf(stderr, "%s:%d: %s -> %s\n", __FILE__, __LINE__, #expr,        \
              crust_status_string(status_));                                 \
      exit(1);                                                               \
    }                                                                        \
  } while (0)

/* Camera at (0,0,3) looking down -Z; column-major world-to-view. */
static const double kView[16] = {
    1.0, 0.0, 0.0, 0.0,  /* col 0 */
    0.0, 1.0, 0.0, 0.0,  /* col 1 */
    0.0, 0.0, 1.0, 0.0,  /* col 2 */
    0.0, 0.0, -3.0, 1.0, /* col 3 */
};

static void gl_perspective(double out[16]) {
  const double pi = 3.14159265358979323846;
  const double f = 1.0 / tan((45.0 * pi / 180.0) / 2.0);
  const double n = 0.1, fa = 100.0;
  memset(out, 0, 16 * sizeof(double));
  out[0] = f;
  out[5] = f;
  out[10] = -(fa + n) / (fa - n);
  out[11] = -1.0;
  out[14] = -2.0 * fa * n / (fa - n);
}

static CrustRenderer* build_and_commit(const CrustStopToken* token,
                                       uint32_t* out_quad_id) {
  CrustScene* scene = crust_scene_create();
  assert(scene != NULL);

  CrustMaterial material;
  crust_material_default(&material);
  material.base_color[0] = 0.8f;
  material.base_color[1] = 0.4f;
  material.base_color[2] = 0.2f;

  /* Two triangles spanning [-0.6, 0.6]^2 at z = 0. */
  const float positions[12] = {
      -0.6f, -0.6f, 0.0f, /**/ 0.6f, -0.6f, 0.0f,
      0.6f,  0.6f,  0.0f, /**/ -0.6f, 0.6f, 0.0f,
  };
  const uint32_t indices[6] = {0, 1, 2, 0, 2, 3};
  CHECK(crust_scene_add_mesh(scene, positions, 4, indices, 2, NULL, &material,
                             out_quad_id));

  const float center[3] = {0.0f, 0.0f, 2.0f};
  const float radiance[3] = {12.0f, 12.0f, 12.0f};
  CHECK(crust_scene_add_sphere_light(scene, center, 0.5f, radiance));

  /* Touch the remaining light types (their math is pinned in Rust tests;
   * here we prove the symbols and argument marshalling). */
  const float dir[3] = {0.0f, -1.0f, -0.2f};
  const float irradiance[3] = {0.05f, 0.05f, 0.05f};
  CHECK(crust_scene_add_distant_light(scene, dir, irradiance, 0.53f));
  const float rect_origin[3] = {-2.0f, 2.0f, 1.0f};
  const float edge_u[3] = {0.5f, 0.0f, 0.0f};
  const float edge_v[3] = {0.0f, 0.0f, -0.5f};
  const float rect_radiance[3] = {0.5f, 0.5f, 0.5f};
  CHECK(crust_scene_add_rect_light(scene, rect_origin, edge_u, edge_v,
                                   rect_radiance));
  const float tint[3] = {0.02f, 0.02f, 0.03f};
  const float identity9[9] = {1, 0, 0, 0, 1, 0, 0, 0, 1};
  CHECK(crust_scene_add_dome_light(scene, tint, 0, 0, NULL, identity9));

  /* One extra sphere so add_sphere is exercised too. */
  CrustMaterial mirror;
  crust_material_default(&mirror);
  mirror.metalness = 1.0f;
  mirror.roughness = 0.1f;
  const float sphere_center[3] = {1.5f, 0.0f, 0.0f};
  uint32_t sphere_id = 0;
  CHECK(crust_scene_add_sphere(scene, sphere_center, 0.4f, &mirror, &sphere_id));

  double proj[16];
  gl_perspective(proj);
  CHECK(crust_scene_set_camera(scene, kView, proj, 0.0f, 3.0f));

  CrustRenderSettings settings;
  crust_render_settings_default(&settings);
  settings.width = W;
  settings.height = H;
  settings.samples_per_pixel = SPP;
  settings.max_depth = 4;
  CHECK(crust_scene_set_render_settings(scene, &settings));

  CrustRenderer* renderer = NULL;
  CHECK(crust_scene_commit(scene, token, &renderer));
  assert(renderer != NULL);

  /* A committed scene is spent. */
  uint32_t dummy = 0;
  CrustMaterial grey;
  crust_material_default(&grey);
  const float origin[3] = {0, 0, 0};
  assert(crust_scene_add_sphere(scene, origin, 1.0f, &grey, &dummy) ==
         CRUST_ERROR_BAD_STATE);
  crust_scene_destroy(scene);
  return renderer;
}

static void render_to_completion(CrustRenderer* renderer, float* rgba) {
  CrustStepStatus status = CRUST_STEP_IN_PROGRESS;
  uint32_t spp_done = 0;
  while (status != CRUST_STEP_COMPLETE) {
    CHECK(crust_renderer_step(renderer, 3, &status, &spp_done));
    assert(status != CRUST_STEP_STOPPED);
  }
  assert(spp_done == SPP);
  assert(crust_renderer_is_converged(renderer));
  assert(crust_renderer_spp_done(renderer) == SPP);
  CHECK(crust_renderer_read_color(renderer, rgba, PX));
}

/* Phase 2 surface: geometry cache, instanced placement, in-place edits,
 * file-based dome lights. */
static void exercise_phase2(void) {
  CrustGeoCache* cache = crust_geo_cache_create();
  assert(cache != NULL);
  assert(!crust_geo_cache_contains(cache, 42, 1));

  CrustScene* scene = crust_scene_create();
  CrustMaterial material;
  crust_material_default(&material);
  assert(material.coat_weight == 0.0f && material.coat_roughness == 0.0f);
  material.base_color[0] = 0.9f;
  material.coat_weight = 0.3f;

  const float positions[9] = {-0.5f, -0.5f, 0.0f, 0.5f, -0.5f, 0.0f,
                              0.0f,  0.5f,  0.0f};
  const uint32_t indices[3] = {0, 1, 2};
  const double identity[16] = {1, 0, 0, 0, 0, 1, 0, 0,
                               0, 0, 1, 0, 0, 0, 0, 1};
  uint32_t id_a = 0, id_b = 0;
  /* Miss populates the cache; hit places the shared prototype with NULL
   * arrays. */
  CHECK(crust_scene_add_instance(scene, cache, 42, 1, positions, 3, indices,
                                 1, NULL, identity, &material, &id_a));
  assert(crust_geo_cache_contains(cache, 42, 1));
  const double shifted[16] = {1, 0, 0, 0, 0, 1, 0, 0,
                              0, 0, 1, 0, 1.5, 0, 0, 1};
  CHECK(crust_scene_add_instance(scene, cache, 42, 1, NULL, 0, NULL, 0, NULL,
                                 shifted, &material, &id_b));
  assert(id_b == id_a + 1);

  /* Dome-light file: a missing path is a clean error, not a crash. */
  const float tint[3] = {0.2f, 0.2f, 0.2f};
  const float identity9[9] = {1, 0, 0, 0, 1, 0, 0, 0, 1};
  assert(crust_scene_add_dome_light_file(scene, tint, "/no/such/file.exr",
                                         identity9) ==
         CRUST_ERROR_INVALID_ARGUMENT);
  CHECK(crust_scene_add_dome_light(scene, tint, 0, 0, NULL, identity9));

  double proj[16];
  gl_perspective(proj);
  CHECK(crust_scene_set_camera(scene, kView, proj, 0.0f, 3.0f));
  CrustRenderSettings settings;
  crust_render_settings_default(&settings);
  settings.width = 16;
  settings.height = 16;
  settings.samples_per_pixel = 4;
  settings.max_depth = 4;
  CHECK(crust_scene_set_render_settings(scene, &settings));

  CrustRenderer* renderer = NULL;
  CHECK(crust_scene_commit(scene, NULL, &renderer));
  crust_scene_destroy(scene);

  CrustStepStatus status = CRUST_STEP_IN_PROGRESS;
  uint32_t done = 0;
  CHECK(crust_renderer_step(renderer, 2, &status, &done));

  /* In-place edits: camera shift and a resolution change both restart
   * sampling without a rebuild. */
  double view2[16];
  memcpy(view2, kView, sizeof(view2));
  view2[12] = 0.25; /* translate x */
  CrustStopToken* fresh = crust_stop_token_create();
  CHECK(crust_renderer_update_camera(renderer, view2, proj, 0.0f, 3.0f, fresh));
  assert(crust_renderer_spp_done(renderer) == 0);
  settings.width = 8;
  settings.height = 8;
  CHECK(crust_renderer_update_settings(renderer, &settings, NULL));
  uint32_t w = 0, h = 0;
  crust_renderer_get_dimensions(renderer, &w, &h);
  assert(w == 8 && h == 8);
  status = CRUST_STEP_IN_PROGRESS;
  while (status != CRUST_STEP_COMPLETE) {
    CHECK(crust_renderer_step(renderer, 2, &status, &done));
  }
  static float small[8 * 8 * 4];
  CHECK(crust_renderer_read_color(renderer, small, 8 * 8));
  for (size_t i = 0; i < 8 * 8 * 4; i++) assert(!isnan(small[i]));

  crust_stop_token_destroy(fresh);
  crust_renderer_destroy(renderer);
  crust_geo_cache_remove(cache, 42);
  assert(!crust_geo_cache_contains(cache, 42, 1));
  crust_geo_cache_clear(cache);
  crust_geo_cache_destroy(cache);
  crust_geo_cache_destroy(NULL); /* NULL is a no-op */
  printf("smoke.c: phase 2 surface OK\n");
}

int main(void) {
  assert(crust_api_version() == CRUST_API_VERSION);
  uint32_t major = 0, minor = 0, patch = 0;
  crust_library_version(&major, &minor, &patch);
  printf("crust-capi %u.%u.%u (api %u)\n", major, minor, patch,
         crust_api_version());

  CrustStopToken* token = crust_stop_token_create();
  assert(token != NULL);
  assert(!crust_stop_token_is_stopped(token));

  uint32_t quad_id = 99;
  CrustRenderer* renderer = build_and_commit(token, &quad_id);
  assert(quad_id == 0);

  uint32_t width = 0, height = 0;
  crust_renderer_get_dimensions(renderer, &width, &height);
  assert(width == W && height == H);

  static float rgba_a[PX * 4], rgba_b[PX * 4];
  render_to_completion(renderer, rgba_a);

  /* AOVs: the quad covers the image center; the corner escapes. */
  static float depth[PX], normal[PX * 3], alpha[PX];
  static uint32_t ids[PX * 2];
  CHECK(crust_renderer_read_aov_depth(renderer, depth, PX));
  CHECK(crust_renderer_read_aov_normal(renderer, normal, PX));
  CHECK(crust_renderer_read_aov_id(renderer, ids, PX));
  CHECK(crust_renderer_read_aov_alpha(renderer, alpha, PX));
  const size_t center = 16 * W + 16, corner = 0;
  assert(alpha[center] == 1.0f && alpha[corner] == 0.0f);
  assert(fabsf(depth[center] - 3.0f) < 0.05f);
  assert(isinf(depth[corner]));
  assert(fabsf(normal[center * 3 + 2] - 1.0f) < 0.05f);
  assert(ids[center * 2] == quad_id);
  assert(ids[corner * 2] == UINT32_MAX && ids[corner * 2 + 1] == UINT32_MAX);
  assert(rgba_a[center * 4 + 3] == 1.0f && rgba_a[corner * 4 + 3] == 0.0f);

  /* Error paths give exact statuses. */
  assert(crust_renderer_read_color(renderer, rgba_b, 2) ==
         CRUST_ERROR_BUFFER_TOO_SMALL);
  CrustStepStatus step_status;
  assert(crust_renderer_step(NULL, 1, &step_status, NULL) ==
         CRUST_ERROR_NULL_ARGUMENT);
  crust_renderer_destroy(renderer);

  /* Determinism through the ABI: a second identical run is byte-equal. */
  uint32_t quad_id2 = 99;
  CrustRenderer* renderer2 = build_and_commit(NULL, &quad_id2);
  render_to_completion(renderer2, rgba_b);
  crust_renderer_destroy(renderer2);
  assert(memcmp(rgba_a, rgba_b, sizeof(rgba_a)) == 0);

  int nonzero = 0;
  for (size_t i = 0; i < PX * 4; i++) {
    if (rgba_a[i] > 0.0f) nonzero++;
    assert(!isnan(rgba_a[i]));
  }
  assert(nonzero > 0);

  /* Stop token: fired before stepping, the chunk is cut short; the token
   * is permanent. */
  crust_stop_token_stop(token);
  assert(crust_stop_token_is_stopped(token));
  uint32_t quad_id3 = 0;
  CrustRenderer* renderer3 = build_and_commit(token, &quad_id3);
  CrustStepStatus s3;
  uint32_t done3 = 42;
  CHECK(crust_renderer_step(renderer3, SPP, &s3, &done3));
  assert(s3 == CRUST_STEP_STOPPED && done3 == 0);
  assert(!crust_renderer_is_converged(renderer3));
  crust_renderer_destroy(renderer3);

  crust_stop_token_destroy(token);
  crust_stop_token_destroy(NULL); /* NULL is a no-op */
  crust_renderer_destroy(NULL);
  crust_scene_destroy(NULL);

  exercise_phase2();

  printf("smoke.c: all checks passed (%d nonzero channel values)\n", nonzero);
  return 0;
}
