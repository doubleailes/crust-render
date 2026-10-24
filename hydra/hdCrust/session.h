// hdCrust: the crust session behind the delegate (crust_hydra.h), with its
// directory of layers and its log routed to Tf.
#ifndef HDCRUST_SESSION_H
#define HDCRUST_SESSION_H

#include "pxr/pxr.h"

#include "crust_hydra.h"

#include <chrono>
#include <cstdint>
#include <string>
#include <vector>

PXR_NAMESPACE_OPEN_SCOPE

class HdCrustSession
{
public:
    /// A session on `directory`, or on a fresh temporary directory removed
    /// with the session when `directory` is empty.
    explicit HdCrustSession(std::string directory);
    ~HdCrustSession();

    HdCrustSession(HdCrustSession const &) = delete;
    HdCrustSession &operator=(HdCrustSession const &) = delete;

    bool IsValid() const { return _handle != nullptr; }
    std::string const &GetDirectory() const { return _directory; }
    std::string ScenePath() const;
    std::string EditsPath() const;

    /// crust_hydra_load / apply, reporting a failure as a Tf error.
    bool Load();
    bool Apply();
    void Cancel();
    bool IsConverged() const;

    /// How the last load or apply reached crust's scene: `settings`,
    /// `materials`, `lights` (in place) or `full`; empty before the first.
    std::string Applied() const;

    /// Copies the latest image into `rgba` when it is newer than the last
    /// one copied; whether it copied. `spp`, when given, receives the
    /// samples every pixel has taken.
    bool Snapshot(std::vector<float> &rgba, size_t width, size_t height,
                  uint32_t *spp = nullptr);

    /// After a restart (a load or an applied change), waits until every
    /// pixel of the new render has a sample, at most `cap`, and copies that
    /// image into `rgba`: so a host that redraws once per change (playback,
    /// a tumble) shows the new frame, whole, rather than the last one, or a
    /// first tile on black. Whether it copied; false at once when nothing
    /// restarted since the last call.
    bool AwaitFirstPass(std::vector<float> &rgba, size_t width, size_t height,
                        std::chrono::milliseconds cap);

    /// The completed render's AOV `name`.
    bool Aov(std::string const &name, std::vector<float> &data, size_t width, size_t height,
             size_t channels);

private:
    void _Report(crust_hydra_status status, char const *what) const;

    CrustHydraSession *_handle = nullptr;
    std::string _directory;
    bool _temporary = false;
    uint64_t _generation = 0;
    /// A load or an apply restarted the render since AwaitFirstPass.
    bool _restarted = false;
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif
