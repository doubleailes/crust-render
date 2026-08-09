#include "renderBuffer.h"

#include <pxr/base/tf/diagnostic.h>

#include <cstdint>

PXR_NAMESPACE_OPEN_SCOPE

HdCrustRenderBuffer::HdCrustRenderBuffer(SdfPath const& id)
    : HdRenderBuffer(id) {}

bool HdCrustRenderBuffer::Allocate(GfVec3i const& dimensions, HdFormat format,
                                   bool multiSampled) {
    _Deallocate();
    // Crust resolves its own per-pixel film; Hydra-side multisampling is
    // declined rather than emulated.
    (void)multiSampled;

    // Reject rather than wrap: a negative dimension cast to unsigned turns
    // into a huge value, and an under-allocation from a later overflow
    // would become an out-of-bounds write in the render pass.
    if (dimensions[0] <= 0 || dimensions[1] <= 0) {
        TF_WARN("hdCrust: invalid render buffer dimensions %d x %d",
                dimensions[0], dimensions[1]);
        return false;
    }
    if (dimensions[2] != 1) {
        TF_WARN("hdCrust render buffers are 2D; depth %d ignored",
                dimensions[2]);
    }
    const size_t pixelSize = HdDataSizeOfFormat(format);
    if (pixelSize == 0) {
        TF_WARN("hdCrust: unsupported render buffer format %d", int(format));
        return false;
    }
    const size_t w = static_cast<size_t>(dimensions[0]);
    const size_t h = static_cast<size_t>(dimensions[1]);
    if (w > SIZE_MAX / h || w * h > SIZE_MAX / pixelSize) {
        TF_WARN("hdCrust: render buffer size %zu x %zu x %zu overflows",
                w, h, pixelSize);
        return false;
    }
    _width = static_cast<unsigned int>(w);
    _height = static_cast<unsigned int>(h);
    _format = format;
    _storage.assign(w * h * pixelSize, 0);
    return true;
}

void HdCrustRenderBuffer::_Deallocate() {
    _width = 0;
    _height = 0;
    _format = HdFormatInvalid;
    _storage.clear();
    _converged.store(false);
}

PXR_NAMESPACE_CLOSE_SCOPE
