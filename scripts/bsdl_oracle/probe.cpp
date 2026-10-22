// Evaluates BSDL's MaterialX lobes (OSL `src/libbsdl`, `BSDL/MTX/`) for the
// cases `scripts/bsdl_oracle.py` writes, one per stdin line:
//
//     <id> leaf=<leaf> <name>=<value> ... | <vx> <vy> <vz> | <lx> <ly> <lz> ...
//
// A value is a float, a word, or comma-separated floats for a colour or a
// vector2. The shading frame is Z-up with the normal +Z and the tangent +X;
// the view is above the plane, and `back=1` says the ray met the back face
// (OSL face-forwards N, so `backfacing` is the only trace of it).
//
// Each lobe is built as OSL's `testrender` builds it (`BSDL_WRAP` in
// `src/testrender/shading.cpp`): `BsdfGlobals(wo, N, N, backfacing,
// path_roughness = 0, outer_ior = 1, lambda_0 = 0)`, so RGB, with no path
// regularisation. It runs on BSDL's default configuration (exact libm math;
// testrender swaps in OIIO's fast approximations, which this does not need).
//
// For each case it prints one line:
//
//     <id> | <albedo rgb> | <filter rgb> | <albedo_impl rgb> | <value rgb> ...
//
// - albedo: (π / N) Σ f(v, l) over the cosine-weighted midpoint grid of
//   `GRID`² directions on each side of the surface (`grid_direction` below;
//   the replay in `crates/crust-core/tests/bsdl_oracle.rs` builds the same
//   grid);
// - filter: `filter_o(wo)`, what the lobe lets through to a layer's base
//   (0 for a lobe without one, which testrender treats as opaque);
// - albedo_impl: the lobe's own albedo estimate, which testrender picks
//   lobes by;
// - value: `eval_impl` toward each light, `weight · pdf`, the BSDF times the
//   cosine (crust's `Material::eval` convention).
//
// Built and run by `scripts/bsdl_oracle.py`; not part of the Cargo build.

#define BSDL_UNROLL()

#include <BSDL/config.h>
// BSDL's defaults, without the Jakob–Hanika spectral table: the probe runs
// in RGB (`lambda_0 = 0`), which never reads it.
struct BSDLConfig : public bsdl::BSDLDefaultConfig {
    static const JakobHanikaLut* get_jakobhanika_lut(ColorSpaceTag) { return nullptr; }
};

#include <BSDL/MTX/bsdf_burley_diffuse_impl.h>
#include <BSDL/MTX/bsdf_conductor_impl.h>
#include <BSDL/MTX/bsdf_dielectric_impl.h>
#include <BSDL/MTX/bsdf_oren_nayar_diffuse_impl.h>
#include <BSDL/MTX/bsdf_schlick_impl.h>
#include <BSDL/MTX/bsdf_sheen_impl.h>
#include <BSDL/MTX/bsdf_translucent_impl.h>
#include <BSDL/bsdf_impl.h>
#include <BSDL/microfacet_tools_impl.h>
#include <BSDL/spectrum_impl.h>

#include <cmath>
#include <cstdio>
#include <iostream>
#include <map>
#include <sstream>
#include <string>
#include <vector>

using namespace bsdl;
using Imath::C3f;
using Imath::V3f;

static constexpr int GRID = 32;

// What BSDL asks of the class its lobes derive from (testrender's
// `BSDLLobe`, without testrender).
struct Root {
    template<typename L> Root(L*, float r, float, bool) : m_roughness(r) {}
    void set_roughness(float r) { m_roughness = r; }
    float roughness() const { return m_roughness; }
    float m_roughness;
};

template<template<typename> class L> struct Wrap : public L<Root> {
    using Base = L<Root>;
    Wrap(const typename Base::Data& data, const BsdfGlobals& globals)
        : Base(this, globals, data)
    {
    }
};

// Malley's method on the cell midpoints of a GRID × GRID square: a
// deterministic cosine-weighted set, in the hemisphere of sign `side`.
static V3f grid_direction(int i, int j, float side)
{
    const float u = (float(i) + 0.5f) / float(GRID);
    const float v = (float(j) + 0.5f) / float(GRID);
    const float r = std::sqrt(u);
    const float phi = 2.0f * float(M_PI) * v;
    return V3f(r * std::cos(phi), r * std::sin(phi), side * std::sqrt(std::max(0.0f, 1.0f - u)));
}

struct Inputs {
    std::string id;
    std::map<std::string, std::string> kv;

    bool has(const std::string& k) const { return kv.count(k) != 0; }
    std::string word(const std::string& k, const std::string& def) const
    {
        auto it = kv.find(k);
        return it == kv.end() ? def : it->second;
    }
    std::vector<float> floats(const std::string& k) const
    {
        std::vector<float> out;
        std::stringstream s(kv.at(k));
        std::string t;
        while (std::getline(s, t, ','))
            out.push_back(std::stof(t));
        return out;
    }
    float f(const std::string& k, float def) const { return has(k) ? floats(k)[0] : def; }
    C3f c(const std::string& k, C3f def) const
    {
        if (!has(k))
            return def;
        auto v = floats(k);
        return v.size() == 1 ? C3f(v[0]) : C3f(v[0], v[1], v[2]);
    }
    // A vector2 roughness, or a float broadcast to both axes.
    std::pair<float, float> pair(const std::string& k, float def) const
    {
        if (!has(k))
            return { def, def };
        auto v = floats(k);
        return { v[0], v.size() > 1 ? v[1] : v[0] };
    }
};

static void print(const C3f& c) { std::printf(" %.9g %.9g %.9g", c.x, c.y, c.z); }

// The probe's answers for one lobe.
template<typename L>
static void answer(const L& lobe, const V3f& wo, const std::vector<V3f>& lights, const C3f& filter)
{
    const V3f lo = lobe.frame.local(wo);
    auto value = [&](const V3f& wi) {
        Sample s = lobe.eval_impl(lo, lobe.frame.local(wi));
        return C3f(s.weight.toRGB(0)) * s.pdf;
    };
    double sum[3] = { 0, 0, 0 };
    for (float side : { 1.0f, -1.0f })
        for (int i = 0; i < GRID; ++i)
            for (int j = 0; j < GRID; ++j) {
                const V3f l = grid_direction(i, j, side);
                const float cos_l = std::abs(l.z);
                if (cos_l <= 0.0f)
                    continue;
                const C3f f = value(l) / cos_l;
                sum[0] += f.x;
                sum[1] += f.y;
                sum[2] += f.z;
            }
    const double k = M_PI / double(GRID * GRID);
    print(C3f(float(sum[0] * k), float(sum[1] * k), float(sum[2] * k)));
    std::printf(" |");
    print(filter);
    std::printf(" |");
    print(C3f(lobe.albedo_impl().toRGB(0)));
    std::printf(" |");
    for (const V3f& l : lights)
        print(value(l));
}

static bool run(const Inputs& in, const V3f& wo, const std::vector<V3f>& lights)
{
    const V3f N(0, 0, 1), U(1, 0, 0);
    const bool back = in.f("back", 0) != 0;
    const BsdfGlobals g(wo, N, N, back, 0.0f, 1.0f, 0.0f);
    const std::string leaf = in.word("leaf", "");
    const std::string mode = in.word("mode", "R");
    const C3f black(0);
    if (leaf == "dielectric" || leaf == "schlick") {
        const auto r = in.pair("roughness", 0.0f);
        if (leaf == "dielectric") {
            using T = Wrap<mtx::DielectricLobe>;
            T::Data d {};
            d.N = N;
            d.U = U;
            const C3f tint = in.c("tint", C3f(1));
            d.refl_tint = mode == "T" ? black : tint;
            d.refr_tint = mode == "R" ? black : tint;
            d.roughness_x = r.first;
            d.roughness_y = r.second;
            d.IOR = in.f("ior", 1.5f);
            d.thinfilm_thickness = in.f("thinfilm_thickness", 0.0f);
            d.thinfilm_ior = in.f("thinfilm_ior", 1.5f);
            d.dispersion = 0;
            d.absorption = black;
            T lobe(d, g);
            answer(lobe, wo, lights, C3f(lobe.filter_o(lobe.frame.local(wo)).toRGB(0)));
        } else {
            using T = Wrap<mtx::SchlickLobe>;
            T::Data d {};
            d.N = N;
            d.U = U;
            d.refl_tint = mode == "T" ? black : C3f(1);
            d.refr_tint = mode == "R" ? black : C3f(1);
            d.roughness_x = r.first;
            d.roughness_y = r.second;
            d.F0 = in.c("color0", C3f(1));
            d.F90 = in.c("color90", C3f(1));
            d.exponent = in.f("exponent", 5.0f);
            T lobe(d, g);
            answer(lobe, wo, lights, C3f(lobe.filter_o(lobe.frame.local(wo)).toRGB(0)));
        }
    } else if (leaf == "conductor") {
        using T = Wrap<mtx::ConductorLobe>;
        T::Data d {};
        const auto r = in.pair("roughness", 0.2f);
        d.N = N;
        d.U = U;
        d.roughness_x = r.first;
        d.roughness_y = r.second;
        d.IOR = in.c("ior", C3f(0.18299f, 0.42108f, 1.37340f));
        d.extinction = in.c("extinction", C3f(3.42420f, 2.34590f, 1.77040f));
        d.thinfilm_thickness = in.f("thinfilm_thickness", 0.0f);
        d.thinfilm_ior = in.f("thinfilm_ior", 1.5f);
        T lobe(d, g);
        answer(lobe, wo, lights, black);
    } else if (leaf == "sheen") {
        using T = Wrap<mtx::SheenLobe>;
        T::Data d {};
        d.N = N;
        d.albedo = in.c("color", C3f(1));
        d.roughness = in.f("roughness", 0.3f);
        d.mode = in.word("sheen_mode", "conty_kulla") == "zeltner" ? 1 : 0;
        T lobe(d, g);
        answer(lobe, wo, lights, C3f(lobe.filter_o(lobe.frame.local(wo)).toRGB(0)));
    } else if (leaf == "oren_nayar") {
        using T = Wrap<mtx::OrenNayarDiffuseLobe>;
        T::Data d {};
        d.N = N;
        d.albedo = in.c("color", C3f(0.18f));
        d.roughness = in.f("roughness", 0.0f);
        d.energy_compensation = int(in.f("energy_compensation", 0));
        T lobe(d, g);
        answer(lobe, wo, lights, black);
    } else if (leaf == "burley") {
        using T = Wrap<mtx::BurleyDiffuseLobe>;
        T::Data d {};
        d.N = N;
        d.albedo = in.c("color", C3f(0.18f));
        d.roughness = in.f("roughness", 0.0f);
        T lobe(d, g);
        answer(lobe, wo, lights, black);
    } else if (leaf == "translucent") {
        using T = Wrap<mtx::TranslucentLobe>;
        T::Data d {};
        d.N = N;
        d.albedo = in.c("color", C3f(1));
        T lobe(d, g);
        answer(lobe, wo, lights, black);
    } else {
        return false;
    }
    return true;
}

int main()
{
    std::string line;
    while (std::getline(std::cin, line)) {
        if (line.empty() || line[0] == '#')
            continue;
        std::stringstream parts(line);
        std::string head, view_part, lights_part;
        std::getline(parts, head, '|');
        std::getline(parts, view_part, '|');
        std::getline(parts, lights_part, '|');

        Inputs in;
        std::stringstream hs(head);
        std::string token;
        hs >> in.id;
        while (hs >> token) {
            const size_t eq = token.find('=');
            if (eq == std::string::npos) {
                std::fprintf(stderr, "case %s: bad input '%s'\n", in.id.c_str(), token.c_str());
                return 1;
            }
            in.kv[token.substr(0, eq)] = token.substr(eq + 1);
        }

        std::stringstream vs(view_part);
        V3f wo;
        vs >> wo.x >> wo.y >> wo.z;
        wo.normalize();
        std::vector<V3f> lights;
        std::stringstream ls(lights_part);
        V3f l;
        while (ls >> l.x >> l.y >> l.z)
            lights.push_back(l.normalized());

        std::printf("%s |", in.id.c_str());
        if (!run(in, wo, lights)) {
            std::fprintf(stderr, "case %s: unknown leaf\n", in.id.c_str());
            return 1;
        }
        std::printf("\n");
    }
    return 0;
}
