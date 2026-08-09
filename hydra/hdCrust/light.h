#ifndef HDCRUST_LIGHT_H
#define HDCRUST_LIGHT_H

#include <pxr/imaging/hd/light.h>
#include <pxr/pxr.h>

PXR_NAMESPACE_OPEN_SCOPE

/// One HdLight covering the four supported UsdLux types (sphere, rect,
/// distant, dome), distinguished by the type token it was created with.
/// Sync caches the raw UsdLux parameters in the render param; the render
/// pass converts them to crust's per-type conventions when it rebuilds.
class HdCrustLight final : public HdLight {
public:
    HdCrustLight(SdfPath const& id, TfToken const& lightType);
    ~HdCrustLight() override = default;

    HdDirtyBits GetInitialDirtyBitsMask() const override;

    void Sync(HdSceneDelegate* sceneDelegate, HdRenderParam* renderParam,
              HdDirtyBits* dirtyBits) override;

    void Finalize(HdRenderParam* renderParam) override;

private:
    TfToken _lightType;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif // HDCRUST_LIGHT_H
