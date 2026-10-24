// hdCrustTest: a USD stage rendered through the Crust render delegate with
// no GUI and no GL, the way usdview's engine drives it (hydra-delegate
// design, D9): UsdImaging's scene indices, a render index on the delegate by
// back-end emulation, one render pass executed until it converges, and the
// color buffer written as an EXR that `crust diff` compares with `crust
// render` of the layers the delegate wrote.
//
//   hdCrustTest <stage> <out.exr> <session dir> [--camera PATH] [--width W]
//               [--height H] [--spp N] [--frame F] [ACTION]
//
// With an ACTION, the stage is rendered to convergence, the action applied as
// a host would, and the render run to convergence again; the image written is
// the last one. Several edit actions apply in order, each run to convergence
// (a prim hidden, then shown). Each convergence prints `applied: <class>`, how
// crust's session took the change (`full`, `settings`, `lights`, `materials`):
//   --move DX,DY,DZ        the viewing camera moves (the stage is not edited)
//   --set /PRIM.ATTR=V     an attribute edited on the stage (V: one number, or
//                          comma-separated components)
//   --deactivate /PRIM     a prim removed
//   --activate /PRIM       a prim restored
//   --to-frame F           the current time changed
//   --setting KEY=V        a renderer setting changed (a number, or a token)
//   --measure N            the design record's measurements: time to the first
//                          image, N camera drags (each its sync and first
//                          pass), the copy of a refining image, the peak memory
//   --playback F0,F1       usdview's player: the time stepped one frame per
//                          redraw from F0 to F1, one Execute each; prints how
//                          many frames showed a new image, and the time each
//                          took (exits 1 unless every frame did)
// With --aov NAME (Hydra's name: depth, normal, Neye, Peye), that AOV is bound
// too, and written as <out>.<NAME>.exr, its channel named NAME as `crust
// render` names a scalar var's (a vector's NAME.X, .Y, .Z).
//
// Run with PXR_PLUGINPATH_NAME pointing at the installed hdCrust plugin.
// Exits 0 when the render converged and the image was written.

#include "pxr/pxr.h"
#include "pxr/base/gf/rect2i.h"
#include "pxr/base/gf/vec3i.h"
#include "pxr/base/tf/diagnostic.h"
#include "pxr/base/tf/stringUtils.h"
#include "pxr/base/gf/vec3d.h"
#include "pxr/base/gf/vec3f.h"
#include "pxr/imaging/cameraUtil/framing.h"
#include "pxr/imaging/hd/camera.h"
#include "pxr/imaging/hd/pluginRenderDelegateUniqueHandle.h"
#include "pxr/imaging/hd/renderBuffer.h"
#include "pxr/imaging/hd/renderDelegate.h"
#include "pxr/imaging/hd/renderIndex.h"
#include "pxr/imaging/hd/renderPass.h"
#include "pxr/imaging/hd/renderPassState.h"
#include "pxr/imaging/hd/rendererPlugin.h"
#include "pxr/imaging/hd/rendererPluginRegistry.h"
#include "pxr/imaging/hd/rprimCollection.h"
#include "pxr/imaging/hd/tokens.h"
#include "pxr/imaging/hdsi/sceneGlobalsSceneIndex.h"
#include "pxr/usd/usd/attribute.h"
#include "pxr/usd/usd/primRange.h"
#include "pxr/usd/usd/stage.h"
#include "pxr/usd/usdGeom/camera.h"
#include "pxr/usdImaging/usdImaging/sceneIndices.h"
#include "pxr/usdImaging/usdImaging/stageSceneIndex.h"

#include <algorithm>
#include <chrono>
#ifdef _WIN32
#include <windows.h>
#include <psapi.h>
#else
#include <sys/resource.h>
#endif
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>
#include <thread>
#include <vector>

PXR_NAMESPACE_USING_DIRECTIVE

namespace {

struct Args
{
    std::string stage, output, session, camera;
    int width = 160, height = 90, spp = 16;
    double frame = 0.0;
    bool hasFrame = false;
    std::string action, operand;
    /// Every action, in order: `action` and `operand` are the first.
    std::vector<std::pair<std::string, std::string>> actions;
    /// An AOV bound besides color, written as `<output>.<aov>.exr`.
    std::string aov;
};

/// A render pass state whose view is moved by `offset` (in view space): how a
/// host's free camera tumbles without touching the stage.
class MovableState : public HdRenderPassState
{
public:
    GfMatrix4d offset{1.0};
    GfMatrix4d GetWorldToViewMatrix() const override
    {
        return HdRenderPassState::GetWorldToViewMatrix() * offset;
    }
};

/// The comma-separated numbers of `text`.
std::vector<double>
Numbers(std::string const &text)
{
    std::vector<double> out;
    for (std::string const &part : TfStringSplit(text, ",")) {
        out.push_back(std::atof(part.c_str()));
    }
    return out;
}

/// `text` as a value of `type`: what --set and --setting take.
VtValue
ValueOf(std::string const &text, SdfValueTypeName const &type)
{
    std::vector<double> const n = Numbers(text);
    TfType const t = type.GetType();
    if (t == TfType::Find<float>()) {
        return VtValue(float(n.at(0)));
    }
    if (t == TfType::Find<double>()) {
        return VtValue(n.at(0));
    }
    if (t == TfType::Find<int>()) {
        return VtValue(int(n.at(0)));
    }
    if (t == TfType::Find<GfVec3f>()) {
        return VtValue(GfVec3f(float(n.at(0)), float(n.at(1)), float(n.at(2))));
    }
    if (t == TfType::Find<GfVec3d>()) {
        return VtValue(GfVec3d(n.at(0), n.at(1), n.at(2)));
    }
    if (t == TfType::Find<TfToken>()) {
        return VtValue(TfToken(text));
    }
    return VtValue(text);
}

bool
Parse(int argc, char **argv, Args *args)
{
    std::vector<std::string> positional;
    for (int i = 1; i < argc; ++i) {
        std::string const a = argv[i];
        auto next = [&]() -> std::string { return i + 1 < argc ? argv[++i] : std::string(); };
        if (a == "--camera") {
            args->camera = next();
        } else if (a == "--width") {
            args->width = std::atoi(next().c_str());
        } else if (a == "--height") {
            args->height = std::atoi(next().c_str());
        } else if (a == "--spp") {
            args->spp = std::atoi(next().c_str());
        } else if (a == "--frame") {
            args->frame = std::atof(next().c_str());
            args->hasFrame = true;
        } else if (a == "--aov") {
            args->aov = next();
        } else if (a == "--move" || a == "--set" || a == "--deactivate" || a == "--activate"
                   || a == "--to-frame" || a == "--setting" || a == "--playback"
                   || a == "--measure") {
            args->actions.emplace_back(a, next());
            if (args->action.empty()) {
                args->action = args->actions.back().first;
                args->operand = args->actions.back().second;
            }
        } else {
            positional.push_back(a);
        }
    }
    if (positional.size() != 3 || args->width <= 0 || args->height <= 0) {
        return false;
    }
    args->stage = positional[0];
    args->output = positional[1];
    args->session = positional[2];
    return true;
}

/// The first camera on the stage, when none was named.
std::string
FirstCamera(UsdStageRefPtr const &stage)
{
    for (UsdPrim const &prim : stage->Traverse()) {
        if (prim.IsA<UsdGeomCamera>()) {
            return prim.GetPath().GetString();
        }
    }
    return std::string();
}

/// Writes `pixels` (`width` x `height` x `names.size()` floats, interleaved
/// in `names` order, rows top-down) as an uncompressed scanline float EXR
/// with those channel names, bare, as `crust render` names a beauty (`R`,
/// `G`, `B`) or a scalar AOV (its var's name): HioImage's writer would put
/// them in a layer.
bool
WriteExr(std::string const &path, int width, int height, std::vector<std::string> const &names,
         std::vector<float> const &pixels)
{
    std::vector<char> out;
    auto bytes = [&](void const *p, size_t n) {
        char const *c = static_cast<char const *>(p);
        out.insert(out.end(), c, c + n);
    };
    auto i32 = [&](int32_t v) { bytes(&v, 4); };
    auto str = [&](char const *s) { bytes(s, std::strlen(s) + 1); };
    auto attribute = [&](char const *name, char const *type, int32_t size) {
        str(name);
        str(type);
        i32(size);
    };
    // EXR stores channels in alphabetical order.
    std::vector<size_t> order(names.size());
    for (size_t c = 0; c < order.size(); ++c) {
        order[c] = c;
    }
    std::sort(order.begin(), order.end(), [&](size_t a, size_t b) { return names[a] < names[b]; });
    size_t const n = names.size();

    i32(20000630); // magic
    i32(2);        // version 2, scanline
    int32_t listSize = 1;
    for (std::string const &name : names) {
        listSize += int32_t(name.size() + 1 + 16);
    }
    attribute("channels", "chlist", listSize);
    for (size_t c : order) {
        str(names[c].c_str());
        i32(2); // FLOAT
        i32(0); // pLinear + reserved
        i32(1);
        i32(1);
    }
    out.push_back(0);
    attribute("compression", "compression", 1);
    out.push_back(0);
    int32_t const window[4] = {0, 0, width - 1, height - 1};
    attribute("dataWindow", "box2i", 16);
    bytes(window, 16);
    attribute("displayWindow", "box2i", 16);
    bytes(window, 16);
    attribute("lineOrder", "lineOrder", 1);
    out.push_back(0);
    float const one = 1.0f, zero[2] = {0.0f, 0.0f};
    attribute("pixelAspectRatio", "float", 4);
    bytes(&one, 4);
    attribute("screenWindowCenter", "v2f", 8);
    bytes(zero, 8);
    attribute("screenWindowWidth", "float", 4);
    bytes(&one, 4);
    out.push_back(0);
    // One scanline per chunk: the offset table, then each line's channels.
    size_t const line = size_t(width) * n * 4;
    uint64_t offset = out.size() + size_t(height) * 8;
    for (int y = 0; y < height; ++y) {
        bytes(&offset, 8);
        offset += 8 + line;
    }
    for (int y = 0; y < height; ++y) {
        i32(y);
        i32(int32_t(line));
        for (size_t c : order) {
            for (int x = 0; x < width; ++x) {
                bytes(&pixels[(size_t(y) * width + x) * n + c], 4);
            }
        }
    }
    FILE *f = std::fopen(path.c_str(), "wb");
    if (!f) {
        return false;
    }
    bool const ok = std::fwrite(out.data(), 1, out.size(), f) == out.size();
    return std::fclose(f) == 0 && ok;
}

/// A bound buffer's pixels, rows top-down (Hydra's go up), `components`
/// floats per pixel.
std::vector<float>
ReadBuffer(HdRenderBuffer *buffer, size_t components)
{
    size_t const w = buffer->GetWidth(), h = buffer->GetHeight();
    size_t const stride = HdGetComponentCount(buffer->GetFormat());
    std::vector<float> out(w * h * components);
    float const *data = static_cast<float const *>(buffer->Map());
    for (size_t y = 0; y < h; ++y) {
        float const *row = data + (h - 1 - y) * w * stride;
        for (size_t x = 0; x < w; ++x) {
            for (size_t c = 0; c < components; ++c) {
                out[(y * w + x) * components + c] = row[x * stride + c];
            }
        }
    }
    buffer->Unmap();
    return out;
}

/// The process's peak resident memory, in MiB.
double
PeakMemoryMiB()
{
#ifdef _WIN32
    PROCESS_MEMORY_COUNTERS counters{};
    if (GetProcessMemoryInfo(GetCurrentProcess(), &counters, sizeof counters)) {
        return double(counters.PeakWorkingSetSize) / (1024.0 * 1024.0);
    }
    return 0.0;
#else
    struct rusage usage{};
    getrusage(RUSAGE_SELF, &usage);
    return double(usage.ru_maxrss) / 1024.0; // KiB on Linux
#endif
}

/// A delegate render stat, as a double (0 when absent).
double
Stat(HdRenderDelegate *delegate, char const *name)
{
    VtDictionary const stats = delegate->GetRenderStats();
    auto const it = stats.find(name);
    return it != stats.end() && it->second.IsHolding<double>() ? it->second.UncheckedGet<double>()
                                                              : 0.0;
}

} // namespace

int
main(int argc, char **argv)
{
    auto const processStarted = std::chrono::steady_clock::now();
    Args args;
    if (!Parse(argc, argv, &args)) {
        std::fprintf(stderr,
                     "usage: hdCrustTest <stage> <out.exr> <session dir> [--camera PATH] "
                     "[--width W] [--height H] [--spp N] [--frame F]\n");
        return 2;
    }
    UsdStageRefPtr stage = UsdStage::Open(args.stage);
    double const openMs = std::chrono::duration<double, std::milli>(
                              std::chrono::steady_clock::now() - processStarted)
                              .count();
    if (!stage) {
        std::fprintf(stderr, "hdCrustTest: cannot open %s\n", args.stage.c_str());
        return 1;
    }
    if (args.camera.empty()) {
        args.camera = FirstCamera(stage);
    }
    if (args.camera.empty()) {
        std::fprintf(stderr, "hdCrustTest: %s has no camera\n", args.stage.c_str());
        return 1;
    }

    // The scene indices usdview's engine builds, at the frame.
    UsdImagingCreateSceneIndicesInfo info;
    info.stage = stage;
    UsdImagingSceneIndices sceneIndices = UsdImagingCreateSceneIndices(info);
    // The scene globals the engine keeps over them, whose current frame it
    // sets with the stage's time (UsdImagingGLEngine::SetTime).
    HdsiSceneGlobalsSceneIndexRefPtr globals =
        HdsiSceneGlobalsSceneIndex::New(sceneIndices.finalSceneIndex);
    auto setTime = [&](UsdTimeCode time) {
        sceneIndices.stageSceneIndex->SetTime(time);
        globals->SetCurrentFrame(time.GetValue());
    };
    setTime(args.hasFrame ? UsdTimeCode(args.frame) : UsdTimeCode::Default());

    // The delegate through its plugin, with the session's settings.
    HdRenderSettingsMap settings;
    settings[TfToken("crust:samplesPerPixel")] = VtValue(args.spp);
    settings[TfToken("crust:sessionDirectory")] = VtValue(args.session);
    HdPluginRenderDelegateUniqueHandle delegate =
        HdRendererPluginRegistry::GetInstance().CreateRenderDelegate(
            TfToken("HdCrustRendererPlugin"), settings);
    if (!delegate) {
        std::fprintf(stderr, "hdCrustTest: the Crust renderer plugin is not found "
                             "(set PXR_PLUGINPATH_NAME)\n");
        return 1;
    }
    for (auto const &[key, value] : settings) {
        delegate->SetRenderSetting(key, value);
    }
    std::unique_ptr<HdRenderIndex> index(
        HdRenderIndex::NewForBackendEmulation(delegate.Get(), {}, globals));

    // The color AOV, at the viewport's size.
    SdfPath const bufferPath("/hdCrustTest/color");
    HdRenderBuffer *color = static_cast<HdRenderBuffer *>(
        delegate->CreateBprim(HdPrimTypeTokens->renderBuffer, bufferPath));
    color->Allocate(GfVec3i(args.width, args.height, 1), HdFormatFloat32Vec4, false);
    HdRenderPassAovBinding binding;
    binding.aovName = HdAovTokens->color;
    binding.renderBuffer = color;
    binding.renderBufferId = bufferPath;
    binding.clearValue = VtValue(GfVec4f(0.0f));

    HdRprimCollection const collection(HdTokens->geometry, HdReprSelector(HdReprTokens->smoothHull));
    HdRenderPassSharedPtr pass = delegate->CreateRenderPass(index.get(), collection);
    auto state = std::make_shared<MovableState>();
    HdRenderPassAovBindingVector bindings = {binding};
    HdRenderBuffer *aovBuffer = nullptr;
    size_t const aovComponents = args.aov == "depth" ? 1 : 3;
    if (!args.aov.empty()) {
        SdfPath const path("/hdCrustTest/aov");
        aovBuffer = static_cast<HdRenderBuffer *>(
            delegate->CreateBprim(HdPrimTypeTokens->renderBuffer, path));
        aovBuffer->Allocate(GfVec3i(args.width, args.height, 1),
                            aovComponents == 1 ? HdFormatFloat32 : HdFormatFloat32Vec3, false);
        HdRenderPassAovBinding aov;
        aov.aovName = TfToken(args.aov);
        aov.renderBuffer = aovBuffer;
        aov.renderBufferId = path;
        bindings.push_back(aov);
    }
    state->SetAovBindings(bindings);
    state->SetFraming(CameraUtilFraming(GfRect2i(GfVec2i(0), args.width, args.height)));
    TfTokenVector const renderTags = {HdRenderTagTokens->geometry, HdRenderTagTokens->render};

    HdTaskSharedPtrVector tasks;
    HdTaskContext context;
    // Renders until the pass converges, then says how the session took the
    // last change.
    auto converge = [&]() -> bool {
        auto const started = std::chrono::steady_clock::now();
        for (;;) {
            sceneIndices.stageSceneIndex->ApplyPendingUpdates();
            index->SyncAll(&tasks, &context);
            HdCamera const *camera = static_cast<HdCamera const *>(
                index->GetSprim(HdPrimTypeTokens->camera, SdfPath(args.camera)));
            if (!camera) {
                std::fprintf(stderr, "hdCrustTest: %s is not a camera\n", args.camera.c_str());
                return false;
            }
            state->SetCamera(camera);
            state->Prepare(delegate->GetResourceRegistry());
            pass->Sync();
            pass->Execute(state, renderTags);
            if (pass->IsConverged()) {
                break;
            }
            if (std::chrono::steady_clock::now() - started > std::chrono::minutes(10)) {
                std::fprintf(stderr, "hdCrustTest: the render did not converge\n");
                return false;
            }
            std::this_thread::sleep_for(std::chrono::milliseconds(20));
        }
        VtDictionary const stats = delegate->GetRenderStats();
        auto const it = stats.find("crust:applied");
        std::printf("applied: %s\n",
                    it != stats.end() ? it->second.Get<std::string>().c_str() : "?");
        std::fflush(stdout);
        return true;
    };
    if (args.action == "--measure") {
        using Clock = std::chrono::steady_clock;
        auto const since = [](Clock::time_point t) {
            return std::chrono::duration<double, std::milli>(Clock::now() - t).count();
        };
        auto redraw = [&]() {
            sceneIndices.stageSceneIndex->ApplyPendingUpdates();
            index->SyncAll(&tasks, &context);
            HdCamera const *camera = static_cast<HdCamera const *>(
                index->GetSprim(HdPrimTypeTokens->camera, SdfPath(args.camera)));
            state->SetCamera(camera);
            state->Prepare(delegate->GetResourceRegistry());
            pass->Sync();
            pass->Execute(state, renderTags);
        };
        // The first image: population, the layers written, crust's import,
        // its first full pass.
        auto const firstStarted = Clock::now();
        redraw();
        double const firstMs = since(firstStarted);
        std::printf("stage open: %.0f ms\n", openMs);
        std::printf("first redraw: %.0f ms (sync %.0f: layers + import; first pass %.0f)\n",
                    firstMs, Stat(delegate.Get(), "crust:syncMs"),
                    Stat(delegate.Get(), "crust:firstPassMs"));
        std::printf("time to first image: %.0f ms from process start\n", since(processStarted));
        // Drags: the camera moves a little per redraw, as a tumble does.
        int const drags = std::atoi(args.operand.c_str());
        double syncSum = 0, syncMax = 0, passSum = 0, passMax = 0, redrawSum = 0;
        for (int i = 1; i <= drags; ++i) {
            state->offset.SetTranslate(GfVec3d(0.02 * i, 0.0, 0.0));
            auto const started = Clock::now();
            redraw();
            double const total = since(started);
            double const sync = Stat(delegate.Get(), "crust:syncMs");
            double const pass1 = Stat(delegate.Get(), "crust:firstPassMs");
            syncSum += sync;
            syncMax = std::max(syncMax, sync);
            passSum += pass1;
            passMax = std::max(passMax, pass1);
            redrawSum += total;
        }
        if (drags > 0) {
            std::printf("drag (%d): redraw %.0f ms mean; sync (settings rebuild) %.0f mean, %.0f max; "
                        "first pass %.0f mean, %.0f max\n",
                        drags, redrawSum / drags, syncSum / drags, syncMax, passSum / drags,
                        passMax);
        }
        // The copy of a refining image into the buffer, redraw by redraw.
        double copySum = 0;
        int copies = 0;
        for (int i = 0; i < 40 && copies < 20; ++i) {
            std::this_thread::sleep_for(std::chrono::milliseconds(50));
            redraw();
            double const copy = Stat(delegate.Get(), "crust:copyMs");
            if (copy > 0.0) {
                copySum += copy;
                ++copies;
            }
        }
        if (copies > 0) {
            std::printf("snapshot copy at %dx%d: %.2f ms mean over %d copies\n", args.width,
                        args.height, copySum / copies, copies);
        }
        std::printf("peak memory: %.0f MiB\n", PeakMemoryMiB());
        std::fflush(stdout);
        return 0;
    }
    if (!converge()) {
        return 1;
    }
    if (args.action == "--playback") {
        // One redraw per frame, as the player does: no waiting for
        // convergence. A frame shows when its redraw changed the buffer.
        std::vector<double> const range = Numbers(args.operand);
        auto pixels = [&]() {
            float const *p = static_cast<float const *>(color->Map());
            std::vector<float> copy(p, p + size_t(args.width) * args.height * 4);
            color->Unmap();
            return copy;
        };
        std::vector<float> last = pixels();
        // A frame with a crust tile (16 x 16) still black: shown before its
        // first pass was whole. The playback samples have no black regions.
        auto hasBlackTile = [&](std::vector<float> const &img) {
            for (int ty = 0; ty < args.height; ty += 16) {
                for (int tx = 0; tx < args.width; tx += 16) {
                    bool black = true;
                    for (int y = ty; black && y < std::min(ty + 16, args.height); ++y) {
                        for (int x = tx; black && x < std::min(tx + 16, args.width); ++x) {
                            float const *p = &img[(size_t(y) * args.width + x) * 4];
                            black = p[0] == 0.0f && p[1] == 0.0f && p[2] == 0.0f;
                        }
                    }
                    if (black) {
                        return true;
                    }
                }
            }
            return false;
        };
        int frames = 0, shown = 0, partial = 0;
        auto const started = std::chrono::steady_clock::now();
        for (double f = range.at(0); f <= range.at(1); f += 1.0) {
            setTime(UsdTimeCode(f));
            sceneIndices.stageSceneIndex->ApplyPendingUpdates();
            index->SyncAll(&tasks, &context);
            state->Prepare(delegate->GetResourceRegistry());
            pass->Sync();
            pass->Execute(state, renderTags);
            std::vector<float> now = pixels();
            ++frames;
            shown += now != last;
            partial += hasBlackTile(now);
            last.swap(now);
        }
        double const ms = std::chrono::duration<double, std::milli>(
                              std::chrono::steady_clock::now() - started)
                              .count();
        std::printf("playback: %d/%d frames shown, %d with a black tile, %.1f ms per frame\n",
                    shown, frames, partial, ms / std::max(frames, 1));
        if (shown != frames || partial != 0) {
            return 1;
        }
        if (!converge()) {
            return 1;
        }
    } else {
        for (auto const &[action, operand] : args.actions) {
            if (action == "--move") {
                std::vector<double> const d = Numbers(operand);
                state->offset.SetTranslate(GfVec3d(d.at(0), d.at(1), d.at(2)));
            } else if (action == "--set") {
                std::string::size_type const eq = operand.find('=');
                UsdAttribute attr = stage->GetAttributeAtPath(SdfPath(operand.substr(0, eq)));
                if (!attr || !attr.Set(ValueOf(operand.substr(eq + 1), attr.GetTypeName()))) {
                    std::fprintf(stderr, "hdCrustTest: cannot set %s\n", operand.c_str());
                    return 1;
                }
            } else if (action == "--deactivate" || action == "--activate") {
                UsdPrim prim = stage->GetPrimAtPath(SdfPath(operand));
                if (!prim || !prim.SetActive(action == "--activate")) {
                    std::fprintf(stderr, "hdCrustTest: cannot %s %s\n", action.c_str() + 2,
                                 operand.c_str());
                    return 1;
                }
            } else if (action == "--to-frame") {
                setTime(UsdTimeCode(std::atof(operand.c_str())));
            } else if (action == "--setting") {
                std::string::size_type const eq = operand.find('=');
                std::string const key = operand.substr(0, eq), text = operand.substr(eq + 1);
                char *end = nullptr;
                double const number = std::strtod(text.c_str(), &end);
                VtValue value = *end == '\0' && !text.empty()
                    ? (text.find('.') == std::string::npos ? VtValue(int(number)) : VtValue(number))
                    : VtValue(TfToken(text));
                delegate->SetRenderSetting(TfToken(key), value);
            }
            if (!converge()) {
                return 1;
            }
        }
    }

    // The color buffer as an RGB float EXR: the beauty `crust render` writes
    // is RGB too.
    if (!WriteExr(args.output, args.width, args.height, {"R", "G", "B"}, ReadBuffer(color, 3))) {
        std::fprintf(stderr, "hdCrustTest: cannot write %s\n", args.output.c_str());
        return 1;
    }
    if (aovBuffer) {
        std::string const out =
            args.output.substr(0, args.output.rfind('.')) + "." + args.aov + ".exr";
        std::vector<std::string> names = {args.aov};
        if (aovComponents == 3) {
            names = {args.aov + ".X", args.aov + ".Y", args.aov + ".Z"};
        }
        std::vector<float> const aovPixels = ReadBuffer(aovBuffer, aovComponents);
        if (args.aov == "depth") {
            // Hydra's depth: normalized device depth in [0, 1], what the host
            // composites with; something in front of the far plane.
            bool inRange = true, hit = false;
            for (float d : aovPixels) {
                inRange = inRange && d >= 0.0f && d <= 1.0f;
                hit = hit || d < 1.0f;
            }
            std::printf("depth: %s, %s\n", inRange ? "in [0, 1]" : "OUT OF [0, 1]",
                        hit ? "something hit" : "NOTHING HIT");
            if (!inRange || !hit) {
                return 1;
            }
        }
        if (!WriteExr(out, args.width, args.height, names, aovPixels)) {
            std::fprintf(stderr, "hdCrustTest: cannot write %s\n", out.c_str());
            return 1;
        }
        delegate->DestroyBprim(aovBuffer);
    }
    delegate->DestroyBprim(color);
    return 0;
}
