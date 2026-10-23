#include "renderBuffer.h"

#include "pxr/base/gf/half.h"
#include "pxr/base/gf/vec3i.h"

#include <algorithm>
#include <cstring>

PXR_NAMESPACE_OPEN_SCOPE

HdCrustRenderBuffer::HdCrustRenderBuffer(SdfPath const &id)
    : HdRenderBuffer(id)
{
}

HdCrustRenderBuffer::~HdCrustRenderBuffer() = default;

bool
HdCrustRenderBuffer::Allocate(GfVec3i const &dimensions, HdFormat format, bool)
{
    _Deallocate();
    if (dimensions[2] != 1) {
        TF_WARN("hdCrust: render buffer %s is %dx%dx%d; only 2D buffers are supported",
                GetId().GetText(), dimensions[0], dimensions[1], dimensions[2]);
        return false;
    }
    _width = static_cast<unsigned int>(dimensions[0]);
    _height = static_cast<unsigned int>(dimensions[1]);
    _format = format;
    _buffer.assign(size_t(_width) * _height * HdDataSizeOfFormat(format), 0);
    return true;
}

void
HdCrustRenderBuffer::_Deallocate()
{
    _width = _height = 0;
    _format = HdFormatInvalid;
    _buffer.clear();
    _mappers.store(0);
    _converged.store(false);
}

void *
HdCrustRenderBuffer::Map()
{
    _mappers.fetch_add(1);
    return _buffer.data();
}

void
HdCrustRenderBuffer::Unmap()
{
    _mappers.fetch_sub(1);
}

namespace {

/// Writes component `c` of `value` into one pixel of `format` at `dst`.
void
_Store(uint8_t *dst, HdFormat format, size_t c, float value)
{
    switch (HdGetComponentFormat(format)) {
    case HdFormatFloat32:
        reinterpret_cast<float *>(dst)[c] = value;
        break;
    case HdFormatFloat16:
        reinterpret_cast<GfHalf *>(dst)[c] = GfHalf(value);
        break;
    case HdFormatUNorm8:
        dst[c] = static_cast<uint8_t>(std::clamp(value, 0.0f, 1.0f) * 255.0f + 0.5f);
        break;
    case HdFormatSNorm8:
        reinterpret_cast<int8_t *>(dst)[c] =
            static_cast<int8_t>(std::clamp(value, -1.0f, 1.0f) * 127.0f);
        break;
    case HdFormatInt32:
        reinterpret_cast<int32_t *>(dst)[c] = static_cast<int32_t>(value);
        break;
    default:
        break;
    }
}

} // namespace

void
HdCrustRenderBuffer::Write(const float *pixels, size_t width, size_t height, size_t channels)
{
    if (width != _width || height != _height || _buffer.empty()) {
        return;
    }
    const size_t components = HdGetComponentCount(_format);
    const size_t stride = HdDataSizeOfFormat(_format);
    for (size_t y = 0; y < height; ++y) {
        // Crust's rows go down, Hydra's up.
        uint8_t *row = _buffer.data() + (height - 1 - y) * width * stride;
        const float *src = pixels + y * width * channels;
        for (size_t x = 0; x < width; ++x) {
            for (size_t c = 0; c < components; ++c) {
                // A missing component: alpha 1, anything else 0.
                const float v = c < channels ? src[x * channels + c] : (c == 3 ? 1.0f : 0.0f);
                _Store(row + x * stride, _format, c, v);
            }
        }
    }
}

void
HdCrustRenderBuffer::Clear(float value)
{
    const size_t components = HdGetComponentCount(_format);
    const size_t stride = HdDataSizeOfFormat(_format);
    for (size_t i = 0; i < size_t(_width) * _height; ++i) {
        for (size_t c = 0; c < components; ++c) {
            _Store(_buffer.data() + i * stride, _format, c, value);
        }
    }
}

PXR_NAMESPACE_CLOSE_SCOPE
