// hdCrust: crust's own prim attributes (`crust:volume:*`, `crust:light:*`,
// `crust:rayMask`, `crust:subdivisionLevel`, ...) carried into Hydra.
#ifndef HDCRUST_CRUST_ATTRIBUTES_ADAPTER_H
#define HDCRUST_CRUST_ATTRIBUTES_ADAPTER_H

#include "pxr/pxr.h"
#include "pxr/usdImaging/usdImaging/apiSchemaAdapter.h"

#include "crustAttributes.h"

PXR_NAMESPACE_OPEN_SCOPE

/// A keyless UsdImaging API schema adapter (registered with an empty
/// `apiSchemaName`, so UsdImaging consults it for every prim). Hydra carries
/// only schema data and primvars, never arbitrary attributes; this adapter
/// publishes every authored `crust:*` attribute of a prim under
/// `HdCrustAttributesToken()`, so the delegate can write them back and the
/// layers it renders keep crust's own prim-level extensions.
class HdCrust_CrustAttributesAdapter : public UsdImagingAPISchemaAdapter
{
public:
    using BaseAdapter = UsdImagingAPISchemaAdapter;

    HdContainerDataSourceHandle GetImagingSubprimData(
        UsdPrim const &prim, TfToken const &subprim, TfToken const &appliedInstanceName,
        UsdImagingDataSourceStageGlobals const &stageGlobals) override;

    HdDataSourceLocatorSet InvalidateImagingSubprim(
        UsdPrim const &prim, TfToken const &subprim, TfToken const &appliedInstanceName,
        TfTokenVector const &properties, UsdImagingPropertyInvalidationType invalidationType) override;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif
