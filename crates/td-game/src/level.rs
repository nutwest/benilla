//! A level as data: the map, the arena, the enemy path and the waves, read from a RON file at
//! boot (`levels/goldshire.ron`, or `$TD_LEVEL`). Positions in the file are WoW world coords; the
//! [`EnemyPath`] holds them in Bevy space, flat, for the movers to ground themselves.

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
    path: Vec<(f32, f32)>,
    waves: Vec<WaveDef>,
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

#[derive(Resource)]
pub struct Level {
    pub name: String,
    pub map_id: u32,
    /// WoW coords; `z` is a guess until the terrain answers.
    pub center: [f32; 3],
    pub lives: u32,
    pub path: EnemyPath,
    pub waves: Vec<WaveDef>,
}

impl Level {
    /// The arena centre in Bevy space.
    pub fn center_bevy(&self) -> Vec3 {
        wow_to_bevy(self.center)
    }
}

/// A polyline on the ground plane (Bevy space, `y = 0`), walked by arc length.
pub struct EnemyPath {
    points: Vec<Vec3>,
    /// Arc length at each point; `cumulative[0] = 0`.
    cumulative: Vec<f32>,
}

impl EnemyPath {
    fn new(points: Vec<Vec3>) -> Self {
        let mut cumulative = Vec::with_capacity(points.len());
        let mut total = 0.0;
        for (i, p) in points.iter().enumerate() {
            if i > 0 {
                total += p.distance(points[i - 1]);
            }
            cumulative.push(total);
        }
        Self { points, cumulative }
    }

    pub fn points(&self) -> &[Vec3] {
        &self.points
    }

    pub fn length(&self) -> f32 {
        self.cumulative.last().copied().unwrap_or(0.0)
    }

    /// The flat point `along` yards down the path, clamped to its ends.
    pub fn point_at(&self, along: f32) -> Vec3 {
        let along = along.clamp(0.0, self.length());
        let i = self
            .cumulative
            .partition_point(|&c| c <= along)
            .clamp(1, self.points.len() - 1);
        let (a, b) = (self.points[i - 1], self.points[i]);
        let span = self.cumulative[i] - self.cumulative[i - 1];
        let t = if span > 0.0 {
            (along - self.cumulative[i - 1]) / span
        } else {
            0.0
        };
        a.lerp(b, t)
    }

    /// The direction of the first segment, for a spawn's facing.
    pub fn start_direction(&self) -> Vec3 {
        (self.points[1] - self.points[0]).normalize_or(Vec3::NEG_Z)
    }
}

/// Reads the level named by `$TD_LEVEL`, else the crate's Goldshire level.
pub fn load() -> Result<Level, String> {
    let path = std::env::var("TD_LEVEL").unwrap_or_else(|_| {
        concat!(env!("CARGO_MANIFEST_DIR"), "/levels/goldshire.ron").to_string()
    });
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{path}: {e}"))?;
    let file: LevelFile = ron::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    if file.path.len() < 2 {
        return Err(format!("{path}: the path needs at least two waypoints"));
    }
    if file.waves.is_empty() {
        return Err(format!("{path}: no waves"));
    }
    let points: Vec<Vec3> = file
        .path
        .iter()
        .map(|&(x, y)| wow_to_bevy([x, y, 0.0]))
        .collect();
    let (cx, cy, cz) = file.arena_center;
    let level = Level {
        name: file.name,
        map_id: file.map_id,
        center: [cx, cy, cz],
        lives: file.lives,
        path: EnemyPath::new(points),
        waves: file.waves,
    };
    info!(
        "level: {} from {path} — map {}, path {:.0} yd over {} waypoints, {} waves",
        level.name,
        level.map_id,
        level.path.length(),
        level.path.points.len(),
        level.waves.len()
    );
    Ok(level)
}
