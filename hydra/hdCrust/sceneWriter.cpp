#include "sceneWriter.h"
#include "crustAttributes.h"

#include "pxr/base/gf/camera.h"
#include "pxr/base/gf/math.h"
#include "pxr/base/gf/transform.h"
#include "pxr/base/gf/vec2f.h"
#include "pxr/base/gf/vec2i.h"
#include "pxr/base/tf/stringUtils.h"
#include "pxr/imaging/hd/basisCurvesSchema.h"
#include "pxr/imaging/hd/basisCurvesTopologySchema.h"
#include "pxr/imaging/hd/cameraSchema.h"
#include "pxr/imaging/hd/capsuleSchema.h"
#include "pxr/imaging/hd/collectionSchema.h"
#include "pxr/imaging/hd/collectionsSchema.h"
#include "pxr/imaging/hd/coneSchema.h"
#include "pxr/imaging/hd/cubeSchema.h"
#include "pxr/imaging/hd/cylinderSchema.h"
#include "pxr/imaging/hd/geomSubsetSchema.h"
#include "pxr/imaging/hd/instancedBySchema.h"
#include "pxr/imaging/hd/instancerTopologySchema.h"
#include "pxr/imaging/hd/lightSchema.h"
#include "pxr/imaging/hd/purposeSchema.h"
#include "pxr/imaging/hd/renderSettingsSchema.h"
#include "pxr/imaging/hd/sceneGlobalsSchema.h"
#include "pxr/imaging/hd/materialBindingSchema.h"
#include "pxr/imaging/hd/materialBindingsSchema.h"
#include "pxr/imaging/hd/sphereSchema.h"
#include "pxr/imaging/hd/materialConnectionSchema.h"
#include "pxr/imaging/hd/materialNetworkSchema.h"
#include "pxr/imaging/hd/materialNodeParameterSchema.h"
#include "pxr/imaging/hd/materialNodeSchema.h"
#include "pxr/imaging/hd/materialSchema.h"
#include "pxr/imaging/hd/meshSchema.h"
#include "pxr/imaging/hd/meshTopologySchema.h"
#include "pxr/imaging/hd/primvarSchema.h"
#include "pxr/imaging/hd/primvarsSchema.h"
#include "pxr/imaging/hd/subdivisionTagsSchema.h"
#include "pxr/imaging/hd/tokens.h"
#include "pxr/imaging/hd/visibilitySchema.h"
#include "pxr/imaging/hd/xformSchema.h"
#include "pxr/usd/sdf/attributeSpec.h"
#include "pxr/usd/sdf/copyUtils.h"
#include "pxr/usd/sdf/layer.h"
#include "pxr/usd/sdf/primSpec.h"
#include "pxr/usd/sdf/relationshipSpec.h"
#include "pxr/usd/sdf/types.h"
#include "pxr/usd/usd/stage.h"
#include "pxr/usd/sdf/schema.h"
#include "pxr/usd/usdGeom/basisCurves.h"
#include "pxr/usd/usdGeom/camera.h"
#include "pxr/usd/usdGeom/mesh.h"
#include "pxr/usd/usdGeom/pointInstancer.h"
#include "pxr/usd/usdGeom/points.h"
#include "pxr/usd/usdGeom/subset.h"
#include "pxr/usd/usdGeom/tokens.h"
#include "pxr/usd/usdShade/material.h"
#include "pxr/usd/usdShade/materialBindingAPI.h"
#include "pxr/usd/usdShade/shader.h"
#include "pxr/usd/usdShade/tokens.h"

#include <algorithm>
#include <map>
#include "pxr/usd/usdGeom/primvarsAPI.h"
#include "pxr/usd/usdGeom/scope.h"
#include "pxr/usd/usdGeom/xformable.h"
#include "pxr/usd/usdRender/product.h"
#include "pxr/usd/usdRender/settings.h"
#include "pxr/usd/usdRender/var.h"
#include "pxr/usd/usdRender/tokens.h"

PXR_NAMESPACE_OPEN_SCOPE

SdfPath const &
HdCrustPaths::Root()
{
    static SdfPath const p("/__hdCrust");
    return p;
}

SdfPath const &
HdCrustPaths::Settings()
{
    static SdfPath const p("/__hdCrust/settings");
    return p;
}

SdfPath const &
HdCrustPaths::Camera()
{
    static SdfPath const p("/__hdCrust/camera");
    return p;
}

SdfPath const &
HdCrustPaths::Dicing()
{
    static SdfPath const p("/__hdCrust/dicing");
    return p;
}

namespace {

/// A typed data source's value at shutter offset 0, or `fallback`.
template <class T, class Handle>
T
_Value(Handle const &ds, T const &fallback = T())
{
    return ds ? ds->GetTypedValue(0.0f) : fallback;
}

/// The prim's Hydra transform as its only op. At the top level it is the
/// world transform and resets the stack, so no ancestor written beside it
/// composes into it; inside a PointInstancer's prototype (`reset` false) it
/// is relative to the instancer, as Hydra gives it.
void
_WriteXform(UsdPrim const &prim, HdContainerDataSourceHandle const &ds, bool reset)
{
    UsdGeomXformable xformable(prim);
    if (!xformable) {
        return;
    }
    HdXformSchema xform = HdXformSchema::GetFromParent(ds);
    GfMatrix4d const matrix = _Value(xform.GetMatrix(), GfMatrix4d(1.0));
    xformable.SetResetXformStack(reset);
    xformable.AddTransformOp().Set(matrix);
}

/// The USD type of a primvar's value, its role kept.
SdfValueTypeName
_TypeOf(VtValue const &value, TfToken const &role)
{
    if (role == HdPrimvarSchemaTokens->point && value.IsHolding<VtVec3fArray>()) {
        return SdfValueTypeNames->Point3fArray;
    }
    if (role == HdPrimvarSchemaTokens->normal && value.IsHolding<VtVec3fArray>()) {
        return SdfValueTypeNames->Normal3fArray;
    }
    if (role == HdPrimvarSchemaTokens->color && value.IsHolding<VtVec3fArray>()) {
        return SdfValueTypeNames->Color3fArray;
    }
    if (role == HdPrimvarSchemaTokens->textureCoordinate && value.IsHolding<VtVec2fArray>()) {
        return SdfValueTypeNames->TexCoord2fArray;
    }
    return SdfGetValueTypeNameForValue(value);
}

/// Every primvar of a gprim: `points` as a point-based prim's points,
/// `widths` as curves' and points' widths (with their interpolation), the
/// others as `primvars:<name>` with their interpolation and indices.
void
_WritePrimvars(UsdPrim const &prim, HdContainerDataSourceHandle const &ds)
{
    HdPrimvarsSchema primvars = HdPrimvarsSchema::GetFromParent(ds);
    UsdGeomPrimvarsAPI api(prim);
    UsdGeomPointBased gprim(prim);
    for (TfToken const &name : primvars.GetPrimvarNames()) {
        HdPrimvarSchema primvar = primvars.GetPrimvar(name);
        HdSampledDataSourceHandle indexed = primvar.GetIndexedPrimvarValue();
        HdIntArrayDataSourceHandle indicesDs = primvar.GetIndices();
        HdSampledDataSourceHandle value = indexed ? indexed : primvar.GetPrimvarValue();
        if (!value) {
            continue;
        }
        VtValue const v = value->GetValue(0.0f);
        if (v.IsEmpty()) {
            continue;
        }
        TfToken const interpolation = _Value(primvar.GetInterpolation(), UsdGeomTokens->constant);
        if (name == HdTokens->points) {
            if (gprim && v.IsHolding<VtVec3fArray>()) {
                gprim.GetPointsAttr().Set(v.UncheckedGet<VtVec3fArray>());
            }
            continue;
        }
        if (name == HdTokens->widths && v.IsHolding<VtFloatArray>()) {
            if (UsdGeomCurves curves{prim}) {
                curves.GetWidthsAttr().Set(v);
                curves.SetWidthsInterpolation(interpolation);
                continue;
            }
            if (UsdGeomPoints points{prim}) {
                points.GetWidthsAttr().Set(v);
                points.SetWidthsInterpolation(interpolation);
                continue;
            }
        }
        TfToken const role = _Value(primvar.GetRole(), TfToken());
        SdfValueTypeName const type = _TypeOf(v, role);
        if (!type) {
            continue;
        }
        UsdGeomPrimvar out = api.CreatePrimvar(name, type, interpolation);
        out.Set(v);
        if (indexed && indicesDs) {
            out.SetIndices(indicesDs->GetTypedValue(0.0f));
        }
        if (HdIntDataSourceHandle size = primvar.GetElementSize()) {
            out.SetElementSize(size->GetTypedValue(0.0f));
        }
    }
}

/// The material Hydra resolved for the prim (composition and inheritance
/// already applied), as a direct binding. The purpose is crust's own, the
/// import's: `full`, else all-purpose. The scene index keeps every purpose
/// apart, and ALab binds only `full` and `preview`, so all-purpose alone
/// rendered every asset in the default grey. A `proxy` prim, drawn only when
/// the host asks for proxies (usdview's default) and never by a final
/// render, falls back to its `preview` binding: ALab binds its proxies for
/// preview alone.
void
_WriteBinding(UsdPrim const &prim, HdContainerDataSourceHandle const &ds)
{
    HdMaterialBindingsSchema const bindings = HdMaterialBindingsSchema::GetFromParent(ds);
    SdfPath material = _Value(bindings.GetMaterialBinding(TfToken("full")).GetPath(), SdfPath());
    if (material.IsEmpty()) {
        material = _Value(bindings.GetMaterialBinding().GetPath(), SdfPath());
    }
    if (material.IsEmpty()
        && _Value(HdPurposeSchema::GetFromParent(ds).GetPurpose(), TfToken())
            == HdRenderTagTokens->proxy) {
        material =
            _Value(bindings.GetMaterialBinding(TfToken("preview")).GetPath(), SdfPath());
    }
    if (material.IsEmpty()) {
        return;
    }
    UsdShadeMaterialBindingAPI::Apply(prim);
    prim.CreateRelationship(UsdShadeTokens->materialBinding).SetTargets({material});
}

/// What every gprim carries beside its own schema: primvars, binding and
/// world transform.
void
_WriteGprim(UsdPrim const &prim, HdContainerDataSourceHandle const &ds, bool reset)
{
    _WritePrimvars(prim, ds);
    _WriteBinding(prim, ds);
    _WriteXform(prim, ds, reset);
}

void
_WriteMesh(UsdStageRefPtr const &stage, SdfPath const &path, HdContainerDataSourceHandle const &ds,
           bool reset)
{
    UsdGeomMesh mesh = UsdGeomMesh::Define(stage, path);
    HdMeshSchema schema = HdMeshSchema::GetFromParent(ds);
    HdMeshTopologySchema topology = schema.GetTopology();
    mesh.GetFaceVertexCountsAttr().Set(_Value(topology.GetFaceVertexCounts(), VtIntArray()));
    mesh.GetFaceVertexIndicesAttr().Set(_Value(topology.GetFaceVertexIndices(), VtIntArray()));
    if (auto holes = topology.GetHoleIndices()) {
        mesh.GetHoleIndicesAttr().Set(holes->GetTypedValue(0.0f));
    }
    if (auto orientation = topology.GetOrientation()) {
        mesh.GetOrientationAttr().Set(orientation->GetTypedValue(0.0f));
    }
    if (auto scheme = schema.GetSubdivisionScheme()) {
        mesh.GetSubdivisionSchemeAttr().Set(scheme->GetTypedValue(0.0f));
    }
    if (auto doubleSided = schema.GetDoubleSided()) {
        mesh.GetDoubleSidedAttr().Set(doubleSided->GetTypedValue(0.0f));
    }
    HdSubdivisionTagsSchema tags = schema.GetSubdivisionTags();
    if (tags) {
        if (auto v = tags.GetInterpolateBoundary()) {
            mesh.GetInterpolateBoundaryAttr().Set(v->GetTypedValue(0.0f));
        }
        if (auto v = tags.GetFaceVaryingLinearInterpolation()) {
            mesh.GetFaceVaryingLinearInterpolationAttr().Set(v->GetTypedValue(0.0f));
        }
        if (auto v = tags.GetTriangleSubdivisionRule()) {
            mesh.GetTriangleSubdivisionRuleAttr().Set(v->GetTypedValue(0.0f));
        }
        if (auto v = tags.GetCreaseIndices()) {
            mesh.GetCreaseIndicesAttr().Set(v->GetTypedValue(0.0f));
        }
        if (auto v = tags.GetCreaseLengths()) {
            mesh.GetCreaseLengthsAttr().Set(v->GetTypedValue(0.0f));
        }
        if (auto v = tags.GetCreaseSharpnesses()) {
            mesh.GetCreaseSharpnessesAttr().Set(v->GetTypedValue(0.0f));
        }
        if (auto v = tags.GetCornerIndices()) {
            mesh.GetCornerIndicesAttr().Set(v->GetTypedValue(0.0f));
        }
        if (auto v = tags.GetCornerSharpnesses()) {
            mesh.GetCornerSharpnessesAttr().Set(v->GetTypedValue(0.0f));
        }
    }
    _WriteGprim(mesh.GetPrim(), ds, reset);
}

void
_WriteCurves(UsdStageRefPtr const &stage, SdfPath const &path, HdContainerDataSourceHandle const &ds,
             bool reset)
{
    UsdGeomBasisCurves curves = UsdGeomBasisCurves::Define(stage, path);
    HdBasisCurvesTopologySchema topology = HdBasisCurvesSchema::GetFromParent(ds).GetTopology();
    curves.GetCurveVertexCountsAttr().Set(_Value(topology.GetCurveVertexCounts(), VtIntArray()));
    if (auto v = topology.GetType()) {
        curves.GetTypeAttr().Set(v->GetTypedValue(0.0f));
    }
    if (auto v = topology.GetBasis()) {
        curves.GetBasisAttr().Set(v->GetTypedValue(0.0f));
    }
    if (auto v = topology.GetWrap()) {
        curves.GetWrapAttr().Set(v->GetTypedValue(0.0f));
    }
    _WriteGprim(curves.GetPrim(), ds, reset);
}

void
_WritePoints(UsdStageRefPtr const &stage, SdfPath const &path, HdContainerDataSourceHandle const &ds,
             bool reset)
{
    _WriteGprim(UsdGeomPoints::Define(stage, path).GetPrim(), ds, reset);
}

/// An implicit shape as its own schema, which crust tessellates as GeomUtil
/// does. Hydra gives cylinders and capsules two radii: equal ones are
/// `Cylinder` / `Capsule`, different ones `Cylinder_1` / `Capsule_1`.
void
_WriteImplicit(UsdStageRefPtr const &stage, SdfPath const &path, TfToken const &type,
               HdContainerDataSourceHandle const &ds, bool reset)
{
    UsdPrim prim;
    auto setDouble = [&](char const *name, HdDoubleDataSourceHandle const &h) {
        if (h) {
            prim.CreateAttribute(TfToken(name), SdfValueTypeNames->Double).Set(h->GetTypedValue(0.0f));
        }
    };
    auto setAxis = [&](HdTokenDataSourceHandle const &h) {
        if (h) {
            prim.CreateAttribute(UsdGeomTokens->axis, SdfValueTypeNames->Token, SdfVariabilityUniform)
                .Set(h->GetTypedValue(0.0f));
        }
    };
    // Two radii, or one when they agree.
    auto radii = [&](char const *one, char const *two, HdDoubleDataSourceHandle const &top,
                     HdDoubleDataSourceHandle const &bottom, HdDoubleDataSourceHandle const &radius) {
        double const t = _Value(top, _Value(radius, 1.0));
        double const b = _Value(bottom, _Value(radius, 1.0));
        prim = stage->DefinePrim(path, TfToken(t == b ? one : two));
        if (t == b) {
            prim.CreateAttribute(UsdGeomTokens->radius, SdfValueTypeNames->Double).Set(t);
        } else {
            prim.CreateAttribute(UsdGeomTokens->radiusTop, SdfValueTypeNames->Double).Set(t);
            prim.CreateAttribute(UsdGeomTokens->radiusBottom, SdfValueTypeNames->Double).Set(b);
        }
    };
    if (type == HdPrimTypeTokens->cube) {
        prim = stage->DefinePrim(path, TfToken("Cube"));
        setDouble("size", HdCubeSchema::GetFromParent(ds).GetSize());
    } else if (type == HdPrimTypeTokens->sphere) {
        prim = stage->DefinePrim(path, TfToken("Sphere"));
        setDouble("radius", HdSphereSchema::GetFromParent(ds).GetRadius());
    } else if (type == HdPrimTypeTokens->cone) {
        HdConeSchema s = HdConeSchema::GetFromParent(ds);
        prim = stage->DefinePrim(path, TfToken("Cone"));
        setDouble("radius", s.GetRadius());
        setDouble("height", s.GetHeight());
        setAxis(s.GetAxis());
    } else if (type == HdPrimTypeTokens->cylinder) {
        HdCylinderSchema s = HdCylinderSchema::GetFromParent(ds);
        radii("Cylinder", "Cylinder_1", s.GetRadiusTop(), s.GetRadiusBottom(), s.GetRadius());
        setDouble("height", s.GetHeight());
        setAxis(s.GetAxis());
    } else if (type == HdPrimTypeTokens->capsule) {
        HdCapsuleSchema s = HdCapsuleSchema::GetFromParent(ds);
        radii("Capsule", "Capsule_1", s.GetRadiusTop(), s.GetRadiusBottom(), s.GetRadius());
        setDouble("height", s.GetHeight());
        setAxis(s.GetAxis());
    } else {
        return;
    }
    _WriteGprim(prim, ds, reset);
}

void
_WriteCamera(UsdStageRefPtr const &stage, SdfPath const &path, HdContainerDataSourceHandle const &ds,
             bool reset)
{
    UsdGeomCamera camera = UsdGeomCamera::Define(stage, path);
    HdCameraSchema schema = HdCameraSchema::GetFromParent(ds);
    // Hydra's lens values are in scene units; USD's in tenths of one.
    auto lens = [](HdFloatDataSourceHandle const &h, double unit) {
        return h ? float(h->GetTypedValue(0.0f) / unit) : 0.0f;
    };
    if (auto p = schema.GetProjection()) {
        camera.GetProjectionAttr().Set(p->GetTypedValue(0.0f) == HdCameraSchemaTokens->orthographic
                                           ? UsdGeomTokens->orthographic
                                           : UsdGeomTokens->perspective);
    }
    if (auto h = schema.GetFocalLength()) {
        camera.GetFocalLengthAttr().Set(lens(h, GfCamera::FOCAL_LENGTH_UNIT));
    }
    if (auto h = schema.GetHorizontalAperture()) {
        camera.GetHorizontalApertureAttr().Set(lens(h, GfCamera::APERTURE_UNIT));
    }
    if (auto h = schema.GetVerticalAperture()) {
        camera.GetVerticalApertureAttr().Set(lens(h, GfCamera::APERTURE_UNIT));
    }
    if (auto h = schema.GetHorizontalApertureOffset()) {
        camera.GetHorizontalApertureOffsetAttr().Set(lens(h, GfCamera::APERTURE_UNIT));
    }
    if (auto h = schema.GetVerticalApertureOffset()) {
        camera.GetVerticalApertureOffsetAttr().Set(lens(h, GfCamera::APERTURE_UNIT));
    }
    if (auto h = schema.GetFStop()) {
        camera.GetFStopAttr().Set(h->GetTypedValue(0.0f));
    }
    if (auto h = schema.GetFocusDistance()) {
        camera.GetFocusDistanceAttr().Set(h->GetTypedValue(0.0f));
    }
    if (auto h = schema.GetClippingRange()) {
        GfVec2f const r = h->GetTypedValue(0.0f);
        camera.GetClippingRangeAttr().Set(r);
    }
    _WriteXform(camera.GetPrim(), ds, reset);
}

/// `value` as the layer carries it: an asset path resolved, so the layer
/// reads the same file from the session directory as the stage did from its
/// own (spec: "Asset paths SHALL be written resolved").
VtValue
_Resolved(VtValue const &value)
{
    if (value.IsHolding<SdfAssetPath>()) {
        SdfAssetPath const &a = value.UncheckedGet<SdfAssetPath>();
        if (!a.GetResolvedPath().empty()) {
            return VtValue(SdfAssetPath(a.GetResolvedPath()));
        }
    }
    return value;
}

/// A UsdLux light: its schema type (Hydra's `domeLight` is `DomeLight`) and
/// its inputs. UsdImaging publishes a light's UsdLux inputs as a material
/// network whose `light` terminal node carries them as parameters; the
/// light schema itself lists only Hydra's own fields (`filters`, `isLight`).
void
_WriteLight(UsdStageRefPtr const &stage, SdfPath const &path, TfToken const &hydraType,
            HdContainerDataSourceHandle const &ds, bool reset)
{
    std::string type = hydraType.GetString();
    if (!type.empty()) {
        type[0] = static_cast<char>(std::toupper(static_cast<unsigned char>(type[0])));
    }
    UsdPrim prim = stage->DefinePrim(path, TfToken(type));
    HdMaterialNetworkSchema network = HdMaterialSchema::GetFromParent(ds).GetMaterialNetwork();
    HdMaterialConnectionSchema terminal =
        network.GetTerminals().Get(HdMaterialTerminalTokens->light);
    TfToken const node = _Value(terminal.GetUpstreamNodePath(), TfToken());
    HdMaterialNodeParameterContainerSchema params =
        network.GetNodes().Get(node).GetParameters();
    for (TfToken const &name : params.GetNames()) {
        HdSampledDataSourceHandle value = params.Get(name).GetValue();
        if (!value) {
            continue;
        }
        VtValue const v = _Resolved(value->GetValue(0.0f));
        SdfValueTypeName const t = SdfGetValueTypeNameForValue(v);
        if (v.IsEmpty() || !t) {
            continue;
        }
        std::string const attr = TfStringStartsWith(name.GetString(), "inputs:")
            ? name.GetString()
            : "inputs:" + name.GetString();
        prim.CreateAttribute(TfToken(attr), t).Set(v);
    }
    _WriteXform(prim, ds, reset);
}

/// The render contexts a material network is read from, in preference:
/// MaterialX's (`outputs:mtlx:surface`), then the universal one
/// (`outputs:surface`, UsdPreviewSurface), as crust's import prefers them.
TfToken const &
_MtlxContext()
{
    static TfToken const t("mtlx");
    return t;
}

/// Where a network node is written: at its own path when it sits under the
/// material (the authored layout), else as a child of the material.
SdfPath
_NodePath(SdfPath const &material, TfToken const &node)
{
    SdfPath const path(node.GetString());
    if (path.IsAbsolutePath() && path.HasPrefix(material) && path != material) {
        return path;
    }
    return material.AppendChild(TfToken(TfMakeValidIdentifier(node.GetString())));
}

/// A material: its network, read in the context crust prefers, written back
/// as `Shader` prims with their `info:id`, parameters and connections, and
/// the network's terminals as the material's outputs in that context.
void
_WriteMaterial(UsdStageRefPtr const &stage, SdfPath const &path, HdContainerDataSourceHandle const &ds)
{
    UsdShadeMaterial material = UsdShadeMaterial::Define(stage, path);
    HdMaterialSchema schema = HdMaterialSchema::GetFromParent(ds);
    TfToken context = _MtlxContext();
    HdMaterialNetworkSchema network = schema.GetMaterialNetwork(context);
    if (!network) {
        context = HdMaterialSchemaTokens->universalRenderContext;
        network = schema.GetMaterialNetwork();
    }
    if (!network) {
        return;
    }
    HdMaterialNodeContainerSchema nodes = network.GetNodes();
    std::map<TfToken, UsdShadeShader> shaders;
    for (TfToken const &name : nodes.GetNames()) {
        shaders[name] = UsdShadeShader::Define(stage, _NodePath(path, name));
    }
    // An upstream output, created on first use with its consumer's type.
    auto output = [&](TfToken const &node, TfToken const &name, SdfValueTypeName const &type) {
        auto it = shaders.find(node);
        if (it == shaders.end()) {
            return UsdShadeOutput();
        }
        UsdShadeOutput out = it->second.GetOutput(name);
        return out ? out : it->second.CreateOutput(name, type);
    };
    for (auto &[name, shader] : shaders) {
        HdMaterialNodeSchema node = nodes.Get(name);
        if (auto id = node.GetNodeIdentifier()) {
            shader.CreateIdAttr(VtValue(id->GetTypedValue(0.0f)));
        }
        HdMaterialNodeParameterContainerSchema params = node.GetParameters();
        auto typeOf = [&](TfToken const &input, VtValue const &value) {
            HdMaterialNodeParameterSchema p = params.Get(input);
            if (auto t = p.GetTypeName()) {
                if (SdfValueTypeName type = SdfSchema::GetInstance().FindType(t->GetTypedValue(0.0f))) {
                    return type;
                }
            }
            SdfValueTypeName const type = SdfGetValueTypeNameForValue(value);
            return type ? type : SdfValueTypeNames->Token;
        };
        for (TfToken const &input : params.GetNames()) {
            HdMaterialNodeParameterSchema p = params.Get(input);
            HdSampledDataSourceHandle value = p.GetValue();
            if (!value) {
                continue;
            }
            VtValue const v = _Resolved(value->GetValue(0.0f));
            if (v.IsEmpty()) {
                continue;
            }
            UsdShadeInput in = shader.CreateInput(input, typeOf(input, v));
            in.Set(v);
            if (auto space = p.GetColorSpace()) {
                in.GetAttr().SetColorSpace(space->GetTypedValue(0.0f));
            }
        }
        HdMaterialConnectionVectorContainerSchema connections = node.GetInputConnections();
        for (TfToken const &input : connections.GetNames()) {
            HdMaterialConnectionVectorSchema sources = connections.Get(input);
            UsdShadeInput in = shader.GetInput(input);
            if (!in) {
                in = shader.CreateInput(input, typeOf(input, VtValue()));
            }
            for (size_t i = 0; i < sources.GetNumElements(); ++i) {
                HdMaterialConnectionSchema c = sources.GetElement(i);
                TfToken const upstream = _Value(c.GetUpstreamNodePath(), TfToken());
                TfToken const outName = _Value(c.GetUpstreamNodeOutputName(), TfToken("out"));
                if (UsdShadeOutput src = output(upstream, outName, in.GetTypeName())) {
                    in.ConnectToSource(UsdShadeConnectionSourceInfo(src),
                                     UsdShadeConnectionModification::Append);
                }
            }
        }
    }
    HdMaterialConnectionContainerSchema terminals = network.GetTerminals();
    for (TfToken const &terminal : terminals.GetNames()) {
        HdMaterialConnectionSchema c = terminals.Get(terminal);
        TfToken const upstream = _Value(c.GetUpstreamNodePath(), TfToken());
        TfToken const outName = _Value(c.GetUpstreamNodeOutputName(), terminal);
        UsdShadeOutput src = output(upstream, outName, SdfValueTypeNames->Token);
        if (!src) {
            continue;
        }
        TfToken const name = context.IsEmpty()
            ? terminal
            : TfToken(context.GetString() + ":" + terminal.GetString());
        material.CreateOutput(name, SdfValueTypeNames->Token).ConnectToSource(src);
    }
}

/// Whether Hydra presents the prim as visible.
bool
_Visible(HdContainerDataSourceHandle const &ds)
{
    auto v = HdVisibilitySchema::GetFromParent(ds).GetVisibility();
    return !v || v->GetTypedValue(0.0f);
}

/// The instancers that instance the prim, empty for a prim placed once.
VtArray<SdfPath>
_InstancedBy(HdContainerDataSourceHandle const &ds)
{
    return _Value(HdInstancedBySchema::GetFromParent(ds).GetPaths(), VtArray<SdfPath>());
}

/// A per-face material binding of a mesh, as the mesh's `GeomSubset` child.
void
_WriteSubset(UsdStageRefPtr const &stage, SdfPath const &path, HdContainerDataSourceHandle const &ds)
{
    HdGeomSubsetSchema schema = HdGeomSubsetSchema::GetFromParent(ds);
    if (_Value(schema.GetType(), TfToken()) != HdGeomSubsetSchemaTokens->typeFaceSet) {
        return;
    }
    UsdGeomSubset subset = UsdGeomSubset::Define(stage, path);
    subset.GetElementTypeAttr().Set(UsdGeomTokens->face);
    subset.GetIndicesAttr().Set(_Value(schema.GetIndices(), VtIntArray()));
    subset.GetFamilyNameAttr().Set(UsdShadeTokens->materialBind);
    _WriteBinding(subset.GetPrim(), ds);
}

/// The prim types the writer does not report as unread: Hydra's render
/// settings and bookkeeping, which crust reads from the layers it writes or
/// does not need.
bool
_Bookkeeping(TfToken const &type)
{
    static TfToken const types[] = {
        TfToken(),                    TfToken("renderSettings"), TfToken("renderProduct"),
        TfToken("renderVar"),         TfToken("renderPass"),     TfToken("integrator"),
        TfToken("sampleFilter"),      TfToken("displayFilter"),  TfToken("coordSys"),
        TfToken("drawTarget"),        TfToken("extComputation"), TfToken("renderBuffer"),
        TfToken("imageShader"),       TfToken("task"),
    };
    for (TfToken const &t : types) {
        if (type == t) {
            return true;
        }
    }
    return false;
}

/// The prim's `crust:*` attributes, as hdCrust's UsdImaging adapter
/// publishes them (crustAttributesAdapter.h), written back onto `prim`:
/// Hydra carries no other route for them. Whether there were any.
bool
_WriteCrustAttributes(UsdPrim const &prim, HdContainerDataSourceHandle const &ds)
{
    HdContainerDataSourceHandle crust =
        HdContainerDataSource::Cast(ds->Get(HdCrustAttributesToken()));
    if (!crust) {
        return false;
    }
    bool any = false;
    for (TfToken const &name : crust->GetNames()) {
        auto sampled = HdSampledDataSource::Cast(crust->Get(name));
        if (!sampled) {
            continue;
        }
        VtValue const v = _Resolved(sampled->GetValue(0.0f));
        SdfValueTypeName const type = SdfGetValueTypeNameForValue(v);
        if (v.IsEmpty() || !type) {
            continue;
        }
        prim.CreateAttribute(name, type).Set(v);
        any = true;
    }
    return any;
}

/// One scene layer written from the terminal scene index.
struct _Writer
{
    HdSceneIndexBaseRefPtr si;
    UsdStageRefPtr stage;
    TfTokenVector const &renderTags;
    HdCrustSceneReport &report;

    /// The prototypes being copied, innermost last: the instancer, the
    /// prototype's root in the scene index and where it is written.
    struct Copy
    {
        SdfPath instancer, root, dstRoot;
    };
    std::vector<Copy> copies;

    /// A copied prim's binding still names the material where the scene
    /// index has it. A material the same instancer instances was copied
    /// with the prototype, so the binding follows it there (ALab's native
    /// instances bind `UsdNiPrototype/MATERIAL/…`, written under `p<i>`).
    void Rebind(SdfPath const &dst)
    {
        UsdPrim const prim = stage->GetPrimAtPath(dst);
        UsdRelationship const rel =
            prim ? prim.GetRelationship(UsdShadeTokens->materialBinding) : UsdRelationship();
        SdfPathVector targets;
        if (!rel || !rel.GetTargets(&targets)) {
            return;
        }
        for (SdfPath &target : targets) {
            for (auto c = copies.rbegin(); c != copies.rend(); ++c) {
                if (!target.HasPrefix(c->root)) {
                    continue;
                }
                HdSceneIndexPrim const material = si->GetPrim(target);
                VtArray<SdfPath> const by =
                    material.dataSource ? _InstancedBy(material.dataSource) : VtArray<SdfPath>();
                if (std::find(by.begin(), by.end(), c->instancer) != by.end()) {
                    target = target.ReplacePrefix(c->root, c->dstRoot);
                    break;
                }
            }
        }
        rel.SetTargets(targets);
    }

    /// Whether the render pass draws a gprim of `ds`'s purpose (empty tags:
    /// every purpose).
    bool Drawn(HdContainerDataSourceHandle const &ds) const
    {
        if (renderTags.empty()) {
            return true;
        }
        TfToken purpose = _Value(HdPurposeSchema::GetFromParent(ds).GetPurpose(), TfToken());
        // A prim of no particular purpose draws with the `geometry` tag.
        if (purpose.IsEmpty() || purpose == TfToken("default")) {
            purpose = HdRenderTagTokens->geometry;
        }
        return std::find(renderTags.begin(), renderTags.end(), purpose) != renderTags.end();
    }

    /// Notes data the layer cannot carry yet, once per kind, with the first
    /// prim that had it.
    void Unsupported(std::string const &kind, SdfPath const &path, std::string const &detail = {})
    {
        report.unsupported.emplace(kind, detail.empty() ? path.GetString()
                                                        : path.GetString() + " = " + detail);
    }

    /// Motion over the shutter on the prim's transform or points: the layer
    /// holds the values at the frame only.
    void CheckMotion(SdfPath const &path, HdContainerDataSourceHandle const &ds)
    {
        std::vector<HdSampledDataSource::Time> times;
        auto moves = [&](HdSampledDataSourceHandle const &h) {
            times.clear();
            return h != nullptr && h->GetContributingSampleTimesForInterval(-0.5f, 0.5f, &times)
                && times.size() > 1;
        };
        if (moves(HdXformSchema::GetFromParent(ds).GetMatrix())
            || moves(HdPrimvarsSchema::GetFromParent(ds).GetPrimvar(HdTokens->points).GetPrimvarValue())) {
            Unsupported("motion over the shutter (the frame's values are rendered)", path);
        }
    }

    /// Writes the Hydra prim at `src` as `dst`; `top` for a prim placed once
    /// (world transform), false inside an instancer's prototype.
    void Prim(SdfPath const &src, SdfPath const &dst, HdSceneIndexPrim const &prim, bool top)
    {
        TfToken const &type = prim.primType;
        HdContainerDataSourceHandle const &ds = prim.dataSource;
        bool const gprim = type == HdPrimTypeTokens->mesh || type == HdPrimTypeTokens->basisCurves
            || type == HdPrimTypeTokens->points || type == HdPrimTypeTokens->cube
            || type == HdPrimTypeTokens->sphere || type == HdPrimTypeTokens->cylinder
            || type == HdPrimTypeTokens->cone || type == HdPrimTypeTokens->capsule;
        if (gprim && !Drawn(ds)) {
            return;
        }
        if (gprim || HdPrimTypeIsLight(type) || type == HdPrimTypeTokens->instancer) {
            CheckMotion(src, ds);
        }
        if (type == HdPrimTypeTokens->mesh) {
            _WriteMesh(stage, dst, ds, top);
        } else if (type == HdPrimTypeTokens->basisCurves) {
            _WriteCurves(stage, dst, ds, top);
        } else if (type == HdPrimTypeTokens->points) {
            _WritePoints(stage, dst, ds, top);
            Unsupported("prim type points (crust does not render points)", src);
        } else if (gprim) {
            _WriteImplicit(stage, dst, type, ds, top);
        } else if (type == HdPrimTypeTokens->camera) {
            _WriteCamera(stage, dst, ds, top);
        } else if (HdPrimTypeIsLight(type)) {
            _WriteLight(stage, dst, type, ds, top);
            // UsdLux's links arrive as the light's collections. An unlinked
            // light's is everything: `//`, or `~//*.*` (every prim, no
            // property) as UsdCollectionAPI expands its default.
            static SdfPathExpression const allPrims("~//*.*");
            HdCollectionsSchema collections = HdCollectionsSchema::GetFromParent(ds);
            for (TfToken const &link : {HdTokens->lightLink, HdTokens->shadowLink}) {
                SdfPathExpression const members = _Value(
                    collections.GetCollection(link).GetMembershipExpression(),
                    SdfPathExpression::Everything());
                if (!members.IsEmpty() && members != SdfPathExpression::Everything()
                    && members != allPrims) {
                    Unsupported("light linking (the light illuminates everything)",
                                src.AppendProperty(link), members.GetText());
                }
            }
        } else if (type == HdPrimTypeTokens->material) {
            _WriteMaterial(stage, dst, ds);
        } else if (type == HdPrimTypeTokens->geomSubset) {
            _WriteSubset(stage, dst, ds);
        } else if (type == HdPrimTypeTokens->instancer) {
            Instancer(src, dst, ds, top);
        } else if (type == HdPrimTypeTokens->volume || type == TfToken("openvdbAsset")
                   || type == TfToken("field3dAsset")) {
            Unsupported("volume fields", src);
        } else if (!_Bookkeeping(type)) {
            Unsupported("prim type " + type.GetString(), src);
        }
        if (UsdPrim const written = stage->GetPrimAtPath(dst)) {
            _WriteCrustAttributes(written, ds);
        }
    }

    /// The scene: every visible prim placed once, at its own path; the
    /// prims an instancer instances are written by it, under it.
    void Walk(SdfPath const &path)
    {
        HdSceneIndexPrim const prim = si->GetPrim(path);
        if (prim.dataSource && _Visible(prim.dataSource) && _InstancedBy(prim.dataSource).empty()) {
            Prim(path, path, prim, true);
            // A prim written for nothing else, an Xform among them, keeps
            // the crust:* attributes it authors (inherited by what is below).
            if (!path.IsAbsoluteRootPath() && !stage->GetPrimAtPath(path)
                && prim.dataSource->Get(HdCrustAttributesToken())) {
                _WriteCrustAttributes(stage->DefinePrim(path), prim.dataSource);
            }
        }
        for (SdfPath const &child : si->GetChildPrimPaths(path)) {
            // Hydra's own prims (the task controller's, the host's default
            // camera light and free camera) live under root names starting
            // with `_`, and are not the stage's.
            if (path.IsAbsoluteRootPath() && TfStringStartsWith(child.GetName(), "_")) {
                continue;
            }
            Walk(child);
        }
    }

    /// An instancer as a `PointInstancer` at `dst` (D3): each instance's
    /// transform factored into position, orientation and scale, hidden
    /// instances as `invisibleIds`, and each prototype's prims copied under
    /// `dst/Prototypes/p<i>`, where the prototype's own transforms are
    /// relative to the instancer, as Hydra gives them.
    void Instancer(SdfPath const &src, SdfPath const &dst, HdContainerDataSourceHandle const &ds,
                   bool top)
    {
        UsdGeomPointInstancer instancer = UsdGeomPointInstancer::Define(stage, dst);
        HdInstancerTopologySchema topology = HdInstancerTopologySchema::GetFromParent(ds);
        VtArray<SdfPath> const prototypes = _Value(topology.GetPrototypes(), VtArray<SdfPath>());
        HdIntArrayVectorSchema indices = topology.GetInstanceIndices();

        // Which prototype each instance places.
        VtIntArray protoIndices;
        for (size_t p = 0; p < prototypes.size() && p < indices.GetNumElements(); ++p) {
            for (int id : _Value(indices.GetElement(p), VtIntArray())) {
                if (id < 0) {
                    continue;
                }
                if (size_t(id) >= protoIndices.size()) {
                    protoIndices.resize(size_t(id) + 1, 0);
                }
                protoIndices[id] = int(p);
            }
        }
        size_t const n = protoIndices.size();

        // Each instance's transform, as Hydra composes it: the instance
        // matrix, then scale, rotation and translation (row vectors).
        HdPrimvarsSchema primvars = HdPrimvarsSchema::GetFromParent(ds);
        auto values = [&](TfToken const &name) {
            HdSampledDataSourceHandle h = primvars.GetPrimvar(name).GetFlattenedPrimvarValue();
            return h ? h->GetValue(0.0f) : VtValue();
        };
        VtValue const translations = values(HdInstancerTokens->instanceTranslations);
        VtValue const rotations = values(HdInstancerTokens->instanceRotations);
        VtValue const scales = values(HdInstancerTokens->instanceScales);
        VtValue const matrices = values(HdInstancerTokens->instanceTransforms);
        // Whatever precision Hydra holds them in (half rotations, float
        // matrices), as the types the factoring below takes.
        VtVec3fArray const t =
            VtValue::Cast<VtVec3fArray>(translations).GetWithDefault<VtVec3fArray>();
        VtQuatfArray const r = VtValue::Cast<VtQuatfArray>(rotations).GetWithDefault<VtQuatfArray>();
        VtVec3fArray const s = VtValue::Cast<VtVec3fArray>(scales).GetWithDefault<VtVec3fArray>();
        VtMatrix4dArray const m =
            VtValue::Cast<VtMatrix4dArray>(matrices).GetWithDefault<VtMatrix4dArray>();

        VtVec3fArray positions(n);
        VtQuathArray orientations(n);
        VtVec3fArray scaleOut(n);
        bool sheared = false;
        for (size_t i = 0; i < n; ++i) {
            GfMatrix4d local(1.0);
            if (i < t.size()) {
                local.SetTranslate(GfVec3d(t[i]));
            }
            if (i < r.size()) {
                GfMatrix4d rotate(1.0);
                rotate.SetRotate(GfRotation(GfQuatd(r[i])));
                local = rotate * local;
            }
            if (i < s.size()) {
                GfMatrix4d scale(1.0);
                scale.SetScale(GfVec3d(s[i]));
                local = scale * local;
            }
            if (i < m.size()) {
                local = m[i] * local;
            }
            GfTransform const factored(local);
            positions[i] = GfVec3f(factored.GetTranslation());
            orientations[i] = GfQuath(factored.GetRotation().GetQuat());
            scaleOut[i] = GfVec3f(factored.GetScale());
            sheared = sheared
                || !GfIsClose(factored.GetPivotOrientation().GetAngle(), 0.0, 1e-6);
        }
        if (sheared) {
            Unsupported("sheared instance transforms (written without their shear)", src);
        }
        instancer.GetProtoIndicesAttr().Set(protoIndices);
        instancer.GetPositionsAttr().Set(positions);
        instancer.GetOrientationsAttr().Set(orientations);
        instancer.GetScalesAttr().Set(scaleOut);

        // Hidden instances.
        VtBoolArray const mask = _Value(topology.GetMask(), VtBoolArray());
        VtInt64Array invisible;
        for (size_t i = 0; i < mask.size() && i < n; ++i) {
            if (!mask[i]) {
                invisible.push_back(int64_t(i));
            }
        }
        if (!invisible.empty()) {
            instancer.GetInvisibleIdsAttr().Set(invisible);
        }

        SdfPathVector targets;
        for (size_t p = 0; p < prototypes.size(); ++p) {
            SdfPath const root = dst.AppendChild(TfToken("Prototypes"))
                                     .AppendChild(TfToken(TfStringPrintf("p%zu", p)));
            stage->DefinePrim(root);
            targets.push_back(root);
            copies.push_back({src, prototypes[p], root});
            Prototype(src, prototypes[p], prototypes[p], root);
            copies.pop_back();
        }
        instancer.GetPrototypesRel().SetTargets(targets);
        _WriteXform(instancer.GetPrim(), ds, top);
    }

    /// Copies the prims of the prototype rooted at `root` that `instancer`
    /// instances (a nested instancer included, its own prototypes left to
    /// it) from `path` down, under `dstRoot`.
    void Prototype(SdfPath const &instancer, SdfPath const &root, SdfPath const &path,
                   SdfPath const &dstRoot)
    {
        HdSceneIndexPrim const prim = si->GetPrim(path);
        if (prim.dataSource && _Visible(prim.dataSource)) {
            VtArray<SdfPath> const by = _InstancedBy(prim.dataSource);
            if (std::find(by.begin(), by.end(), instancer) != by.end()) {
                SdfPath const dst = path == root
                    ? dstRoot
                    : dstRoot.AppendPath(path.MakeRelativePath(root));
                Prim(path, dst, prim, false);
                Rebind(dst);
            }
        }
        for (SdfPath const &child : si->GetChildPrimPaths(path)) {
            Prototype(instancer, root, child, dstRoot);
        }
    }
};

} // namespace

bool
HdCrustWriteScene(HdSceneIndexBaseRefPtr const &sceneIndex, TfTokenVector const &renderTags,
                  std::string const &path, HdCrustSceneReport *report, SdfLayerRefPtr *layer,
                  std::string *error)
{
    UsdStageRefPtr stage = UsdStage::CreateInMemory("scene.usdc");
    if (layer) {
        *layer = stage->GetRootLayer();
    }
    HdCrustSceneReport ignored;
    if (sceneIndex) {
        _Writer writer{sceneIndex, stage, renderTags, report ? *report : ignored};
        writer.Walk(SdfPath::AbsoluteRootPath());
    }
    if (!stage->GetRootLayer()->Export(path)) {
        if (error) {
            *error = "cannot write " + path;
        }
        return false;
    }
    return true;
}

bool
HdCrustWritePrims(HdSceneIndexBaseRefPtr const &sceneIndex, TfTokenVector const &renderTags,
                  SdfPathVector const &paths, UsdStageRefPtr const &stage,
                  SdfPathVector *hidden, HdCrustSceneReport *report)
{
    HdCrustSceneReport ignored;
    _Writer writer{sceneIndex, stage, renderTags, report ? *report : ignored};
    for (SdfPath const &path : paths) {
        HdSceneIndexPrim const prim = sceneIndex->GetPrim(path);
        if (!prim.dataSource) {
            continue;
        }
        // An instance's prototype prims and an instancer are written with
        // their instancer, as a whole: not alone.
        if (!_InstancedBy(prim.dataSource).empty() || prim.primType == HdPrimTypeTokens->instancer) {
            return false;
        }
        if (!_Visible(prim.dataSource)) {
            hidden->push_back(path);
            continue;
        }
        writer.Prim(path, path, prim, true);
        if (!stage->GetPrimAtPath(path) && prim.dataSource->Get(HdCrustAttributesToken())) {
            _WriteCrustAttributes(stage->DefinePrim(path), prim.dataSource);
        }
    }
    return true;
}

namespace {

/// The attribute or relationship spec at `path` in the overs, else in the
/// scene: what is in effect for it.
SdfPropertySpecHandle
_Effective(SdfLayerHandle const &scene, SdfLayerHandle const &overs, SdfPath const &path)
{
    if (SdfPropertySpecHandle spec = overs->GetPropertyAtPath(path)) {
        return spec;
    }
    return scene->GetPropertyAtPath(path);
}

} // namespace

bool
HdCrustOverlay(SdfLayerHandle const &scene, SdfLayerHandle const &overs,
               SdfLayerHandle const &scratch, SdfPathVector const &roots, std::string *why)
{
    auto fail = [&](std::string const &reason) {
        if (why) {
            *why = reason;
        }
        return false;
    };
    for (SdfPath const &root : roots) {
        SdfPrimSpecHandle const written = scratch->GetPrimAtPath(root);
        if (!written) {
            continue;
        }
        // Each prim the writer wrote for `root` (a material's shaders too).
        std::vector<SdfPrimSpecHandle> prims = {written};
        for (size_t i = 0; i < prims.size(); ++i) {
            for (SdfPrimSpecHandle const &child : prims[i]->GetNameChildren()) {
                prims.push_back(child);
            }
        }
        for (SdfPrimSpecHandle const &prim : prims) {
            SdfPath const path = prim->GetPath();
            SdfPrimSpecHandle const before = scene->GetPrimAtPath(path);
            if (!before && !overs->GetPrimAtPath(path)) {
                // An added prim: its definition (a full import).
                SdfCreatePrimInLayer(overs, path.GetParentPath());
                if (!SdfCopySpec(scratch, path, overs, path)) {
                    return fail("cannot copy the added prim " + path.GetString());
                }
                continue;
            }
            if (before && before->GetTypeName() != prim->GetTypeName()) {
                return fail(path.GetString() + " changed type");
            }
            if (before) {
                for (SdfPropertySpecHandle const &old : before->GetProperties()) {
                    if (!scratch->GetPropertyAtPath(old->GetPath())) {
                        return fail(old->GetPath().GetString() + " was removed");
                    }
                }
            }
            for (SdfPropertySpecHandle const &now : prim->GetProperties()) {
                SdfPath const property = now->GetPath();
                SdfPropertySpecHandle const effective = _Effective(scene, overs, property);
                if (SdfAttributeSpecHandle attr = TfDynamic_cast<SdfAttributeSpecHandle>(now)) {
                    SdfAttributeSpecHandle const was = TfDynamic_cast<SdfAttributeSpecHandle>(effective);
                    VtValue const value = attr->GetDefaultValue();
                    if (was && was->GetDefaultValue() == value) {
                        continue;
                    }
                    // Only the value, with its declaration: what the
                    // session's diff reads as a value change.
                    SdfPrimSpecHandle over = SdfCreatePrimInLayer(overs, path);
                    SdfAttributeSpecHandle out = overs->GetAttributeAtPath(property);
                    if (!out) {
                        out = SdfAttributeSpec::New(over, attr->GetNameToken(), attr->GetTypeName(),
                                                    attr->GetVariability(), attr->IsCustom());
                    }
                    if (!out) {
                        return fail("cannot author " + property.GetString());
                    }
                    out->SetDefaultValue(value);
                } else if (SdfRelationshipSpecHandle rel = TfDynamic_cast<SdfRelationshipSpecHandle>(now)) {
                    SdfRelationshipSpecHandle const was = TfDynamic_cast<SdfRelationshipSpecHandle>(effective);
                    SdfPathVector const targets = rel->GetTargetPathList().GetAppliedItems();
                    if (was && was->GetTargetPathList().GetAppliedItems() == targets) {
                        continue;
                    }
                    SdfPrimSpecHandle over = SdfCreatePrimInLayer(overs, path);
                    SdfRelationshipSpecHandle out = overs->GetRelationshipAtPath(property);
                    if (!out) {
                        out = SdfRelationshipSpec::New(over, rel->GetNameToken(), rel->IsCustom());
                    }
                    if (!out) {
                        return fail("cannot author " + property.GetString());
                    }
                    out->GetTargetPathList().ClearEditsAndMakeExplicit();
                    for (SdfPath const &target : targets) {
                        out->GetTargetPathList().GetExplicitItems().push_back(target);
                    }
                }
            }
        }
    }
    return true;
}

void
HdCrustDeactivate(SdfLayerHandle const &overs, SdfPath const &path)
{
    if (SdfPrimSpecHandle over = SdfCreatePrimInLayer(overs, path)) {
        over->SetActive(false);
    }
}

HdCrustStageSettings
HdCrustReadStageSettings(HdSceneIndexBaseRefPtr const &sceneIndex)
{
    HdCrustStageSettings result;
    if (!sceneIndex) {
        return result;
    }
    HdSceneGlobalsSchema globals = HdSceneGlobalsSchema::GetFromSceneIndex(sceneIndex);
    SdfPath settingsPath = _Value(globals.GetActiveRenderSettingsPrim(), SdfPath());
    if (settingsPath.IsEmpty()) {
        settingsPath = SdfPath("/Render/settings");
    }
    HdSceneIndexPrim const prim = sceneIndex->GetPrim(settingsPath);
    if (prim.primType == HdPrimTypeTokens->renderSettings && prim.dataSource) {
        HdRenderSettingsSchema settings = HdRenderSettingsSchema::GetFromParent(prim.dataSource);
        result.camera = _Value(settings.GetCamera(), SdfPath());
        HdSampledDataSourceContainerSchema namespaced = settings.GetNamespacedSettings();
        for (TfToken const &name : namespaced.GetNames()) {
            if (!TfStringStartsWith(name.GetString(), "crust:")) {
                continue;
            }
            if (HdSampledDataSourceHandle v = namespaced.Get(name)) {
                result.crust[name] = _Resolved(v->GetValue(0.0f));
            }
        }
    }
    if (result.camera.IsEmpty()) {
        result.camera = _Value(globals.GetPrimaryCameraPrim(), SdfPath());
    }
    // Only a camera of the stage, which the scene layer holds: a host's own
    // (usdview's free camera, `/_UsdImaging_.../camera`, its primary camera
    // when the stage names none) is not written, and would leave
    // `crust:dicingCamera` naming nothing.
    if (!result.camera.IsEmpty()) {
        SdfPath root = result.camera;
        while (root.GetPathElementCount() > 1) {
            root = root.GetParentPath();
        }
        if (TfStringStartsWith(root.GetName(), "_")
            || sceneIndex->GetPrim(result.camera).primType != HdPrimTypeTokens->camera) {
            result.camera = SdfPath();
        }
    }
    return result;
}

bool
HdCrustAovSource(TfToken const &name, TfToken *source, SdfValueTypeName *type,
                 size_t *components)
{
    struct Entry
    {
        char const *hydra, *crust;
        SdfValueTypeName type;
        size_t components;
    };
    static Entry const entries[] = {
        {"depth", "depth", SdfValueTypeNames->Float, 1},
        {"normal", "N", SdfValueTypeNames->Normal3f, 3},
        {"Neye", "Neye", SdfValueTypeNames->Normal3f, 3},
        {"Peye", "Peye", SdfValueTypeNames->Point3f, 3},
    };
    for (Entry const &e : entries) {
        if (name == e.hydra) {
            *source = TfToken(e.crust);
            *type = e.type;
            *components = e.components;
            return true;
        }
    }
    return false;
}

namespace {

/// A camera prim from a view's matrices (D4): the lens from the projection,
/// the transform from the inverse view matrix.
void
_WriteViewCamera(UsdStageRefPtr const &stage, SdfPath const &path, HdCrustView const &view)
{
    GfCamera lens;
    lens.SetFromViewAndProjectionMatrix(view.worldToView, view.projection);
    UsdGeomCamera::Define(stage, path).SetFromCamera(lens, UsdTimeCode::Default());
}

} // namespace

bool
HdCrustWriteEdits(HdCrustEdits const &edits, std::string const &path, std::string *error)
{
    // Authored through a stage for the schemas' API, which has no sublayer
    // yet: a stage on an anonymous layer would try to compose
    // `./scene.usdc` relative to nothing. The sublayer is added to a copy
    // that no stage observes, and that copy is what is written.
    UsdStageRefPtr stage = UsdStage::CreateInMemory("edits.usda");

    UsdGeomScope::Define(stage, HdCrustPaths::Root());
    _WriteViewCamera(stage, HdCrustPaths::Camera(), edits.view);
    if (edits.dicingCamera == HdCrustPaths::Dicing()) {
        _WriteViewCamera(stage, HdCrustPaths::Dicing(), edits.dicingView);
    }

    UsdRenderSettings settings = UsdRenderSettings::Define(stage, HdCrustPaths::Settings());
    settings.GetCameraRel().SetTargets({HdCrustPaths::Camera()});
    settings.GetResolutionAttr().Set(GfVec2i(edits.view.width, edits.view.height));
    UsdPrim prim = settings.GetPrim();
    for (auto const &[name, value] : edits.settings) {
        SdfValueTypeName const type = SdfGetValueTypeNameForValue(value);
        if (!value.IsEmpty() && type) {
            prim.CreateAttribute(name, type).Set(value);
        }
    }
    if (!edits.dicingCamera.IsEmpty()) {
        prim.CreateRelationship(TfToken("crust:dicingCamera")).SetTargets({edits.dicingCamera});
    }

    // The bound AOVs as one product, each a var named after Hydra's AOV, so
    // the session renders them and `crust render` of the layer writes them.
    SdfPathVector vars;
    for (TfToken const &aov : edits.aovs) {
        TfToken source;
        SdfValueTypeName type;
        size_t components = 0;
        if (!HdCrustAovSource(aov, &source, &type, &components)) {
            continue;
        }
        SdfPath const path = HdCrustPaths::Root().AppendChild(TfToken("aovs")).AppendChild(
            TfToken(TfMakeValidIdentifier(aov.GetString())));
        UsdRenderVar var = UsdRenderVar::Define(stage, path);
        var.GetSourceNameAttr().Set(source.GetString());
        var.GetDataTypeAttr().Set(type.GetAsToken());
        var.GetPrim()
            .CreateAttribute(TfToken("driver:parameters:aov:name"), SdfValueTypeNames->String)
            .Set(aov.GetString());
        vars.push_back(path);
    }
    if (!vars.empty()) {
        UsdRenderProduct product = UsdRenderProduct::Define(stage, HdCrustPaths::Root().AppendChild(TfToken("aovs")));
        product.GetProductNameAttr().Set(TfToken("aovs.exr"));
        product.GetOrderedVarsRel().SetTargets(vars);
        settings.GetProductsRel().SetTargets({product.GetPath()});
    }

    SdfLayerRefPtr layer = SdfLayer::CreateAnonymous("edits.usda");
    layer->TransferContent(stage->GetRootLayer());
    if (edits.overs) {
        for (SdfPrimSpecHandle const &root : edits.overs->GetRootPrims()) {
            SdfCopySpec(edits.overs, root->GetPath(), layer, root->GetPath());
        }
    }
    layer->SetSubLayerPaths({"./scene.usdc"});
    layer->GetPseudoRoot()->SetInfo(UsdRenderTokens->renderSettingsPrimPath,
                                    VtValue(HdCrustPaths::Settings().GetString()));
    if (!layer->Export(path)) {
        if (error) {
            *error = "cannot write " + path;
        }
        return false;
    }
    return true;
}

PXR_NAMESPACE_CLOSE_SCOPE
