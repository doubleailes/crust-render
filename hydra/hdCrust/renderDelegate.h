// hdCrust: the render delegate. It creates no rprims: geometry, materials,
// lights and instancers are read from the terminal scene index it observes
// (hydra-delegate design, D1), not synced as Hydra 1.0 prims.
#ifndef HDCRUST_RENDER_DELEGATE_H
#define HDCRUST_RENDER_DELEGATE_H

#include "pxr/pxr.h"
#include "pxr/imaging/hd/renderDelegate.h"
#include "pxr/imaging/hd/resourceRegistry.h"
#include "pxr/imaging/hd/sceneIndex.h"
#include "pxr/imaging/hd/sceneIndexObserver.h"

#include "sceneWriter.h"
#include "session.h"

#include <atomic>
#include <memory>
#include <mutex>
#include <map>
#include <set>
#include <string>

PXR_NAMESPACE_OPEN_SCOPE

class HdCrustRenderDelegate final : public HdRenderDelegate
{
public:
    HdCrustRenderDelegate();
    explicit HdCrustRenderDelegate(HdRenderSettingsMap const &settingsMap);
    ~HdCrustRenderDelegate() override;

    const TfTokenVector &GetSupportedRprimTypes() const override;
    const TfTokenVector &GetSupportedSprimTypes() const override;
    const TfTokenVector &GetSupportedBprimTypes() const override;

    HdResourceRegistrySharedPtr GetResourceRegistry() const override;

    HdRenderPassSharedPtr CreateRenderPass(
        HdRenderIndex *index, HdRprimCollection const &collection) override;

    HdInstancer *CreateInstancer(HdSceneDelegate *delegate, SdfPath const &id) override;
    void DestroyInstancer(HdInstancer *instancer) override;

    HdRprim *CreateRprim(TfToken const &typeId, SdfPath const &rprimId) override;
    void DestroyRprim(HdRprim *rPrim) override;

    HdSprim *CreateSprim(TfToken const &typeId, SdfPath const &sprimId) override;
    HdSprim *CreateFallbackSprim(TfToken const &typeId) override;
    void DestroySprim(HdSprim *sprim) override;

    HdBprim *CreateBprim(TfToken const &typeId, SdfPath const &bprimId) override;
    HdBprim *CreateFallbackBprim(TfToken const &typeId) override;
    void DestroyBprim(HdBprim *bprim) override;

    void CommitResources(HdChangeTracker *tracker) override;

    HdAovDescriptor GetDefaultAovDescriptor(TfToken const &name) const override;

    /// `crust:applied` (how the last change reached crust's scene),
    /// `crust:converged`, `crust:sessionDirectory`.
    VtDictionary GetRenderStats() const override;

    /// The renderer settings the host shows, with crust's defaults (5.4).
    HdRenderSettingDescriptorList GetRenderSettingDescriptors() const override;

    /// Records which settings the host set: only those override the stage's.
    void SetRenderSetting(TfToken const &key, VtValue const &value) override;

    /// Hydra 2.0: where the observer attaches.
    void SetTerminalSceneIndex(const HdSceneIndexBaseRefPtr &terminalSceneIndex) override;

    /// Brings crust up to date with the scene and the viewport `view`, once
    /// per `_Execute`: the scene written and imported when it changed (or on
    /// the first call), else the override layer rewritten and applied when
    /// the view changed (D5). The session, or null if it cannot start.
    HdCrustSession *Sync(HdCrustView const &view, TfTokenVector const &renderTags,
                         TfTokenVector const &aovs);

    /// The last redraw's costs, in milliseconds, reported by the render
    /// pass: the sync (layers written, crust told), the wait for a restarted
    /// render's first full pass, and copying the image into the buffer.
    void RecordTimings(double syncMs, double firstPassMs, double copyMs)
    {
        _syncMs.store(syncMs);
        _firstPassMs.store(firstPassMs);
        _copyMs.store(copyMs);
    }

    /// Something crust renders changed in the scene index.
    void MarkSceneDirty() { _sceneDirty.store(true); }

private:
    /// Collects the scene index's notices; does no work on Hydra's thread.
    class _Observer final : public HdSceneIndexObserver
    {
    public:
        explicit _Observer(HdCrustRenderDelegate *delegate) : _delegate(delegate) {}
        void PrimsAdded(HdSceneIndexBase const &, AddedPrimEntries const &entries) override;
        void PrimsRemoved(HdSceneIndexBase const &, RemovedPrimEntries const &entries) override;
        void PrimsDirtied(HdSceneIndexBase const &, DirtiedPrimEntries const &entries) override;
        void PrimsRenamed(HdSceneIndexBase const &, RenamedPrimEntries const &entries) override;

    private:
        HdCrustRenderDelegate *_delegate;
    };

    HdSceneIndexBaseRefPtr _sceneIndex;
    HdResourceRegistrySharedPtr _resourceRegistry;
    _Observer _observer{this};
    std::atomic<bool> _sceneDirty{true};
    std::atomic<double> _syncMs{0.0}, _firstPassMs{0.0}, _copyMs{0.0};
    std::mutex _syncMutex;
    std::unique_ptr<HdCrustSession> _session;
    std::map<TfToken, VtValue> _CrustSettings(HdCrustStageSettings const &stage) const;

    /// The incremental path (5.2): the changed prims as overs, applied.
    /// False when the scene must be written whole instead.
    bool _ApplyChanges(std::set<SdfPath> const &changed, std::set<SdfPath> const &removed,
                       HdCrustView const &view);

    /// The scene index's notices since the last sync.
    std::mutex _pendingMutex;
    std::set<SdfPath> _changed;
    std::set<SdfPath> _removed;
    /// The scene layer as last written whole.
    SdfLayerRefPtr _sceneLayer;
    /// The frame the scene was written at.
    double _frame = 0.0;

    bool _loaded = false;
    /// The last full write or import failed: retried at the next change.
    bool _failed = false;
    std::string _sessionDirectory;
    /// The override layer as last written.
    HdCrustEdits _edits;
    /// The stage's own render settings, as of the last full write.
    HdCrustStageSettings _stage;
    TfTokenVector _renderTags;
    unsigned _settingsVersion = 0;
    /// The settings the host set (rather than left at their defaults).
    std::set<TfToken> _explicitSettings;
    /// The kinds of unsupported data already reported: once per kind.
    std::set<std::string> _warned;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif
