#include "renderPass.h"
#include "renderBuffer.h"
#include "renderDelegate.h"

#include "pxr/imaging/hd/renderPassState.h"
#include "sceneWriter.h"

#include <algorithm>
#include <chrono>
#include <cmath>

PXR_NAMESPACE_OPEN_SCOPE

HdCrustRenderPass::HdCrustRenderPass(HdRenderIndex *index, HdRprimCollection const &collection,
                                     HdCrustRenderDelegate *delegate)
    : HdRenderPass(index, collection)
    , _delegate(delegate)
{
}

HdCrustRenderPass::~HdCrustRenderPass() = default;

namespace {

/// crust's camera-space depth (distance along the view axis, +inf where
/// nothing was hit) as Hydra's `depth`: the point projected by `projection`
/// (row vectors, eye space looking down -Z) to normalized device depth in
/// [0, 1], misses at the far 1, as hdEmbree writes it. The host composites
/// the color with this depth (HgiInterop writes it to `gl_FragDepth` under a
/// depth test); camera-space values there discarded every pixel.
void
_ToHydraDepth(GfMatrix4d const &projection, std::vector<float> &depth)
{
    for (float &d : depth) {
        if (!std::isfinite(d)) {
            d = 1.0f;
            continue;
        }
        double const z = -double(d);
        double const clipZ = z * projection[2][2] + projection[3][2];
        double const clipW = z * projection[2][3] + projection[3][3];
        double const ndc = clipW != 0.0 ? clipZ / clipW : 1.0;
        d = float(std::clamp(0.5 * ndc + 0.5, 0.0, 1.0));
    }
}

} // namespace

bool
HdCrustRenderPass::IsConverged() const
{
    return _converged;
}

void
HdCrustRenderPass::_Execute(HdRenderPassStateSharedPtr const &renderPassState,
                            TfTokenVector const &renderTags)
{
    HdCrustRenderBuffer *color = nullptr;
    for (HdRenderPassAovBinding const &aov : renderPassState->GetAovBindings()) {
        if (aov.aovName == HdAovTokens->color) {
            color = dynamic_cast<HdCrustRenderBuffer *>(aov.renderBuffer);
        }
    }
    if (!color || color->GetWidth() == 0 || color->GetHeight() == 0) {
        _converged = true;
        return;
    }
    // The viewport as crust renders it: the framed view and projection, at
    // the color buffer's size.
    HdCrustView view;
    view.worldToView = renderPassState->GetWorldToViewMatrix();
    view.projection = renderPassState->GetProjectionMatrix();
    view.width = int(color->GetWidth());
    view.height = int(color->GetHeight());
    TfTokenVector aovs;
    for (HdRenderPassAovBinding const &aov : renderPassState->GetAovBindings()) {
        if (aov.aovName != HdAovTokens->color) {
            aovs.push_back(aov.aovName);
        }
    }
    using Clock = std::chrono::steady_clock;
    auto const ms = [](Clock::time_point a, Clock::time_point b) {
        return std::chrono::duration<double, std::milli>(b - a).count();
    };
    auto const synced = Clock::now();
    HdCrustSession *session = _delegate->Sync(view, renderTags, aovs);
    auto const afterSync = Clock::now();
    if (!session) {
        _converged = true;
        return;
    }
    size_t const w = color->GetWidth(), h = color->GetHeight();
    // Convergence is read before the image: once the render has returned,
    // the snapshot that follows is its final image. Read after, a render
    // finishing in between would report converged over the last published
    // image, a tile short of the final one.
    bool const converged = session->IsConverged();
    // After a restart, the new render's first full pass (capped), so each
    // frame of a playback or a tumble shows its own image; else whatever the
    // render published since the last redraw.
    double firstPassMs = 0.0, copyMs = 0.0;
    if (session->AwaitFirstPass(_pixels, w, h, std::chrono::milliseconds(2000))) {
        auto const waited = Clock::now();
        firstPassMs = ms(afterSync, waited);
        color->Write(_pixels.data(), w, h, 4);
        copyMs = ms(waited, Clock::now());
    } else {
        auto const before = Clock::now();
        if (session->Snapshot(_pixels, w, h)) {
            color->Write(_pixels.data(), w, h, 4);
            copyMs = ms(before, Clock::now());
        }
    }
    _delegate->RecordTimings(ms(synced, afterSync), firstPassMs, copyMs);
    _converged = converged;
    if (!_converged) {
        _aovsCopied = false;
    }
    // The other AOVs, once the render completed (spec "The render is shown
    // while it refines": AOVs at completion), each copied once; one crust
    // does not render keeps its clear value. Every bound buffer reports the
    // render's convergence: the task controller waits on each one.
    for (HdRenderPassAovBinding const &aov : renderPassState->GetAovBindings()) {
        auto *buffer = dynamic_cast<HdCrustRenderBuffer *>(aov.renderBuffer);
        if (!buffer) {
            continue;
        }
        if (buffer != color && _converged && !_aovsCopied) {
            TfToken source;
            SdfValueTypeName type;
            size_t components = 0;
            if (HdCrustAovSource(aov.aovName, &source, &type, &components)
                && session->Aov(aov.aovName.GetString(), _pixels, buffer->GetWidth(),
                                buffer->GetHeight(), components)) {
                if (aov.aovName == HdAovTokens->depth) {
                    _ToHydraDepth(view.projection, _pixels);
                }
                buffer->Write(_pixels.data(), buffer->GetWidth(), buffer->GetHeight(), components);
            } else {
                buffer->Clear(aov.aovName == HdAovTokens->depth ? 1.0f : 0.0f);
            }
        }
        buffer->SetConverged(_converged);
    }
    if (_converged) {
        _aovsCopied = true;
    }
}

PXR_NAMESPACE_CLOSE_SCOPE
