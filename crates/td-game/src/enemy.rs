//! Enemies: creatures walking the level's path by arc length, grounded on the terrain, turning
//! smoothly into their direction of travel. One that reaches the end despawns and costs a life.

use bevy::prelude::*;

use benilla_world::world_point::TerrainHeights;

use crate::anim::{CreatureAnim, Gait};
use crate::level::Level;
use crate::waves::Lives;

/// How far ahead the facing aims, in yards, so a corner is turned into rather than snapped.
const LOOK_AHEAD: f32 = 2.0;

/// How quickly the facing closes on its aim, per second (exponential).
const TURN_SHARPNESS: f32 = 8.0;

#[derive(Component)]
pub struct Enemy {
    /// The wave it belongs to, 0-based.
    pub wave: usize,
    /// Yards covered along the path.
    pub along: f32,
    /// Ground speed in yards per second.
    pub speed: f32,
    pub health: f32,
}

#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnemyMove;

pub fn plugin(app: &mut App) {
    app.add_systems(Update, follow_path.in_set(EnemyMove));
}

fn follow_path(
    mut commands: Commands,
    time: Res<Time>,
    level: Res<Level>,
    heights: TerrainHeights,
    mut lives: ResMut<Lives>,
    mut enemies: Query<(Entity, &mut Enemy, &mut Transform, &mut CreatureAnim)>,
) {
    let dt = time.delta_secs();
    let path = &level.path;
    let turn = 1.0 - (-TURN_SHARPNESS * dt).exp();
    for (entity, mut enemy, mut xf, mut anim) in &mut enemies {
        enemy.along += enemy.speed * dt;
        if enemy.along >= path.length() {
            commands.entity(entity).despawn();
            // Past defeat the stragglers still leave the road, but cost and report nothing.
            if lives.0 > 0 {
                lives.0 -= 1;
                info!(
                    "leak: a wave {} enemy reached the end with {} hp — lives {}",
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
        let flat = path.point_at(enemy.along);
        let y = heights.under(flat).unwrap_or(xf.translation.y);
        xf.translation = Vec3::new(flat.x, y, flat.z);

        let aim = path.point_at(enemy.along + LOOK_AHEAD) - flat;
        if aim.length_squared() > 1e-4 {
            let target = Transform::IDENTITY.looking_to(aim, Vec3::Y).rotation;
            xf.rotation = xf.rotation.slerp(target, turn);
        }
        anim.set(Gait::for_speed(enemy.speed), enemy.speed);
    }
}
