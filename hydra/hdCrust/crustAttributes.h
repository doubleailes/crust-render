// hdCrust: where crust's own prim attributes travel through Hydra. Kept apart
// from the UsdImaging adapter that publishes them: UsdImaging's headers bring
// <windows.h>, whose `interface` macro breaks Hd's material schemas.
#ifndef HDCRUST_CRUST_ATTRIBUTES_H
#define HDCRUST_CRUST_ATTRIBUTES_H

#include "pxr/pxr.h"
#include "pxr/base/tf/token.h"

PXR_NAMESPACE_OPEN_SCOPE

/// The data source name the `crust:*` attributes are published under: a
/// container of attribute name to value (crustAttributesAdapter.h).
TfToken const &HdCrustAttributesToken();

PXR_NAMESPACE_CLOSE_SCOPE

#endif
