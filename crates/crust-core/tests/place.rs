//! Placement (`crust_core::place`) and the world queries it rests on.

use crust_core::Scene;
use crust_core::rt::{Piece, RayMask};
use glam::{DMat4, DVec3, Vec3A};
use std::path::{Path, PathBuf};

fn work_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("crust_place").join(name);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    dir.canonicalize().expect("canonical")
}

fn write(path: &Path, text: &str) {
    std::fs::write(path, text).expect("write layer");
}

/// A unit quad in the y = 0 plane, as a child `geo` of the prim it is in.
const QUAD: &str = r#"
        def Mesh "geo"
        {
            int[] faceVertexCounts = [4]
            int[] faceVertexIndices = [0, 1, 2, 3]
            point3f[] points = [(0, 0, 0), (1, 0, 0), (1, 0, 1), (0, 0, 1)]
        }
"#;
const QUAD_POINTS: [[f64; 3]; 4] = [
    [0.0, 0.0, 0.0],
    [1.0, 0.0, 0.0],
    [1.0, 0.0, 1.0],
    [0.0, 0.0, 1.0],
];

fn sample(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../samples")
        .join(name)
}

/// The geometric hit placement casts lands where the camera ray `probe`
/// casts does, and the triangle it reports is one `visit` hands out for that
/// geometry, in world space.
#[test]
fn geometric_hits_agree_with_the_shading_hit_and_visit() {
    let scene = Scene::from_usd(&sample("cornellbox.usda")).expect("cornellbox.usda");
    let (w, h) = scene.settings.get_dimensions();
    let mut hits = 0;
    for (x, y) in [
        (w / 2, h / 2),
        (w / 4, h / 3),
        (3 * w / 4, 2 * h / 3),
        (w / 2, 3 * h / 4),
    ] {
        let s = (x as f32 + 0.5) / w as f32;
        let t = ((h - 1 - y) as f32 + 0.5) / h as f32;
        let ray = scene.camera.get_ray(s, t, [0.5, 0.5], 0.0);
        let shaded = scene
            .world
            .intersect(&ray, crust_core::TRACE_T_MIN, f32::INFINITY);
        let rt = ray.rt().with_mask(RayMask::CAMERA);
        let geometric = scene.world.intersect_geometry(&rt);
        let ids = (
            shaded.as_ref().map(|h| h.geom_id),
            geometric.map(|h| h.geom_id),
        );
        let (Some(shaded), Some(geometric)) = (shaded, geometric) else {
            panic!("pixel ({x}, {y}): shaded, geometric hit {ids:?}");
        };
        hits += 1;
        assert_eq!(shaded.geom_id, geometric.geom_id);
        assert_eq!(shaded.prim_id, geometric.prim_id);
        assert_eq!(shaded.rec.t, geometric.t);
        assert!(geometric.normal.dot(rt.dir) < 0.0, "turned against the ray");

        // The hit point lies on one of the visited triangles of that geometry.
        let p = rt.at(geometric.t);
        let mut on = false;
        scene.world.visit(
            geometric.geom_id..geometric.geom_id + 1,
            RayMask::CAMERA,
            |_, piece| {
                if let Piece::Triangle([a, b, c]) = piece {
                    let n = (b - a).cross(c - a);
                    let inside = [(a, b), (b, c), (c, a)]
                        .iter()
                        .all(|&(u, v)| (v - u).cross(p - u).dot(n) >= -1e-6 * n.length_squared());
                    on |= inside && (p - a).dot(n.normalize()).abs() < 1e-3;
                }
            },
        );
        assert!(
            on,
            "pixel ({x}, {y}): hit point {p:?} on no visited triangle"
        );
    }
    assert_eq!(hits, 4);
}

/// `geometry_of` is `prim_of_geometry` turned around: every id a prim emitted
/// names that prim, a prim's ranges hold exactly its own and its
/// descendants' runs, paths match by component, and instances and
/// PointInstancers answer under the instance or instancer prim.
#[test]
fn geometry_of_turns_prim_of_geometry_around() {
    for name in [
        "cornellbox.usda",
        "instancing.usda",
        "nested_instancing.usda",
        "subdivision_adaptive.usda",
    ] {
        let scene = Scene::from_usd(&sample(name)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let prov = scene
            .provenance
            .as_ref()
            .expect("a USD import records provenance");
        let all = prov.geometry_of("/");
        assert!(!all.is_empty(), "{name}: no geometry");
        let mut previous_end = 0;
        for ids in &all {
            assert!(
                ids.start >= previous_end,
                "{name}: runs overlap or are unsorted"
            );
            previous_end = ids.end;
            assert!(
                ids.end as usize <= scene.world.count(),
                "{name}: {ids:?} past the world"
            );
            let prim = prov
                .prim_of_geometry(ids.start)
                .expect("an emitted id names its prim");
            for id in ids.clone() {
                assert_eq!(prov.prim_of_geometry(id), Some(prim), "{name}: id {id}");
            }
            assert!(
                prov.geometry_of(prim).contains(ids),
                "{name}: {prim} lost {ids:?}"
            );
        }
    }

    let scene = Scene::from_usd(&sample("instancing.usda")).expect("instancing.usda");
    let prov = scene.provenance.as_ref().expect("provenance");
    let scatter = prov.geometry_of("/World/Scatter");
    assert!(!scatter.is_empty(), "the instancer covers its placements");
    assert!(
        prov.geometry_of("/World/Scatter/Protos/Gem").is_empty(),
        "a prototype is drawn only through its instancer"
    );
    for tower in ["/World/TowerA", "/World/TowerB", "/World/TowerC"] {
        assert_eq!(
            prov.geometry_of(tower).len(),
            1,
            "{tower}: one run per instance"
        );
    }
    assert!(
        prov.geometry_of("/World/Tower").is_empty(),
        "no component prefix match"
    );
    let world = prov.geometry_of("/World");
    let n = |r: &[std::ops::Range<u32>]| r.iter().map(|r| r.len()).sum::<usize>();
    assert!(
        n(&world) >= n(&scatter) + 3,
        "an ancestor holds its descendants' runs"
    );
}

/// `prim_frames` reads each prim as the import composed it: the ops a payload
/// authors, a pivot with `!invert!`, a reset, and the placement op's value;
/// its `local_to_world` is where the renderer put the prim's geometry.
#[test]
fn prim_frames_are_the_import_s_transforms() {
    let dir = work_dir("frames");
    write(
        &dir.join("asset.usda"),
        &format!(
            r#"#usda 1.0
(
    defaultPrim = "Rock"
)

def Xform "Rock"
{{
    double3 xformOp:translate = (1, 2, 3)
    float xformOp:rotateY = 90
    uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:rotateY"]
{QUAD}
}}
"#
        ),
    );
    let shot = dir.join("shot.usda");
    write(
        &shot,
        &format!(
            r#"#usda 1.0

def Xform "World"
{{
    double3 xformOp:translate = (10, 0, 0)
    float3 xformOp:scale = (2, 1, 3)
    uniform token[] xformOpOrder = ["xformOp:translate", "xformOp:scale"]

    def Xform "rock" (
        prepend payload = @./asset.usda@
    )
    {{
    }}

    def Xform "pivoted"
    {{
        double3 xformOp:translate:pivot = (0.5, 0, 0.5)
        float xformOp:rotateY = 30
        matrix4d xformOp:transform:crustPlace = ( (1, 0, 0, 0), (0, 1, 0, 0), (0, 0, 1, 0), (0, 4, 0, 1) )
        uniform token[] xformOpOrder = ["xformOp:transform:crustPlace", "xformOp:translate:pivot", "xformOp:rotateY", "!invert!xformOp:translate:pivot"]
{QUAD}
    }}

    def Xform "reset"
    {{
        double3 xformOp:translate = (0, 5, 0)
        uniform token[] xformOpOrder = ["!resetXformStack!", "xformOp:translate"]
{QUAD}
    }}

    def Scope "scope"
    {{
{QUAD}
    }}
}}
"#
        ),
    );
    let scene = Scene::from_usd(&shot).expect("shot.usda");
    let prov = scene.provenance.as_ref().expect("provenance");
    let paths: Vec<String> = [
        "/World/rock",
        "/World/pivoted",
        "/World/reset",
        "/World/scope",
    ]
    .map(String::from)
    .into();
    let frames = prov
        .prim_frames(&shot, &paths, "xformOp:transform:crustPlace")
        .expect("frames");
    let world = DMat4::from_scale_rotation_translation(
        DVec3::new(2.0, 1.0, 3.0),
        glam::DQuat::IDENTITY,
        DVec3::new(10.0, 0.0, 0.0),
    );

    let rock = &frames[0];
    assert_eq!(
        rock.xform_op_order,
        ["xformOp:translate", "xformOp:rotateY"],
        "the payload's ops"
    );
    assert!(rock.xformable && !rock.resets_xform_stack && rock.place_op.is_none());
    assert!(rock.parent_to_world.abs_diff_eq(world, 1e-6));

    let pivoted = &frames[1];
    assert_eq!(pivoted.xform_op_order.len(), 4);
    assert!(
        pivoted
            .place_op
            .expect("authored")
            .abs_diff_eq(DMat4::from_translation(DVec3::new(0.0, 4.0, 0.0)), 1e-12)
    );

    let reset = &frames[2];
    assert!(reset.resets_xform_stack);
    assert!(
        reset
            .local_to_world
            .abs_diff_eq(DMat4::from_translation(DVec3::new(0.0, 5.0, 0.0)), 1e-6)
    );

    let scope = &frames[3];
    assert!(!scope.xformable, "a Scope holds no transform");
    assert!(scope.local_to_world.abs_diff_eq(world, 1e-6));

    // Each prim's `local_to_world` takes its quad's points to where the
    // renderer put them.
    for frame in &frames {
        let geo = format!("{}/geo", frame.path);
        let mut rendered = Vec::new();
        for ids in prov.geometry_of(&geo) {
            scene.world.visit(ids, RayMask::CAMERA, |_, piece| {
                if let Piece::Triangle(vs) = piece {
                    rendered.extend(vs);
                }
            });
        }
        assert!(!rendered.is_empty(), "{geo}: no geometry");
        let expected: Vec<Vec3A> = QUAD_POINTS
            .iter()
            .map(|p| {
                frame
                    .local_to_world
                    .transform_point3(DVec3::from_array(*p))
                    .as_vec3()
                    .into()
            })
            .collect();
        for v in rendered {
            assert!(
                expected.iter().any(|e| e.abs_diff_eq(v, 1e-4)),
                "{geo}: rendered vertex {v:?} is none of {expected:?}"
            );
        }
    }

    assert!(
        prov.prim_frames(&shot, &["/World/nope".into()], "x")
            .is_err()
    );
}

// ---------------------------------------------------------------------
// Placement
// ---------------------------------------------------------------------

use crust_core::place::{self, Amount, Dropped, ScatterRequest, UpAxis};

/// A mesh prim from points and faces, under an Xform `name` translated by
/// `at`.
fn mesh(name: &str, at: [f64; 3], points: &[[f64; 3]], faces: &[&[u32]]) -> String {
    let pts: Vec<String> = points
        .iter()
        .map(|p| format!("({}, {}, {})", p[0], p[1], p[2]))
        .collect();
    let counts: Vec<String> = faces.iter().map(|f| f.len().to_string()).collect();
    let idx: Vec<String> = faces
        .iter()
        .flat_map(|f| f.iter().map(|i| i.to_string()))
        .collect();
    format!(
        r#"
    def Xform "{name}"
    {{
        double3 xformOp:translate = ({}, {}, {})
        uniform token[] xformOpOrder = ["xformOp:translate"]
        def Mesh "geo"
        {{
            int[] faceVertexCounts = [{}]
            int[] faceVertexIndices = [{}]
            point3f[] points = [{}]
        }}
    }}
"#,
        at[0],
        at[1],
        at[2],
        counts.join(", "),
        idx.join(", "),
        pts.join(", ")
    )
}

/// A cube of half-side `s` centred on its prim's origin.
fn cube(name: &str, at: [f64; 3], s: f64) -> String {
    let p = |x: f64, y: f64, z: f64| [x * s, y * s, z * s];
    let points = [
        p(-1., -1., -1.),
        p(1., -1., -1.),
        p(1., 1., -1.),
        p(-1., 1., -1.),
        p(-1., -1., 1.),
        p(1., -1., 1.),
        p(1., 1., 1.),
        p(-1., 1., 1.),
    ];
    let faces: [&[u32]; 6] = [
        &[0, 3, 2, 1],
        &[4, 5, 6, 7],
        &[0, 1, 5, 4],
        &[2, 3, 7, 6],
        &[1, 2, 6, 5],
        &[0, 4, 7, 3],
    ];
    mesh(name, at, &points, &faces)
}

/// A flat, upward-facing quad (y = 0) from `lo` to `hi` in x and z.
fn plate(name: &str, at: [f64; 3], lo: [f64; 2], hi: [f64; 2]) -> String {
    let points = [
        [lo[0], 0., lo[1]],
        [lo[0], 0., hi[1]],
        [hi[0], 0., hi[1]],
        [hi[0], 0., lo[1]],
    ];
    mesh(name, at, &points, &[&[0, 1, 2, 3]])
}

/// The drop stage: a floor, a table with a shelf above it, a bumpy terrain,
/// a slope of 30° about X, and prims to drop.
fn drop_stage(dir: &Path) -> PathBuf {
    let n = 12;
    let mut terrain_pts = Vec::new();
    for j in 0..=n {
        for i in 0..=n {
            let (x, z) = (4.0 * i as f64 / n as f64, 4.0 * j as f64 / n as f64 - 2.0);
            let y = 0.5 + 0.3 * (2.0 * x).sin() * (1.5 * z).cos();
            terrain_pts.push([x, y, z]);
        }
    }
    let mut terrain_faces: Vec<Vec<u32>> = Vec::new();
    for j in 0..n {
        for i in 0..n {
            let k = (j * (n + 1) + i) as u32;
            let w = (n + 1) as u32;
            terrain_faces.push(vec![k, k + w, k + w + 1, k + 1]);
        }
    }
    let faces: Vec<&[u32]> = terrain_faces.iter().map(|f| f.as_slice()).collect();
    let (s, c) = (30f64.to_radians().sin(), 30f64.to_radians().cos());
    // The slope's corners: (±2, 0, 0) ± 2 (0, -sin, cos), about (-8, 1, 0).
    let slope = [
        [-2.0, 2.0 * s, -2.0 * c],
        [-2.0, -2.0 * s, 2.0 * c],
        [2.0, -2.0 * s, 2.0 * c],
        [2.0, 2.0 * s, -2.0 * c],
    ];
    let stage = dir.join("drop.usda");
    let mut text =
        String::from("#usda 1.0\n(\n    upAxis = \"Y\"\n)\n\nclass Xform \"_Cube\"\n{\n");
    text += &cube("body", [0., 0., 0.], 0.25);
    text += "}\n\ndef Xform \"World\"\n{\n";
    text += &plate("floor", [0., 0., 0.], [-20., -20.], [20., 20.]);
    text += &plate("table", [0., 1., 0.], [-1., -1.], [1., 1.]);
    text += &plate("shelf", [0., 2., 0.], [-0.5, -0.5], [0.5, 0.5]);
    text += &mesh("terrain", [6., 0., 0.], &terrain_pts, &faces);
    text += &mesh("slope", [-8., 1., 0.], &slope, &[&[0, 1, 2, 3]]);
    text += &cube("onFloor", [3., 2.25, -5.], 0.25);
    text += &cube("sunk", [-3., 0., -5.], 0.25);
    text += &cube("cup", [0., 3.1, 0.], 0.1);
    text += &cube("rock", [8., 3., 0.3], 0.3);
    text += &cube("box", [-8., 3., 0.], 0.25);
    text += &cube("away", [30., 3., 0.], 0.25);
    text += r#"
    def Xform "empty"
    {
    }

    def Xform "inst" (
        instanceable = true
        references = </_Cube>
    )
    {
        double3 xformOp:translate = (12, 2, 0)
        uniform token[] xformOpOrder = ["xformOp:translate"]
    }
}
"#;
    write(&stage, &text);
    stage
}

/// The world-space vertices of `prim`'s geometry.
fn vertices(scene: &Scene, prim: &str) -> Vec<DVec3> {
    let prov = scene.provenance.as_ref().expect("provenance");
    let mut out = Vec::new();
    for ids in prov.geometry_of(prim) {
        scene.world.visit(ids, RayMask::CAMERA, |_, piece| {
            if let Piece::Triangle(vs) = piece {
                out.extend(vs.map(|v| v.as_dvec3()));
            }
        });
    }
    out
}

fn placed(d: &Dropped) -> (DMat4, DVec3, Option<DVec3>) {
    match d {
        Dropped::Placed {
            delta,
            contact,
            normal,
        } => (*delta, *contact, *normal),
        other => panic!("not placed: {other:?}"),
    }
}

/// The height of the surface of `onto`'s geometry straight below `p`.
fn surface_below(scene: &Scene, onto: &str, p: DVec3) -> Option<f64> {
    let prov = scene.provenance.as_ref().expect("provenance");
    let ids = prov.geometry_of(onto);
    let origin = Vec3A::new(p.x as f32, 100.0, p.z as f32);
    let accept = |id: u32| ids.iter().any(|r| r.contains(&id));
    place::first_hit(&scene.world, origin, -Vec3A::Y, &accept).map(|f| f.point.y as f64)
}

#[test]
fn drops_rest_prims_on_the_surface_below() {
    let dir = work_dir("drop");
    let stage = drop_stage(&dir);
    let scene = Scene::from_usd(&stage).expect("drop.usda");
    let prov = scene.provenance.as_ref().expect("provenance");
    let names = ["onFloor", "sunk", "cup", "rock", "empty", "away", "inst"];
    let prims: Vec<String> = names.iter().map(|n| format!("/World/{n}")).collect();
    let dropped = place::drop_prims(&scene.world, prov, &prims, None, UpAxis::Y, false);

    // Lowest vertex after the move, per prim.
    let lowest = |prim: &str, delta: DMat4| {
        vertices(&scene, prim)
            .iter()
            .map(|v| delta.transform_point3(*v).y)
            .fold(f64::INFINITY, f64::min)
    };
    let tol = 1e-4;

    // A flat floor: the cube's bottom on y = 0, moved along y only.
    let (delta, _, normal) = placed(&dropped[0]);
    assert!(normal.is_none());
    assert!(lowest("/World/onFloor", delta).abs() < tol);
    let t = delta.w_axis.truncate();
    assert!(
        t.x.abs() < 1e-12 && t.z.abs() < 1e-12 && (t.y + 2.0).abs() < tol,
        "{t:?}"
    );

    // A sunk cube is raised.
    let (delta, _, _) = placed(&dropped[1]);
    assert!(delta.w_axis.y > 0.2 && lowest("/World/sunk", delta).abs() < tol);

    // The cup lands on the shelf, the highest surface beneath it.
    let (delta, contact, _) = placed(&dropped[2]);
    assert!((lowest("/World/cup", delta) - 2.0).abs() < tol);
    assert!((contact.y - 2.0).abs() < tol);

    // On uneven terrain: no vertex below it, and one on it.
    let (delta, _, _) = placed(&dropped[3]);
    let gaps: Vec<f64> = vertices(&scene, "/World/rock")
        .iter()
        .map(|v| delta.transform_point3(*v))
        .filter_map(|v| Some(v.y - surface_below(&scene, "/World/terrain", v)?))
        .collect();
    assert!(!gaps.is_empty());
    assert!(
        gaps.iter().all(|g| *g > -tol),
        "a vertex below the terrain: {gaps:?}"
    );
    assert!(
        gaps.iter().cloned().fold(f64::INFINITY, f64::min) < tol,
        "{gaps:?}"
    );

    assert_eq!(dropped[4], Dropped::NoGeometry);
    assert_eq!(dropped[5], Dropped::NothingBelow);

    // A native instance drops by its instance prim, onto the floor.
    let (delta, _, _) = placed(&dropped[6]);
    assert!(lowest("/World/inst", delta).abs() < tol);

    // Onto the target only: through the shelf, onto the table.
    let onto_table = place::drop_prims(
        &scene.world,
        prov,
        &["/World/cup".into()],
        Some(&["/World/table".into()]),
        UpAxis::Y,
        false,
    );
    let (delta, _, _) = placed(&onto_table[0]);
    assert!((lowest("/World/cup", delta) - 1.0).abs() < tol);

    // The same on one thread as on many.
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("pool")
        .install(|| place::drop_prims(&scene.world, prov, &prims, None, UpAxis::Y, false));
    assert_eq!(one, dropped);
}

#[test]
fn an_aligned_drop_follows_the_slope() {
    let dir = work_dir("align");
    let stage = drop_stage(&dir);
    let scene = Scene::from_usd(&stage).expect("drop.usda");
    let prov = scene.provenance.as_ref().expect("provenance");
    let dropped = place::drop_prims(
        &scene.world,
        prov,
        &["/World/box".into()],
        None,
        UpAxis::Y,
        true,
    );
    let (delta, _, normal) = placed(&dropped[0]);
    let (s, c) = (30f64.to_radians().sin(), 30f64.to_radians().cos());
    let n = DVec3::new(0.0, c, s);
    assert!(normal.expect("aligned").abs_diff_eq(n, 1e-4), "{normal:?}");
    // The box's up axis is the slope's normal.
    assert!(
        delta
            .transform_vector3(DVec3::Y)
            .normalize()
            .abs_diff_eq(n, 1e-4)
    );
    // Its lowest vertices rest on the slope, none below.
    let centre = DVec3::new(-8.0, 1.0, 0.0);
    let heights: Vec<f64> = vertices(&scene, "/World/box")
        .iter()
        .map(|v| (delta.transform_point3(*v) - centre).dot(n))
        .collect();
    let min = heights.iter().cloned().fold(f64::INFINITY, f64::min);
    assert!(min.abs() < 1e-4, "{heights:?}");
}

/// Two separate triangles of areas 1 and 3 in y = 0, a flat quad facing up,
/// and a vertical one.
fn scatter_stage(dir: &Path) -> PathBuf {
    let stage = dir.join("scatter.usda");
    let mut text = String::from("#usda 1.0\n\ndef Xform \"World\"\n{\n");
    text += &mesh(
        "pair",
        [0., 0., 0.],
        &[
            [0., 0., 0.],
            [0., 0., 1.],
            [2., 0., 0.],
            [10., 0., 0.],
            [10., 0., 1.],
            [16., 0., 0.],
        ],
        &[&[0, 1, 2], &[3, 4, 5]],
    );
    text += &mesh(
        "steps",
        [0., 0., 5.],
        &[
            [0., 0., 0.],
            [0., 0., 1.],
            [1., 0., 1.],
            [1., 0., 0.],
            [1., 1., 0.],
            [1., 1., 1.],
        ],
        &[&[0, 1, 2, 3], &[3, 2, 5, 4]],
    );
    text += "}\n";
    write(&stage, &text);
    stage
}

fn request(target: &str, amount: Amount, seed: u32) -> ScatterRequest {
    ScatterRequest {
        target: vec![target.into()],
        amount,
        seed,
        max_slope_deg: None,
        random_rotation: true,
        scale: Some([0.5, 2.0]),
        prototypes: 2,
        up: UpAxis::Y,
    }
}

#[test]
fn scatter_follows_area_and_the_seed() {
    let dir = work_dir("scatter");
    let stage = scatter_stage(&dir);
    let scene = Scene::from_usd(&stage).expect("scatter.usda");
    let prov = scene.provenance.as_ref().expect("provenance");

    let req = request("/World/pair", Amount::Count(10_000), 7);
    let s = place::scatter(&scene.world, prov, &req).expect("scatter");
    assert!((s.area - 4.0).abs() < 1e-5, "{}", s.area);
    assert_eq!(s.instances.len(), 10_000);
    let on_small = s.instances.iter().filter(|i| i.position.x < 5.0).count();
    assert!(
        (on_small as f64 / 10_000.0 - 0.25).abs() < 0.005,
        "{on_small} of 10000 on the triangle of area 1"
    );
    for i in &s.instances {
        let p = i.position;
        assert!(p.y.abs() < 1e-4, "{p:?} off the plane");
        let (x0, w) = if p.x < 5.0 { (0.0, 2.0) } else { (10.0, 6.0) };
        // Inside the right triangle with legs w along x and 1 along z.
        let (u, v) = ((p.x - x0) / w, p.z);
        assert!(
            u > -1e-4 && v > -1e-4 && u + v < 1.0 + 1e-4,
            "{p:?} outside its triangle"
        );
        assert!((0.5..=2.0).contains(&i.scale));
        assert!((0.0..std::f64::consts::TAU).contains(&i.angle));
        assert!(i.prototype < 2);
    }
    assert!(
        s.instances.iter().any(|i| i.prototype == 0)
            && s.instances.iter().any(|i| i.prototype == 1)
    );

    // The same on one thread, and another seed differs.
    let one = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .expect("pool")
        .install(|| place::scatter(&scene.world, prov, &req).expect("scatter"));
    assert_eq!(one, s);
    let other = place::scatter(
        &scene.world,
        prov,
        &request("/World/pair", Amount::Count(10_000), 8),
    )
    .expect("scatter");
    assert_ne!(other.instances[0].position, s.instances[0].position);

    // A density over the eligible area.
    let d = place::scatter(
        &scene.world,
        prov,
        &request("/World/pair", Amount::Density(2.5), 1),
    )
    .expect("scatter");
    assert_eq!(d.instances.len(), 10);

    // Steep faces stay bare.
    let mut steep = request("/World/steps", Amount::Count(500), 3);
    steep.max_slope_deg = Some(30.0);
    let s = place::scatter(&scene.world, prov, &steep).expect("scatter");
    assert!(
        (s.area - 1.0).abs() < 1e-5,
        "only the flat quad: {}",
        s.area
    );
    assert!(s.instances.iter().all(|i| i.position.y.abs() < 1e-4));

    // Refusals.
    assert!(
        place::scatter(
            &scene.world,
            prov,
            &request("/World/none", Amount::Count(1), 0)
        )
        .is_err()
    );
    assert!(
        place::scatter(
            &scene.world,
            prov,
            &request("/World/pair", Amount::Count(100_001), 0)
        )
        .is_err()
    );
    assert!(
        place::scatter(
            &scene.world,
            prov,
            &request("/World/pair", Amount::Density(0.01), 0)
        )
        .is_err()
    );
}
