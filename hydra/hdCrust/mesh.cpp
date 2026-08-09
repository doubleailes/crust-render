#include "mesh.h"

#include "instancer.h"
#include "renderParam.h"

#include <pxr/base/gf/vec3f.h>
#include <pxr/base/tf/hash.h>
#include <pxr/imaging/hd/changeTracker.h>
#include <pxr/imaging/hd/meshUtil.h>
#include <pxr/imaging/hd/renderIndex.h>
#include <pxr/imaging/hd/sceneDelegate.h>
#include <pxr/imaging/hd/tokens.h>

PXR_NAMESPACE_OPEN_SCOPE

HdCrustMesh::HdCrustMesh(SdfPath const& id) : HdMesh(id) {}

HdDirtyBits HdCrustMesh::GetInitialDirtyBitsMask() const {
    return HdChangeTracker::Clean
        | HdChangeTracker::DirtyPoints
        | HdChangeTracker::DirtyTopology
        | HdChangeTracker::DirtyTransform
        | HdChangeTracker::DirtyVisibility
        | HdChangeTracker::DirtyPrimvar
        | HdChangeTracker::DirtyNormals
        | HdChangeTracker::DirtyInstancer
        | HdChangeTracker::DirtyMaterialId
        | HdChangeTracker::DirtyPrimID;
}

void HdCrustMesh::_InitRepr(TfToken const& reprToken, HdDirtyBits* dirtyBits) {
    (void)reprToken;
    (void)dirtyBits;
}

HdDirtyBits HdCrustMesh::_PropagateDirtyBits(HdDirtyBits bits) const {
    return bits;
}

// The constant/uniform displayColor, or mid-grey — Phase 1 is
// displayColor-only (HdMaterialNetwork translation is Phase 2).
static GfVec3f _GetDisplayColor(HdSceneDelegate* sceneDelegate,
                                SdfPath const& id) {
    VtValue value = sceneDelegate->Get(id, HdTokens->displayColor);
    if (value.IsHolding<VtVec3fArray>()) {
        VtVec3fArray colors = value.UncheckedGet<VtVec3fArray>();
        if (!colors.empty()) {
            return colors[0];
        }
    } else if (value.IsHolding<GfVec3f>()) {
        return value.UncheckedGet<GfVec3f>();
    }
    return GfVec3f(0.5f, 0.5f, 0.5f);
}

void HdCrustMesh::Sync(HdSceneDelegate* sceneDelegate,
                       HdRenderParam* renderParam, HdDirtyBits* dirtyBits,
                       TfToken const& reprToken) {
    (void)reprToken;
    SdfPath const& id = GetId();
    auto* param = static_cast<HdCrustRenderParam*>(renderParam);

    if (HdChangeTracker::IsVisibilityDirty(*dirtyBits, id)) {
        _UpdateVisibility(sceneDelegate, dirtyBits);
    }
    _UpdateInstancer(sceneDelegate, dirtyBits);
    if (*dirtyBits & HdChangeTracker::DirtyMaterialId) {
        SetMaterialId(sceneDelegate->GetMaterialId(id));
    }

    // The GEOMETRY version moves only when the geometry itself did — that
    // is what lets the render pass keep hitting the crust prototype cache
    // across transform/material/color/instancing edits.
    if (_geoVersion == 0 ||
        (*dirtyBits & (HdChangeTracker::DirtyPoints |
                       HdChangeTracker::DirtyTopology |
                       HdChangeTracker::DirtyNormals))) {
        ++_geoVersion;
    }

    // The cache entry itself is recomputed from scratch on every Sync —
    // cheap next to the render, and immune to partial-dirty bookkeeping
    // bugs; only the version above is stateful.
    HdCrustCachedMesh mesh;
    mesh.visible = IsVisible();
    mesh.primId = GetPrimId();
    mesh.geoVersion = _geoVersion;
    mesh.materialId = GetMaterialId();
    mesh.xform = sceneDelegate->GetTransform(id);
    mesh.displayColor = _GetDisplayColor(sceneDelegate, id);

    VtValue pointsValue = sceneDelegate->Get(id, HdTokens->points);
    if (pointsValue.IsHolding<VtVec3fArray>()) {
        mesh.points = pointsValue.UncheckedGet<VtVec3fArray>();
    }

    // Hydra's own triangulator: handles arbitrary polygons and holes, and
    // keeps triangle indices in terms of the original vertex order, so
    // vertex-interpolated primvars (points, normals) stay aligned.
    HdMeshTopology topology = GetMeshTopology(sceneDelegate);
    HdMeshUtil meshUtil(&topology, id);
    VtIntArray trianglePrimitiveParams;
    meshUtil.ComputeTriangleIndices(&mesh.triangles, &trianglePrimitiveParams);

    // Vertex-interpolated authored normals only; anything else falls back
    // to the kernel's geometric normals.
    for (HdPrimvarDescriptor const& pv :
         sceneDelegate->GetPrimvarDescriptors(id, HdInterpolationVertex)) {
        if (pv.name == HdTokens->normals) {
            VtValue normalsValue = sceneDelegate->Get(id, HdTokens->normals);
            if (normalsValue.IsHolding<VtVec3fArray>()) {
                VtVec3fArray normals = normalsValue.UncheckedGet<VtVec3fArray>();
                if (normals.size() == mesh.points.size()) {
                    mesh.normals = normals;
                }
            }
        }
    }

    SdfPath instancerId = GetInstancerId();
    if (!instancerId.IsEmpty()) {
        // The render index does NOT sync instancers on its own — the rprim
        // must pull its instancer chain up to date before reading it
        // (hdEmbree does the same; the helper is mutex-guarded for
        // concurrent rprim syncs).
        HdInstancer::_SyncInstancerAndParents(sceneDelegate->GetRenderIndex(),
                                              instancerId);
        HdInstancer* instancer =
            sceneDelegate->GetRenderIndex().GetInstancer(instancerId);
        if (TF_VERIFY(instancer)) {
            mesh.instanceXforms =
                static_cast<HdCrustInstancer*>(instancer)
                    ->ComputeInstanceTransforms(id);
        }
    }

    param->UpdateMesh(id, std::move(mesh));
    *dirtyBits = HdChangeTracker::Clean;
}

void HdCrustMesh::Finalize(HdRenderParam* renderParam) {
    auto* param = static_cast<HdCrustRenderParam*>(renderParam);
    param->RemoveMesh(GetId());
    // Let the render pass evict this mesh's prototype from the crust
    // geometry cache — same key derivation as the rebuild uses.
    param->AddDeadGeoKey(TfHash()(GetId()));
}

PXR_NAMESPACE_CLOSE_SCOPE
