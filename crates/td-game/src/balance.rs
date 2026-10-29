//! The battle's running tallies, and the balance test: a scripted battle that ends itself and
//! prints one verdict line.
//!
//! - `TD_BALANCE=<name>`: when the waves are done (or the lives run out, or after
//!   `TD_BALANCE_MAX` game seconds, default 400) print `BALANCE <name>: …` and exit.
//! - `TD_TIME_SCALE=<x>`: run game time `x` times faster (real time, the bake and shots, does not
//!   change).
//! - `TD_WAVE_LIMIT=<n>`: play only the first `n` waves (applied as the level loads, in `boot`).
//!
//! The defenders come from the usual scripts: `TD_UNITS`, `TD_BUILD`, `TD_COMMANDER`.

use bevy::prelude::*;

use crate::economy::Stores;
use crate::level::Level;
use crate::waves::{Lives, WaveRunner};

/// What has happened so far this battle.
#[derive(Resource, Default)]
pub struct BattleStats {
    pub spawned: u32,
    pub kills: u32,
    pub leaks: u32,
    pub knockouts: u32,
    pub commander_deaths: u32,
}

#[derive(Resource)]
struct Balance {
    name: String,
    max_secs: f32,
    /// When the verdict was printed; the exit follows a beat later so the log flushes.
    done_at: Option<f32>,
}

pub fn plugin(app: &mut App) {
    app.init_resource::<BattleStats>()
        .add_systems(Startup, time_scale);
    if let Ok(name) = std::env::var("TD_BALANCE") {
        let max_secs = std::env::var("TD_BALANCE_MAX")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(400.0);
        app.insert_resource(Balance {
            name,
            max_secs,
            done_at: None,
        })
        .add_systems(Update, verdict);
    }
}

fn time_scale(mut time: ResMut<Time<Virtual>>) {
    if let Some(x) = std::env::var("TD_TIME_SCALE")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
    {
        time.set_relative_speed(x);
        info!("balance: game time runs at {x}×");
    }
}

#[allow(clippy::too_many_arguments)]
fn verdict(
    time: Res<Time>,
    real: Res<Time<Real>>,
    level: Res<Level>,
    lives: Res<Lives>,
    runner: Res<WaveRunner>,
    stats: Res<BattleStats>,
    stores: Res<Stores>,
    commander: Query<&crate::units::Unit, With<crate::commander::Commander>>,
    mut balance: ResMut<Balance>,
    mut exit: MessageWriter<AppExit>,
) {
    if let Some(t) = balance.done_at {
        if real.elapsed_secs() >= t + 1.0 {
            exit.write(AppExit::Success);
        }
        return;
    }
    let now = time.elapsed_secs();
    let why = if lives.0 == 0 {
        "defeat"
    } else if runner.finished {
        "waves done"
    } else if now >= balance.max_secs {
        "time limit"
    } else {
        return;
    };
    let total: u32 = level.waves.iter().map(|w| w.count).sum();
    let a = stores.amount();
    info!(
        "BALANCE {}: {why} at {now:.0}s — killed {}/{total}, leaked {} (lives {}/{}), spawned {}, \
         knockouts {}, commander deaths {} (health left {}), gold {:.0}",
        balance.name,
        stats.kills,
        stats.leaks,
        lives.0,
        level.lives,
        stats.spawned,
        stats.knockouts,
        stats.commander_deaths,
        commander
            .single()
            .map_or("-".to_string(), |u| format!("{:.0}", u.health.max(0.0))),
        a.gold,
    );
    balance.done_at = Some(real.elapsed_secs());
}
