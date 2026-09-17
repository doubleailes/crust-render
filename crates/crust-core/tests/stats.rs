//! Render statistics: phase recording, the counters, their conversions
//! from kernel and settings types, and the formatted report.

use crust_core::rt::{MemoryFootprint, PrimitiveBreakdown};
use crust_core::{
    ImageCounters, MemorySample, Phase, PrimitiveCounts, RayStats, RenderSettings, RenderStats,
    SceneCounters, peak_memory_bytes,
};
use std::time::Duration;

#[test]
fn a_new_report_is_empty_but_well_formed() {
    let s = RenderStats::new();
    assert!(s.phases.is_empty());
    assert_eq!(s.total(), Duration::ZERO);
    let out = s.report();
    assert!(out.contains("Render Statistics"));
    assert!(
        !out.contains("Profile by execution tree"),
        "no phases, no profile"
    );
    // `Display` and `report` are the same formatter (memory is sampled
    // live, so only the fixed part is compared).
    assert!(
        s.to_string()
            .starts_with(&out[..out.find("peak memory").unwrap_or(out.len())])
    );
}

#[test]
fn record_appends_phases_in_order_with_their_depth() {
    let mut s = RenderStats::new();
    s.record("Parse", 0, Duration::from_millis(100));
    s.record("Open", 1, Duration::from_millis(40));
    s.record("Render", 0, Duration::from_millis(900));
    assert_eq!(s.phases.len(), 3);
    assert_eq!(s.phases[0].name, "Parse");
    assert_eq!(s.phases[1].depth, 1);
    assert_eq!(s.phases[2].duration, Duration::from_millis(900));
    assert_eq!(s.total(), Duration::from_millis(1000));
}

#[test]
fn record_at_keeps_the_memory_sample_it_was_given() {
    let mut s = RenderStats::new();
    let mem = MemorySample {
        rss: Some(1024 * 1024),
        peak: Some(3 * 1024 * 1024),
    };
    s.record_at("Build", 0, Duration::from_secs(1), mem);
    let p: &Phase = &s.phases[0];
    assert_eq!(p.rss_end, Some(1024 * 1024));
    assert_eq!(p.peak_end, Some(3 * 1024 * 1024));
    let out = s.report();
    assert!(out.contains("1.00 MiB"), "{out}");
    assert!(out.contains("3.00 MiB"), "{out}");
}

#[test]
fn memory_sample_now_reads_the_process_on_linux() {
    let m = MemorySample::now();
    if cfg!(target_os = "linux") {
        assert!(m.rss.is_some_and(|b| b > 0));
        assert!(m.peak.is_some_and(|b| b > 0));
        assert!(peak_memory_bytes().is_some());
        assert!(
            m.peak.unwrap() >= m.rss.unwrap(),
            "peak is a high-water mark"
        );
    }
    let d = MemorySample::default();
    assert!(d.rss.is_none() && d.peak.is_none());
}

#[test]
fn report_formats_long_durations_as_minutes() {
    let mut s = RenderStats::new();
    s.record("Render", 0, Duration::from_secs(125));
    let out = s.report();
    assert!(out.contains("02:05.0"), "{out}");
    let mut s = RenderStats::new();
    s.record("Quick", 0, Duration::from_millis(1234));
    assert!(s.report().contains("1.234s"));
}

#[test]
fn report_sorts_the_time_view_largest_first() {
    let mut s = RenderStats::new();
    s.record("Alpha", 0, Duration::from_secs(1));
    s.record("Beta", 0, Duration::from_secs(5));
    s.record("Gamma", 0, Duration::from_secs(3));
    let out = s.report();
    let by_time = out.split("Profile by time").nth(1).expect("time view");
    let pos = |n: &str| by_time.find(n).unwrap();
    assert!(
        pos("Beta") < pos("Gamma") && pos("Gamma") < pos("Alpha"),
        "{by_time}"
    );
    // Nested phases are marked.
    let mut s = RenderStats::new();
    s.record("Top", 0, Duration::from_secs(2));
    s.record("Sub", 1, Duration::from_secs(1));
    let out = s.report();
    let by_time = out.split("Profile by time").nth(1).unwrap();
    assert!(by_time.contains("*Sub"));
    assert!(!by_time.contains("*Top"));
}

#[test]
fn image_counters_come_from_render_settings() {
    let settings = RenderSettings::new(24, 9, 300, 200, 8, 0.05, 0);
    let img: ImageCounters = (&settings).into();
    assert_eq!((img.width, img.height), (300, 200));
    assert_eq!(img.samples_per_pixel, 24);
    assert_eq!(img.max_depth, 9);
    let s = RenderStats {
        image: img,
        ..RenderStats::default()
    };
    let out = s.report();
    assert!(out.contains("300x200"));
    assert!(out.contains("samples per pixel"));
}

#[test]
fn primitive_counts_convert_from_the_kernel_breakdown() {
    let br = PrimitiveBreakdown {
        triangles: 10,
        spheres: 2,
        curve_segments: 3,
        cubic_curve_spans: 4,
        instances: 5,
    };
    let pc: PrimitiveCounts = br.into();
    assert_eq!(pc.triangles, 10);
    assert_eq!(pc.spheres, 2);
    assert_eq!(pc.curve_segments, 3);
    assert_eq!(pc.cubic_curve_spans, 4);
    assert_eq!(pc.instances, 5);
    assert_eq!(pc.total(), 24);
    assert_eq!(PrimitiveCounts::default().total(), 0);
}

#[test]
fn scene_counters_appear_in_the_report_with_thousands_grouping() {
    let s = RenderStats {
        scene: SceneCounters {
            geometries: 1_234_567,
            top_level: PrimitiveCounts {
                triangles: 2_000_000,
                ..Default::default()
            },
            lights: 3,
            volumes: 2,
            ..Default::default()
        },
        ..Default::default()
    };
    let out = s.report();
    assert!(out.contains("1 234 567"), "{out}");
    assert!(out.contains("2 000 000"), "{out}");
    assert!(out.contains("volume regions"), "{out}");
    // Zero volumes are not mentioned.
    let none = RenderStats::default().report();
    assert!(!none.contains("volume regions"));
}

#[test]
fn kernel_footprint_is_reported_by_structure() {
    let s = RenderStats {
        scene: SceneCounters {
            footprint: MemoryFootprint {
                prim_nodes: 4096,
                bvh_nodes: 2048,
                packets: 0,
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let out = s.report();
    assert!(out.contains("kernel memory"), "{out}");
    assert!(out.contains("primitive nodes"), "{out}");
    assert!(out.contains("BVH nodes"), "{out}");
    assert!(!out.contains("triangle packets"), "zero rows are skipped");
    // No footprint at all: the block is absent.
    assert!(!RenderStats::default().report().contains("kernel memory"));
}

#[test]
fn ray_stats_derived_quantities() {
    let r = RayStats {
        camera_rays: 100,
        closest_hit: 250,
        shadow_rays: 150,
        vertices: 300,
        rr_tested: 40,
        rr_killed: 10,
        ended_escaped: 60,
        ended_depth: 5,
    };
    assert_eq!(r.total_rays(), 400);
    assert!((r.mean_path_length() - 3.0).abs() < 1e-9);
    assert!((r.rr_kill_rate() - 0.25).abs() < 1e-9);
    // Empty stats never divide by zero.
    let e = RayStats::default();
    assert_eq!(e.total_rays(), 0);
    assert_eq!(e.mean_path_length(), 0.0);
    assert_eq!(e.rr_kill_rate(), 0.0);
}

#[test]
fn ray_stats_merge_adds_every_counter() {
    let mut a = RayStats {
        camera_rays: 1,
        closest_hit: 2,
        shadow_rays: 3,
        vertices: 4,
        rr_tested: 5,
        rr_killed: 6,
        ended_escaped: 7,
        ended_depth: 8,
    };
    let b = RayStats {
        camera_rays: 10,
        closest_hit: 20,
        shadow_rays: 30,
        vertices: 40,
        rr_tested: 50,
        rr_killed: 60,
        ended_escaped: 70,
        ended_depth: 80,
    };
    a.merge(&b);
    assert_eq!(a.camera_rays, 11);
    assert_eq!(a.closest_hit, 22);
    assert_eq!(a.shadow_rays, 33);
    assert_eq!(a.vertices, 44);
    assert_eq!(a.rr_tested, 55);
    assert_eq!(a.rr_killed, 66);
    assert_eq!(a.ended_escaped, 77);
    assert_eq!(a.ended_depth, 88);
    // Merging the empty stats is the identity.
    let before = a;
    a.merge(&RayStats::default());
    assert_eq!(a.total_rays(), before.total_rays());
}

#[test]
fn ray_statistics_block_appears_only_with_counts() {
    let mut s = RenderStats::default();
    assert!(!s.report().contains("Ray Statistics"));
    s.rays.camera_rays = 1000;
    s.rays.closest_hit = 1000;
    s.rays.vertices = 2500;
    let out = s.report();
    assert!(out.contains("Ray Statistics"), "{out}");
    assert!(out.contains("1 000"), "{out}");
    assert!(out.contains("mean path length"), "{out}");
    // Throughput needs a Render phase.
    assert!(!out.contains("throughput"));
    s.record("Render", 0, Duration::from_secs(1));
    let out = s.report();
    assert!(out.contains("throughput"), "{out}");
    assert!(out.contains("Kray/s"), "1000 rays in one second: {out}");
}

#[test]
fn throughput_unit_scales_with_the_rate() {
    // Throughput counts ray *queries* (closest-hit plus shadow), not
    // camera rays.
    let mut s = RenderStats::default();
    s.rays.camera_rays = 1;
    s.rays.closest_hit = 5_000_000;
    s.record("Render", 0, Duration::from_secs(1));
    assert!(s.report().contains("Mray/s"), "{}", s.report());
    let mut s = RenderStats::default();
    s.rays.camera_rays = 1;
    s.rays.closest_hit = 50;
    s.record("Render", 0, Duration::from_secs(1));
    let out = s.report();
    assert!(out.contains(" ray/s"), "{out}");
}

#[test]
fn stats_clone_and_debug() {
    let mut s = RenderStats::new();
    s.record("X", 0, Duration::from_secs(1));
    let c = s.clone();
    assert_eq!(c.phases.len(), 1);
    assert!(format!("{c:?}").contains("phases"));
}
