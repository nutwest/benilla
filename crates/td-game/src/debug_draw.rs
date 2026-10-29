//! Debug overlays, drawn with gizmos over the scene:
//! - F1: the route the flow field gives from each spawn to the exit (`TD_PATH_DEBUG=1` opens on).
//! - F2: the grid: blocked cells by reason, road cells, structure cells and flow arrows
//!   (`TD_GRID_DEBUG=1` opens on).

use std::f32::consts::FRAC_PI_2;

use bevy::prelude::*;

use benilla_assets::coords::wow_to_bevy;

use crate::flow::Flow;
use crate::grid::{Grid, HOLE, SOLID, STEEP, WATER};
use crate::level::Level;

/// Height of the overlays above the ground, in yards.
const LIFT: f32 = 0.3;

/// Flow arrows on every n-th cell along each axis, so the field reads without a carpet of them.
const ARROW_STRIDE: usize = 3;

/// The route trace's safety cap, in cells.
const MAX_ROUTE: usize = 5_000;

const STRUCTURE_COLOR: Color = Color::srgb(0.9, 0.2, 0.9);

#[derive(Resource)]
struct Overlays {
    route: bool,
    grid: bool,
}

pub fn plugin(app: &mut App) {
    let env_on = |name: &str| std::env::var(name).as_deref() == Ok("1");
    app.insert_resource(Overlays {
        route: env_on("TD_PATH_DEBUG"),
        grid: env_on("TD_GRID_DEBUG"),
    })
    .add_systems(Startup, configure)
    .add_systems(Update, (toggle, draw_grid, draw_route).chain());
}

/// Thick lines drawn over the scene, so the overlays read through grass and props.
fn configure(mut store: ResMut<GizmoConfigStore>) {
    let (config, _) = store.config_mut::<DefaultGizmoConfigGroup>();
    config.line.width = 3.0;
    config.depth_bias = -1.0;
}

fn toggle(keys: Res<ButtonInput<KeyCode>>, mut overlays: ResMut<Overlays>) {
    let on_off = |b: bool| if b { "on" } else { "off" };
    if keys.just_pressed(KeyCode::F1) {
        overlays.route = !overlays.route;
        info!("debug: route overlay {}", on_off(overlays.route));
    }
    if keys.just_pressed(KeyCode::F2) {
        overlays.grid = !overlays.grid;
        info!("debug: grid overlay {}", on_off(overlays.grid));
    }
}

fn lifted(grid: &Grid, i: usize) -> Vec3 {
    grid.center_bevy(i) + Vec3::Y * LIFT
}

fn draw_grid(overlays: Res<Overlays>, grid: Res<Grid>, flow: Res<Flow>, mut gizmos: Gizmos) {
    if !overlays.grid || !grid.ready {
        return;
    }
    // A cell's square, flat on the ground (gizmo rects are drawn in their local XY plane).
    let flat = Quat::from_rotation_x(FRAC_PI_2);
    let square = Vec2::splat(0.8);
    for i in 0..grid.len() {
        let at = lifted(&grid, i);
        let flags = grid.flags(i);
        let color = if grid.has_structure(i) {
            Some(STRUCTURE_COLOR)
        } else if flags & SOLID != 0 {
            Some(Color::srgb(0.95, 0.15, 0.1))
        } else if flags & WATER != 0 {
            Some(Color::srgb(0.2, 0.5, 1.0))
        } else if flags & (STEEP | HOLE) != 0 {
            Some(Color::srgb(1.0, 0.55, 0.0))
        } else if grid.is_road(i) {
            Some(Color::srgb(0.85, 0.75, 0.45))
        } else {
            None
        };
        if let Some(color) = color {
            gizmos.rect(Isometry3d::new(at, flat), square, color);
        }

        let (ix, iy) = grid.coords(i);
        if ix % ARROW_STRIDE == 1 && iy % ARROW_STRIDE == 1 && !grid.blocked(i) {
            let d = flow.direction(i);
            if d != Vec2::ZERO {
                let tip = wow_to_bevy([d.x, d.y, 0.0]) * 0.9;
                gizmos
                    .arrow(at - tip * 0.5, at + tip * 0.5, Color::srgb(0.2, 1.0, 0.4))
                    .with_tip_length(0.35);
            }
        }
    }
}

/// From each spawn, the cells the field steps through to the exit.
fn draw_route(
    overlays: Res<Overlays>,
    level: Res<Level>,
    grid: Res<Grid>,
    flow: Res<Flow>,
    mut gizmos: Gizmos,
) {
    if !overlays.route || !flow.ready() {
        return;
    }
    // Structure cells too: they are what the route bends around.
    let flat = Quat::from_rotation_x(FRAC_PI_2);
    for i in (0..grid.len()).filter(|&i| grid.has_structure(i)) {
        gizmos.rect(
            Isometry3d::new(lifted(&grid, i), flat),
            Vec2::splat(0.8),
            STRUCTURE_COLOR,
        );
    }
    for &spawn in &level.spawns {
        let Some(mut i) = grid.cell_at(spawn) else {
            continue;
        };
        let mut strip = vec![lifted(&grid, i)];
        for _ in 0..MAX_ROUTE {
            let Some(k) = flow.step(i) else { break };
            let Some(next) = grid.step(i, crate::grid::NEIGHBOURS[k]) else {
                break;
            };
            i = next;
            strip.push(lifted(&grid, i));
        }
        let ends = (strip[0], *strip.last().unwrap_or(&strip[0]));
        gizmos.linestrip(strip, Color::srgb(1.0, 0.85, 0.1));
        gizmos.sphere(Isometry3d::from_translation(ends.0), 1.2, Color::srgb(0.2, 1.0, 0.2));
        gizmos.sphere(Isometry3d::from_translation(ends.1), 1.2, Color::srgb(1.0, 0.2, 0.2));
    }
}
