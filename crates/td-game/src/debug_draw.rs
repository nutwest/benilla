//! Debug overlays, drawn with gizmos. F1 toggles the enemy path; `TD_PATH_DEBUG=1` opens with it
//! on, for a shot.

use bevy::prelude::*;

use benilla_world::world_point::TerrainHeights;

use crate::level::Level;

/// Spacing of the ground samples along each segment, in yards.
const SAMPLE_STEP: f32 = 2.0;

/// Height of the line above the ground, in yards.
const LIFT: f32 = 0.3;

#[derive(Resource)]
struct PathDebug(bool);

pub fn plugin(app: &mut App) {
    let on = std::env::var("TD_PATH_DEBUG").as_deref() == Ok("1");
    app.insert_resource(PathDebug(on))
        .add_systems(Startup, configure)
        .add_systems(Update, (toggle, draw_path).chain());
}

/// Thick lines drawn over the scene, so the path reads through grass and props.
fn configure(mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<DefaultGizmoConfigGroup>();
    config.line.width = 4.0;
    config.depth_bias = -1.0;
}

fn toggle(keys: Res<ButtonInput<KeyCode>>, mut overlay: ResMut<PathDebug>) {
    if keys.just_pressed(KeyCode::F1) {
        overlay.0 = !overlay.0;
        info!("debug: path overlay {}", if overlay.0 { "on" } else { "off" });
    }
}

fn draw_path(
    overlay: Res<PathDebug>,
    level: Res<Level>,
    heights: TerrainHeights,
    mut gizmos: Gizmos,
) {
    if !overlay.0 {
        return;
    }
    let fallback = level.center_bevy().y;
    let ground = |p: Vec3| Vec3::new(p.x, heights.under(p).unwrap_or(fallback) + LIFT, p.z);
    let points = level.path.points();
    let line = Color::srgb(1.0, 0.85, 0.1);

    // The polyline hugs the ground: each segment sampled every few yards.
    let mut strip = Vec::new();
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let steps = (a.distance(b) / SAMPLE_STEP).ceil().max(1.0) as usize;
        for i in 0..steps {
            strip.push(ground(a.lerp(b, i as f32 / steps as f32)));
        }
    }
    if let Some(&last) = points.last() {
        strip.push(ground(last));
    }
    gizmos.linestrip(strip, line);

    // Waypoints: green start, red end, yellow turns.
    for (i, &p) in points.iter().enumerate() {
        let color = if i == 0 {
            Color::srgb(0.2, 1.0, 0.2)
        } else if i == points.len() - 1 {
            Color::srgb(1.0, 0.2, 0.2)
        } else {
            line
        };
        gizmos.sphere(Isometry3d::from_translation(ground(p)), 1.2, color);
    }
}
