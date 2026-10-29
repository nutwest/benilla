//! The flow field: a Dijkstra integration field from the exit over the grid's walkable cells, and
//! each cell's step towards the exit. Rebuilt whole on every [`GridChanged`]; at 30,000 cells that
//! is a few milliseconds. Enemies steer by [`Flow::steer`], a bilinear blend of the four nearest
//! cells' directions, so they do not zig-zag along cell edges.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use bevy::prelude::*;

use crate::grid::{Grid, GridChanged, NEIGHBOURS};
use crate::level::Level;

/// Step cost on a road cell and off it; off-road is dearer, so the road wins until blocked.
const ROAD_COST: f32 = 1.0;
const OPEN_COST: f32 = 2.0;

/// Extra cost per unit of slope (rise over run), so enemies prefer flat ground.
const SLOPE_COST: f32 = 1.0;

/// No direction: an exit cell, or one that cannot reach the exit.
const NONE: u8 = u8::MAX;

#[derive(Resource, Default)]
pub struct Flow {
    /// Cost to the exit per cell; `f32::INFINITY` where unreachable.
    integration: Vec<f32>,
    /// Index into [`NEIGHBOURS`] of each cell's step, or [`NONE`].
    dir: Vec<u8>,
    /// How many times the field has been built.
    pub generation: u32,
}

impl Flow {
    pub fn ready(&self) -> bool {
        self.generation > 0
    }

    /// The cost still to walk to the exit from a WoW `(x, y)`; infinite off the field.
    pub fn remaining(&self, grid: &Grid, wow: Vec2) -> f32 {
        grid.cell_at(wow)
            .and_then(|i| self.integration.get(i).copied())
            .unwrap_or(f32::INFINITY)
    }

    /// The step from cell `i`, if it has one.
    pub fn step(&self, i: usize) -> Option<usize> {
        let k = self.dir.get(i).copied().filter(|&k| k != NONE)?;
        Some(k as usize)
    }

    /// The unit direction (WoW `(x, y)`) of cell `i`'s step, zero for none.
    pub fn direction(&self, i: usize) -> Vec2 {
        self.step(i).map_or(Vec2::ZERO, |k| {
            let (dx, dy) = NEIGHBOURS[k];
            Vec2::new(dx as f32, dy as f32).normalize()
        })
    }

    /// The steering direction at a WoW `(x, y)`: the four surrounding cell centres' directions,
    /// bilinearly weighted. `None` when nothing around points anywhere (at the exit, or cut off).
    pub fn steer(&self, grid: &Grid, wow: Vec2) -> Option<Vec2> {
        // Cell-centre coordinates: the low-left of the four centres around `wow`.
        let g = (wow - grid.min) / crate::grid::CELL - 0.5;
        let base = g.floor();
        let t = g - base;
        let mut sum = Vec2::ZERO;
        for (ox, oy, wgt) in [
            (0, 0, (1.0 - t.x) * (1.0 - t.y)),
            (1, 0, t.x * (1.0 - t.y)),
            (0, 1, (1.0 - t.x) * t.y),
            (1, 1, t.x * t.y),
        ] {
            let (cx, cy) = (base.x as i32 + ox, base.y as i32 + oy);
            if cx < 0 || cy < 0 || cx as usize >= grid.w || cy as usize >= grid.h {
                continue;
            }
            sum += self.direction(grid.index(cx as usize, cy as usize)) * wgt;
        }
        (sum.length_squared() > 1e-4).then(|| sum.normalize())
    }
}

pub fn plugin(app: &mut App) {
    app.init_resource::<Flow>().add_systems(
        Update,
        rebuild.after(crate::grid::GridSet).run_if(on_message::<GridChanged>),
    );
}

/// The cost of standing on cell `i`.
fn cell_cost(grid: &Grid, i: usize) -> f32 {
    let base = if grid.is_road(i) { ROAD_COST } else { OPEN_COST };
    base + grid.slope(i) * SLOPE_COST
}

pub(crate) fn rebuild(
    time: Res<Time<bevy::time::Real>>,
    level: Res<Level>,
    grid: Res<Grid>,
    mut flow: ResMut<Flow>,
    mut changed: MessageReader<GridChanged>,
) {
    changed.clear();
    if !grid.ready {
        return;
    }
    let started = std::time::Instant::now();
    let n = grid.len();
    let mut integration = vec![f32::INFINITY; n];
    let mut heap = BinaryHeap::new();
    for i in grid.cells_within(level.exit_center, level.exit_radius) {
        if !grid.blocked(i) {
            integration[i] = 0.0;
            heap.push(Reverse((0u32, i as u32)));
        }
    }
    let none = |_: usize| false;
    // Non-negative floats order the same as their bits, so the heap keys on `to_bits`.
    while let Some(Reverse((bits, i))) = heap.pop() {
        let i = i as usize;
        let d = f32::from_bits(bits);
        if d > integration[i] {
            continue;
        }
        for k in 0..NEIGHBOURS.len() {
            let Some(nb) = grid.can_step(i, k, &none) else {
                continue;
            };
            let len = if k % 2 == 1 { std::f32::consts::SQRT_2 } else { 1.0 };
            let nd = d + (cell_cost(&grid, i) + cell_cost(&grid, nb)) * 0.5 * len;
            if nd < integration[nb] {
                integration[nb] = nd;
                heap.push(Reverse((nd.to_bits(), nb as u32)));
            }
        }
    }

    // Each walkable cell steps to its cheapest allowed neighbour; a blocked cell (an enemy caught
    // under a new structure) steps to its cheapest walkable neighbour, corners notwithstanding.
    let mut dir = vec![NONE; n];
    for (i, slot) in dir.iter_mut().enumerate() {
        let here = if grid.blocked(i) {
            f32::INFINITY
        } else {
            integration[i]
        };
        let mut best = (here, NONE);
        for (k, &offset) in NEIGHBOURS.iter().enumerate() {
            let nb = if grid.blocked(i) {
                grid.step(i, offset).filter(|&nb| !grid.blocked(nb))
            } else {
                grid.can_step(i, k, &none)
            };
            if let Some(nb) = nb {
                if integration[nb] < best.0 {
                    best = (integration[nb], k as u8);
                }
            }
        }
        *slot = best.1;
    }

    let reachable = integration.iter().filter(|v| v.is_finite()).count();
    let spawn_cost: Vec<String> = level
        .spawns
        .iter()
        .map(|&s| {
            grid.cell_at(s)
                .map_or("off-grid".into(), |i| format!("{:.0}", integration[i]))
        })
        .collect();
    flow.integration = integration;
    flow.dir = dir;
    flow.generation += 1;
    info!(
        "flow: rebuilt #{} in {:.1} ms — {reachable} cells reach the exit, spawn cost [{}] ({:.1}s)",
        flow.generation,
        started.elapsed().as_secs_f64() * 1000.0,
        spawn_cost.join(", "),
        time.elapsed_secs()
    );
}
