// hdCrust: the renderer plugin usdview lists as "Crust".
#ifndef HDCRUST_RENDERER_PLUGIN_H
#define HDCRUST_RENDERER_PLUGIN_H

#include "pxr/pxr.h"
#include "pxr/imaging/hd/rendererPlugin.h"

PXR_NAMESPACE_OPEN_SCOPE

/// Creates HdCrustRenderDelegate. No `_CreateRenderer` override: on OpenUSD
/// 26.x the base class wraps the delegate in an
/// HdRenderDelegateAdapterRenderer, which hands it the terminal scene index
/// (hydra-delegate design, D1).
class HdCrustRendererPlugin final : public HdRendererPlugin
{
public:
    HdCrustRendererPlugin() = default;
    ~HdCrustRendererPlugin() override = default;

    HdContainerDataSourceHandle GetSceneIndexCreateArgs() const override;

    HdRenderDelegate *CreateRenderDelegate() override;
    HdRenderDelegate *CreateRenderDelegate(
        HdRenderSettingsMap const &settingsMap) override;
    void DeleteRenderDelegate(HdRenderDelegate *renderDelegate) override;

    bool IsSupported(
        const HdRendererCreateArgsSchema &rendererCreateArgs,
        std::string *reasonWhyNot = nullptr) const override;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif
