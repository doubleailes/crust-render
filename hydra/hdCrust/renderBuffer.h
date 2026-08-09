#ifndef HDCRUST_RENDER_BUFFER_H
#define HDCRUST_RENDER_BUFFER_H

#include <pxr/base/gf/vec3i.h>
#include <pxr/imaging/hd/renderBuffer.h>
#include <pxr/pxr.h>

#include <atomic>
#include <vector>

PXR_NAMESPACE_OPEN_SCOPE

/// A CPU render buffer: owned storage, refcounted Map/Unmap, no
/// multisampling (crust resolves its own film; Resolve is a no-op).
/// Rows follow the Hydra/GL convention — row 0 is the bottom scanline —
/// which is also exactly what the crust C API produces, so the render pass
/// copies rows straight through.
class HdCrustRenderBuffer final : public HdRenderBuffer {
public:
    explicit HdCrustRenderBuffer(SdfPath const& id);
    ~HdCrustRenderBuffer() override = default;

    bool Allocate(GfVec3i const& dimensions, HdFormat format,
                  bool multiSampled) override;

    unsigned int GetWidth() const override { return _width; }
    unsigned int GetHeight() const override { return _height; }
    unsigned int GetDepth() const override { return 1; }
    HdFormat GetFormat() const override { return _format; }
    bool IsMultiSampled() const override { return false; }

    void* Map() override {
        _mappers.fetch_add(1);
        return _storage.data();
    }
    void Unmap() override { _mappers.fetch_sub(1); }
    bool IsMapped() const override { return _mappers.load() != 0; }

    /// The render pass writes the film directly; there is nothing to
    /// resolve.
    void Resolve() override {}

    bool IsConverged() const override { return _converged.load(); }
    void SetConverged(bool converged) { _converged.store(converged); }

    /// Direct storage access for the render pass (the producer side of
    /// Map, without the refcount ceremony).
    uint8_t* GetStorage() { return _storage.data(); }
    size_t GetStorageSize() const { return _storage.size(); }

private:
    void _Deallocate() override;

    unsigned int _width = 0;
    unsigned int _height = 0;
    HdFormat _format = HdFormatInvalid;
    std::vector<uint8_t> _storage;
    std::atomic<int> _mappers{0};
    std::atomic<bool> _converged{false};
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif // HDCRUST_RENDER_BUFFER_H
