#ifndef HDCRUST_INSTANCER_H
#define HDCRUST_INSTANCER_H

#include <pxr/base/gf/matrix4d.h>
#include <pxr/base/tf/hashmap.h>
#include <pxr/base/vt/array.h>
#include <pxr/base/vt/value.h>
#include <pxr/imaging/hd/instancer.h>
#include <pxr/pxr.h>

#include <mutex>

PXR_NAMESPACE_OPEN_SCOPE

/// The canonical CPU instancer (hdEmbree's pattern): sync the
/// instance-rate primvars, then compose per-prototype transforms from
/// instanceTranslations / instanceRotations / instanceScales /
/// instanceTransforms under the instancer's own transform — recursing into
/// a parent instancer when nested. Primvars are kept as VtValues and
/// accessed by held type, so quaternion encodings (quath/quatf/quatd, or
/// legacy vec4) are handled without layout assumptions.
class HdCrustInstancer final : public HdInstancer {
public:
    HdCrustInstancer(HdSceneDelegate* delegate, SdfPath const& id);
    ~HdCrustInstancer() override;

    void Sync(HdSceneDelegate* sceneDelegate, HdRenderParam* renderParam,
              HdDirtyBits* dirtyBits) override;

    VtMatrix4dArray ComputeInstanceTransforms(SdfPath const& prototypeId);

private:
    void _SyncPrimvars(HdSceneDelegate* sceneDelegate, HdDirtyBits dirtyBits);

    std::mutex _instanceLock;
    TfHashMap<TfToken, VtValue, TfToken::HashFunctor> _primvarMap;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif // HDCRUST_INSTANCER_H
