// hdCrust: the render pass, where each `_Execute` turns the viewport's state
// into the session's layers and copies crust's latest image back.
#ifndef HDCRUST_RENDER_PASS_H
#define HDCRUST_RENDER_PASS_H

#include "pxr/pxr.h"
#include "pxr/imaging/hd/renderPass.h"

#include <vector>

PXR_NAMESPACE_OPEN_SCOPE

class HdCrustRenderDelegate;

class HdCrustRenderPass final : public HdRenderPass
{
public:
    HdCrustRenderPass(HdRenderIndex *index, HdRprimCollection const &collection,
                      HdCrustRenderDelegate *delegate);
    ~HdCrustRenderPass() override;

    bool IsConverged() const override;

protected:
    void _Execute(HdRenderPassStateSharedPtr const &renderPassState,
                  TfTokenVector const &renderTags) override;

private:
    HdCrustRenderDelegate *_delegate;
    std::vector<float> _pixels;
    bool _converged = false;
    /// Whether the AOVs of the converged render were copied.
    bool _aovsCopied = false;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif
