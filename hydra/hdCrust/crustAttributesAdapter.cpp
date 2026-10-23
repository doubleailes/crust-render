#include "crustAttributesAdapter.h"

#include "pxr/base/tf/stringUtils.h"
#include "pxr/base/tf/type.h"
#include "pxr/imaging/hd/retainedDataSource.h"
#include "pxr/usdImaging/usdImaging/dataSourceAttribute.h"

#include <vector>

PXR_NAMESPACE_OPEN_SCOPE

TF_REGISTRY_FUNCTION(TfType)
{
    using Adapter = HdCrust_CrustAttributesAdapter;
    TfType t = TfType::Define<Adapter, TfType::Bases<Adapter::BaseAdapter>>();
    t.SetFactory<UsdImagingAPISchemaAdapterFactory<Adapter>>();
}

TfToken const &
HdCrustAttributesToken()
{
    static TfToken const t("crust");
    return t;
}

namespace {

/// Whether `name` is one of crust's own properties.
bool
_IsCrust(TfToken const &name)
{
    return TfStringStartsWith(name.GetString(), "crust:");
}

} // namespace

HdContainerDataSourceHandle
HdCrust_CrustAttributesAdapter::GetImagingSubprimData(
    UsdPrim const &prim, TfToken const &subprim, TfToken const &,
    UsdImagingDataSourceStageGlobals const &stageGlobals)
{
    if (!subprim.IsEmpty()) {
        return nullptr;
    }
    std::vector<TfToken> names;
    std::vector<HdDataSourceBaseHandle> values;
    for (UsdAttribute const &attr : prim.GetAuthoredAttributes()) {
        TfToken const &name = attr.GetName();
        if (!_IsCrust(name)) {
            continue;
        }
        names.push_back(name);
        // Time-varying values and their invalidation, as UsdImaging's own
        // attributes have them.
        values.push_back(UsdImagingDataSourceAttributeNew(
            attr, stageGlobals, prim.GetPath(),
            HdDataSourceLocator(HdCrustAttributesToken(), name)));
    }
    if (names.empty()) {
        return nullptr;
    }
    return HdRetainedContainerDataSource::New(
        HdCrustAttributesToken(),
        HdRetainedContainerDataSource::New(names.size(), names.data(), values.data()));
}

HdDataSourceLocatorSet
HdCrust_CrustAttributesAdapter::InvalidateImagingSubprim(
    UsdPrim const &, TfToken const &subprim, TfToken const &, TfTokenVector const &properties,
    UsdImagingPropertyInvalidationType)
{
    HdDataSourceLocatorSet result;
    if (!subprim.IsEmpty()) {
        return result;
    }
    for (TfToken const &name : properties) {
        if (_IsCrust(name)) {
            result.insert(HdDataSourceLocator(HdCrustAttributesToken()));
            break;
        }
    }
    return result;
}

PXR_NAMESPACE_CLOSE_SCOPE
