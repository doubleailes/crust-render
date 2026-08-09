#include "rendererPlugin.h"

#include "renderDelegate.h"

#include <pxr/imaging/hd/rendererPluginRegistry.h>

PXR_NAMESPACE_OPEN_SCOPE

TF_REGISTRY_FUNCTION(TfType) {
    HdRendererPluginRegistry::Define<HdCrustRendererPlugin>();
}

HdRenderDelegate* HdCrustRendererPlugin::CreateRenderDelegate() {
    return new HdCrustRenderDelegate();
}

HdRenderDelegate* HdCrustRendererPlugin::CreateRenderDelegate(
    HdRenderSettingsMap const& settingsMap) {
    return new HdCrustRenderDelegate(settingsMap);
}

void HdCrustRendererPlugin::DeleteRenderDelegate(
    HdRenderDelegate* renderDelegate) {
    delete renderDelegate;
}

bool HdCrustRendererPlugin::IsSupported(
    HdRendererCreateArgs const& rendererCreateArgs,
    std::string* reasonWhyNot) const {
    (void)rendererCreateArgs;
    (void)reasonWhyNot;
    return true;
}

PXR_NAMESPACE_CLOSE_SCOPE
