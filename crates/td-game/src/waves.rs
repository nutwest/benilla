//! The wave runner: once the flow field is built and every wave's creature can spawn, it spawns
//! each wave's enemies at the level's spawns in turn, `interval` apart, then waits the next
//! wave's `delay`. It stops spawning when the lives run out.

use bevy::prelude::*;

use benilla_assets::coords::wow_to_bevy;
use benilla_world::world_point::TerrainHeights;

use crate::creature::CreatureSpawner;
use crate::economy::{Amount, Stores};
use crate::enemy::{Enemy, EnemyMove};
use crate::flow::Flow;
use crate::grid::Grid;
use crate::level::Level;
use crate::shot::ShotMoments;

/// Enemies on the road at once for the `mid_wave` shot moment.
const MID_WAVE_CROWD: usize = 6;

#[derive(Resource)]
pub struct Lives(pub u32);

#[derive(Resource, Default)]
struct WaveRunner {
    started: bool,
    /// The wave spawning now; `waves.len()` once all have spawned.
    index: usize,
    spawned: u32,
    /// Seconds to the next spawn.
    timer: f32,
    finished: bool,
    /// Waves whose clearing grant has been paid.
    paid: Vec<bool>,
}

pub fn plugin(app: &mut App) {
    app.init_resource::<WaveRunner>()
        .add_systems(Startup, |mut commands: Commands, level: Res<Level>| {
            commands.insert_resource(Lives(level.lives));
        })
        .add_systems(Update, (run_waves.before(EnemyMove), pay_income));
}

/// The level's income: a grant per wave cleared (all its enemies spawned and gone, leaked or,
/// later, killed) and the Lumber trickle while waves run.
fn pay_income(
    time: Res<Time>,
    level: Res<Level>,
    lives: Res<Lives>,
    mut runner: ResMut<WaveRunner>,
    mut stores: ResMut<Stores>,
    enemies: Query<&Enemy>,
) {
    if !runner.started || lives.0 == 0 {
        return;
    }
    if !runner.finished {
        stores.trickle(Amount {
            lumber: level.economy.lumber_per_sec * time.delta_secs(),
            ..default()
        });
    }
    runner.paid.resize(level.waves.len(), false);
    for wave in 0..runner.index.min(level.waves.len()) {
        if !runner.paid[wave] && !enemies.iter().any(|e| e.wave == wave) {
            runner.paid[wave] = true;
            stores.grant(level.economy.per_wave, &format!("wave {} cleared", wave + 1));
        }
    }
}

fn run_waves(
    time: Res<Time>,
    level: Res<Level>,
    lives: Res<Lives>,
    heights: TerrainHeights,
    grid: Res<Grid>,
    flow: Res<Flow>,
    mut runner: ResMut<WaveRunner>,
    mut spawner: CreatureSpawner,
    mut moments: ResMut<ShotMoments>,
    enemies: Query<&Enemy>,
) {
    // Nothing walks before the field does.
    if lives.0 == 0 || runner.finished || !flow.ready() {
        return;
    }
    if !runner.started {
        // Every display up front, so no wave stalls on a model mid-game.
        let mut all = true;
        for wave in &level.waves {
            all &= spawner.ready(wave.display_id);
        }
        if !all {
            return;
        }
        runner.started = true;
        runner.timer = level.waves[0].delay;
        info!("waves: {} ready, lives {}", level.waves.len(), lives.0);
    }

    let alive = enemies.iter().count();
    if alive >= MID_WAVE_CROWD {
        moments.announce("mid_wave");
    }

    let Some(wave) = level.waves.get(runner.index) else {
        if alive == 0 {
            runner.finished = true;
            info!("waves: all {} done — lives {}", level.waves.len(), lives.0);
        }
        return;
    };
    runner.timer -= time.delta_secs();
    if runner.timer > 0.0 {
        return;
    }
    // Spawns take turns; each enemy faces down the field from its spawn cell.
    let spawn = level.spawns[runner.spawned as usize % level.spawns.len()];
    let Some(ground) = heights.under(wow_to_bevy([spawn.x, spawn.y, 0.0])) else {
        return;
    };
    let facing = grid
        .cell_at(spawn)
        .map(|i| flow.direction(i))
        .filter(|d| *d != Vec2::ZERO)
        .unwrap_or_else(|| (level.exit_center - spawn).normalize_or(Vec2::X));
    let at = wow_to_bevy([spawn.x, spawn.y, ground]);
    let facing = wow_to_bevy([facing.x, facing.y, 0.0]);
    let Some(entity) = spawner.spawn(wave.display_id, at, facing) else {
        return;
    };
    spawner.commands().entity(entity).insert(Enemy {
        wave: runner.index,
        speed: wave.speed,
        health: wave.health,
    });
    if runner.spawned == 0 {
        info!(
            "wave {}/{}: {}x display {} at {} yd/s — lives {}",
            runner.index + 1,
            level.waves.len(),
            wave.count,
            wave.display_id,
            wave.speed,
            lives.0
        );
    }
    runner.spawned += 1;
    runner.timer = wave.interval;
    if runner.spawned >= wave.count {
        runner.index += 1;
        runner.spawned = 0;
        if let Some(next) = level.waves.get(runner.index) {
            runner.timer = next.delay;
        }
    }
}
