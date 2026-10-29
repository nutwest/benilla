//! A debug structure: F3 toggles a barrier across the first road, on the structure layer, so the
//! flow field reroutes enemies around it. `TD_TEST_BLOCK=1` raises it as soon as the grid is
//! baked, for a shot. It spans the road plus six yards past each edge, three yards thick, so going
//! around it is a real detour over open ground rather than a step around its end.

use bevy::prelude::*;

use crate::grid::{Grid, GridChanged, GridSet};
use crate::level::Level;

/// Where along the first road's first segment the barrier stands (0 start, 1 end).
const ALONG: f32 = 0.45;

/// The barrier's thickness along the road, and its overhang past each road edge, in yards.
const THICKNESS: f32 = 3.0;
const OVERHANG: f32 = 6.0;

#[derive(Resource, Default)]
pub struct TestBlock {
    /// The footprint, computed once the grid is ready.
    cells: Vec<usize>,
    pub on: bool,
    /// `TD_TEST_BLOCK=1` still to apply.
    pending_env: bool,
}

pub fn plugin(app: &mut App) {
    app.insert_resource(TestBlock {
        pending_env: std::env::var("TD_TEST_BLOCK").as_deref() == Ok("1"),
        ..default()
    })
    .add_systems(Update, toggle.in_set(GridSet).after(crate::grid::bake));
}

fn toggle(
    keys: Res<ButtonInput<KeyCode>>,
    level: Res<Level>,
    mut grid: ResMut<Grid>,
    mut block: ResMut<TestBlock>,
    mut changed: MessageWriter<GridChanged>,
) {
    if !grid.ready {
        return;
    }
    let pressed = keys.just_pressed(KeyCode::F3);
    if !pressed && !block.pending_env {
        return;
    }
    block.pending_env = false;
    if block.cells.is_empty() {
        block.cells = footprint(&grid, &level);
    }
    if block.cells.is_empty() {
        warn!("test block: the level has no road segment to block");
        return;
    }
    if block.on {
        grid.mark_structure(&block.cells, false);
        block.on = false;
        info!("test block: off");
    } else {
        if !grid.spawns_reach_exit(&level, &block.cells) {
            warn!("test block: refused — it would cut a spawn off from the exit");
            return;
        }
        grid.mark_structure(&block.cells, true);
        block.on = true;
        info!("test block: on, {} cells", block.cells.len());
    }
    changed.write(GridChanged);
}

/// The barrier's cells: a rectangle across the first road segment at [`ALONG`].
fn footprint(grid: &Grid, level: &Level) -> Vec<usize> {
    let Some(road) = level.roads.first() else {
        return Vec::new();
    };
    let [a, b] = [road.points[0], road.points[1]];
    let along = (b - a).normalize_or_zero();
    let across = along.perp();
    let center = a.lerp(b, ALONG);
    let half_len = road.width * 0.5 + OVERHANG;
    (0..grid.len())
        .filter(|&i| {
            let d = grid.center(i) - center;
            d.dot(across).abs() <= half_len && d.dot(along).abs() <= THICKNESS * 0.5
        })
        .collect()
}
