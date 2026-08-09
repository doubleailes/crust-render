#include "instancer.h"

#include <pxr/base/gf/quatd.h>
#include <pxr/base/gf/quatf.h>
#include <pxr/base/gf/quath.h>
#include <pxr/base/gf/rotation.h>
#include <pxr/base/gf/vec3f.h>
#include <pxr/base/gf/vec4f.h>
#include <pxr/imaging/hd/changeTracker.h>
#include <pxr/imaging/hd/renderIndex.h>
#include <pxr/imaging/hd/sceneDelegate.h>
#include <pxr/imaging/hd/tokens.h>

PXR_NAMESPACE_OPEN_SCOPE

HdCrustInstancer::HdCrustInstancer(HdSceneDelegate* delegate, SdfPath const& id)
    : HdInstancer(delegate, id) {}

HdCrustInstancer::~HdCrustInstancer() = default;

void HdCrustInstancer::Sync(HdSceneDelegate* sceneDelegate,
                            HdRenderParam* renderParam,
                            HdDirtyBits* dirtyBits) {
    (void)renderParam;
    _UpdateInstancer(sceneDelegate, dirtyBits);
    if (HdChangeTracker::IsAnyPrimvarDirty(*dirtyBits, GetId())) {
        _SyncPrimvars(sceneDelegate, *dirtyBits);
    }
}

void HdCrustInstancer::_SyncPrimvars(HdSceneDelegate* sceneDelegate,
                                     HdDirtyBits dirtyBits) {
    SdfPath const& id = GetId();
    HdPrimvarDescriptorVector primvars =
        sceneDelegate->GetPrimvarDescriptors(id, HdInterpolationInstance);
    std::lock_guard<std::mutex> lock(_instanceLock);
    for (HdPrimvarDescriptor const& pv : primvars) {
        if (!HdChangeTracker::IsPrimvarDirty(dirtyBits, id, pv.name)) {
            continue;
        }
        VtValue value = sceneDelegate->Get(id, pv.name);
        if (!value.IsEmpty()) {
            _primvarMap[pv.name] = value;
        }
    }
}

/// One rotation as GfQuatd, whatever encoding the scene delegate handed
/// over; identity when the index or type is unexpected.
static GfQuatd _RotationAt(VtValue const& value, int index) {
    size_t i = size_t(index);
    if (value.IsHolding<VtQuathArray>()) {
        auto const& a = value.UncheckedGet<VtQuathArray>();
        if (i < a.size()) return GfQuatd(a[i]);
    } else if (value.IsHolding<VtQuatfArray>()) {
        auto const& a = value.UncheckedGet<VtQuatfArray>();
        if (i < a.size()) return GfQuatd(a[i]);
    } else if (value.IsHolding<VtQuatdArray>()) {
        auto const& a = value.UncheckedGet<VtQuatdArray>();
        if (i < a.size()) return a[i];
    } else if (value.IsHolding<VtVec4fArray>()) {
        // Legacy <real, i, j, k> vec4 encoding (hdEmbree's convention).
        auto const& a = value.UncheckedGet<VtVec4fArray>();
        if (i < a.size()) return GfQuatd(a[i][0], a[i][1], a[i][2], a[i][3]);
    }
    return GfQuatd::GetIdentity();
}

static GfVec3f _Vec3At(VtValue const& value, int index, GfVec3f fallback) {
    if (value.IsHolding<VtVec3fArray>()) {
        auto const& a = value.UncheckedGet<VtVec3fArray>();
        if (size_t(index) < a.size()) return a[index];
    }
    return fallback;
}

VtMatrix4dArray HdCrustInstancer::ComputeInstanceTransforms(
    SdfPath const& prototypeId) {
    // The composition below is USD row-vector convention: point * M applies
    // the leftmost factor first, so `final = instanceTransform * scale *
    // rotate * translate * instancerTransform` is scale∘rotate∘translate
    // under the instancer's own transform — UsdGeomPointInstancer's order.
    HdSceneDelegate* delegate = GetDelegate();
    SdfPath const& id = GetId();
    VtIntArray instanceIndices = delegate->GetInstanceIndices(id, prototypeId);
    GfMatrix4d instancerTransform = delegate->GetInstancerTransform(id);

    VtMatrix4dArray transforms(instanceIndices.size());
    for (size_t i = 0; i < instanceIndices.size(); ++i) {
        transforms[i] = instancerTransform;
    }

    std::lock_guard<std::mutex> lock(_instanceLock);

    auto found = _primvarMap.find(HdInstancerTokens->instanceTranslations);
    if (found != _primvarMap.end()) {
        for (size_t i = 0; i < instanceIndices.size(); ++i) {
            GfMatrix4d mat(1.0);
            mat.SetTranslate(GfVec3d(
                _Vec3At(found->second, instanceIndices[i], GfVec3f(0.0f))));
            transforms[i] = mat * transforms[i];
        }
    }

    found = _primvarMap.find(HdInstancerTokens->instanceRotations);
    if (found != _primvarMap.end()) {
        for (size_t i = 0; i < instanceIndices.size(); ++i) {
            GfMatrix4d mat(1.0);
            mat.SetRotate(GfRotation(_RotationAt(found->second, instanceIndices[i])));
            transforms[i] = mat * transforms[i];
        }
    }

    found = _primvarMap.find(HdInstancerTokens->instanceScales);
    if (found != _primvarMap.end()) {
        for (size_t i = 0; i < instanceIndices.size(); ++i) {
            GfMatrix4d mat(1.0);
            mat.SetScale(GfVec3d(
                _Vec3At(found->second, instanceIndices[i], GfVec3f(1.0f))));
            transforms[i] = mat * transforms[i];
        }
    }

    found = _primvarMap.find(HdInstancerTokens->instanceTransforms);
    if (found != _primvarMap.end() &&
        found->second.IsHolding<VtMatrix4dArray>()) {
        auto const& mats = found->second.UncheckedGet<VtMatrix4dArray>();
        for (size_t i = 0; i < instanceIndices.size(); ++i) {
            if (size_t(instanceIndices[i]) < mats.size()) {
                transforms[i] = mats[instanceIndices[i]] * transforms[i];
            }
        }
    }

    SdfPath parentId = GetParentId();
    if (parentId.IsEmpty()) {
        return transforms;
    }

    HdInstancer* parent =
        GetDelegate()->GetRenderIndex().GetInstancer(parentId);
    if (!TF_VERIFY(parent)) {
        return transforms;
    }
    VtMatrix4dArray parentTransforms =
        static_cast<HdCrustInstancer*>(parent)->ComputeInstanceTransforms(id);

    VtMatrix4dArray composed(parentTransforms.size() * transforms.size());
    for (size_t i = 0; i < parentTransforms.size(); ++i) {
        for (size_t j = 0; j < transforms.size(); ++j) {
            composed[i * transforms.size() + j] =
                transforms[j] * parentTransforms[i];
        }
    }
    return composed;
}

PXR_NAMESPACE_CLOSE_SCOPE
