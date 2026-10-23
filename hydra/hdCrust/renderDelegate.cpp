#include "renderDelegate.h"
#include "renderBuffer.h"
#include "renderPass.h"

#include "pxr/imaging/hd/camera.h"
#include "pxr/imaging/hd/instancer.h"
#include "pxr/imaging/hd/rprim.h"
#include "pxr/imaging/hd/tokens.h"
#include "pxr/base/tf/stringUtils.h"
#include "pxr/imaging/hd/sceneGlobalsSchema.h"
#include "pxr/usd/usd/stage.h"

#include <algorithm>
#include <cmath>

PXR_NAMESPACE_OPEN_SCOPE

HdCrustRenderDelegate::HdCrustRenderDelegate()
    : _resourceRegistry(std::make_shared<HdResourceRegistry>())
{
}

HdCrustRenderDelegate::HdCrustRenderDelegate(HdRenderSettingsMap const &settingsMap)
    : HdRenderDelegate(settingsMap)
    , _resourceRegistry(std::make_shared<HdResourceRegistry>())
{
}

HdCrustRenderDelegate::~HdCrustRenderDelegate()
{
    if (_sceneIndex) {
        _sceneIndex->RemoveObserver(HdSceneIndexObserverPtr(&_observer));
    }
}

const TfTokenVector &
HdCrustRenderDelegate::GetSupportedRprimTypes() const
{
    // None: the scene index is observed whole, so no rprim needs to exist in
    // the render index for crust to see it (design D1).
    static const TfTokenVector types;
    return types;
}

const TfTokenVector &
HdCrustRenderDelegate::GetSupportedSprimTypes() const
{
    // The render pass state's camera.
    static const TfTokenVector types = {HdPrimTypeTokens->camera};
    return types;
}

const TfTokenVector &
HdCrustRenderDelegate::GetSupportedBprimTypes() const
{
    // The AOV bindings' buffers.
    static const TfTokenVector types = {HdPrimTypeTokens->renderBuffer};
    return types;
}

HdResourceRegistrySharedPtr
HdCrustRenderDelegate::GetResourceRegistry() const
{
    return _resourceRegistry;
}

HdRenderPassSharedPtr
HdCrustRenderDelegate::CreateRenderPass(HdRenderIndex *index, HdRprimCollection const &collection)
{
    return std::make_shared<HdCrustRenderPass>(index, collection, this);
}

HdInstancer *
HdCrustRenderDelegate::CreateInstancer(HdSceneDelegate *delegate, SdfPath const &id)
{
    // The render index inserts every instancer and tracks it; crust reads
    // instancers from the scene index, so the base class does nothing more.
    return new HdInstancer(delegate, id);
}

void
HdCrustRenderDelegate::DestroyInstancer(HdInstancer *instancer)
{
    delete instancer;
}

HdRprim *
HdCrustRenderDelegate::CreateRprim(TfToken const &, SdfPath const &)
{
    return nullptr;
}

void
HdCrustRenderDelegate::DestroyRprim(HdRprim *rPrim)
{
    delete rPrim;
}

HdSprim *
HdCrustRenderDelegate::CreateSprim(TfToken const &typeId, SdfPath const &sprimId)
{
    if (typeId == HdPrimTypeTokens->camera) {
        return new HdCamera(sprimId);
    }
    return nullptr;
}

HdSprim *
HdCrustRenderDelegate::CreateFallbackSprim(TfToken const &typeId)
{
    return CreateSprim(typeId, SdfPath::EmptyPath());
}

void
HdCrustRenderDelegate::DestroySprim(HdSprim *sprim)
{
    delete sprim;
}

HdBprim *
HdCrustRenderDelegate::CreateBprim(TfToken const &typeId, SdfPath const &bprimId)
{
    if (typeId == HdPrimTypeTokens->renderBuffer) {
        return new HdCrustRenderBuffer(bprimId);
    }
    return nullptr;
}

HdBprim *
HdCrustRenderDelegate::CreateFallbackBprim(TfToken const &typeId)
{
    return CreateBprim(typeId, SdfPath::EmptyPath());
}

void
HdCrustRenderDelegate::DestroyBprim(HdBprim *bprim)
{
    delete bprim;
}

void
HdCrustRenderDelegate::CommitResources(HdChangeTracker *)
{
}

HdAovDescriptor
HdCrustRenderDelegate::GetDefaultAovDescriptor(TfToken const &name) const
{
    if (name == HdAovTokens->color) {
        return HdAovDescriptor(HdFormatFloat32Vec4, false, VtValue(GfVec4f(0.0f)));
    }
    if (name == HdAovTokens->depth) {
        return HdAovDescriptor(HdFormatFloat32, false, VtValue(1.0f));
    }
    return HdAovDescriptor();
}

void
HdCrustRenderDelegate::SetTerminalSceneIndex(const HdSceneIndexBaseRefPtr &terminalSceneIndex)
{
    if (_sceneIndex) {
        _sceneIndex->RemoveObserver(HdSceneIndexObserverPtr(&_observer));
    }
    _sceneIndex = terminalSceneIndex;
    if (_sceneIndex) {
        _sceneIndex->AddObserver(HdSceneIndexObserverPtr(&_observer));
    }
    MarkSceneDirty();
}

namespace {

/// Whether a notice about `path` is about the scene rather than Hydra's own
/// bookkeeping: the task controller's prims and the free camera live under
/// root names starting with `_` (`/_UsdImaging...`), and a change to them (a
/// tumble) must not rewrite the scene.
bool
_InScene(SdfPath const &path)
{
    if (path.IsAbsoluteRootPath()) {
        return true;
    }
    SdfPath root = path;
    while (root.GetPathElementCount() > 1) {
        root = root.GetParentPath();
    }
    return !TfStringStartsWith(root.GetName(), "_");
}

} // namespace

void
HdCrustRenderDelegate::_Observer::PrimsAdded(HdSceneIndexBase const &,
                                             AddedPrimEntries const &entries)
{
    std::lock_guard<std::mutex> lock(_delegate->_pendingMutex);
    for (auto const &e : entries) {
        if (_InScene(e.primPath)) {
            _delegate->_changed.insert(e.primPath);
        }
    }
}

void
HdCrustRenderDelegate::_Observer::PrimsRemoved(HdSceneIndexBase const &,
                                               RemovedPrimEntries const &entries)
{
    std::lock_guard<std::mutex> lock(_delegate->_pendingMutex);
    for (auto const &e : entries) {
        if (_InScene(e.primPath)) {
            _delegate->_removed.insert(e.primPath);
        }
    }
}

void
HdCrustRenderDelegate::_Observer::PrimsDirtied(HdSceneIndexBase const &,
                                               DirtiedPrimEntries const &entries)
{
    std::lock_guard<std::mutex> lock(_delegate->_pendingMutex);
    for (auto const &e : entries) {
        if (_InScene(e.primPath)) {
            _delegate->_changed.insert(e.primPath);
        }
    }
}

void
HdCrustRenderDelegate::_Observer::PrimsRenamed(HdSceneIndexBase const &,
                                               RenamedPrimEntries const &entries)
{
    for (auto const &e : entries) {
        if (_InScene(e.oldPrimPath) || _InScene(e.newPrimPath)) {
            _delegate->MarkSceneDirty();
            return;
        }
    }
}

namespace {

/// The renderer settings a host shows (spec "Render settings are exposed to
/// the host"), each the `crust:*` attribute it is written as, with crust's
/// own default: what a render of a stage that authors none uses.
struct _Setting
{
    char const *name;
    char const *key;
    VtValue fallback;
    /// Written even when neither the stage nor the host sets it: the
    /// viewport's own default, where it differs from a final render's.
    bool viewport = false;
};

std::vector<_Setting> const &
_Settings()
{
    static std::vector<_Setting> const settings = {
        {"Samples per pixel", "crust:samplesPerPixel", VtValue(128)},
        {"Minimum samples per pixel", "crust:minSamplesPerPixel", VtValue(32)},
        {"Variance threshold", "crust:varianceThreshold", VtValue(0.05f)},
        {"Max depth", "crust:maxDepth", VtValue(32)},
        {"Indirect clamp", "crust:indirectClamp", VtValue(10.0f)},
        // A string, not a token: usdview's settings dialog edits bools,
        // ints, floats and strings only. Written as the token crust reads.
        {"Sampling strategy", "crust:samplingStrategy", VtValue(std::string("power"))},
        // A viewport mostly shows a few samples per pixel, where blue noise
        // across pixels reads as finer grain than Sobol's white noise.
        {"Sampler", "crust:sampler", VtValue(std::string("pmjBn")), true},
    };
    return settings;
}

/// Not written to the layer: where the layers live.
TfToken const &
_SessionDirectory()
{
    static TfToken const t("crust:sessionDirectory");
    return t;
}

/// `value` as the type of `fallback`, which is the attribute type crust
/// reads: a host may hand an int as a double, or a token as a string.
VtValue
_AsFallbackType(VtValue const &value, VtValue const &fallback)
{
    if (fallback.IsHolding<TfToken>() && value.IsHolding<std::string>()) {
        return VtValue(TfToken(value.UncheckedGet<std::string>()));
    }
    VtValue cast = VtValue::CastToTypeOf(value, fallback);
    return cast.IsEmpty() ? value : cast;
}

} // namespace

bool
HdCrustRenderDelegate::_ApplyChanges(std::set<SdfPath> const &changed,
                                     std::set<SdfPath> const &removed, HdCrustView const &view)
{
    // Dirtied and added prims, written as the scene writer writes them, and
    // diffed into the overs: only their changed values (D2). Removed ones
    // inactive. The session classifies what the overs changed as it would an
    // MCP edit: a light's inputs in place, a transform by an import.
    UsdStageRefPtr scratch = UsdStage::CreateInMemory("changes.usda");
    SdfPathVector paths(changed.begin(), changed.end());
    SdfPathVector hidden;
    HdCrustSceneReport report;
    std::string why;
    if (!HdCrustWritePrims(_sceneIndex, _renderTags, paths, scratch, &hidden, &report)) {
        TF_STATUS("hdCrust: the scene is written whole: an instanced prim changed");
        return false;
    }
    if (!HdCrustOverlay(_sceneLayer, _edits.overs, scratch->GetRootLayer(), paths, &why)) {
        TF_STATUS("hdCrust: the scene is written whole: %s", why.c_str());
        return false;
    }
    for (SdfPath const &path : removed) {
        HdCrustDeactivate(_edits.overs, path);
    }
    for (SdfPath const &path : hidden) {
        if (_sceneLayer->GetPrimAtPath(path)) {
            HdCrustDeactivate(_edits.overs, path);
        }
    }
    // The stage's own render settings and camera may be what changed.
    _stage = HdCrustReadStageSettings(_sceneIndex);
    _edits.view = view;
    _edits.settings = _CrustSettings(_stage);
    if (!_stage.camera.IsEmpty()) {
        _edits.dicingCamera = _stage.camera;
    }
    std::string error;
    if (!HdCrustWriteEdits(_edits, _session->EditsPath(), &error)) {
        TF_RUNTIME_ERROR("hdCrust: %s", error.c_str());
        return true;
    }
    _session->Apply();
    return true;
}

VtDictionary
HdCrustRenderDelegate::GetRenderStats() const
{
    VtDictionary stats;
    if (_session) {
        stats["crust:applied"] = VtValue(_session->Applied());
        stats["crust:converged"] = VtValue(_session->IsConverged());
        stats["crust:sessionDirectory"] = VtValue(_session->GetDirectory());
        // The last redraw's costs (milliseconds): what the measurements of
        // the hydra-delegate design record are taken from.
        stats["crust:syncMs"] = VtValue(_syncMs.load());
        stats["crust:firstPassMs"] = VtValue(_firstPassMs.load());
        stats["crust:copyMs"] = VtValue(_copyMs.load());
    }
    return stats;
}

HdRenderSettingDescriptorList
HdCrustRenderDelegate::GetRenderSettingDescriptors() const
{
    HdRenderSettingDescriptorList list;
    for (_Setting const &s : _Settings()) {
        list.push_back({s.name, TfToken(s.key), s.fallback});
    }
    list.push_back({"Session directory (kept when set)", _SessionDirectory(), VtValue(std::string())});
    return list;
}

void
HdCrustRenderDelegate::SetRenderSetting(TfToken const &key, VtValue const &value)
{
    HdRenderDelegate::SetRenderSetting(key, value);
    std::lock_guard<std::mutex> lock(_syncMutex);
    _explicitSettings.insert(key);
}

std::map<TfToken, VtValue>
HdCrustRenderDelegate::_CrustSettings(HdCrustStageSettings const &stage) const
{
    // The stage's own settings, then what the host set over them: a setting
    // the user never touched leaves the stage's value (or crust's default,
    // except a viewport default, which is written in its place).
    std::map<TfToken, VtValue> settings = stage.crust;
    for (_Setting const &s : _Settings()) {
        TfToken const key(s.key);
        if (_explicitSettings.count(key)) {
            VtValue const value = GetRenderSetting(key);
            if (!value.IsEmpty()) {
                VtValue v = _AsFallbackType(value, s.fallback);
                if (v.IsHolding<std::string>()) {
                    v = VtValue(TfToken(v.UncheckedGet<std::string>()));
                }
                settings[key] = v;
            }
        } else if (s.viewport && !settings.count(key)) {
            settings[key] = VtValue(TfToken(s.fallback.Get<std::string>()));
        }
    }
    return settings;
}

HdCrustSession *
HdCrustRenderDelegate::Sync(HdCrustView const &view, TfTokenVector const &renderTags,
                            TfTokenVector const &aovs)
{
    std::lock_guard<std::mutex> lock(_syncMutex);
    // A session directory set (or changed) starts a session there.
    std::string const dir = GetRenderSetting<std::string>(_SessionDirectory(), std::string());
    if (!_session || dir != _sessionDirectory) {
        _session.reset();
        _session = std::make_unique<HdCrustSession>(dir);
        _sessionDirectory = dir;
        _loaded = false;
    }
    if (!_session->IsValid()) {
        return nullptr;
    }
    std::string error;
    TfTokenVector tags = renderTags;
    std::sort(tags.begin(), tags.end());
    bool const retagged = tags != _renderTags;
    unsigned const settingsVersion = GetRenderSettingsVersion();
    // A new set of bound AOVs is a new product in the override layer.
    TfTokenVector supported;
    for (TfToken const &aov : aovs) {
        TfToken source;
        SdfValueTypeName type;
        size_t components = 0;
        if (HdCrustAovSource(aov, &source, &type, &components)) {
            supported.push_back(aov);
        } else if (_warned.insert("aov " + aov.GetString()).second) {
            TF_WARN("hdCrust: the %s AOV is not rendered by crust; its buffer keeps its "
                    "clear value", aov.GetText());
        }
    }
    bool const resettled = settingsVersion != _settingsVersion || supported != _edits.aovs;
    _edits.aovs = supported;
    _settingsVersion = settingsVersion;
    // What the scene index reported since the last sync.
    std::set<SdfPath> changed, removed;
    {
        std::lock_guard<std::mutex> pending(_pendingMutex);
        changed.swap(_changed);
        removed.swap(_removed);
    }
    // A time change rewrites both layers at the new time (spec "A time
    // change renders the new frame"), which also compacts the overs.
    HdSceneGlobalsSchema globals = HdSceneGlobalsSchema::GetFromSceneIndex(_sceneIndex);
    double const frame = globals.GetCurrentFrame()
        ? globals.GetCurrentFrame()->GetTypedValue(0.0f)
        : _frame;
    // The default time code is NaN, which never equals itself: two of them
    // are the same frame, not a change on every sync.
    bool const retimed = !(frame == _frame || (std::isnan(frame) && std::isnan(_frame)));
    _frame = frame;
    // After a failed write or import, the scene is written again only once
    // something changes (spec: the next change that yields an openable layer
    // renders it), not on every redraw: retried per redraw, a stage too
    // large for its disk rewrote gigabytes until the disk was full.
    bool const changes = !changed.empty() || !removed.empty();
    bool whole = _sceneDirty.exchange(false) || retagged || retimed
        || (!_loaded && (!_failed || changes));
    if (whole) {
        _failed = false;
    } else if (!_loaded) {
        return _session.get();
    }
    if (!whole && changes) {
        whole = !_ApplyChanges(changed, removed, view);
        if (!whole) {
            return _session.get();
        }
    }
    if (whole) {
        _renderTags = tags;
        _session->Cancel();
        _edits.overs = SdfLayer::CreateAnonymous("overs.usda");
        _stage = HdCrustReadStageSettings(_sceneIndex);
        // The stage's render camera dices; without one, the viewing camera
        // at first population does, and keeps dicing while it moves (D4).
        if (!_loaded || _edits.dicingCamera != HdCrustPaths::Dicing()) {
            _edits.dicingView = view;
        }
        _edits.dicingCamera = _stage.camera.IsEmpty() ? HdCrustPaths::Dicing() : _stage.camera;
        _edits.view = view;
        _edits.settings = _CrustSettings(_stage);
        HdCrustSceneReport report;
        if (!HdCrustWriteScene(_sceneIndex, _renderTags, _session->ScenePath(), &report,
                               &_sceneLayer, &error)
            || !HdCrustWriteEdits(_edits, _session->EditsPath(), &error)) {
            TF_RUNTIME_ERROR("hdCrust: %s (the scene is written again at the next change; "
                             "set the session directory on a disk with room for it)",
                             error.c_str());
            _loaded = false;
            _failed = true;
            return _session.get();
        }
        // Spec: what the layer cannot carry is reported once per kind,
        // naming the first prim; the rest of the scene still renders.
        for (auto const &[kind, prim] : report.unsupported) {
            if (_warned.insert(kind).second) {
                TF_WARN("hdCrust: %s is not carried to crust yet (first at %s)", kind.c_str(),
                        prim.c_str());
            }
        }
        _loaded = _session->Load();
        _failed = !_loaded;
    } else if (view != _edits.view || resettled) {
        // A camera move or a renderer setting: the override layer only,
        // which the session applies as a `settings` change (D4, D5).
        _edits.view = view;
        _edits.settings = _CrustSettings(_stage);
        if (!HdCrustWriteEdits(_edits, _session->EditsPath(), &error)) {
            TF_RUNTIME_ERROR("hdCrust: %s", error.c_str());
            return _session.get();
        }
        _session->Apply();
    }
    return _session.get();
}

PXR_NAMESPACE_CLOSE_SCOPE
