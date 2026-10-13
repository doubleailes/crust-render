// Prints what OpenUSD's GeomUtil mesh generators produce for a list of
// implicit-shape cases, for `crates/crust-core/tests/geomutil_oracle.rs`.
//
// Built and run by `scripts/geomutil_oracle.py` against a pinned conda-forge
// `openusd`. Each stdin line is one case:
//
//   <id> cube     <size>
//   <id> plane    <width> <length> <axis>
//   <id> cylinder <N> <radiusBottom> <radiusTop> <height> <axis>
//   <id> cone     <N> <radius> <height> <axis>
//   <id> capsule  <N> <M> <radiusBottom> <radiusTop> <height> <axis>
//
// and each answer is one line:
//
//   <id> | <scheme> | <counts> | <indices> | <points> | <interpolation> | <normals>
//
// Points and normals are written as Storm writes them: into a VtVec3fArray
// (so the generators run their float instantiation), through the frame of
// UsdImagingGprimAdapter::GetImplicitBasis(axis) for every shape with an
// `axis`, and with no frame for the cube, as Hydra's implicit-surface scene
// index calls them. `%.9g` round-trips a float exactly.

#include <pxr/base/gf/matrix4d.h>
#include <pxr/base/vt/types.h>
#include <pxr/imaging/geomUtil/capsuleMeshGenerator.h>
#include <pxr/imaging/geomUtil/coneMeshGenerator.h>
#include <pxr/imaging/geomUtil/cuboidMeshGenerator.h>
#include <pxr/imaging/geomUtil/cylinderMeshGenerator.h>
#include <pxr/imaging/geomUtil/planeMeshGenerator.h>
#include <pxr/imaging/pxOsd/meshTopology.h>
#include <pxr/usd/usdGeom/tokens.h>
#include <pxr/usdImaging/usdImaging/gprimAdapter.h>

#include <cstdio>
#include <iostream>
#include <sstream>
#include <string>

PXR_NAMESPACE_USING_DIRECTIVE

namespace {

TfToken axisToken(const std::string& a)
{
    if (a == "X") return UsdGeomTokens->x;
    if (a == "Y") return UsdGeomTokens->y;
    return UsdGeomTokens->z;
}

void writeVecs(std::ostringstream& out, const VtVec3fArray& v)
{
    char buf[64];
    bool first = true;
    for (const GfVec3f& p : v) {
        for (int i = 0; i < 3; ++i) {
            std::snprintf(buf, sizeof buf, "%s%.9g", first ? "" : " ", p[i]);
            out << buf;
            first = false;
        }
    }
}

void writeInts(std::ostringstream& out, const VtIntArray& v)
{
    bool first = true;
    for (int i : v) {
        out << (first ? "" : " ") << i;
        first = false;
    }
}

std::string answer(
    const std::string& id,
    const PxOsdMeshTopology& topo,
    const VtVec3fArray& points,
    const TfToken& interpolation,
    const VtVec3fArray& normals)
{
    std::ostringstream out;
    out << id << " | " << topo.GetScheme().GetString() << " | ";
    writeInts(out, topo.GetFaceVertexCounts());
    out << " | ";
    writeInts(out, topo.GetFaceVertexIndices());
    out << " | ";
    writeVecs(out, points);
    out << " | " << interpolation.GetString() << " | ";
    writeVecs(out, normals);
    return out.str();
}

}  // namespace

int main()
{
    std::string line;
    while (std::getline(std::cin, line)) {
        if (line.empty()) continue;
        std::istringstream in(line);
        std::string id, family;
        in >> id >> family;
        if (family == "cube") {
            double size;
            in >> size;
            using G = GeomUtilCuboidMeshGenerator;
            VtVec3fArray pts(G::ComputeNumPoints());
            G::GeneratePoints(pts.begin(), size, size, size);
            VtVec3fArray nrm(G::ComputeNumNormals());
            G::GenerateNormals(nrm.begin());
            std::cout << answer(id, G::GenerateTopology(), pts,
                                G::GetNormalsInterpolation(), nrm) << "\n";
        } else if (family == "plane") {
            double width, length;
            std::string axis;
            in >> width >> length >> axis;
            const GfMatrix4d basis =
                UsdImagingGprimAdapter::GetImplicitBasis(axisToken(axis));
            using G = GeomUtilPlaneMeshGenerator;
            VtVec3fArray pts(G::ComputeNumPoints());
            G::GeneratePoints(pts.begin(), width, length, &basis);
            VtVec3fArray nrm(G::ComputeNumNormals());
            G::GenerateNormals(nrm.begin(), &basis);
            std::cout << answer(id, G::GenerateTopology(), pts,
                                G::GetNormalsInterpolation(), nrm) << "\n";
        } else if (family == "cylinder") {
            size_t n;
            double rb, rt, h;
            std::string axis;
            in >> n >> rb >> rt >> h >> axis;
            const GfMatrix4d basis =
                UsdImagingGprimAdapter::GetImplicitBasis(axisToken(axis));
            using G = GeomUtilCylinderMeshGenerator;
            VtVec3fArray pts(G::ComputeNumPoints(n));
            G::GeneratePoints(pts.begin(), n, rb, rt, h, &basis);
            VtVec3fArray nrm(G::ComputeNumNormals(n));
            G::GenerateNormals(nrm.begin(), n, rb, rt, h, &basis);
            std::cout << answer(id, G::GenerateTopology(n), pts,
                                G::GetNormalsInterpolation(), nrm) << "\n";
        } else if (family == "cone") {
            size_t n;
            double r, h;
            std::string axis;
            in >> n >> r >> h >> axis;
            const GfMatrix4d basis =
                UsdImagingGprimAdapter::GetImplicitBasis(axisToken(axis));
            using G = GeomUtilConeMeshGenerator;
            VtVec3fArray pts(G::ComputeNumPoints(n));
            G::GeneratePoints(pts.begin(), n, r, h, &basis);
            VtVec3fArray nrm(G::ComputeNumNormals(n));
            G::GenerateNormals(nrm.begin(), n, r, h, &basis);
            std::cout << answer(id, G::GenerateTopology(n), pts,
                                G::GetNormalsInterpolation(), nrm) << "\n";
        } else if (family == "capsule") {
            size_t n, m;
            double rb, rt, h;
            std::string axis;
            in >> n >> m >> rb >> rt >> h >> axis;
            const GfMatrix4d basis =
                UsdImagingGprimAdapter::GetImplicitBasis(axisToken(axis));
            using G = GeomUtilCapsuleMeshGenerator;
            VtVec3fArray pts(G::ComputeNumPoints(n, m));
            G::GeneratePoints(pts.begin(), n, m, rb, rt, h, &basis);
            VtVec3fArray nrm(G::ComputeNumNormals(n, m));
            G::GenerateNormals(nrm.begin(), n, m, rb, rt, h, &basis);
            std::cout << answer(id, G::GenerateTopology(n, m), pts,
                                G::GetNormalsInterpolation(), nrm) << "\n";
        } else {
            std::cerr << "unknown family '" << family << "' in case " << id << "\n";
            return 1;
        }
    }
    return 0;
}
