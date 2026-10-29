//! Enemies: creatures steering down the flow field towards the exit, grounded on the terrain,
//! turning smoothly into their direction of travel and keeping a little apart. One that reaches
//! the exit despawns and costs a life.

use bevy::prelude::*;

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_world::world_point::TerrainHeights;

use crate::anim::{CreatureAnim, Gait};
use crate::flow::Flow;
use crate::grid::Grid;
use crate::level::Level;
use crate::waves::Lives;

/// How quickly the facing closes on the travel direction, per second (exponential).
const TURN_SHARPNESS: f32 = 8.0;

/// Enemies closer than this push apart, in yards, and how hard (yards per second at contact).
const SEPARATION: f32 = 1.2;
const SEPARATION_PUSH: f32 = 1.5;

#[derive(Component)]
pub struct Enemy {
    /// The wave it belongs to, 0-based.
    pub wave: usize,
    /// Ground speed in yards per second.
    pub speed: f32,
    pub health: f32,
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnemyMove;

pub fn plugin(app: &mut App) {
    app.add_systems(
        Update,
        steer.in_set(EnemyMove).after(crate::flow::rebuild),
    );
}

fn steer(
    mut commands: Commands,
    time: Res<Time>,
    level: Res<Level>,
    grid: Res<Grid>,
    flow: Res<Flow>,
    heights: TerrainHeights,
    mut lives: ResMut<Lives>,
    mut enemies: Query<(Entity, &Enemy, &mut Transform, &mut CreatureAnim)>,
) {
    if !flow.ready() {
        return;
    }
    let dt = time.delta_secs();
    let turn = 1.0 - (-TURN_SHARPNESS * dt).exp();
    // Positions this frame, WoW (x, y), for the separation pass.
    let positions: Vec<(Entity, Vec2)> = enemies
        .iter()
        .map(|(e, _, xf, _)| (e, wow_xy(xf.translation)))
        .collect();

    for (entity, enemy, mut xf, mut anim) in &mut enemies {
        let here = wow_xy(xf.translation);
        if here.distance(level.exit_center) <= level.exit_radius {
            commands.entity(entity).despawn();
            // Past defeat the stragglers still leave the road, but cost and report nothing.
            if lives.0 > 0 {
                lives.0 -= 1;
                info!(
                    "leak: a wave {} enemy reached the exit with {} hp — lives {}",
                    enemy.wave + 1,
                    enemy.health,
                    lives.0
                );
                if lives.0 == 0 {
                    info!("defeat: no lives left");
                }
            }
            continue;
        }

        // The field's direction; at its edge (the exit cells, a cut-off pocket) head straight for
        // the exit, which only the last yard before it should ever need.
        let heading = flow
            .steer(&grid, here)
            .unwrap_or_else(|| (level.exit_center - here).normalize_or_zero());

        let mut push = Vec2::ZERO;
        for &(other, there) in &positions {
            let d = here.distance(there);
            if other != entity && d > 1e-3 && d < SEPARATION {
                push += (here - there) / d * (SEPARATION - d) / SEPARATION;
            }
        }
        let velocity = heading * enemy.speed + push * SEPARATION_PUSH;
        let next = slide(&grid, here, velocity * dt);

        let ground = heights
            .under(wow_to_bevy([next.x, next.y, 0.0]))
            .unwrap_or(xf.translation.y);
        let at = wow_to_bevy([next.x, next.y, ground]);
        xf.translation = at;

        let facing = wow_to_bevy([heading.x, heading.y, 0.0]);
        if facing.length_squared() > 1e-4 {
            let target = Transform::IDENTITY.looking_to(facing, Vec3::Y).rotation;
            xf.rotation = xf.rotation.slerp(target, turn);
        }
        anim.set(Gait::for_speed(enemy.speed), enemy.speed);
    }
}

/// The move from `from` by `delta`, sliding along blocked cells: the whole step if it lands on a
/// walkable cell, else each axis alone, else none. An enemy already on a blocked cell (one the
/// test block dropped on it) moves freely, as the field leads it out.
fn slide(grid: &Grid, from: Vec2, delta: Vec2) -> Vec2 {
    if !grid.walkable_at(from) {
        return from + delta;
    }
    for step in [delta, Vec2::new(delta.x, 0.0), Vec2::new(0.0, delta.y)] {
        if grid.walkable_at(from + step) {
            return from + step;
        }
    }
    from
}

fn wow_xy(bevy: Vec3) -> Vec2 {
    let w = bevy_to_wow(bevy);
    Vec2::new(w[0], w[1])
}
