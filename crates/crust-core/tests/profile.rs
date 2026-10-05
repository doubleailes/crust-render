//! The render profile (`--profile`). A test binary of its own, because the
//! switch is process-global: another test rendering while it is on would
//! record into this one's profile.

use crust_core::profile::{self, Section};
use crust_core::{Buffer, RayStats, RenderSettings, Renderer, get_settings, simple_scene};

const W: usize = 24;
const H: usize = 16;

fn renderer() -> Renderer {
    let (world, lights) = simple_scene();
    let (camera, _) = get_settings();
    // Adaptive stop off, so both renders take exactly the same samples.
    let settings = RenderSettings::default()
        .with_resolution(W, H)
        .with_samples_per_pixel(4)
        .with_max_depth(4)
        .with_adaptive_sampling(4, 0.0);
    Renderer::new(camera, world, lights, settings)
}

fn render(r: &Renderer, tiled: bool) -> (Buffer, RayStats) {
    r.render_with_stats(tiled, &|_, _| {})
}

fn same_image(a: &Buffer, b: &Buffer) -> bool {
    (0..H).all(|y| (0..W).all(|x| a.get_pixel(x, y).to_array() == b.get_pixel(x, y).to_array()))
}

// One test rather than several: they would share the global switch.
#[test]
fn profile_counts_match_ray_stats_and_leave_the_image_alone() {
    let r = renderer();
    for tiled in [true, false] {
        profile::set_enabled(false);
        let (plain, plain_stats) = render(&r, tiled);
        assert!(profile::take().is_none(), "nothing recorded while disabled");

        profile::set_enabled(true);
        let (profiled, stats) = render(&r, tiled);
        let p = profile::take().expect("an enabled render records a profile");
        profile::set_enabled(false);

        // Profiling is scheduling-free bookkeeping: same image, same work.
        assert!(same_image(&plain, &profiled), "tiled={tiled}");
        assert_eq!(plain_stats, stats, "tiled={tiled}");

        // Each section is entered exactly where its counter is bumped.
        assert_eq!(p.section(Section::MainLoop).calls, (W * H) as u64);
        assert_eq!(p.section(Section::GeneratePrimary).calls, stats.camera_rays);
        assert_eq!(p.section(Section::Contributions).calls, stats.camera_rays);
        assert_eq!(p.section(Section::Trace).calls, stats.closest_hit);
        assert_eq!(p.section(Section::Occlusion).calls, stats.shadow_rays);
        assert_eq!(
            p.section(Section::EvalBsdfs).calls,
            stats.surface_vertices(),
            "one shading point per surface vertex"
        );
        assert!(stats.shadow_rays > 0, "the scene has lights, so NEE ran");

        // Every other section nests under MainLoop, so it holds all of the
        // thread time, and the local times partition it.
        let main = p.section(Section::MainLoop);
        assert_eq!(main.total, p.thread_time());
        let locals: std::time::Duration = Section::ALL.iter().map(|&s| p.section(s).local).sum();
        assert_eq!(locals, p.thread_time());
    }
}
