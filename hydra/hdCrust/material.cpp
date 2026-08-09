#include "material.h"

#include "renderParam.h"

#include <pxr/base/gf/vec3f.h>
#include <pxr/base/tf/staticTokens.h>
#include <pxr/imaging/hd/sceneDelegate.h>
#include <pxr/imaging/hd/tokens.h>

#include <algorithm>

PXR_NAMESPACE_OPEN_SCOPE

// usdImaging (where UsdImagingTokens->UsdPreviewSurface lives) is not a
// dependency of a render delegate; a private token is what Storm itself
// does (hdSt/materialNetwork.cpp). The strings are pinned by usdShaders'
// shaderDefs.usda.
TF_DEFINE_PRIVATE_TOKENS(_tokens,
    (UsdPreviewSurface)
    (diffuseColor)
    (emissiveColor)
    (metallic)
    (roughness)
    (ior)
    (opacity)
    (clearcoat)
    (clearcoatRoughness)
);

HdCrustMaterial::HdCrustMaterial(SdfPath const& id) : HdMaterial(id) {}

static void _ReadFloat(HdMaterialNode2 const& node, TfToken const& name,
                       float* out) {
    auto it = node.parameters.find(name);
    if (it != node.parameters.end() && it->second.IsHolding<float>()) {
        *out = it->second.UncheckedGet<float>();
    }
}

static bool _ReadColor(HdMaterialNode2 const& node, TfToken const& name,
                       float out[3]) {
    auto it = node.parameters.find(name);
    if (it != node.parameters.end() && it->second.IsHolding<GfVec3f>()) {
        GfVec3f c = it->second.UncheckedGet<GfVec3f>();
        out[0] = c[0];
        out[1] = c[1];
        out[2] = c[2];
        return true;
    }
    return false;
}

// The engine's `preview_surface_openpbr` mapping, from HdMaterialNetwork
// data instead of USD attributes. `crust_material_default()` equals the
// OpenPBR defaults the engine falls back to, so unset (or
// texture-connected, hence absent) inputs land in the same place.
static CrustMaterial _TranslatePreviewSurface(SdfPath const& materialId,
                                              HdMaterialNode2 const& node) {
    CrustMaterial m;
    crust_material_default(&m);
    _ReadColor(node, _tokens->diffuseColor, m.base_color); // verbatim, no decode
    _ReadFloat(node, _tokens->metallic, &m.metalness);
    _ReadFloat(node, _tokens->roughness, &m.roughness);
    _ReadFloat(node, _tokens->ior, &m.ior);
    _ReadFloat(node, _tokens->opacity, &m.opacity);
    _ReadFloat(node, _tokens->clearcoat, &m.coat_weight);
    _ReadFloat(node, _tokens->clearcoatRoughness, &m.coat_roughness);
    float emissive[3] = {0.0f, 0.0f, 0.0f};
    if (_ReadColor(node, _tokens->emissiveColor, emissive)) {
        m.emission_color[0] = emissive[0];
        m.emission_color[1] = emissive[1];
        m.emission_color[2] = emissive[2];
        m.emission_luminance =
            std::max({emissive[0], emissive[1], emissive[2]}) > 0.0f ? 1.0f
                                                                     : 0.0f;
    }
    if (!node.inputConnections.empty()) {
        TF_WARN(
            "hdCrust: material %s has %zu texture-connected input(s); "
            "surface textures are not supported yet, using constants",
            materialId.GetText(), node.inputConnections.size());
    }
    return m;
}

void HdCrustMaterial::Sync(HdSceneDelegate* sceneDelegate,
                           HdRenderParam* renderParam,
                           HdDirtyBits* dirtyBits) {
    SdfPath const& id = GetId();
    auto* param = static_cast<HdCrustRenderParam*>(renderParam);

    CrustMaterial material;
    crust_material_default(&material);
    bool translated = false;

    VtValue resource = sceneDelegate->GetMaterialResource(id);
    if (resource.IsHolding<HdMaterialNetworkMap>()) {
        HdMaterialNetwork2 network = HdConvertToHdMaterialNetwork2(
            resource.UncheckedGet<HdMaterialNetworkMap>());
        auto terminal =
            network.terminals.find(HdMaterialTerminalTokens->surface);
        if (terminal != network.terminals.end()) {
            auto node = network.nodes.find(terminal->second.upstreamNode);
            if (node != network.nodes.end()) {
                if (node->second.nodeTypeId == _tokens->UsdPreviewSurface) {
                    material = _TranslatePreviewSurface(id, node->second);
                    translated = true;
                } else {
                    TF_WARN(
                        "hdCrust: material %s's surface terminal is %s; only "
                        "UsdPreviewSurface is supported — shading mid-grey",
                        id.GetText(), node->second.nodeTypeId.GetText());
                }
            }
        }
    }
    if (!translated) {
        // The engine's unknown-shader fallback is mid-grey diffuse.
        material.base_color[0] = 0.5f;
        material.base_color[1] = 0.5f;
        material.base_color[2] = 0.5f;
    }

    param->UpdateMaterial(id, material);
    *dirtyBits = HdMaterial::Clean;
}

void HdCrustMaterial::Finalize(HdRenderParam* renderParam) {
    static_cast<HdCrustRenderParam*>(renderParam)->RemoveMaterial(GetId());
}

PXR_NAMESPACE_CLOSE_SCOPE
