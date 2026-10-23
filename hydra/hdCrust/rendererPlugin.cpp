#include "rendererPlugin.h"
#include "renderDelegate.h"

#include "pxr/imaging/hd/renderDelegateInfo.h"
#include "pxr/imaging/hd/rendererPluginRegistry.h"
#include "pxr/imaging/hd/retainedDataSource.h"
#include "pxr/imaging/hd/sceneIndexCreateArgsSchema.h"

#include "crust_hydra.h"

PXR_NAMESPACE_OPEN_SCOPE

TF_REGISTRY_FUNCTION(TfType)
{
    HdRendererPluginRegistry::Define<HdCrustRendererPlugin>();
}

HdContainerDataSourceHandle
HdCrustRendererPlugin::GetSceneIndexCreateArgs() const
{
    HdRenderDelegateInfo info;
    // What a `crust render` binds: the full purpose.
    info.materialBindingPurpose = HdTokens->full;
    info.isPrimvarFilteringNeeded = false;
    info.isCoordSysSupported = false;
    // Motion over the shutter is not carried in v1 (spec: reported).
    static HdContainerDataSourceHandle const result =
        HdSceneIndexCreateArgsSchema::Builder()
            .SetMotionBlurSupport(
                HdRetainedTypedSampledDataSource<bool>::New(false))
            .SetCameraMotionBlurSupport(
                HdRetainedTypedSampledDataSource<bool>::New(false))
            .SetLegacyRenderDelegateInfo(
                HdRetainedTypedSampledDataSource<HdRenderDelegateInfo>::New(info))
            .Build();
    return result;
}

HdRenderDelegate *
HdCrustRendererPlugin::CreateRenderDelegate()
{
    return new HdCrustRenderDelegate();
}

HdRenderDelegate *
HdCrustRendererPlugin::CreateRenderDelegate(HdRenderSettingsMap const &settingsMap)
{
    return new HdCrustRenderDelegate(settingsMap);
}

void
HdCrustRendererPlugin::DeleteRenderDelegate(HdRenderDelegate *renderDelegate)
{
    delete renderDelegate;
}

bool
HdCrustRendererPlugin::IsSupported(
    const HdRendererCreateArgsSchema &, std::string *reasonWhyNot) const
{
    // A plugin built against another crust-hydra header is refused rather
    // than called with the wrong signatures.
    if (crust_hydra_abi_version() != CRUST_HYDRA_ABI_VERSION) {
        if (reasonWhyNot) {
            *reasonWhyNot = "crust-hydra's ABI version does not match the plugin's header";
        }
        return false;
    }
    return true;
}

PXR_NAMESPACE_CLOSE_SCOPE
