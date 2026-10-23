// hdCrust: an AOV's pixels in host memory, which the render pass fills from
// crust's snapshots and AOVs.
#ifndef HDCRUST_RENDER_BUFFER_H
#define HDCRUST_RENDER_BUFFER_H

#include "pxr/pxr.h"
#include "pxr/imaging/hd/renderBuffer.h"

#include <atomic>
#include <cstdint>
#include <vector>

PXR_NAMESPACE_OPEN_SCOPE

class HdCrustRenderBuffer final : public HdRenderBuffer
{
public:
    explicit HdCrustRenderBuffer(SdfPath const &id);
    ~HdCrustRenderBuffer() override;

    bool Allocate(GfVec3i const &dimensions, HdFormat format, bool multiSampled) override;

    unsigned int GetWidth() const override { return _width; }
    unsigned int GetHeight() const override { return _height; }
    unsigned int GetDepth() const override { return 1; }
    HdFormat GetFormat() const override { return _format; }
    bool IsMultiSampled() const override { return false; }

    void *Map() override;
    void Unmap() override;
    bool IsMapped() const override { return _mappers.load() != 0; }

    void Resolve() override {}
    bool IsConverged() const override { return _converged.load(); }
    void SetConverged(bool converged) { _converged.store(converged); }

    /// Writes `width` × `height` pixels of `channels` floats, rows top-down
    /// (crust's order), converted to the buffer's format with rows bottom-up
    /// (Hydra's). Pixels outside the buffer are dropped; a mismatched size
    /// writes nothing.
    void Write(const float *pixels, size_t width, size_t height, size_t channels);

    /// Fills the buffer with `value` in every component.
    void Clear(float value);

private:
    void _Deallocate() override;

    unsigned int _width = 0;
    unsigned int _height = 0;
    HdFormat _format = HdFormatInvalid;
    std::vector<uint8_t> _buffer;
    std::atomic<int> _mappers{0};
    std::atomic<bool> _converged{false};
};

PXR_NAMESPACE_CLOSE_SCOPE

#endif
