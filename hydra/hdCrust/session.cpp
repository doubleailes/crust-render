#include "session.h"

#include "pxr/base/tf/diagnostic.h"
#include "pxr/base/tf/stringUtils.h"

#include <atomic>
#include <chrono>
#include <filesystem>
#include <system_error>
#include <thread>

PXR_NAMESPACE_OPEN_SCOPE

namespace {

/// crust's log, as Tf diagnostics: errors and warnings as warnings (a crust
/// error never ends the host), the rest as status lines.
void
_Log(int32_t level, char const *message, void *)
{
    if (level <= 1) {
        TF_WARN("crust: %s", message);
    } else {
        TF_STATUS("crust: %s", message);
    }
}

/// A fresh directory under the system's temporary one.
std::string
_TemporaryDirectory()
{
    static std::atomic<unsigned> counter{0};
    std::error_code ec;
    auto base = std::filesystem::temp_directory_path(ec);
    if (ec) {
        base = ".";
    }
    for (;;) {
        auto const stamp = std::chrono::steady_clock::now().time_since_epoch().count();
        auto dir = base / TfStringPrintf("hdCrust-%llx-%u", (unsigned long long)stamp, counter++);
        if (std::filesystem::create_directories(dir, ec)) {
            return dir.string();
        }
        if (ec) {
            return std::string();
        }
    }
}

} // namespace

HdCrustSession::HdCrustSession(std::string directory)
    : _directory(std::move(directory))
{
    if (_directory.empty()) {
        _directory = _TemporaryDirectory();
        _temporary = true;
    }
    _handle = crust_hydra_new(_directory.c_str(), _Log, nullptr);
    if (!_handle) {
        TF_RUNTIME_ERROR("hdCrust: cannot start a crust session in %s", _directory.c_str());
    }
}

HdCrustSession::~HdCrustSession()
{
    crust_hydra_free(_handle);
    if (_temporary && !_directory.empty()) {
        std::error_code ec;
        std::filesystem::remove_all(_directory, ec);
    }
}

std::string
HdCrustSession::ScenePath() const
{
    return (std::filesystem::path(_directory) / "scene.usdc").string();
}

std::string
HdCrustSession::EditsPath() const
{
    return (std::filesystem::path(_directory) / "edits.usda").string();
}

void
HdCrustSession::_Report(crust_hydra_status status, char const *what) const
{
    if (status == CRUST_HYDRA_OK || status == CRUST_HYDRA_BUSY) {
        return;
    }
    char message[1024];
    crust_hydra_last_error(_handle, message, sizeof message);
    TF_RUNTIME_ERROR("hdCrust: %s failed: %s", what, message);
}

bool
HdCrustSession::Load()
{
    if (!_handle) {
        return false;
    }
    _generation = 0;
    _restarted = true;
    crust_hydra_status const status = crust_hydra_load(_handle, EditsPath().c_str());
    _Report(status, "importing the scene");
    return status == CRUST_HYDRA_OK;
}

bool
HdCrustSession::Apply()
{
    if (!_handle) {
        return false;
    }
    _restarted = true;
    crust_hydra_status const status = crust_hydra_apply(_handle);
    _Report(status, "applying the change");
    return status == CRUST_HYDRA_OK;
}

void
HdCrustSession::Cancel()
{
    crust_hydra_cancel(_handle);
}

bool
HdCrustSession::IsConverged() const
{
    return _handle && crust_hydra_converged(_handle) != 0;
}

std::string
HdCrustSession::Applied() const
{
    static char const *const names[] = {"settings", "materials", "lights", "full", "geometry"};
    int32_t const applied = _handle ? crust_hydra_applied(_handle) : -1;
    return applied >= 0 && applied < 5 ? names[applied] : std::string();
}

bool
HdCrustSession::Snapshot(std::vector<float> &rgba, size_t width, size_t height, uint32_t *spp)
{
    if (!_handle) {
        return false;
    }
    rgba.resize(width * height * 4);
    uint64_t generation = _generation;
    uint32_t samples = 0;
    crust_hydra_status const status = crust_hydra_snapshot(
        _handle, rgba.data(), width, height, _generation, &generation, &samples);
    if (spp) {
        *spp = samples;
    }
    _Report(status, "reading the image");
    if (status != CRUST_HYDRA_OK || generation == _generation) {
        return false;
    }
    _generation = generation;
    return true;
}

bool
HdCrustSession::AwaitFirstPass(std::vector<float> &rgba, size_t width, size_t height,
                               std::chrono::milliseconds cap)
{
    if (!_restarted || !_handle) {
        return false;
    }
    _restarted = false;
    auto const deadline = std::chrono::steady_clock::now() + cap;
    bool copied = false;
    for (;;) {
        uint32_t spp = 0;
        copied = Snapshot(rgba, width, height, &spp) || copied;
        // A render that already converged has every pixel too.
        if ((copied && spp >= 1) || IsConverged()) {
            return copied || Snapshot(rgba, width, height);
        }
        if (std::chrono::steady_clock::now() >= deadline) {
            return copied;
        }
        std::this_thread::sleep_for(std::chrono::milliseconds(2));
    }
}

bool
HdCrustSession::Aov(std::string const &name, std::vector<float> &data, size_t width,
                    size_t height, size_t channels)
{
    if (!_handle) {
        return false;
    }
    data.resize(width * height * channels);
    crust_hydra_status const status =
        crust_hydra_aov(_handle, name.c_str(), data.data(), width, height, channels);
    _Report(status, "reading an AOV");
    return status == CRUST_HYDRA_OK;
}

PXR_NAMESPACE_CLOSE_SCOPE
