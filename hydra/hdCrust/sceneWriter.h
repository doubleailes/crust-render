// hdCrust: what Hydra presents, written back as USD layers crust imports
// (hydra-delegate design, D2-D4).
#ifndef HDCRUST_SCENE_WRITER_H
#define HDCRUST_SCENE_WRITER_H

#include "pxr/pxr.h"
#include "pxr/base/gf/matrix4d.h"
#include "pxr/imaging/hd/sceneIndex.h"
#include "pxr/base/vt/value.h"
#include "pxr/usd/sdf/layer.h"
#include "pxr/usd/sdf/path.h"
#include "pxr/usd/usd/common.h"

// On Windows, the Sdf / Usd headers above bring <windows.h>, whose
// `interface` macro (COM's `struct`) breaks the Hd material schemas that
// name a field `interface` (HdMaterialNetworkSchema). Nothing here is COM.
#ifdef interface
#undef interface
#endif

#include <map>
#include <string>

PXR_NAMESPACE_OPEN_SCOPE

/// The viewport, as the render pass state gives it.
struct HdCrustView
{
    GfMatrix4d worldToView{1.0};
    GfMatrix4d projection{1.0};
    int width = 0;
    int height = 0;

    bool operator==(HdCrustView const &o) const
    {
        return worldToView == o.worldToView && projection == o.projection && width == o.width
            && height == o.height;
    }
    bool operator!=(HdCrustView const &o) const { return !(*this == o); }
};

/// The prims the delegate writes for itself, beside the scene's.
struct HdCrustPaths
{
    static SdfPath const &Root();     // /__hdCrust
    static SdfPath const &Settings(); // /__hdCrust/settings
    static SdfPath const &Camera();   // /__hdCrust/camera
    static SdfPath const &Dicing();   // /__hdCrust/dicing
};

/// What the stage's own render settings say, as Hydra presents them: its
/// `crust:*` settings and the camera it renders through (empty: none).
struct HdCrustStageSettings
{
    std::map<TfToken, VtValue> crust;
    SdfPath camera;
};

/// The active RenderSettings prim's (the scene globals', else
/// `/Render/settings`) `crust:*` settings and camera; else the scene's
/// primary camera.
HdCrustStageSettings HdCrustReadStageSettings(HdSceneIndexBaseRefPtr const &sceneIndex);

/// The override layer's contents.
struct HdCrustEdits
{
    HdCrustView view;
    /// `crust:*` settings authored on `/__hdCrust/settings`: the stage's,
    /// with the host's renderer settings over them.
    std::map<TfToken, VtValue> settings;
    /// The camera geometry is diced for (`crust:dicingCamera`): the stage's
    /// render camera, or `/__hdCrust/dicing`, written from `dicingView`.
    SdfPath dicingCamera;
    HdCrustView dicingView;
    /// The changes since the scene was last written whole, as overs.
    SdfLayerRefPtr overs;
    /// The AOVs the viewport binds besides color (Hydra's names), which a
    /// RenderProduct asks crust for; only those `HdCrustAovSource` knows.
    TfTokenVector aovs;
};

/// The crust source and value type of Hydra's AOV `name` (`depth` is crust's
/// camera-space depth, as the spec says); false for an AOV crust does not
/// render.
bool HdCrustAovSource(TfToken const &name, TfToken *source, SdfValueTypeName *type,
                      size_t *components);

/// What a scene layer could not carry: one entry per kind ("light linking",
/// "prim type foo", ...), with the first prim that had it.
struct HdCrustSceneReport
{
    std::map<std::string, std::string> unsupported;
};

/// Writes the flattened scene layer at `path` from the terminal scene index:
/// one prim per Hydra prim crust reads, at its Hydra path, with its world
/// transform; instancers as PointInstancers with their prototypes under
/// them. Only visible prims are written, and only the gprims whose render
/// tag is in `renderTags` (empty: all). What the layer cannot carry is added
/// to `report`. False, with `error` set, when the layer cannot be written.
/// The layer it wrote is returned in `layer`, for the incremental path.
bool HdCrustWriteScene(HdSceneIndexBaseRefPtr const &sceneIndex, TfTokenVector const &renderTags,
                       std::string const &path, HdCrustSceneReport *report, SdfLayerRefPtr *layer,
                       std::string *error);

/// Writes the prims at `paths` into `stage` as the scene writer would (each
/// at the top level), the hidden ones added to `hidden` instead. False when
/// one cannot be written alone: an instancer or an instance's prototype,
/// which only a full write places.
bool HdCrustWritePrims(HdSceneIndexBaseRefPtr const &sceneIndex, TfTokenVector const &renderTags,
                       SdfPathVector const &paths, UsdStageRefPtr const &stage,
                       SdfPathVector *hidden, HdCrustSceneReport *report);

/// Adds to `overs` what changed between `scene` and the prims `scratch`
/// holds at `roots` (and below): typeless `over`s carrying only the values
/// that differ from what is in effect (overs, then scene), so the session's
/// diff sees value changes alone; an added prim as its definition. False,
/// with `why`, for a change an over cannot carry (a type changed, a property
/// removed): the scene must be written whole.
bool HdCrustOverlay(SdfLayerHandle const &scene, SdfLayerHandle const &overs,
                    SdfLayerHandle const &scratch, SdfPathVector const &roots, std::string *why);

/// Authors the prim at `path` inactive in `overs`: a removed or hidden prim.
void HdCrustDeactivate(SdfLayerHandle const &overs, SdfPath const &path);

/// Writes the override layer at `path`: `subLayers = [@./scene.usdc@]`, the
/// viewing camera, the dicing camera when it is the delegate's, and the
/// render settings that name them.
bool HdCrustWriteEdits(HdCrustEdits const &edits, std::string const &path, std::string *error);

PXR_NAMESPACE_CLOSE_SCOPE

#endif
