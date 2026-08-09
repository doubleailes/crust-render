// Headless end-to-end check of the hdCrust plugin, no GL and no usdview:
// discovers the plugin through Hydra's registry (so PXR_PLUGINPATH_NAME
// must point at the installed resources/ dir), builds a tiny scene with
// hd's unit-test scene delegate, drives the render pass the way
// Hd_TestDriver does, and asserts the color buffer converged to nonzero
// pixels.
//
// Build with -DHDCRUST_BUILD_TESTS=ON; see README.md.

#include <pxr/base/gf/camera.h>
#include <pxr/base/gf/frustum.h>
#include <pxr/base/gf/matrix4f.h>
#include <pxr/base/gf/rect2i.h>
#include <pxr/imaging/cameraUtil/framing.h>
#include <pxr/imaging/hd/camera.h>
#include <pxr/imaging/hd/engine.h>
#include <pxr/imaging/hd/pluginRenderDelegateUniqueHandle.h>
#include <pxr/imaging/hd/renderBuffer.h>
#include <pxr/imaging/hd/renderDelegate.h>
#include <pxr/imaging/hd/renderIndex.h>
#include <pxr/imaging/hd/renderPass.h>
#include <pxr/imaging/hd/renderPassState.h>
#include <pxr/imaging/hd/rendererPluginRegistry.h>
#include <pxr/imaging/hd/task.h>
#include <pxr/imaging/hd/tokens.h>
#include <pxr/imaging/hd/unitTestDelegate.h>

#include <cmath>
#include <cstdio>
#include <cstdlib>
#include <memory>

PXR_NAMESPACE_USING_DIRECTIVE

#define REQUIRE(cond, msg)                                        \
    do {                                                          \
        if (!(cond)) {                                            \
            fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__, msg); \
            return 1;                                             \
        }                                                         \
    } while (0)

namespace {

// The minimal task Hd_TestDriver uses: sync the pass, prepare the state,
// execute.
class DrawTask final : public HdTask {
public:
    DrawTask(HdRenderPassSharedPtr renderPass,
             HdRenderPassStateSharedPtr renderPassState)
        : HdTask(SdfPath::EmptyPath()),
          _renderPass(std::move(renderPass)),
          _renderPassState(std::move(renderPassState)),
          _renderTags{HdRenderTagTokens->geometry} {}

    void Sync(HdSceneDelegate*, HdTaskContext*, HdDirtyBits*) override {
        _renderPass->Sync();
    }
    void Prepare(HdTaskContext*, HdRenderIndex* renderIndex) override {
        _renderPassState->Prepare(renderIndex->GetResourceRegistry());
    }
    void Execute(HdTaskContext*) override {
        _renderPass->Execute(_renderPassState, _renderTags);
    }
    const TfTokenVector& GetRenderTags() const override { return _renderTags; }

private:
    HdRenderPassSharedPtr _renderPass;
    HdRenderPassStateSharedPtr _renderPassState;
    TfTokenVector _renderTags;
};

} // namespace

int main() {
    constexpr int W = 64;
    constexpr int H = 64;

    // 1. The plugin must be discoverable through the registry — this is
    // the piece usdview exercises that no Rust test can.
    HdPluginRenderDelegateUniqueHandle delegate =
        HdRendererPluginRegistry::GetInstance().CreateRenderDelegate(
            TfToken("HdCrustRendererPlugin"));
    REQUIRE(delegate,
            "HdCrustRendererPlugin not found — point PXR_PLUGINPATH_NAME at "
            "the installed hdCrust/resources directory");

    std::unique_ptr<HdRenderIndex> index(
        HdRenderIndex::New(delegate.Get(), HdDriverVector()));
    REQUIRE(index != nullptr, "render index creation failed");

    // 2. A cube, a camera and a color buffer through hd's unit-test scene
    // delegate. No light on purpose: crust's built-in sky guarantees
    // nonzero pixels, and the cube silhouettes against it.
    HdUnitTestDelegate scene(index.get(), SdfPath("/scene"));
    scene.AddCube(SdfPath("/scene/cube"), GfMatrix4f(1.0f));

    SdfPath camId("/scene/camera");
    scene.AddCamera(camId);
    GfFrustum frustum;
    frustum.SetPosition(GfVec3d(0.0, 0.0, 5.0));
    GfCamera gfCam;
    gfCam.SetFromViewAndProjectionMatrix(frustum.ComputeViewMatrix(),
                                         frustum.ComputeProjectionMatrix());
    scene.UpdateTransform(camId, GfMatrix4f(gfCam.GetTransform()));
    scene.UpdateCamera(camId, HdCameraTokens->projection,
                       VtValue(HdCamera::Perspective));
    scene.UpdateCamera(
        camId, HdCameraTokens->focalLength,
        VtValue(gfCam.GetFocalLength() * float(GfCamera::FOCAL_LENGTH_UNIT)));
    scene.UpdateCamera(
        camId, HdCameraTokens->horizontalAperture,
        VtValue(gfCam.GetHorizontalAperture() * float(GfCamera::APERTURE_UNIT)));
    scene.UpdateCamera(
        camId, HdCameraTokens->verticalAperture,
        VtValue(gfCam.GetVerticalAperture() * float(GfCamera::APERTURE_UNIT)));
    scene.UpdateCamera(camId, HdCameraTokens->clippingRange,
                       VtValue(gfCam.GetClippingRange()));

    SdfPath bufferId("/scene/color");
    HdRenderBufferDescriptor desc;
    desc.dimensions = GfVec3i(W, H, 1);
    desc.format = HdFormatFloat32Vec4;
    desc.multiSampled = false;
    scene.AddRenderBuffer(bufferId, desc);

    // 3. The render pass, driven exactly the way usdview's task graph
    // would: repeat Execute until the delegate reports convergence.
    HdRprimCollection collection(HdTokens->geometry,
                                 HdReprSelector(HdReprTokens->smoothHull));
    HdRenderPassSharedPtr renderPass =
        delegate->CreateRenderPass(index.get(), collection);
    HdRenderPassStateSharedPtr renderPassState =
        delegate->CreateRenderPassState();

    HdEngine engine;
    // One warm-up execute so sprims/bprims sync before we look them up.
    {
        HdTaskSharedPtrVector tasks = {
            std::make_shared<DrawTask>(renderPass, renderPassState)};
        engine.Execute(index.get(), &tasks);
    }

    auto const* camera = dynamic_cast<HdCamera const*>(
        index->GetSprim(HdPrimTypeTokens->camera, camId));
    REQUIRE(camera != nullptr, "camera sprim missing");
    renderPassState->SetCamera(camera);
    renderPassState->SetFraming(CameraUtilFraming(
        GfRect2i(GfVec2i(0, 0), W, H)));

    auto* renderBuffer = dynamic_cast<HdRenderBuffer*>(
        index->GetBprim(HdPrimTypeTokens->renderBuffer, bufferId));
    REQUIRE(renderBuffer != nullptr, "render buffer bprim missing");
    HdRenderPassAovBindingVector aovBindings(1);
    aovBindings[0].aovName = HdAovTokens->color;
    aovBindings[0].renderBuffer = renderBuffer;
    aovBindings[0].clearValue = VtValue(GfVec4f(0.0f));
    renderPassState->SetAovBindings(aovBindings);

    int iterations = 0;
    for (; iterations < 256 && !renderPass->IsConverged(); ++iterations) {
        HdTaskSharedPtrVector tasks = {
            std::make_shared<DrawTask>(renderPass, renderPassState)};
        engine.Execute(index.get(), &tasks);
    }
    REQUIRE(renderPass->IsConverged(), "render never converged");
    REQUIRE(renderBuffer->IsConverged(), "buffer not marked converged");

    // 4. The image made it into the Hydra buffer.
    renderBuffer->Resolve();
    auto const* rgba = static_cast<float const*>(renderBuffer->Map());
    REQUIRE(rgba != nullptr, "Map() returned NULL");
    int nonzero = 0;
    bool finite = true;
    for (int i = 0; i < W * H * 4; ++i) {
        if (rgba[i] > 0.0f) nonzero++;
        if (!std::isfinite(rgba[i])) finite = false;
    }
    renderBuffer->Unmap();
    REQUIRE(finite, "non-finite pixels");
    REQUIRE(nonzero > W * H, "image is (almost) all black");

    printf("testHdCrust PASS: converged after %d executes, %d nonzero "
           "channel values\n",
           iterations, nonzero);
    return 0;
}
