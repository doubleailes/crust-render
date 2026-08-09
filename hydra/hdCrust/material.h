#ifndef HDCRUST_MATERIAL_H
#define HDCRUST_MATERIAL_H

#include <pxr/imaging/hd/material.h>
#include <pxr/pxr.h>

PXR_NAMESPACE_OPEN_SCOPE

/// Translates a bound material's HdMaterialNetwork into a CrustMaterial:
/// the surface terminal's UsdPreviewSurface constants map onto crust's
/// OpenPBR übershader with exactly the semantics of the engine's own USD
/// importer (`preview_surface_openpbr` in crust-core) — authored values
/// pass verbatim (no color-space decode), unset inputs keep OpenPBR
/// defaults, emission luminance switches on when emissiveColor is nonzero,
/// clearcoat maps to the coat lobe. Texture-connected inputs warn and use
/// the default (surface textures are a later phase); non-UsdPreviewSurface
/// terminals warn and shade mid-grey.
class HdCrustMaterial final : public HdMaterial {
public:
    explicit HdCrustMaterial(SdfPath const& id);
    ~HdCrustMaterial() override = default;

    HdDirtyBits GetInitialDirtyBitsMask() const override {
        return HdMaterial::AllDirty;
    }

    void Sync(HdSceneDelegate* sceneDelegate, HdRenderParam* renderParam,
              HdDirtyBits* dirtyBits) override;

    void Finalize(HdRenderParam* renderParam) override;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif // HDCRUST_MATERIAL_H
