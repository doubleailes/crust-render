//! A host can recover scene parts without losing volumes or changing the image.

use crust_core::{LightSelection, Renderer, Scene};
use std::path::Path;

#[test]
fn rebuilding_from_owned_parts_preserves_the_render() {
    for name in ["cornellbox.usda", "fog.usda"] {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../samples")
            .join(name);
        let scene = Scene::from_usd(&path).expect("imports sample");
        let settings = scene
            .settings
            .with_resolution(16, 12)
            .with_samples_per_pixel(16)
            .with_adaptive_sampling(16, 0.0)
            .with_indirect_clamp(0.0)
            .with_light_selection(LightSelection::Learned)
            .with_frame(3);
        let renderer = Renderer::new(scene.camera, scene.world, scene.lights, settings)
            .with_volumes(scene.volumes);
        let before = renderer.render_with_tiles();
        let geometries = renderer.world().count();
        let lights = renderer.lights().count();
        let empty_volumes = renderer.volumes().is_empty();
        assert_eq!(empty_volumes, name != "fog.usda");

        let (camera, world, lights_owned, recovered, volumes) = renderer.into_parts();
        assert_eq!(recovered.get_dimensions(), (16, 12));
        assert_eq!(recovered.samples_per_pixel(), 16);
        assert_eq!(recovered.light_selection(), LightSelection::Learned);
        let rebuilt = Renderer::new(camera, world, lights_owned, recovered)
            .with_volumes(volumes.into_regions());
        assert_eq!(rebuilt.world().count(), geometries);
        assert_eq!(rebuilt.lights().count(), lights);
        assert_eq!(rebuilt.volumes().is_empty(), empty_volumes);
        let after = rebuilt.render_with_tiles();
        for y in 0..12 {
            for x in 0..16 {
                assert_eq!(
                    before.get_pixel(x, y).to_array().map(f32::to_bits),
                    after.get_pixel(x, y).to_array().map(f32::to_bits),
                    "{name} at ({x}, {y})"
                );
            }
        }
    }
}
