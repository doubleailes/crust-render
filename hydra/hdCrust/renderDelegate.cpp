#include "renderDelegate.h"

#include "instancer.h"
#include "light.h"
#include "mesh.h"
#include "renderBuffer.h"
#include "renderPass.h"

#include <pxr/base/tf/diagnostic.h>
#include <pxr/imaging/hd/camera.h>
#include <pxr/imaging/hd/material.h>
#include <pxr/imaging/hd/resourceRegistry.h>
#include <pxr/imaging/hd/tokens.h>

PXR_NAMESPACE_OPEN_SCOPE

const TfTokenVector HdCrustRenderDelegate::SUPPORTED_RPRIM_TYPES = {
    HdPrimTypeTokens->mesh,
};

const TfTokenVector HdCrustRenderDelegate::SUPPORTED_SPRIM_TYPES = {
    HdPrimTypeTokens->camera,       HdPrimTypeTokens->material,
    HdPrimTypeTokens->sphereLight,  HdPrimTypeTokens->rectLight,
    HdPrimTypeTokens->distantLight, HdPrimTypeTokens->domeLight,
};

const TfTokenVector HdCrustRenderDelegate::SUPPORTED_BPRIM_TYPES = {
    HdPrimTypeTokens->renderBuffer,
};

/// Accepted and ignored: Phase 1 shades from displayColor, and the stub
/// keeps material-heavy stages (every production asset binds materials
/// everywhere) from spraying unsupported-prim errors.
class HdCrustMaterial final : public HdMaterial {
public:
    explicit HdCrustMaterial(SdfPath const& id) : HdMaterial(id) {}
    HdDirtyBits GetInitialDirtyBitsMask() const override {
        return HdMaterial::AllDirty;
    }
    void Sync(HdSceneDelegate* sceneDelegate, HdRenderParam* renderParam,
              HdDirtyBits* dirtyBits) override {
        (void)sceneDelegate;
        (void)renderParam;
        *dirtyBits = HdMaterial::Clean;
    }
};

HdCrustRenderDelegate::HdCrustRenderDelegate() : HdRenderDelegate() {
    _Initialize();
}

HdCrustRenderDelegate::HdCrustRenderDelegate(
    HdRenderSettingsMap const& settingsMap)
    : HdRenderDelegate(settingsMap) {
    _Initialize();
}

void HdCrustRenderDelegate::_Initialize() {
    _renderParam = std::make_unique<HdCrustRenderParam>();
    _resourceRegistry = std::make_shared<HdResourceRegistry>();
}

HdCrustRenderDelegate::~HdCrustRenderDelegate() = default;

const TfTokenVector& HdCrustRenderDelegate::GetSupportedRprimTypes() const {
    return SUPPORTED_RPRIM_TYPES;
}
const TfTokenVector& HdCrustRenderDelegate::GetSupportedSprimTypes() const {
    return SUPPORTED_SPRIM_TYPES;
}
const TfTokenVector& HdCrustRenderDelegate::GetSupportedBprimTypes() const {
    return SUPPORTED_BPRIM_TYPES;
}

HdRenderParam* HdCrustRenderDelegate::GetRenderParam() const {
    return _renderParam.get();
}

HdResourceRegistrySharedPtr HdCrustRenderDelegate::GetResourceRegistry() const {
    return _resourceRegistry;
}

HdRenderPassSharedPtr HdCrustRenderDelegate::CreateRenderPass(
    HdRenderIndex* index, HdRprimCollection const& collection) {
    return HdRenderPassSharedPtr(
        new HdCrustRenderPass(index, collection, _renderParam.get()));
}

HdInstancer* HdCrustRenderDelegate::CreateInstancer(HdSceneDelegate* delegate,
                                                    SdfPath const& id) {
    return new HdCrustInstancer(delegate, id);
}

void HdCrustRenderDelegate::DestroyInstancer(HdInstancer* instancer) {
    delete instancer;
}

HdRprim* HdCrustRenderDelegate::CreateRprim(TfToken const& typeId,
                                            SdfPath const& rprimId) {
    if (typeId == HdPrimTypeTokens->mesh) {
        return new HdCrustMesh(rprimId);
    }
    TF_CODING_ERROR("Unknown rprim type %s", typeId.GetText());
    return nullptr;
}

void HdCrustRenderDelegate::DestroyRprim(HdRprim* rprim) { delete rprim; }

HdSprim* HdCrustRenderDelegate::CreateSprim(TfToken const& typeId,
                                            SdfPath const& sprimId) {
    if (typeId == HdPrimTypeTokens->camera) {
        return new HdCamera(sprimId);
    }
    if (typeId == HdPrimTypeTokens->material) {
        return new HdCrustMaterial(sprimId);
    }
    if (typeId == HdPrimTypeTokens->sphereLight ||
        typeId == HdPrimTypeTokens->rectLight ||
        typeId == HdPrimTypeTokens->distantLight ||
        typeId == HdPrimTypeTokens->domeLight) {
        return new HdCrustLight(sprimId, typeId);
    }
    TF_CODING_ERROR("Unknown sprim type %s", typeId.GetText());
    return nullptr;
}

HdSprim* HdCrustRenderDelegate::CreateFallbackSprim(TfToken const& typeId) {
    return CreateSprim(typeId, SdfPath::EmptyPath());
}

void HdCrustRenderDelegate::DestroySprim(HdSprim* sprim) { delete sprim; }

HdBprim* HdCrustRenderDelegate::CreateBprim(TfToken const& typeId,
                                            SdfPath const& bprimId) {
    if (typeId == HdPrimTypeTokens->renderBuffer) {
        return new HdCrustRenderBuffer(bprimId);
    }
    TF_CODING_ERROR("Unknown bprim type %s", typeId.GetText());
    return nullptr;
}

HdBprim* HdCrustRenderDelegate::CreateFallbackBprim(TfToken const& typeId) {
    return CreateBprim(typeId, SdfPath::EmptyPath());
}

void HdCrustRenderDelegate::DestroyBprim(HdBprim* bprim) { delete bprim; }

void HdCrustRenderDelegate::CommitResources(HdChangeTracker* tracker) {
    (void)tracker;
}

HdAovDescriptor HdCrustRenderDelegate::GetDefaultAovDescriptor(
    TfToken const& name) const {
    if (name == HdAovTokens->color) {
        return HdAovDescriptor(HdFormatFloat32Vec4, false,
                               VtValue(GfVec4f(0.0f)));
    }
    if (name == HdAovTokens->depth) {
        return HdAovDescriptor(HdFormatFloat32, false, VtValue(1.0f));
    }
    if (name == HdAovTokens->primId || name == HdAovTokens->instanceId ||
        name == HdAovTokens->elementId) {
        return HdAovDescriptor(HdFormatInt32, false, VtValue(-1));
    }
    return HdAovDescriptor();
}

PXR_NAMESPACE_CLOSE_SCOPE
