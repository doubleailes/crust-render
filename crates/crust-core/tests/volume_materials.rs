//! Volume materials, Typhoon's way: a USD `Material` whose `volume` terminal
//! (an inline MaterialX `ND_volume` over a VDF network) is the medium inside
//! the geometry it is bound to — under a MaterialX surface when the network
//! has one, behind a transparent medium boundary when it has none.

use crust_core::{PathSampler, Ray, SamplingStrategy, Scene, Vec3A, Volumes, ray_color};
use std::path::PathBuf;

fn load(name: &str, body: &str) -> Scene {
    let dir = std::env::temp_dir().join("crust_volume_material_tests");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path: PathBuf = dir.join(format!("{name}.usda"));
    std::fs::write(
        &path,
        format!(
            r#"#usda 1.0
(
    defaultPrim = "World"
    upAxis = "Y"
)

def Xform "World"
{{
{body}
}}
"#
        ),
    )
    .expect("write stage");
    Scene::from_usd(&path).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// A volume-only material over one VDF, bound to a unit sphere at the
/// origin, inside a uniform white dome of radiance 1.
fn fog_ball(name: &str, vdf: &str) -> Scene {
    load(
        name,
        &format!(
            r#"
    def Material "Fog"
    {{
        token outputs:mtlx:volume.connect = </World/Fog/Volume.outputs:out>
        def Shader "Volume"
        {{
            uniform token info:id = "ND_volume"
            token inputs:vdf.connect = </World/Fog/Vdf.outputs:out>
            token outputs:out
        }}
        def Shader "Vdf"
        {{
            {vdf}
            token outputs:out
        }}
    }}
    def Sphere "Ball" (prepend apiSchemas = ["MaterialBindingAPI"])
    {{
        double radius = 1
        rel material:binding = </World/Fog>
    }}
    def DomeLight "Sky"
    {{
        float inputs:intensity = 1
    }}
"#
        ),
    )
}

fn mean_radiance(scene: &Scene, ray: &Ray, n: i32, depth: i32) -> Vec3A {
    // No `crust:volume` regions in these stages: the media are materials.
    assert!(scene.volumes.is_empty());
    let volumes = Volumes::default();
    let mut sum = Vec3A::ZERO;
    for i in 0..n {
        sum += ray_color(
            ray,
            &scene.world,
            &scene.lights,
            &volumes,
            depth,
            SamplingStrategy::PowerMis,
            PathSampler::new(1, 2, 0, i),
        );
    }
    sum / n as f32
}

#[test]
fn a_volume_only_material_is_a_medium_boundary() {
    let scene = fog_ball(
        "boundary",
        r#"uniform token info:id = "ND_anisotropic_vdf"
            vector3f inputs:absorption = (0.1, 0.2, 0.3)
            vector3f inputs:scattering = (1, 2, 3)
            float inputs:anisotropy = 0.4"#,
    );
    assert!(scene.world.has_medium_boundaries());
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    let hit = scene.world.intersect(&ray, 1e-3, 1e4).expect("the ball");
    assert!(hit.mat.is_medium_boundary());
    let m = hit.mat.boundary_medium(&ray, &hit.rec).expect("a medium");
    assert_eq!(m.sigma_a, Vec3A::new(0.1, 0.2, 0.3));
    assert_eq!(m.sigma_s, Vec3A::new(1.0, 2.0, 3.0));
    assert_eq!(m.g, 0.4);
    // The boundary scatters nothing: nothing to sample, nothing to emit.
    assert_eq!(hit.mat.emitted_at(&ray, &hit.rec, 1.0), Vec3A::ZERO);
}

/// An absorbing interior seen against a white sky dims by Beer–Lambert over
/// the chord, exactly — and over the chord's *length*, whatever the ray's
/// parameterisation: a camera ray's direction is not unit.
#[test]
fn an_absorbing_boundary_dims_by_beer_lambert_over_the_chord() {
    let scene = fog_ball(
        "absorber",
        r#"uniform token info:id = "ND_absorption_vdf"
            vector3f inputs:absorption = (0.25, 0.5, 1)"#,
    );
    let expect = Vec3A::new((-0.5f32).exp(), (-1.0f32).exp(), (-2.0f32).exp());
    // The crossing restarts the ray the tracer's 0.001 short of the
    // boundary, so the medium starts that much early: σ·0.001 of optical
    // depth, at most 2e-3 here. A ray parameter read as a distance would be
    // off by a factor of ten.
    for len in [1.0, 10.0] {
        let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z * len);
        let got = mean_radiance(&scene, &ray, 4, 8);
        assert!(
            ((got / expect) - Vec3A::ONE).abs().max_element() < 2.5e-3,
            "direction length {len}: {got} vs {expect}"
        );
    }
    // A ray that misses the ball sees the sky whole.
    let miss = Ray::new(Vec3A::new(0.0, 3.0, -5.0), Vec3A::Z);
    assert!(
        (mean_radiance(&scene, &miss, 4, 8) - Vec3A::ONE)
            .abs()
            .max_element()
            < 1e-6
    );
}

/// A white furnace: a non-absorbing scattering interior under a uniform sky
/// of radiance 1 returns exactly 1 in expectation, however much it scatters.
/// Every piece of the transport is in this number — the crossing in and
/// out, the free flights, NEE at each scatter with its shadow ray leaving
/// through the boundary, and the bounce side's MIS against it — so an
/// unpaired weight shows as a mean away from 1.
#[test]
fn a_scattering_boundary_conserves_energy_in_a_white_furnace() {
    let scene = fog_ball(
        "furnace",
        r#"uniform token info:id = "ND_anisotropic_vdf"
            vector3f inputs:scattering = (1.5, 1.5, 1.5)
            float inputs:anisotropy = 0.3"#,
    );
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    let volumes = Volumes::default();
    // Each strategy alone and their MIS combination: all three estimate the
    // same integral.
    for strategy in [
        SamplingStrategy::PowerMis,
        SamplingStrategy::LightOnly,
        SamplingStrategy::BsdfOnly,
    ] {
        let n = 8192;
        let (mut sum, mut scattered) = (Vec3A::ZERO, 0);
        for i in 0..n {
            let l = ray_color(
                &ray,
                &scene.world,
                &scene.lights,
                &volumes,
                256,
                strategy,
                PathSampler::new(1, 2, 0, i),
            );
            sum += l;
            // A path that crossed without scattering sees the sky whole. So
            // does every BSDF-only path — with no absorption each event
            // weighs exactly 1 — but one that ran NEE inside does not.
            if (l - Vec3A::ONE).abs().max_element() > 1e-3 {
                scattered += 1;
            }
        }
        let mean = sum / n as f32;
        if strategy != SamplingStrategy::BsdfOnly {
            assert!(
                scattered > n / 2,
                "{strategy:?}: only {scattered} paths scattered"
            );
        }
        assert!(
            (mean - Vec3A::ONE).abs().max_element() < 0.025,
            "{strategy:?}: furnace mean {mean}"
        );
    }
}

/// With a surface, the volume terminal is the interior a refracted ray
/// carries — replacing the one OpenPBR's own `transmission_*` inputs
/// describe — and the material is no boundary.
#[test]
fn a_volume_terminal_is_the_interior_under_a_materialx_surface() {
    let scene = load(
        "glass",
        r#"
    def Material "Glass"
    {
        token outputs:mtlx:surface.connect = </World/Glass/Surface.outputs:out>
        token outputs:mtlx:volume.connect = </World/Glass/Volume.outputs:out>
        def Shader "Surface"
        {
            uniform token info:id = "ND_open_pbr_surface_surfaceshader"
            float inputs:transmission_weight = 1
            color3f inputs:transmission_color = (0.2, 0.2, 0.2)
            float inputs:transmission_depth = 1
            token outputs:out
        }
        def Shader "Volume"
        {
            uniform token info:id = "ND_volume"
            token inputs:vdf.connect = </World/Glass/Vdf.outputs:out>
            token outputs:out
        }
        def Shader "Vdf"
        {
            uniform token info:id = "ND_anisotropic_vdf"
            vector3f inputs:absorption = (0.5, 0.5, 0.5)
            vector3f inputs:scattering = (2, 2, 2)
            token outputs:out
        }
    }
    def Sphere "Ball" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        double radius = 1
        rel material:binding = </World/Glass>
    }
"#,
    );
    assert!(!scene.world.has_medium_boundaries());
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    let hit = scene.world.intersect(&ray, 1e-3, 1e4).expect("the ball");
    assert!(!hit.mat.is_medium_boundary());
    assert_eq!(hit.mat.kind(), "MaterialX");
    let inside = hit.mat.make_ray(&hit.rec, Vec3A::Z);
    let m = inside
        .medium()
        .expect("a refracted ray carries the interior");
    assert_eq!(m.sigma_a, Vec3A::splat(0.5));
    assert_eq!(m.sigma_s, Vec3A::splat(2.0));
}

/// An inline MaterialX surface with no volume shades as the surface alone;
/// before inline networks were read it fell back to grey OpenPBR.
#[test]
fn an_inline_materialx_surface_is_read() {
    let scene = load(
        "copper",
        r#"
    def Material "Copper"
    {
        token outputs:mtlx:surface.connect = </World/Copper/Surface.outputs:out>
        def Shader "Surface"
        {
            uniform token info:id = "ND_open_pbr_surface_surfaceshader"
            float inputs:base_metalness = 1
            token outputs:out
        }
    }
    def Sphere "Ball" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        double radius = 1
        rel material:binding = </World/Copper>
    }
"#,
    );
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    let hit = scene.world.intersect(&ray, 1e-3, 1e4).expect("the ball");
    assert_eq!(hit.mat.kind(), "MaterialX");
    assert!(!hit.mat.is_medium_boundary());
}

/// A preview surface beside a MaterialX surface keeps winning: `mtlx` is
/// the last surface context consulted, so a stage that rendered through its
/// `UsdPreviewSurface` renders as before.
#[test]
fn a_universal_preview_surface_outranks_an_inline_materialx_one() {
    let scene = load(
        "both",
        r#"
    def Material "Both"
    {
        token outputs:surface.connect = </World/Both/Preview.outputs:surface>
        token outputs:mtlx:surface.connect = </World/Both/Surface.outputs:out>
        def Shader "Preview"
        {
            uniform token info:id = "UsdPreviewSurface"
            color3f inputs:diffuseColor = (0.2, 0.4, 0.6)
            token outputs:surface
        }
        def Shader "Surface"
        {
            uniform token info:id = "ND_open_pbr_surface_surfaceshader"
            token outputs:out
        }
    }
    def Sphere "Ball" (prepend apiSchemas = ["MaterialBindingAPI"])
    {
        double radius = 1
        rel material:binding = </World/Both>
    }
"#,
    );
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -5.0), Vec3A::Z);
    let hit = scene.world.intersect(&ray, 1e-3, 1e4).expect("the ball");
    assert_ne!(hit.mat.kind(), "MaterialX");
}

/// The furnace again, with a white Lambertian ball *inside* the fog: a
/// surface vertex in an enclosure starts its shadow rays in the medium, and
/// the ray it scatters keeps travelling in it, whatever medium (none) the
/// surface's own ray carries. Albedo 1 everywhere, so the answer is still 1.
#[test]
fn a_white_surface_inside_the_fog_keeps_the_furnace_at_one() {
    let scene = load(
        "furnace_ball",
        r#"
    # Three top-level prims: from four on, the importer streams one stage
    # per prim, masked to it, and a binding into a sibling chunk does not
    # resolve there.
    def Scope "Looks"
    {
        def Material "Fog"
        {
            token outputs:mtlx:volume.connect = </World/Looks/Fog/Volume.outputs:out>
            def Shader "Volume"
            {
                uniform token info:id = "ND_volume"
                token inputs:vdf.connect = </World/Looks/Fog/Vdf.outputs:out>
                token outputs:out
            }
            def Shader "Vdf"
            {
                uniform token info:id = "ND_anisotropic_vdf"
                vector3f inputs:scattering = (0.8, 0.8, 0.8)
                float inputs:anisotropy = -0.2
                token outputs:out
            }
        }
        def Material "White"
        {
            token outputs:mtlx:surface.connect = </World/Looks/White/Surface.outputs:out>
            def Shader "Surface"
            {
                uniform token info:id = "ND_surface"
                token inputs:bsdf.connect = </World/Looks/White/Diffuse.outputs:out>
                token outputs:out
            }
            def Shader "Diffuse"
            {
                uniform token info:id = "ND_oren_nayar_diffuse_bsdf"
                color3f inputs:color = (1, 1, 1)
                token outputs:out
            }
        }
    }
    def Xform "Geo"
    {
        def Sphere "Haze" (prepend apiSchemas = ["MaterialBindingAPI"])
        {
            double radius = 2
            rel material:binding = </World/Looks/Fog>
        }
        def Sphere "Ball" (prepend apiSchemas = ["MaterialBindingAPI"])
        {
            double radius = 0.6
            rel material:binding = </World/Looks/White>
        }
    }
    def DomeLight "Sky"
    {
        float inputs:intensity = 1
    }
"#,
    );
    let ray = Ray::new(Vec3A::new(0.0, 0.0, -6.0), Vec3A::Z);
    let got = mean_radiance(&scene, &ray, 16384, 512);
    assert!(
        (got - Vec3A::ONE).abs().max_element() < 0.025,
        "furnace mean {got}"
    );
}
