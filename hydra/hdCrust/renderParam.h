#ifndef HDCRUST_RENDER_PARAM_H
#define HDCRUST_RENDER_PARAM_H

#include <pxr/base/gf/matrix4d.h>
#include <pxr/base/gf/vec3f.h>
#include <pxr/base/tf/token.h>
#include <pxr/base/vt/array.h>
#include <pxr/imaging/hd/renderDelegate.h>
#include <pxr/pxr.h>
#include <pxr/usd/sdf/path.h>

#include <map>
#include <mutex>

PXR_NAMESPACE_OPEN_SCOPE

/// One synced mesh, in object space. The render pass bakes `xform` (and
/// each instance transform) into world-space vertices when it feeds the
/// crust scene — the Phase 1 C API has no instance object yet, so
/// instancing is flattened here (the documented Phase 2 seam).
struct HdCrustCachedMesh {
    VtVec3fArray points;
    VtVec3iArray triangles;      // from HdMeshUtil, indexing `points`
    VtVec3fArray normals;        // vertex-interpolated, or empty
    GfMatrix4d xform{1.0};
    VtMatrix4dArray instanceXforms; // empty = one placement at `xform`
    GfVec3f displayColor{0.5f, 0.5f, 0.5f};
    int primId = 0;              // Hydra's rprim id, for the primId AOV
    bool visible = true;
};

/// One synced light, parameters as UsdLux authors them; the render pass
/// converts to crust's radiance/irradiance conventions per type.
struct HdCrustCachedLight {
    TfToken type;                // HdPrimTypeTokens->{sphere,rect,distant,dome}Light
    GfMatrix4d xform{1.0};
    GfVec3f color{1.0f, 1.0f, 1.0f};
    float intensity = 1.0f;
    float exposure = 0.0f;
    float radius = 0.5f;         // sphereLight
    float width = 1.0f;          // rectLight
    float height = 1.0f;         // rectLight
    float angle = 0.53f;         // distantLight (angular diameter, degrees)
};

/// The rebuild spine: prims write their cached state here during Sync and
/// bump the scene version; the render pass compares versions and rebuilds
/// the crust scene from a snapshot when anything changed —
/// rebuild-everything-on-any-dirty-bit, exactly the Phase 1 contract of
/// docs/hydra_delegate.md.
class HdCrustRenderParam final : public HdRenderParam {
public:
    void UpdateMesh(SdfPath const& id, HdCrustCachedMesh mesh) {
        std::lock_guard<std::mutex> lock(_mutex);
        _meshes[id] = std::move(mesh);
        ++_version;
    }
    void RemoveMesh(SdfPath const& id) {
        std::lock_guard<std::mutex> lock(_mutex);
        if (_meshes.erase(id) > 0) {
            ++_version;
        }
    }
    void UpdateLight(SdfPath const& id, HdCrustCachedLight light) {
        std::lock_guard<std::mutex> lock(_mutex);
        _lights[id] = std::move(light);
        ++_version;
    }
    void RemoveLight(SdfPath const& id) {
        std::lock_guard<std::mutex> lock(_mutex);
        if (_lights.erase(id) > 0) {
            ++_version;
        }
    }

    int GetVersion() const {
        std::lock_guard<std::mutex> lock(_mutex);
        return _version;
    }

    /// A consistent copy for the render pass to build from, with the
    /// version it corresponds to.
    int Snapshot(std::map<SdfPath, HdCrustCachedMesh>* meshes,
                 std::map<SdfPath, HdCrustCachedLight>* lights) const {
        std::lock_guard<std::mutex> lock(_mutex);
        *meshes = _meshes;
        *lights = _lights;
        return _version;
    }

private:
    mutable std::mutex _mutex;
    std::map<SdfPath, HdCrustCachedMesh> _meshes;
    std::map<SdfPath, HdCrustCachedLight> _lights;
    int _version = 1;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif // HDCRUST_RENDER_PARAM_H
