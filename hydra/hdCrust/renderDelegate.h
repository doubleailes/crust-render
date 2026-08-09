#ifndef HDCRUST_RENDER_DELEGATE_H
#define HDCRUST_RENDER_DELEGATE_H

#include "renderParam.h"

#include <pxr/imaging/hd/renderDelegate.h>
#include <pxr/pxr.h>

#include <memory>

PXR_NAMESPACE_OPEN_SCOPE

/// The hdCrust render delegate: meshes, the four UsdLux light types crust
/// supports, cameras, render buffers, and a no-op material stub (Phase 1
/// shades from displayColor; HdMaterialNetwork translation is Phase 2 —
/// see docs/hydra_delegate.md in the crust-render repository).
class HdCrustRenderDelegate final : public HdRenderDelegate {
public:
    HdCrustRenderDelegate();
    explicit HdCrustRenderDelegate(HdRenderSettingsMap const& settingsMap);
    ~HdCrustRenderDelegate() override;

    HdCrustRenderDelegate(const HdCrustRenderDelegate&) = delete;
    HdCrustRenderDelegate& operator=(const HdCrustRenderDelegate&) = delete;

    const TfTokenVector& GetSupportedRprimTypes() const override;
    const TfTokenVector& GetSupportedSprimTypes() const override;
    const TfTokenVector& GetSupportedBprimTypes() const override;

    HdRenderParam* GetRenderParam() const override;
    HdResourceRegistrySharedPtr GetResourceRegistry() const override;

    HdRenderPassSharedPtr CreateRenderPass(
        HdRenderIndex* index, HdRprimCollection const& collection) override;

    HdInstancer* CreateInstancer(HdSceneDelegate* delegate,
                                 SdfPath const& id) override;
    void DestroyInstancer(HdInstancer* instancer) override;

    HdRprim* CreateRprim(TfToken const& typeId, SdfPath const& rprimId) override;
    void DestroyRprim(HdRprim* rprim) override;

    HdSprim* CreateSprim(TfToken const& typeId, SdfPath const& sprimId) override;
    HdSprim* CreateFallbackSprim(TfToken const& typeId) override;
    void DestroySprim(HdSprim* sprim) override;

    HdBprim* CreateBprim(TfToken const& typeId, SdfPath const& bprimId) override;
    HdBprim* CreateFallbackBprim(TfToken const& typeId) override;
    void DestroyBprim(HdBprim* bprim) override;

    void CommitResources(HdChangeTracker* tracker) override;

    HdAovDescriptor GetDefaultAovDescriptor(TfToken const& name) const override;

private:
    void _Initialize();

    static const TfTokenVector SUPPORTED_RPRIM_TYPES;
    static const TfTokenVector SUPPORTED_SPRIM_TYPES;
    static const TfTokenVector SUPPORTED_BPRIM_TYPES;

    std::unique_ptr<HdCrustRenderParam> _renderParam;
    HdResourceRegistrySharedPtr _resourceRegistry;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif // HDCRUST_RENDER_DELEGATE_H
