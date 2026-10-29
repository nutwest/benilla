//! The pathing grid: 1-yard cells over the level's bounds, in WoW `(x, y)`. A static layer is
//! baked once the world has loaded (terrain holes, slope, deep liquid, world geometry), roads are
//! marked from the level's corridors, and a structure layer holds what the player (or the debug
//! test block) has placed. The flow field ([`crate::flow`]) is built over it.
//!
//! The bake asks the engine three things per cell: the terrain height at its corners
//! ([`TerrainHeights`]), the liquid at its centre ([`WorldPoint::liquid_at`]) and what a ray
//! straight down meets ([`WorldCollision::ray_body`]), which sees terrain, doodad hulls and WMO
//! walking colliders: the geometry a player's body collides with.

use std::collections::VecDeque;

use bevy::prelude::*;

use benilla_assets::coords::wow_to_bevy;
use benilla_formats::LiquidKind;
use benilla_world::collision::WorldCollision;
use benilla_world::terrain_stream::WorldLoadProgress;
use benilla_world::world_point::{Subject, TerrainHeights, WorldPoint};

use crate::level::Level;

/// Cell edge in yards.
pub const CELL: f32 = 1.0;

/// Static-layer flags.
pub const HOLE: u8 = 1 << 0;
pub const STEEP: u8 = 1 << 1;
pub const WATER: u8 = 1 << 2;
pub const SOLID: u8 = 1 << 3;
pub const ROAD: u8 = 1 << 4;
const STATIC_BLOCKED: u8 = HOLE | STEEP | WATER | SOLID;

/// Liquid deeper than this over the ground blocks a cell; shallower is a ford.
const FORD_DEPTH: f32 = 1.0;

/// A downward ray hit this far above the terrain is an obstacle (a wall, a trunk, a fence).
const SOLID_CLEARANCE: f32 = 0.75;

/// Where the downward rays start, above the terrain.
const RAY_START: f32 = 15.0;

/// The four inset ray points besides the centre, in yards from it.
const RAY_INSET: f32 = 0.35;

/// Rows baked per frame, so the bake never stalls a frame.
const ROWS_PER_FRAME: usize = 12;

/// The eight neighbour steps, in WoW `(x, y)` cells; diagonals at odd indices.
pub const NEIGHBOURS: [(i32, i32); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

#[derive(Resource)]
pub struct Grid {
    /// The WoW `(x, y)` of cell `(0, 0)`'s low corner.
    pub min: Vec2,
    pub w: usize,
    pub h: usize,
    flags: Vec<u8>,
    /// Terrain height at each cell's centre (WoW `z`), for drawing and placing.
    ground: Vec<f32>,
    /// Steepest rise over run across each cell.
    slope: Vec<f32>,
    /// Structure-layer occupancy (a count, so overlapping footprints clear independently).
    structure: Vec<u8>,
    /// The static bake is complete.
    pub ready: bool,
}

impl Grid {
    fn new(level: &Level) -> Self {
        let size = level.bounds_max - level.bounds_min;
        let w = (size.x / CELL).ceil() as usize;
        let h = (size.y / CELL).ceil() as usize;
        Self {
            min: level.bounds_min,
            w,
            h,
            flags: vec![0; w * h],
            ground: vec![0.0; w * h],
            slope: vec![0.0; w * h],
            structure: vec![0; w * h],
            ready: false,
        }
    }

    pub fn len(&self) -> usize {
        self.w * self.h
    }

    pub fn index(&self, ix: usize, iy: usize) -> usize {
        iy * self.w + ix
    }

    pub fn coords(&self, i: usize) -> (usize, usize) {
        (i % self.w, i / self.w)
    }

    /// The cell `(ix, iy)` plus a step, if still on the grid.
    pub fn step(&self, i: usize, (dx, dy): (i32, i32)) -> Option<usize> {
        let (ix, iy) = self.coords(i);
        let (nx, ny) = (ix as i32 + dx, iy as i32 + dy);
        (nx >= 0 && ny >= 0 && (nx as usize) < self.w && (ny as usize) < self.h)
            .then(|| self.index(nx as usize, ny as usize))
    }

    /// The cell holding a WoW `(x, y)`.
    pub fn cell_at(&self, wow: Vec2) -> Option<usize> {
        let c = ((wow - self.min) / CELL).floor();
        (c.x >= 0.0 && c.y >= 0.0 && (c.x as usize) < self.w && (c.y as usize) < self.h)
            .then(|| self.index(c.x as usize, c.y as usize))
    }

    /// A cell's centre, WoW `(x, y)`.
    pub fn center(&self, i: usize) -> Vec2 {
        let (ix, iy) = self.coords(i);
        self.min + (Vec2::new(ix as f32, iy as f32) + 0.5) * CELL
    }

    /// A cell's centre in Bevy space, on the ground.
    pub fn center_bevy(&self, i: usize) -> Vec3 {
        let c = self.center(i);
        wow_to_bevy([c.x, c.y, self.ground[i]])
    }

    pub fn flags(&self, i: usize) -> u8 {
        self.flags[i]
    }

    pub fn slope(&self, i: usize) -> f32 {
        self.slope[i]
    }

    pub fn is_road(&self, i: usize) -> bool {
        self.flags[i] & ROAD != 0
    }

    pub fn static_blocked(&self, i: usize) -> bool {
        self.flags[i] & STATIC_BLOCKED != 0
    }

    /// What makes a statically blocked cell so, for a refusal's message.
    pub fn blocked_reason(&self, i: usize) -> &'static str {
        let f = self.flags[i];
        if f & SOLID != 0 {
            "a building, tree or other solid"
        } else if f & WATER != 0 {
            "deep water"
        } else if f & STEEP != 0 {
            "ground too steep"
        } else if f & HOLE != 0 {
            "a hole in the terrain"
        } else {
            "open ground"
        }
    }

    pub fn has_structure(&self, i: usize) -> bool {
        self.structure[i] > 0
    }

    pub fn blocked(&self, i: usize) -> bool {
        self.static_blocked(i) || self.has_structure(i)
    }

    /// Whether a WoW `(x, y)` is walkable; off the grid is not.
    pub fn walkable_at(&self, wow: Vec2) -> bool {
        self.cell_at(wow).is_some_and(|i| !self.blocked(i))
    }

    /// A move from `i` by neighbour step `k` is allowed: the target is walkable and, for a
    /// diagonal, both orthogonal cells are too, so nothing cuts a blocked corner.
    pub fn can_step(&self, i: usize, k: usize, extra: &dyn Fn(usize) -> bool) -> Option<usize> {
        let free = |c: usize| !self.blocked(c) && !extra(c);
        let (dx, dy) = NEIGHBOURS[k];
        let n = self.step(i, (dx, dy)).filter(|&n| free(n))?;
        if dx != 0 && dy != 0 {
            let a = self.step(i, (dx, 0)).is_some_and(free);
            let b = self.step(i, (0, dy)).is_some_and(free);
            if !(a && b) {
                return None;
            }
        }
        Some(n)
    }

    /// Adds (`+1`) or removes (`-1`) a footprint from the structure layer.
    pub fn mark_structure(&mut self, cells: &[usize], add: bool) {
        for &i in cells {
            self.structure[i] = if add {
                self.structure[i].saturating_add(1)
            } else {
                self.structure[i].saturating_sub(1)
            };
        }
    }

    /// The cells whose centres fall within `radius` of a WoW `(x, y)`.
    pub fn cells_within(&self, center: Vec2, radius: f32) -> Vec<usize> {
        (0..self.len())
            .filter(|&i| self.center(i).distance(center) <= radius)
            .collect()
    }

    /// Whether every spawn still reaches the exit with `extra` cells also blocked: a
    /// breadth-first pass from the exit over the walkable cells.
    pub fn spawns_reach_exit(&self, level: &Level, extra: &[usize]) -> bool {
        let mut extra_set = vec![false; self.len()];
        for &i in extra {
            extra_set[i] = true;
        }
        let is_extra = |i: usize| extra_set[i];
        let mut seen = vec![false; self.len()];
        let mut queue: VecDeque<usize> = self
            .cells_within(level.exit_center, level.exit_radius)
            .into_iter()
            .filter(|&i| !self.blocked(i) && !is_extra(i))
            .collect();
        for &i in &queue {
            seen[i] = true;
        }
        while let Some(i) = queue.pop_front() {
            for k in 0..NEIGHBOURS.len() {
                if let Some(n) = self.can_step(i, k, &is_extra) {
                    if !seen[n] {
                        seen[n] = true;
                        queue.push_back(n);
                    }
                }
            }
        }
        level
            .spawns
            .iter()
            .all(|&s| self.cell_at(s).is_some_and(|i| seen[i]))
    }
}

/// The bake's progress: the corner heights, then rows of cells.
#[derive(Resource, Default)]
pub(crate) struct Bake {
    /// Terrain height at each cell corner, `(w + 1) × (h + 1)`; `None` over a hole or off the
    /// streamed set.
    corners: Option<Vec<Option<f32>>>,
    next_row: usize,
    started_at: Option<f32>,
}

/// Raised whenever the walkable set or the costs change; the flow field rebuilds on it.
#[derive(Message)]
pub struct GridChanged;

/// The systems that change the grid; the flow field rebuilds after them.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GridSet;

pub fn plugin(app: &mut App) {
    app.add_message::<GridChanged>()
        .init_resource::<Bake>()
        .add_systems(
            Startup,
            |mut commands: Commands, level: Res<Level>| {
                let grid = Grid::new(&level);
                info!("grid: {}×{} cells of {CELL} yd", grid.w, grid.h);
                commands.insert_resource(grid);
            },
        )
        .add_systems(Update, bake.in_set(GridSet));
}

pub(crate) fn bake(
    time: Res<Time<bevy::time::Real>>,
    level: Res<Level>,
    progress: Option<Res<WorldLoadProgress>>,
    heights: TerrainHeights,
    world: WorldPoint,
    collision: WorldCollision,
    mut grid: ResMut<Grid>,
    mut state: ResMut<Bake>,
    mut changed: MessageWriter<GridChanged>,
) {
    if grid.ready {
        return;
    }
    // Every tile and collider around the arena first: a ray before its hull lands sees nothing.
    if !progress.is_some_and(|p| p.focus_resident && p.presentable()) {
        return;
    }
    let (w, h, min) = (grid.w, grid.h, grid.min);
    let height_at = |wow: Vec2| heights.under(wow_to_bevy([wow.x, wow.y, 0.0]));

    let Bake {
        corners,
        next_row,
        started_at,
    } = &mut *state;
    started_at.get_or_insert(time.elapsed_secs());
    let corners = corners.get_or_insert_with(|| {
        (0..=h)
            .flat_map(|iy| (0..=w).map(move |ix| (ix, iy)))
            .map(|(ix, iy)| height_at(min + Vec2::new(ix as f32, iy as f32) * CELL))
            .collect()
    });
    let corner = |ix: usize, iy: usize| corners[iy * (w + 1) + ix];

    let rows = *next_row..(*next_row + ROWS_PER_FRAME).min(h);
    for iy in rows.clone() {
        for ix in 0..w {
            let i = grid.index(ix, iy);
            let quad = [
                corner(ix, iy),
                corner(ix + 1, iy),
                corner(ix, iy + 1),
                corner(ix + 1, iy + 1),
            ];
            let [Some(a), Some(b), Some(c), Some(d)] = quad else {
                grid.flags[i] |= HOLE;
                continue;
            };
            let ground = (a + b + c + d) * 0.25;
            grid.ground[i] = ground;
            let edge = [(a - b), (c - d), (a - c), (b - d)]
                .iter()
                .fold(0.0f32, |m, v| m.max(v.abs()))
                / CELL;
            let diag = (a - d).abs().max((b - c).abs()) / (CELL * std::f32::consts::SQRT_2);
            let slope = edge.max(diag);
            grid.slope[i] = slope;
            if slope > level.slope_limit {
                grid.flags[i] |= STEEP;
            }

            let center = grid.center(i);
            if let Some(hit) = world.liquid_at(Subject::Eye, [center.x, center.y, ground]) {
                let deep = hit.surface_z - ground > FORD_DEPTH;
                if deep || matches!(hit.kind, LiquidKind::Magma | LiquidKind::Slime) {
                    grid.flags[i] |= WATER;
                }
            }

            let probes = [
                Vec2::ZERO,
                Vec2::new(RAY_INSET, RAY_INSET),
                Vec2::new(-RAY_INSET, RAY_INSET),
                Vec2::new(RAY_INSET, -RAY_INSET),
                Vec2::new(-RAY_INSET, -RAY_INSET),
            ];
            let solid = probes.iter().any(|&o| {
                let p = center + o;
                let origin = wow_to_bevy([p.x, p.y, ground + RAY_START]);
                collision
                    .ray_body(origin, Dir3::NEG_Y, RAY_START + 5.0)
                    .is_some_and(|hit| origin.y - hit.distance > ground + SOLID_CLEARANCE)
            });
            if solid {
                grid.flags[i] |= SOLID;
            }
        }
    }
    *next_row = rows.end;
    if *next_row < h {
        return;
    }

    mark_roads(&mut grid, &level);
    grid.ready = true;
    let count = |f: u8| grid.flags.iter().filter(|&&x| x & f != 0).count();
    info!(
        "grid: baked {} cells in {:.2}s — hole {}, steep {}, water {}, solid {}, road {}, walkable {}",
        grid.len(),
        time.elapsed_secs() - started_at.unwrap_or(0.0),
        count(HOLE),
        count(STEEP),
        count(WATER),
        count(SOLID),
        count(ROAD),
        (0..grid.len()).filter(|&i| !grid.blocked(i)).count()
    );
    changed.write(GridChanged);
}

/// Marks every cell whose centre lies within half a road's width of its polyline.
fn mark_roads(grid: &mut Grid, level: &Level) {
    for i in 0..grid.len() {
        let c = grid.center(i);
        let on_road = level.roads.iter().any(|road| {
            road.points
                .windows(2)
                .any(|s| distance_to_segment(c, s[0], s[1]) <= road.width * 0.5)
        });
        if on_road {
            grid.flags[i] |= ROAD;
        }
    }
}

pub fn distance_to_segment(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let t = if ab.length_squared() > 0.0 {
        ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0)
    } else {
        0.0
    };
    p.distance(a + ab * t)
}
