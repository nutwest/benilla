//! A level as data: the map, the arena, the pathing grid's bounds, spawns, exit and roads, and
//! the waves, read from a RON file at boot (`levels/goldshire.ron`, or `$TD_LEVEL`). Positions in
//! the file and here are WoW world coords `(x, y)`; the grid works in the same frame.

use bevy::prelude::*;
use serde::Deserialize;

use benilla_assets::coords::wow_to_bevy;

/// The file as written.
#[derive(Deserialize)]
struct LevelFile {
    name: String,
    map_id: u32,
    arena_center: (f32, f32, f32),
    lives: u32,
    bounds: Bounds,
    slope_limit: f32,
    spawns: Vec<(f32, f32)>,
    exit: Exit,
    roads: Vec<RoadFile>,
    waves: Vec<WaveDef>,
}

#[derive(Deserialize)]
struct Bounds {
    min: (f32, f32),
    max: (f32, f32),
}

#[derive(Deserialize)]
struct Exit {
    center: (f32, f32),
    radius: f32,
}

#[derive(Deserialize)]
struct RoadFile {
    width: f32,
    points: Vec<(f32, f32)>,
}

/// One wave: `count` of one creature display, `interval` seconds apart, starting `delay`
/// seconds after the previous wave's last spawn.
#[derive(Deserialize, Clone, Debug)]
pub struct WaveDef {
    pub display_id: u32,
    pub count: u32,
    pub interval: f32,
    /// Ground speed in yards per second.
    pub speed: f32,
    pub health: f32,
    pub delay: f32,
}

/// A road corridor: a polyline in WoW `(x, y)` and its full width in yards.
pub struct Road {
    pub width: f32,
    pub points: Vec<Vec2>,
}

#[derive(Resource)]
pub struct Level {
    pub name: String,
    pub map_id: u32,
    /// WoW coords; `z` is a guess until the terrain answers.
    pub center: [f32; 3],
    pub lives: u32,
    /// The grid rectangle, WoW `(x, y)`.
    pub bounds_min: Vec2,
    pub bounds_max: Vec2,
    pub slope_limit: f32,
    pub spawns: Vec<Vec2>,
    pub exit_center: Vec2,
    pub exit_radius: f32,
    pub roads: Vec<Road>,
    pub waves: Vec<WaveDef>,
}

impl Level {
    /// The arena centre in Bevy space.
    pub fn center_bevy(&self) -> Vec3 {
        wow_to_bevy(self.center)
    }
}

/// Reads the level named by `$TD_LEVEL`, else the crate's Goldshire level.
pub fn load() -> Result<Level, String> {
    let path = std::env::var("TD_LEVEL").unwrap_or_else(|_| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/levels/goldshire.ron").to_string()
    });
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let file: LevelFile = ron::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    let v = |(x, y): (f32, f32)| Vec2::new(x, y);
    let (min, max) = (v(file.bounds.min), v(file.bounds.max));
    if min.x >= max.x || min.y >= max.y {
        return Err(format!("{path}: bounds min must be below max on both axes"));
    }
    let inside = |p: Vec2| p.cmpge(min).all() && p.cmplt(max).all();
    if file.spawns.is_empty() || !file.spawns.iter().all(|&s| inside(v(s))) {
        return Err(format!("{path}: needs at least one spawn, all inside the bounds"));
    }
    if !inside(v(file.exit.center)) {
        return Err(format!("{path}: the exit is outside the bounds"));
    }
    if file.waves.is_empty() {
        return Err(format!("{path}: no waves"));
    }
    let (cx, cy, cz) = file.arena_center;
    let level = Level {
        name: file.name,
        map_id: file.map_id,
        center: [cx, cy, cz],
        lives: file.lives,
        bounds_min: min,
        bounds_max: max,
        slope_limit: file.slope_limit,
        spawns: file.spawns.into_iter().map(v).collect(),
        exit_center: v(file.exit.center),
        exit_radius: file.exit.radius,
        roads: file
            .roads
            .into_iter()
            .map(|r| Road {
                width: r.width,
                points: r.points.into_iter().map(v).collect(),
            })
            .collect(),
        waves: file.waves,
    };
    let size = max - min;
    info!(
        "level: {} from {path} — map {}, grid {:.0}×{:.0} yd, {} spawn(s), {} road(s), {} waves",
        level.name,
        level.map_id,
        size.x,
        size.y,
        level.spawns.len(),
        level.roads.len(),
        level.waves.len()
    );
    Ok(level)
}
