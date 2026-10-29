//! The battle's stores: Gold and Food pay for units, Lumber and Stone for structures. Start
//! amounts and income come from the level file; spending and refunds go through [`Stores`], which
//! logs every change so a run's economy can be read back from its log.

use bevy::prelude::*;
use serde::Deserialize;

use crate::level::Level;

/// An amount of each currency. Fractional, so a per-second trickle accumulates exactly; shown
/// rounded down.
#[derive(Deserialize, Clone, Copy, Debug, Default, PartialEq)]
#[serde(default)]
pub struct Amount {
    pub gold: f32,
    pub food: f32,
    pub lumber: f32,
    pub stone: f32,
}

impl Amount {
    pub fn scaled(self, k: f32) -> Self {
        Self {
            gold: self.gold * k,
            food: self.food * k,
            lumber: self.lumber * k,
            stone: self.stone * k,
        }
    }

    fn covers(&self, cost: &Amount) -> bool {
        self.gold >= cost.gold
            && self.food >= cost.food
            && self.lumber >= cost.lumber
            && self.stone >= cost.stone
    }

    fn add(&mut self, other: Amount, sign: f32) {
        self.gold += other.gold * sign;
        self.food += other.food * sign;
        self.lumber += other.lumber * sign;
        self.stone += other.stone * sign;
    }
}

impl std::fmt::Display for Amount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "gold {} food {} lumber {} stone {}",
            self.gold.floor(),
            self.food.floor(),
            self.lumber.floor(),
            self.stone.floor()
        )
    }
}

/// The level's economy block.
#[derive(Deserialize, Clone, Debug, Default)]
pub struct EconomyDef {
    pub start: Amount,
    /// Granted when a wave is cleared (its last enemy gone).
    pub per_wave: Amount,
    /// Lumber per second while waves are running.
    pub lumber_per_sec: f32,
}

/// What the player has.
#[derive(Resource)]
pub struct Stores(Amount);

impl Stores {
    pub fn amount(&self) -> Amount {
        self.0
    }

    pub fn can_afford(&self, cost: &Amount) -> bool {
        self.0.covers(cost)
    }

    /// Takes `cost` if it is covered; `false` (and nothing taken) if not.
    pub fn spend(&mut self, cost: Amount, what: &str) -> bool {
        if !self.can_afford(&cost) {
            return false;
        }
        self.0.add(cost, -1.0);
        info!("economy: spent on {what} — {}", self.0);
        true
    }

    pub fn grant(&mut self, amount: Amount, why: &str) {
        self.0.add(amount, 1.0);
        info!("economy: +{why} — {}", self.0);
    }

    /// A trickle, unlogged: it changes every frame.
    pub fn trickle(&mut self, amount: Amount) {
        self.0.add(amount, 1.0);
    }
}

pub fn plugin(app: &mut App) {
    app.add_systems(Startup, |mut commands: Commands, level: Res<Level>| {
        let start = level.economy.start;
        info!("economy: start — {start}");
        commands.insert_resource(Stores(start));
    });
}
