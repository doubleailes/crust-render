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
#include <pxr/base/gf/vec3f.h>
#include <pxr/imaging/cameraUtil/framing.h>
#include <pxr/imaging/hd/material.h>
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

// One-node UsdPreviewSurface network with a constant diffuseColor — the
// shape UsdImaging delivers for a bound preview-surface material.
static VtValue _MakePreviewSurface(GfVec3f const& color) {
    HdMaterialNode node;
    node.path = SdfPath("/materials/mat/preview");
    node.identifier = TfToken("UsdPreviewSurface");
    node.parameters[TfToken("diffuseColor")] = VtValue(color);
    node.parameters[TfToken("roughness")] = VtValue(0.9f);
    HdMaterialNetwork network;
    network.nodes.push_back(node);
    HdMaterialNetworkMap map;
    map.map[HdMaterialTerminalTokens->surface] = network;
    map.terminals.push_back(node.path);
    return VtValue(map);
}

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

    // Drives Execute until convergence and returns the frame; also checks
    // finiteness on every read.
    auto converge = [&]() -> std::vector<float> {
        // Always execute at least once: after an edit, IsConverged() still
        // reports the previous frame's state until an Execute syncs the
        // dirty prims and notices the change — usdview likewise re-executes
        // on scene invalidation rather than polling convergence first.
        int iterations = 0;
        do {
            HdTaskSharedPtrVector tasks = {
                std::make_shared<DrawTask>(renderPass, renderPassState)};
            engine.Execute(index.get(), &tasks);
            ++iterations;
        } while (iterations < 1024 && !renderPass->IsConverged());
        if (!renderPass->IsConverged()) {
            fprintf(stderr, "FAIL: render never converged\n");
            exit(1);
        }
        renderBuffer->Resolve();
        auto const* rgba = static_cast<float const*>(renderBuffer->Map());
        std::vector<float> frame(rgba, rgba + size_t(W) * H * 4);
        renderBuffer->Unmap();
        for (float v : frame) {
            if (!std::isfinite(v)) {
                fprintf(stderr, "FAIL: non-finite pixel\n");
                exit(1);
            }
        }
        return frame;
    };

    // 4. The image made it into the Hydra buffer.
    std::vector<float> frame = converge();
    REQUIRE(renderBuffer->IsConverged(), "buffer not marked converged");
    int nonzero = 0;
    for (float v : frame) {
        if (v > 0.0f) nonzero++;
    }
    REQUIRE(nonzero > W * H, "image is (almost) all black");

    // ---- Phase 2: materials and dirty-driven edits ----------------------

    const size_t center = (size_t(H / 2) * W + W / 2) * 4;

    // 5. Bind a red UsdPreviewSurface. AddMaterialResource inserts the
    // sprim; RebindMaterial marks the cube's DirtyMaterialId (BindMaterial
    // alone marks nothing after the first sync).
    SdfPath matId("/scene/material");
    scene.AddMaterialResource(matId, _MakePreviewSurface(GfVec3f(1.0f, 0.0f, 0.0f)));
    scene.RebindMaterial(SdfPath("/scene/cube"), matId);
    std::vector<float> red = converge();
    REQUIRE(red[center] > 2.0f * red[center + 1] &&
                red[center] > 2.0f * red[center + 2],
            "cube did not shade red from its bound UsdPreviewSurface");

    // 6. A material edit propagates through DirtyResource.
    scene.UpdateMaterialResource(matId, _MakePreviewSurface(GfVec3f(0.0f, 1.0f, 0.0f)));
    std::vector<float> green = converge();
    REQUIRE(green[center + 1] > 2.0f * green[center] &&
                green[center + 1] > 2.0f * green[center + 2],
            "material edit did not turn the cube green");

    // 7. A transform edit re-places the cached prototype (same geometry
    // version — the crust prototype cache hits) and changes the image.
    GfMatrix4f moved(1.0f);
    moved.SetTranslate(GfVec3f(2.0f, 0.0f, 0.0f));
    scene.UpdateTransform(SdfPath("/scene/cube"), moved);
    std::vector<float> shifted = converge();
    REQUIRE(shifted != green, "moving the cube changed nothing");
    REQUIRE(!(shifted[center + 1] > 2.0f * shifted[center]),
            "cube still green at center after moving away");

    // 8. A camera move takes the no-rebuild fast path
    // (crust_renderer_update_camera) and re-converges on a different frame.
    GfFrustum orbit;
    orbit.SetPosition(GfVec3d(1.5, 0.5, 5.0));
    GfCamera orbitCam;
    orbitCam.SetFromViewAndProjectionMatrix(orbit.ComputeViewMatrix(),
                                            orbit.ComputeProjectionMatrix());
    scene.UpdateTransform(camId, GfMatrix4f(orbitCam.GetTransform()));
    // HdUnitTestDelegate::UpdateTransform marks camera sprims with the
    // RPRIM DirtyTransform bit (1<<9), which HdCamera::Sync (expecting
    // HdCamera::DirtyTransform, 1<<0) ignores — mark it properly here.
    index->GetChangeTracker().MarkSprimDirty(camId, HdCamera::AllDirty);
    std::vector<float> orbited = converge();
    REQUIRE(orbited != shifted, "camera move changed nothing");

    // 9. Instancing: three placements of a new cube through an instancer,
    // then move them — both re-converge and alter the image.
    SdfPath instancerId("/scene/instancer");
    scene.AddInstancer(instancerId);
    scene.AddCube(SdfPath("/scene/proto"), GfMatrix4f(1.0f), false, instancerId);
    VtIntArray protoIndices{0, 0, 0};
    VtVec3fArray scales{GfVec3f(0.5f), GfVec3f(0.5f), GfVec3f(0.5f)};
    VtVec4fArray rotates{GfVec4f(1, 0, 0, 0), GfVec4f(1, 0, 0, 0),
                         GfVec4f(1, 0, 0, 0)};
    VtVec3fArray translates{GfVec3f(-2, 0, 0), GfVec3f(-2, 2, 0),
                            GfVec3f(-2, -2, 0)};
    scene.SetInstancerProperties(instancerId, protoIndices, scales, rotates,
                                 translates);
    std::vector<float> instanced = converge();
    REQUIRE(instanced != orbited, "instanced cubes changed nothing");

    VtVec3fArray movedTranslates{GfVec3f(-1, 0, 0), GfVec3f(-1, 2, 0),
                                 GfVec3f(-1, -2, 0)};
    scene.SetInstancerProperties(instancerId, protoIndices, scales, rotates,
                                 movedTranslates);
    std::vector<float> instancesMoved = converge();
    REQUIRE(instancesMoved != instanced, "moving instances changed nothing");

    // 10. A render buffer with hostile dimensions must refuse allocation
    // (negative values would wrap to huge unsigned sizes) and end up with
    // zero-sized storage the render pass then refuses to write into.
    SdfPath badBufferId("/scene/badBuffer");
    HdRenderBufferDescriptor badDesc;
    badDesc.dimensions = GfVec3i(-4, H, 1);
    badDesc.format = HdFormatFloat32Vec4;
    badDesc.multiSampled = false;
    scene.AddRenderBuffer(badBufferId, badDesc);
    (void)converge(); // syncs the new bprim (Allocate runs and refuses)
    auto* badBuffer = dynamic_cast<HdRenderBuffer*>(
        index->GetBprim(HdPrimTypeTokens->renderBuffer, badBufferId));
    REQUIRE(badBuffer != nullptr, "bad render buffer bprim missing");
    REQUIRE(badBuffer->GetWidth() == 0 && badBuffer->GetHeight() == 0,
            "negative-dimension Allocate was not refused");

    printf("testHdCrust PASS: beauty (%d nonzero), materials bind and edit, "
           "transform/camera/instancer edits re-converge, hostile buffer "
           "dims refused\n",
           nonzero);
    return 0;
}
