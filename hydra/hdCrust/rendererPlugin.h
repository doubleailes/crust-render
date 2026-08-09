#ifndef HDCRUST_RENDERER_PLUGIN_H
#define HDCRUST_RENDERER_PLUGIN_H

#include <pxr/imaging/hd/rendererPlugin.h>
#include <pxr/pxr.h>

PXR_NAMESPACE_OPEN_SCOPE

/// The plugin entry point Hydra's plugin registry discovers through
/// plugInfo.json and instantiates when a host selects the "Crust" renderer.
class HdCrustRendererPlugin final : public HdRendererPlugin {
public:
    HdCrustRendererPlugin() = default;
    ~HdCrustRendererPlugin() override = default;

    HdCrustRendererPlugin(const HdCrustRendererPlugin&) = delete;
    HdCrustRendererPlugin& operator=(const HdCrustRendererPlugin&) = delete;

    HdRenderDelegate* CreateRenderDelegate() override;
    HdRenderDelegate* CreateRenderDelegate(
        HdRenderSettingsMap const& settingsMap) override;
    void DeleteRenderDelegate(HdRenderDelegate* renderDelegate) override;

    /// A CPU path tracer: no GPU needed, supported everywhere.
    bool IsSupported(HdRendererCreateArgs const& rendererCreateArgs,
                     std::string* reasonWhyNot = nullptr) const override;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif // HDCRUST_RENDERER_PLUGIN_H
