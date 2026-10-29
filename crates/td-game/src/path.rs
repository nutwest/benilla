//! Unit movement over the grid: a straight line when every cell along it is walkable, else A*
//! (8-connected, no corner cutting, octile distance), the path as cell centres. Units do not
//! follow the flow field, which only leads to the exit.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use bevy::prelude::*;

use crate::grid::{Grid, NEIGHBOURS};

/// The search gives up past this many expanded cells: a goal that far around is not worth it.
const MAX_EXPANDED: usize = 20_000;

/// Whether every cell a straight segment crosses is walkable, sampled every half cell.
pub fn line_walkable(grid: &Grid, from: Vec2, to: Vec2) -> bool {
    let steps = (from.distance(to) / (crate::grid::CELL * 0.5)).ceil().max(1.0) as usize;
    (0..=steps).all(|i| grid.walkable_at(from.lerp(to, i as f32 / steps as f32)))
}

/// Waypoints (WoW `(x, y)`) from `from` to `to`, ending exactly at `to`; `None` when no path
/// exists within the search budget or either end is off the walkable grid.
pub fn find(grid: &Grid, from: Vec2, to: Vec2) -> Option<Vec<Vec2>> {
    if line_walkable(grid, from, to) {
        return Some(vec![to]);
    }
    let start = grid.cell_at(from)?;
    let goal = grid.cell_at(to).filter(|&g| !grid.blocked(g))?;
    let n = grid.len();
    let mut cost = vec![f32::INFINITY; n];
    let mut came = vec![u32::MAX; n];
    let mut heap = BinaryHeap::new();
    let h = |i: usize| {
        let d = (grid.center(i) - grid.center(goal)).abs() / crate::grid::CELL;
        let (lo, hi) = (d.x.min(d.y), d.x.max(d.y));
        hi + (std::f32::consts::SQRT_2 - 1.0) * lo
    };
    cost[start] = 0.0;
    heap.push(Reverse((h(start).to_bits(), start as u32)));
    let none = |_: usize| false;
    let mut expanded = 0;
    while let Some(Reverse((_, i))) = heap.pop() {
        let i = i as usize;
        if i == goal {
            break;
        }
        expanded += 1;
        if expanded > MAX_EXPANDED {
            return None;
        }
        for (k, &offset) in NEIGHBOURS.iter().enumerate() {
            // A unit standing on a blocked cell (a new wall) may step off it.
            let next = if grid.blocked(i) {
                grid.step(i, offset).filter(|&nb| !grid.blocked(nb))
            } else {
                grid.can_step(i, k, &none)
            };
            let Some(nb) = next else { continue };
            let step = if k % 2 == 1 { std::f32::consts::SQRT_2 } else { 1.0 };
            let c = cost[i] + step;
            if c < cost[nb] {
                cost[nb] = c;
                came[nb] = i as u32;
                heap.push(Reverse(((c + h(nb)).to_bits(), nb as u32)));
            }
        }
    }
    if !cost[goal].is_finite() {
        return None;
    }
    let mut cells = vec![goal];
    let mut at = goal;
    while at != start {
        at = came[at] as usize;
        cells.push(at);
    }
    cells.reverse();
    // Drop the start cell and any waypoint the next one can be reached past in a straight line.
    let mut path: Vec<Vec2> = Vec::new();
    let mut anchor = from;
    let points: Vec<Vec2> = cells[1..].iter().map(|&c| grid.center(c)).collect();
    for (j, &p) in points.iter().enumerate() {
        let next = points.get(j + 1).copied().unwrap_or(to);
        if !line_walkable(grid, anchor, next) {
            path.push(p);
            anchor = p;
        }
    }
    path.push(to);
    Some(path)
}
