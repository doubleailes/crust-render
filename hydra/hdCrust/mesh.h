#ifndef HDCRUST_MESH_H
#define HDCRUST_MESH_H

#include <pxr/imaging/hd/mesh.h>
#include <pxr/pxr.h>

PXR_NAMESPACE_OPEN_SCOPE

/// An HdMesh whose Sync does exactly one thing: write the prim's current
/// state (triangulated topology, points, vertex normals, transform,
/// displayColor, flattened instance transforms) into the render param's
/// scene cache and bump the scene version. All rendering happens later,
/// when the render pass rebuilds the crust scene from that cache.
class HdCrustMesh final : public HdMesh {
public:
    explicit HdCrustMesh(SdfPath const& id);
    ~HdCrustMesh() override = default;

    HdDirtyBits GetInitialDirtyBitsMask() const override;

    void Sync(HdSceneDelegate* sceneDelegate, HdRenderParam* renderParam,
              HdDirtyBits* dirtyBits, TfToken const& reprToken) override;

    void Finalize(HdRenderParam* renderParam) override;

protected:
    void _InitRepr(TfToken const& reprToken, HdDirtyBits* dirtyBits) override;
    HdDirtyBits _PropagateDirtyBits(HdDirtyBits bits) const override;

private:
    /// The (path-hash, geoVersion) pair keys the crust geometry cache:
    /// bumped only when a Sync saw geometry-changing dirty bits, so
    /// transform/material/instancing edits keep hitting the committed
    /// prototype.
    uint32_t _geoVersion = 0;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif // HDCRUST_MESH_H
