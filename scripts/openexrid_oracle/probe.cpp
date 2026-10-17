// Reads an OpenEXRId matte crust wrote with OpenEXRId's own reader
// (`openexrid::Mask::read`, which OpenEXR's DeepScanLineInputFile backs) and
// checks it against the flat beauty of the same render: per pixel, the
// coverage of the matte's samples sums to the beauty's alpha, and a coloured
// matte's samples sum to the beauty where the alpha is one. The names are
// decoded as OpenEXRId's Nuke plug-in decodes file version 3 (base64, zlib,
// one name per line). See scripts/openexrid_oracle.py.
//
//   probe <matte.exr> <flat.exr>
//
// Prints one summary line and exits non-zero on the first mismatch.
#include <cmath>
#include <cstdio>
#include <string>
#include <vector>

#include <ImfArray.h>
#include <ImfFrameBuffer.h>
#include <ImfInputFile.h>
#include <ImfIntAttribute.h>
#include <ImfStringAttribute.h>
#include <ImfDeepScanLineInputFile.h>
#include <ImathBox.h>

#include "Mask.h"

extern std::string b64decode(const std::string &str);
extern std::string inflate(const std::string &str);

int main(int argc, char **argv) {
    if (argc != 3) {
        std::fprintf(stderr, "usage: probe <matte.exr> <flat.exr>\n");
        return 2;
    }
    try {
        openexrid::Mask mask;
        mask.read(argv[1]);

        // The names, the Nuke plug-in's way for version 3.
        Imf::DeepScanLineInputFile deep(argv[1]);
        const Imf::IntAttribute *version =
            deep.header().findTypedAttribute<Imf::IntAttribute>("EXRIdVersion");
        const Imf::StringAttribute *packed =
            deep.header().findTypedAttribute<Imf::StringAttribute>("EXRIdNames");
        if (!version || version->value() != 3 || !packed) {
            std::fprintf(stderr, "%s: not an OpenEXRId version 3 file\n", argv[1]);
            return 1;
        }
        const std::string text = inflate(b64decode(packed->value()));
        std::vector<std::string> names;
        size_t at = 0;
        while (at <= text.size()) {
            size_t eol = text.find('\n', at);
            if (eol == std::string::npos) eol = text.size();
            names.push_back(text.substr(at, eol - at));
            at = eol + 1;
        }

        // The flat beauty.
        Imf::InputFile flat(argv[2]);
        const Imath::Box2i dw = flat.header().dataWindow();
        const int fw = dw.max.x - dw.min.x + 1, fh = dw.max.y - dw.min.y + 1;
        Imf::Array2D<float> r(fh, fw), g(fh, fw), b(fh, fw), a(fh, fw);
        Imf::FrameBuffer fb;
        const size_t xs = sizeof(float), ys = sizeof(float) * fw;
        char *base = nullptr;
        auto slice = [&](const char *name, Imf::Array2D<float> &plane) {
            base = (char *)(&plane[0][0] - dw.min.x - dw.min.y * fw);
            fb.insert(name, Imf::Slice(Imf::FLOAT, base, xs, ys));
        };
        slice("R", r);
        slice("G", g);
        slice("B", b);
        slice("A", a);
        flat.setFrameBuffer(fb);
        flat.readPixels(dw.min.y, dw.max.y);

        const std::pair<int, int> size = mask.getSize();
        if (size.first != fw || size.second != fh) {
            std::fprintf(stderr, "size %dx%d against the beauty's %dx%d\n", size.first,
                         size.second, fw, fh);
            return 1;
        }
        const int A = mask.findSlice("A");
        const int R = mask.findSlice("R"), G = mask.findSlice("G"), B = mask.findSlice("B");
        if (A < 0) {
            std::fprintf(stderr, "no A slice\n");
            return 1;
        }
        double worst_a = 0.0, worst_rgb = 0.0;
        long samples = 0, covered = 0;
        openexrid::Sample s;
        for (int y = 0; y < fh; ++y) {
            for (int x = 0; x < fw; ++x) {
                const int n = mask.getSampleN(x, y);
                double sum_a = 0.0, sum[3] = {0.0, 0.0, 0.0};
                for (int k = 0; k < n; ++k) {
                    mask.getSample(x, y, k, s);
                    if (s.Id >= names.size()) {
                        std::fprintf(stderr, "(%d, %d): id %u has no name\n", x, y, s.Id);
                        return 1;
                    }
                    sum_a += float(s.Values[A]);
                    if (R >= 0) {
                        sum[0] += float(s.Values[R]);
                        sum[1] += float(s.Values[G]);
                        sum[2] += float(s.Values[B]);
                    }
                }
                samples += n;
                const double da = std::fabs(sum_a - a[y][x]);
                if (da > worst_a) worst_a = da;
                if (da > 4e-3) {
                    std::fprintf(stderr, "(%d, %d): coverage %f against alpha %f\n", x, y, sum_a,
                                 a[y][x]);
                    return 1;
                }
                if (R >= 0 && a[y][x] > 0.9999f) {
                    ++covered;
                    const float want[3] = {r[y][x], g[y][x], b[y][x]};
                    for (int c = 0; c < 3; ++c) {
                        const double d = std::fabs(sum[c] - want[c]) / std::fmax(1.0, std::fabs(want[c]));
                        if (d > worst_rgb) worst_rgb = d;
                        if (d > 1e-2) {
                            std::fprintf(stderr, "(%d, %d): colour %f against %f\n", x, y, sum[c],
                                         want[c]);
                            return 1;
                        }
                    }
                }
            }
        }
        std::printf("%s: %zu names (Mask::read: %u), %ld samples, max |coverage - alpha| %.2e, "
                    "max colour error %.2e over %ld covered pixels\n",
                    argv[1], names.size(), mask.getIdN(), samples, worst_a, worst_rgb, covered);
        return 0;
    } catch (const std::exception &e) {
        std::fprintf(stderr, "%s\n", e.what());
        return 1;
    }
}
