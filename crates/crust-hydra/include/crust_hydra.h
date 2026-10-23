/*
 * crust_hydra.h: the C ABI of crust-hydra, which hdCrust (the Hydra render
 * delegate, hydra/hdCrust) links as a static library.
 *
 * Written by hand and kept in step with crates/crust-hydra/src/ffi.rs: a Rust
 * test parses CRUST_HYDRA_ABI_VERSION from this file and checks it against
 * crust_hydra_abi_version(). Change a signature or a status, bump the
 * version in both.
 *
 * Contracts every call relies on:
 *   - a session handle comes from crust_hydra_new and is passed to
 *     crust_hydra_free exactly once, after every other call on it returned;
 *   - string arguments are NUL-terminated UTF-8 and outlive the call;
 *   - output buffers hold the elements their size arguments describe;
 *   - calls on one handle may come from any thread. Calls that change the
 *     scene (load, apply, cancel) run one at a time, in the order they
 *     arrived. snapshot, converged and aov never wait for an import.
 *
 * No call unwinds or aborts: a panic inside crust returns
 * CRUST_HYDRA_PANICKED, its message kept for crust_hydra_last_error. The
 * session drops its scene, and the next apply imports the layer again.
 */
#ifndef CRUST_HYDRA_H
#define CRUST_HYDRA_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define CRUST_HYDRA_ABI_VERSION 2

typedef enum crust_hydra_status {
    CRUST_HYDRA_OK = 0,
    /* Not yet: no image published, a render not complete, or an image of
       another size than the buffer (the render for the new size has not
       published). Try again later. */
    CRUST_HYDRA_BUSY = 1,
    /* The layer could not be opened or imported. The last image stays. */
    CRUST_HYDRA_IMPORT_FAILED = 2,
    /* crust panicked. The next apply imports the layer again. */
    CRUST_HYDRA_PANICKED = 3,
    /* A null or non-UTF-8 argument, an unknown AOV, a wrong buffer shape, or
       nothing loaded yet. */
    CRUST_HYDRA_INVALID_ARGUMENT = 4
} crust_hydra_status;

typedef struct CrustHydraSession CrustHydraSession;

/* level: 0 error, 1 warning, 2 info, 3 debug. message is valid only during
   the call, which may come from any crust thread. */
typedef void (*crust_hydra_log_fn)(int32_t level, const char *message, void *user);

/* The library's CRUST_HYDRA_ABI_VERSION: refuse to run on a mismatch. */
uint32_t crust_hydra_abi_version(void);

/* A session on the layers in session_dir (created if missing). log may be
   null. Returns null on error, reported through log when there is one. */
CrustHydraSession *crust_hydra_new(const char *session_dir, crust_hydra_log_fn log,
                                   void *user);

/* Cancels the render, joins crust's threads, frees the handle. Null is
   ignored. */
void crust_hydra_free(CrustHydraSession *session);

/* Imports the override layer at edits_path whole, as `crust render -i
   <edits_path>` would, and starts rendering it: the first population and
   every time change. */
crust_hydra_status crust_hydra_load(CrustHydraSession *session, const char *edits_path);

/* The override layer changed on disk: applies the change in place when it is
   confined to settings, lights or materials, else imports the layer, then
   restarts the render. */
crust_hydra_status crust_hydra_apply(CrustHydraSession *session);

/* When the latest image's generation is not `since` (the last one copied, 0
   for none), copies it into rgba (width * height RGBA floats, rows top-down,
   alpha 1) and writes its generation and samples per pixel (either pointer
   may be null). An unchanged generation copies nothing and writes `since`
   back. Generations only grow, across restarts too. */
crust_hydra_status crust_hydra_snapshot(CrustHydraSession *session, float *rgba,
                                        size_t width, size_t height, uint64_t since,
                                        uint64_t *generation, uint32_t *spp);

/* How the last load or apply reached the scene: 0 settings, 1 materials,
   2 lights (each applied in place), 3 full (an import); -1 before the
   first. */
int32_t crust_hydra_applied(CrustHydraSession *session);

/* 1 once the render completed (Hydra's IsConverged), else 0. A cancelled
   render never converges. */
int32_t crust_hydra_converged(CrustHydraSession *session);

/* Copies AOV `name` (a RenderVar's layer name) of the completed render into
   data (width * height * channels floats, interleaved, rows top-down). BUSY
   until the render completes. */
crust_hydra_status crust_hydra_aov(CrustHydraSession *session, const char *name,
                                   float *data, size_t width, size_t height,
                                   size_t channels);

/* Cancels the running render and waits for it to stop. The last image
   stays. */
void crust_hydra_cancel(CrustHydraSession *session);

/* Forwards log lines down to level (0 error, 1 warning, the default, 2 info,
   3 debug) to the session's log callback. Import warnings arrive at 1, once
   per import with their code and prim; panics at 0. */
void crust_hydra_set_log_level(CrustHydraSession *session, int32_t level);

/* Copies the last error's message into buffer (len bytes, NUL-terminated,
   truncated to fit; null or 0 copies nothing). Returns its full length in
   bytes without the NUL, as snprintf does. */
size_t crust_hydra_last_error(CrustHydraSession *session, char *buffer, size_t len);

#ifdef __cplusplus
}
#endif

#endif /* CRUST_HYDRA_H */
