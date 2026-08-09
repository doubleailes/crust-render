#include "renderPass.h"

#include "renderBuffer.h"

#include <pxr/base/gf/half.h>
#include <pxr/base/gf/vec3d.h>
#include <pxr/base/gf/vec3f.h>
#include <pxr/base/tf/diagnostic.h>
#include <pxr/base/tf/envSetting.h>
#include <pxr/imaging/hd/renderPassState.h>
#include <pxr/imaging/hd/tokens.h>

#include <algorithm>
#include <cmath>
#include <cstring>

PXR_NAMESPACE_OPEN_SCOPE

TF_DEFINE_ENV_SETTING(HDCRUST_SAMPLES_PER_PIXEL, 64,
                      "Total path-tracing sample budget per pixel");
TF_DEFINE_ENV_SETTING(HDCRUST_SAMPLES_PER_STEP, 4,
                      "Samples per pixel added per Hydra Execute call");
TF_DEFINE_ENV_SETTING(HDCRUST_MAX_DEPTH, 8, "Path length bound");

HdCrustRenderPass::HdCrustRenderPass(HdRenderIndex* index,
                                     HdRprimCollection const& collection,
                                     HdCrustRenderParam* renderParam)
    : HdRenderPass(index, collection), _renderParam(renderParam) {}

HdCrustRenderPass::~HdCrustRenderPass() { _DestroyRenderer(); }

void HdCrustRenderPass::_DestroyRenderer() {
    if (_token) {
        // Wound-down first so a hypothetical in-flight step ends promptly;
        // in this synchronous design nothing is in flight, but the order
        // costs nothing and survives a future HdRenderThread wrapper.
        crust_stop_token_stop(_token);
    }
    if (_renderer) {
        crust_renderer_destroy(_renderer);
        _renderer = nullptr;
    }
    if (_token) {
        crust_stop_token_destroy(_token);
        _token = nullptr;
    }
}

// UsdLux normalizes light energy as color * intensity * 2^exposure.
static GfVec3f _LightEnergy(HdCrustCachedLight const& light) {
    float scale = light.intensity * std::pow(2.0f, light.exposure);
    return light.color * scale;
}

static void _AddLight(CrustScene* scene, HdCrustCachedLight const& light) {
    GfVec3f energy = _LightEnergy(light);
    float const* energyPtr = energy.data();

    if (light.type == HdPrimTypeTokens->sphereLight) {
        GfVec3d p = light.xform.ExtractTranslation();
        float center[3] = {float(p[0]), float(p[1]), float(p[2])};
        // A uniformly scaled light scales its radius; take the mean axis
        // length for the (rare) non-uniform case.
        GfVec3d rows[3] = {GfVec3d(light.xform.GetRow3(0)),
                           GfVec3d(light.xform.GetRow3(1)),
                           GfVec3d(light.xform.GetRow3(2))};
        double scale =
            (rows[0].GetLength() + rows[1].GetLength() + rows[2].GetLength()) /
            3.0;
        float radius = std::max(1e-4f, float(light.radius * scale));
        crust_scene_add_sphere_light(scene, center, radius, energyPtr);
    } else if (light.type == HdPrimTypeTokens->rectLight) {
        // UsdLux rect: local XY plane, emitting along local -Z. crust's
        // rect light emits along edge_u x edge_v, so span the quad with
        // +X and -Y edges: X x -Y = -Z.
        GfVec3d origin =
            light.xform.Transform(GfVec3d(-light.width / 2.0, light.height / 2.0, 0.0));
        GfVec3d edgeU = light.xform.TransformDir(GfVec3d(light.width, 0.0, 0.0));
        GfVec3d edgeV = light.xform.TransformDir(GfVec3d(0.0, -light.height, 0.0));
        float o[3] = {float(origin[0]), float(origin[1]), float(origin[2])};
        float u[3] = {float(edgeU[0]), float(edgeU[1]), float(edgeU[2])};
        float v[3] = {float(edgeV[0]), float(edgeV[1]), float(edgeV[2])};
        crust_scene_add_rect_light(scene, o, u, v, energyPtr);
    } else if (light.type == HdPrimTypeTokens->distantLight) {
        // Points down its local -Z; energy is irradiance (the engine's
        // distant-light convention matches UsdLux's normalized one).
        GfVec3d dir = light.xform.TransformDir(GfVec3d(0.0, 0.0, -1.0));
        float d[3] = {float(dir[0]), float(dir[1]), float(dir[2])};
        crust_scene_add_distant_light(scene, d, energyPtr, light.angle);
    } else if (light.type == HdPrimTypeTokens->domeLight) {
        // Only the rotation of a dome's frame matters (it sits at
        // infinity). Row-major row-vector 3x3 laid out row-after-row is
        // exactly the column-major column-vector array crust expects —
        // the same transpose-cancellation as the 4x4 camera matrices.
        float rotation[9];
        for (int row = 0; row < 3; ++row) {
            for (int col = 0; col < 3; ++col) {
                rotation[row * 3 + col] = float(light.xform[row][col]);
            }
        }
        crust_scene_add_dome_light(scene, energyPtr, 0, 0, nullptr, rotation);
    }
}

void HdCrustRenderPass::_RebuildScene(GfMatrix4d const& view,
                                      GfMatrix4d const& proj, uint32_t width,
                                      uint32_t height) {
    _DestroyRenderer();
    _converged = false;
    _geomToPrimId.clear();

    std::map<SdfPath, HdCrustCachedMesh> meshes;
    std::map<SdfPath, HdCrustCachedLight> lights;
    _lastVersion = _renderParam->Snapshot(&meshes, &lights);
    _lastView = view;
    _lastProj = proj;
    _width = width;
    _height = height;

    CrustScene* scene = crust_scene_create();

    for (auto const& entry : meshes) {
        HdCrustCachedMesh const& mesh = entry.second;
        if (!mesh.visible || mesh.points.empty() || mesh.triangles.empty()) {
            continue;
        }
        CrustMaterial material;
        crust_material_default(&material);
        material.base_color[0] = mesh.displayColor[0];
        material.base_color[1] = mesh.displayColor[1];
        material.base_color[2] = mesh.displayColor[2];

        // One capi mesh per placement, world-space baked — the Phase 1 C
        // API has no instance object (the documented Phase 2 seam), so
        // instancing is flattened here.
        VtMatrix4dArray const singlePlacement(1, mesh.xform);
        VtMatrix4dArray const& placements =
            mesh.instanceXforms.empty() ? singlePlacement : mesh.instanceXforms;

        std::vector<float> positions(mesh.points.size() * 3);
        std::vector<float> normals;
        std::vector<uint32_t> indices(mesh.triangles.size() * 3);
        for (size_t t = 0; t < mesh.triangles.size(); ++t) {
            indices[3 * t] = uint32_t(mesh.triangles[t][0]);
            indices[3 * t + 1] = uint32_t(mesh.triangles[t][1]);
            indices[3 * t + 2] = uint32_t(mesh.triangles[t][2]);
        }

        for (GfMatrix4d const& placement : placements) {
            GfMatrix4d xform = mesh.instanceXforms.empty()
                                   ? placement
                                   : mesh.xform * placement;
            for (size_t i = 0; i < mesh.points.size(); ++i) {
                GfVec3d p = xform.Transform(GfVec3d(mesh.points[i]));
                positions[3 * i] = float(p[0]);
                positions[3 * i + 1] = float(p[1]);
                positions[3 * i + 2] = float(p[2]);
            }
            const float* normalsPtr = nullptr;
            if (!mesh.normals.empty()) {
                // Normals map through the inverse transpose (mirrors and
                // non-uniform scales included).
                GfMatrix4d normalXf = xform.GetInverse().GetTranspose();
                normals.resize(mesh.normals.size() * 3);
                for (size_t i = 0; i < mesh.normals.size(); ++i) {
                    GfVec3d n = normalXf.TransformDir(GfVec3d(mesh.normals[i]));
                    n.Normalize();
                    normals[3 * i] = float(n[0]);
                    normals[3 * i + 1] = float(n[1]);
                    normals[3 * i + 2] = float(n[2]);
                }
                normalsPtr = normals.data();
            }
            uint32_t geomId = 0;
            CrustStatus status = crust_scene_add_mesh(
                scene, positions.data(), mesh.points.size(), indices.data(),
                mesh.triangles.size(), normalsPtr, &material, &geomId);
            if (status != CRUST_OK) {
                TF_WARN("hdCrust: add_mesh(%s) failed: %s",
                        entry.first.GetText(), crust_status_string(status));
                continue;
            }
            if (_geomToPrimId.size() <= geomId) {
                _geomToPrimId.resize(geomId + 1, -1);
            }
            _geomToPrimId[geomId] = mesh.primId;
        }
    }

    for (auto const& entry : lights) {
        _AddLight(scene, entry.second);
    }

    CrustStatus status = crust_scene_set_camera(
        scene, view.GetArray(), proj.GetArray(), 0.0f, 1.0f);
    if (status != CRUST_OK) {
        TF_WARN("hdCrust: set_camera failed: %s", crust_status_string(status));
    }

    CrustRenderSettings settings;
    crust_render_settings_default(&settings);
    settings.width = width;
    settings.height = height;
    settings.samples_per_pixel =
        uint32_t(std::max(1, TfGetEnvSetting(HDCRUST_SAMPLES_PER_PIXEL)));
    settings.max_depth = uint32_t(std::max(1, TfGetEnvSetting(HDCRUST_MAX_DEPTH)));
    crust_scene_set_render_settings(scene, &settings);

    _token = crust_stop_token_create();
    status = crust_scene_commit(scene, _token, &_renderer);
    crust_scene_destroy(scene);
    if (status != CRUST_OK) {
        TF_WARN("hdCrust: commit failed: %s", crust_status_string(status));
        _DestroyRenderer();
    }
}

// Camera-forward distance -> the [0,1] depth Hydra expects, through the
// projection (row-vector convention: clip = v * P).
static float _NdcDepth(GfMatrix4d const& proj, float distance) {
    if (!std::isfinite(distance)) {
        return 1.0f;
    }
    double zCam = -double(distance);
    double zClip = proj[2][2] * zCam + proj[3][2];
    double wClip = proj[2][3] * zCam + proj[3][3];
    if (wClip == 0.0) {
        return 1.0f;
    }
    double ndc = zClip / wClip;
    return float(GfClamp(ndc * 0.5 + 0.5, 0.0, 1.0));
}

void HdCrustRenderPass::_Execute(
    HdRenderPassStateSharedPtr const& renderPassState,
    TfTokenVector const& renderTags) {
    (void)renderTags;

    HdRenderPassAovBindingVector const& aovBindings =
        renderPassState->GetAovBindings();
    if (aovBindings.empty()) {
        static bool warned = false;
        if (!warned) {
            TF_WARN("hdCrust: no AOV bindings; nothing to render into");
            warned = true;
        }
        return;
    }

    uint32_t width = 0;
    uint32_t height = 0;
    for (HdRenderPassAovBinding const& binding : aovBindings) {
        if (binding.renderBuffer) {
            width = binding.renderBuffer->GetWidth();
            height = binding.renderBuffer->GetHeight();
            break;
        }
    }
    if (width == 0 || height == 0) {
        return;
    }

    GfMatrix4d view = renderPassState->GetWorldToViewMatrix();
    GfMatrix4d proj = renderPassState->GetProjectionMatrix();

    if (!_renderer || _renderParam->GetVersion() != _lastVersion ||
        view != _lastView || proj != _lastProj || width != _width ||
        height != _height) {
        _RebuildScene(view, proj, width, height);
    }
    if (!_renderer) {
        return;
    }

    CrustStepStatus stepStatus = CRUST_STEP_IN_PROGRESS;
    uint32_t sppDone = 0;
    uint32_t chunk = uint32_t(std::max(1, TfGetEnvSetting(HDCRUST_SAMPLES_PER_STEP)));
    crust_renderer_step(_renderer, chunk, &stepStatus, &sppDone);
    _converged = (stepStatus == CRUST_STEP_COMPLETE);

    size_t pixels = size_t(_width) * size_t(_height);
    _rgba.resize(pixels * 4);
    crust_renderer_read_color(_renderer, _rgba.data(), pixels);

    // Both sides are bottom-up (row 0 = bottom scanline), so every write
    // below is a straight y*width+x indexed copy — no flip anywhere.
    for (HdRenderPassAovBinding const& binding : aovBindings) {
        auto* buffer = static_cast<HdCrustRenderBuffer*>(binding.renderBuffer);
        if (!buffer || buffer->GetWidth() != _width ||
            buffer->GetHeight() != _height) {
            continue;
        }
        HdFormat format = buffer->GetFormat();
        uint8_t* out = buffer->GetStorage();

        if (binding.aovName == HdAovTokens->color) {
            if (format == HdFormatFloat32Vec4) {
                std::memcpy(out, _rgba.data(), pixels * 4 * sizeof(float));
            } else if (format == HdFormatFloat16Vec4) {
                auto* half = reinterpret_cast<GfHalf*>(out);
                for (size_t i = 0; i < pixels * 4; ++i) {
                    half[i] = GfHalf(_rgba[i]);
                }
            } else if (format == HdFormatUNorm8Vec4) {
                // 8-bit targets expect display-encoded values.
                for (size_t i = 0; i < pixels * 4; ++i) {
                    float v = GfClamp(_rgba[i], 0.0f, 1.0f);
                    bool alphaChannel = (i % 4) == 3;
                    if (!alphaChannel) {
                        v = v <= 0.0031308f
                                ? v * 12.92f
                                : 1.055f * std::pow(v, 1.0f / 2.4f) - 0.055f;
                    }
                    out[i] = uint8_t(v * 255.0f + 0.5f);
                }
            } else {
                TF_WARN("hdCrust: unsupported color format %d", int(format));
            }
        } else if (binding.aovName == HdAovTokens->depth &&
                   format == HdFormatFloat32) {
            _depth.resize(pixels);
            crust_renderer_read_aov_depth(_renderer, _depth.data(), pixels);
            auto* depthOut = reinterpret_cast<float*>(out);
            for (size_t i = 0; i < pixels; ++i) {
                depthOut[i] = _NdcDepth(proj, _depth[i]);
            }
        } else if (binding.aovName == HdAovTokens->primId &&
                   format == HdFormatInt32) {
            _ids.resize(pixels * 2);
            crust_renderer_read_aov_id(_renderer, _ids.data(), pixels);
            auto* idOut = reinterpret_cast<int32_t*>(out);
            for (size_t i = 0; i < pixels; ++i) {
                uint32_t geomId = _ids[2 * i];
                idOut[i] = geomId < _geomToPrimId.size() ? _geomToPrimId[geomId]
                                                         : -1;
            }
        }

        buffer->SetConverged(_converged);
    }
}

PXR_NAMESPACE_CLOSE_SCOPE
