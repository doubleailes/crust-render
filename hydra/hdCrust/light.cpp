#include "light.h"

#include "renderParam.h"

#include <pxr/imaging/hd/sceneDelegate.h>
#include <pxr/imaging/hd/tokens.h>

PXR_NAMESPACE_OPEN_SCOPE

HdCrustLight::HdCrustLight(SdfPath const& id, TfToken const& lightType)
    : HdLight(id), _lightType(lightType) {}

HdDirtyBits HdCrustLight::GetInitialDirtyBitsMask() const {
    return HdLight::AllDirty;
}

template <typename T>
static T _Param(HdSceneDelegate* sceneDelegate, SdfPath const& id,
                TfToken const& token, T fallback) {
    VtValue value = sceneDelegate->GetLightParamValue(id, token);
    if (value.IsHolding<T>()) {
        return value.UncheckedGet<T>();
    }
    return fallback;
}

void HdCrustLight::Sync(HdSceneDelegate* sceneDelegate,
                        HdRenderParam* renderParam, HdDirtyBits* dirtyBits) {
    SdfPath const& id = GetId();
    auto* param = static_cast<HdCrustRenderParam*>(renderParam);

    HdCrustCachedLight light;
    light.type = _lightType;
    light.xform = sceneDelegate->GetTransform(id);
    light.color = _Param(sceneDelegate, id, HdLightTokens->color,
                         GfVec3f(1.0f, 1.0f, 1.0f));
    light.intensity = _Param(sceneDelegate, id, HdLightTokens->intensity, 1.0f);
    light.exposure = _Param(sceneDelegate, id, HdLightTokens->exposure, 0.0f);
    if (_lightType == HdPrimTypeTokens->sphereLight) {
        light.radius = _Param(sceneDelegate, id, HdLightTokens->radius, 0.5f);
    } else if (_lightType == HdPrimTypeTokens->rectLight) {
        light.width = _Param(sceneDelegate, id, HdLightTokens->width, 1.0f);
        light.height = _Param(sceneDelegate, id, HdLightTokens->height, 1.0f);
    } else if (_lightType == HdPrimTypeTokens->distantLight) {
        light.angle = _Param(sceneDelegate, id, HdLightTokens->angle, 0.53f);
    } else if (_lightType == HdPrimTypeTokens->domeLight) {
        // Dome textures are deferred (Phase 1 renders the uniform color;
        // the crust C API already accepts equirect pixels for when EXR/HDR
        // decoding lands in the plugin).
        VtValue texture =
            sceneDelegate->GetLightParamValue(id, HdLightTokens->textureFile);
        if (!texture.IsEmpty()) {
            TF_WARN(
                "hdCrust: dome light %s has a texture; Phase 1 renders its "
                "uniform color instead",
                id.GetText());
        }
    }

    param->UpdateLight(id, light);
    *dirtyBits = HdLight::Clean;
}

void HdCrustLight::Finalize(HdRenderParam* renderParam) {
    static_cast<HdCrustRenderParam*>(renderParam)->RemoveLight(GetId());
}

PXR_NAMESPACE_CLOSE_SCOPE
