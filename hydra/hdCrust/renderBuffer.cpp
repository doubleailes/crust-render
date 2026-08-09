#include "renderBuffer.h"

#include <pxr/base/tf/diagnostic.h>

PXR_NAMESPACE_OPEN_SCOPE

HdCrustRenderBuffer::HdCrustRenderBuffer(SdfPath const& id)
    : HdRenderBuffer(id) {}

bool HdCrustRenderBuffer::Allocate(GfVec3i const& dimensions, HdFormat format,
                                   bool multiSampled) {
    _Deallocate();
    if (dimensions[2] != 1) {
        TF_WARN("hdCrust render buffers are 2D; depth %d ignored",
                dimensions[2]);
    }
    // Crust resolves its own per-pixel film; Hydra-side multisampling is
    // declined rather than emulated.
    (void)multiSampled;
    _width = static_cast<unsigned int>(dimensions[0]);
    _height = static_cast<unsigned int>(dimensions[1]);
    _format = format;
    size_t pixelSize = HdDataSizeOfFormat(format);
    _storage.assign(
        static_cast<size_t>(_width) * static_cast<size_t>(_height) * pixelSize,
        0);
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
