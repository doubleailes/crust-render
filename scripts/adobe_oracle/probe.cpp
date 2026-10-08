// Evaluates Adobe's OpenPBR BSDF reference (github.com/adobe/openpbr-bsdf) for
// the cases `scripts/adobe_oracle.py` writes, one per stdin line:
//
//     <id> <name>=<value> ... | <vx> <vy> <vz> | <lx> <ly> <lz> ...
//
// A value is a float, or three comma-separated floats for a colour. Unnamed
// inputs keep Adobe's defaults (`openpbr_make_default_resolved_inputs`). The
// shading frame is Z-up with the outward normal +Z: a view direction with
// negative z hits the back face.
//
// For each case it prints one line:
//
//     <id> | <emission rgb> | <albedo rgb> | <value rgb> ...
//
// - emission: `prepared.emission`, the radiance leaving toward the view;
// - albedo: (π / N) Σ f(v, l) over the cosine-weighted midpoint grid of
//   `GRID`² directions on the view's side (`grid_direction` below; the
//   replay in `crates/crust-core/tests/adobe_oracle.rs` builds the same grid);
// - value: `openpbr_eval` toward each light direction, the BSDF times the
//   cosine (Adobe's convention, and crust's `Material::eval`).
//
// Built and run by `scripts/adobe_oracle.py`; not part of the Cargo build.

#include <glm/glm.hpp>

#include "openpbr.h"

#include <cmath>
#include <cstdio>
#include <iostream>
#include <sstream>
#include <string>
#include <vector>

static constexpr int GRID = 32;

// Malley's method on the cell midpoints of a GRID × GRID square: a
// deterministic cosine-weighted set, in the hemisphere of sign `side`.
static vec3 grid_direction(int i, int j, float side)
{
    const float u = (float(i) + 0.5f) / float(GRID);
    const float v = (float(j) + 0.5f) / float(GRID);
    const float r = std::sqrt(u);
    const float phi = 2.0f * float(M_PI) * v;
    return vec3(r * std::cos(phi), r * std::sin(phi), side * std::sqrt(std::max(0.0f, 1.0f - u)));
}

static bool parse_vec3(const std::string& s, vec3& out)
{
    float a, b, c;
    if (std::sscanf(s.c_str(), "%f,%f,%f", &a, &b, &c) != 3)
        return false;
    out = vec3(a, b, c);
    return true;
}

static bool set_input(OpenPBR_ResolvedInputs& in, const std::string& name, const std::string& value)
{
#define F(field)                                \
    if (name == #field) {                       \
        in.field = std::stof(value);            \
        return true;                            \
    }
#define C(field)                                \
    if (name == #field)                         \
        return parse_vec3(value, in.field);
    F(base_weight) C(base_color) F(base_diffuse_roughness) F(base_metalness)
    F(subsurface_weight) C(subsurface_color) F(subsurface_radius) C(subsurface_radius_scale)
    F(subsurface_scatter_anisotropy)
    F(specular_weight) C(specular_color) F(specular_roughness) F(specular_roughness_anisotropy)
    F(specular_ior)
    F(coat_weight) C(coat_color) F(coat_roughness) F(coat_roughness_anisotropy) F(coat_ior)
    F(coat_darkening)
    F(fuzz_weight) C(fuzz_color) F(fuzz_roughness)
    F(transmission_weight) C(transmission_color) F(transmission_depth) C(transmission_scatter)
    F(transmission_scatter_anisotropy) F(transmission_dispersion_scale)
    F(transmission_dispersion_abbe_number)
    F(thin_film_weight) F(thin_film_thickness) F(thin_film_ior)
    F(emission_luminance) C(emission_color)
    F(geometry_opacity)
    if (name == "geometry_thin_walled") {
        in.geometry_thin_walled = value != "0";
        return true;
    }
#undef F
#undef C
    return false;
}

static void print_vec3(const vec3& v)
{
    std::printf(" %.9g %.9g %.9g", v.x, v.y, v.z);
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

        OpenPBR_ResolvedInputs in = openpbr_make_default_resolved_inputs();
        std::stringstream hs(head);
        std::string id, token;
        hs >> id;
        while (hs >> token) {
            const size_t eq = token.find('=');
            if (eq == std::string::npos || !set_input(in, token.substr(0, eq), token.substr(eq + 1))) {
                std::fprintf(stderr, "case %s: bad input '%s'\n", id.c_str(), token.c_str());
                return 1;
            }
        }

        std::stringstream vs(view_part);
        vec3 view;
        vs >> view.x >> view.y >> view.z;
        view = normalize(view);

        const OpenPBR_PreparedBsdf prepared =
            openpbr_prepare(in, vec3(1.0f), OpenPBR_BaseRgbWavelengths_nm, OpenPBR_VacuumIor, view);

        std::printf("%s |", id.c_str());
        print_vec3(prepared.emission);
        std::printf(" |");

        const float side = view.z < 0.0f ? -1.0f : 1.0f;
        glm::dvec3 sum(0.0);
        for (int i = 0; i < GRID; ++i)
            for (int j = 0; j < GRID; ++j) {
                const vec3 l = grid_direction(i, j, side);
                const float cos_l = std::abs(l.z);
                if (cos_l <= 0.0f)
                    continue;
                const vec3 f_cos = openpbr_get_sum_of_diffuse_specular(openpbr_eval(prepared, l));
                sum += glm::dvec3(f_cos / cos_l);
            }
        print_vec3(vec3(sum * (M_PI / double(GRID * GRID))));
        std::printf(" |");

        std::stringstream ls(lights_part);
        vec3 l;
        while (ls >> l.x >> l.y >> l.z)
            print_vec3(openpbr_get_sum_of_diffuse_specular(openpbr_eval(prepared, normalize(l))));
        std::printf("\n");
    }
    return 0;
}
