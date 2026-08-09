#ifndef HDCRUST_RENDER_PASS_H
#define HDCRUST_RENDER_PASS_H

#include "renderParam.h"

#include <pxr/base/gf/matrix4d.h>
#include <pxr/imaging/hd/renderPass.h>
#include <pxr/pxr.h>

#include <crust.h>

#include <vector>

PXR_NAMESPACE_OPEN_SCOPE

/// The synchronous render loop. Each _Execute call:
///  1. rebuilds the crust scene from the render param's cache when the
///     scene version, camera, or buffer size changed (Phase 1 semantics:
///     rebuild everything on any dirty bit);
///  2. advances the render by a few samples per pixel
///     (crust_renderer_step);
///  3. copies the framebuffer/AOVs into the bound HdRenderBuffers.
/// Hydra keeps calling _Execute until IsConverged(), which is what makes
/// this synchronous design progressive in practice.
class HdCrustRenderPass final : public HdRenderPass {
public:
    HdCrustRenderPass(HdRenderIndex* index, HdRprimCollection const& collection,
                      HdCrustRenderParam* renderParam);
    ~HdCrustRenderPass() override;

    bool IsConverged() const override { return _converged; }

protected:
    void _Execute(HdRenderPassStateSharedPtr const& renderPassState,
                  TfTokenVector const& renderTags) override;

private:
    void _DestroyRenderer();
    void _RebuildScene(GfMatrix4d const& view, GfMatrix4d const& proj,
                       uint32_t width, uint32_t height);

    HdCrustRenderParam* _renderParam;

    CrustRenderer* _renderer = nullptr;
    CrustStopToken* _token = nullptr;
    /// Prototype triangles survive scene rebuilds here — a rebuild whose
    /// meshes all hit the cache pays only the top-level BVH over instance
    /// bounds.
    CrustGeoCache* _geoCache = nullptr;

    int _lastVersion = -1;
    GfMatrix4d _lastView{1.0};
    GfMatrix4d _lastProj{1.0};
    uint32_t _width = 0;
    uint32_t _height = 0;
    bool _converged = false;

    /// capi geom_id -> Hydra rprim primId, rebuilt with the scene; drives
    /// the primId AOV that makes viewport picking work.
    std::vector<int32_t> _geomToPrimId;

    std::vector<float> _rgba;
    std::vector<float> _depth;
    std::vector<uint32_t> _ids;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif // HDCRUST_RENDER_PASS_H
