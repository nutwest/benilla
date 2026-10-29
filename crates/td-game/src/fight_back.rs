//! Enemies fight back (M6). An enemy marching down the flow field that has one of the player's
//! units within its aggro radius leaves the field to fight it, then marches on once the unit is
//! down or out of reach. A melee unit holds up to `blocks` enemies at once (the commander 3, a
//! footman 2): further enemies walk past it. A unit that holds none (an archer) can still be
//! engaged, by one enemy at a time. A garrisoned unit (M7) is out of a melee enemy's reach.
//!
//! A unit at 0 health is knocked out, not killed: it plays Death, lies still and cannot act or be
//! targeted, and after its `recovery_secs` it stands up at its post with half its health. The
//! commander falls instead ([`crate::commander`]), and respawns by the exit.

use std::collections::HashMap;

use bevy::prelude::*;

use benilla_assets::coords::wow_to_bevy;
use benilla_world::world_point::TerrainHeights;

use crate::anim::{CreatureAnim, Gait};
use crate::balance::BattleStats;
use crate::commander::Commander;
use crate::enemy::{Enemy, EnemyMove};
use crate::grid::Grid;
use crate::shot::ShotMoments;
use crate::units::{wow_xy, Unit, UnitDefs};

/// An engaged enemy lets its unit go once it is this many aggro radii away.
const LEASH: f32 = 2.0;

/// Added to an enemy's reach: the unit's body, in yards.
const BODY: f32 = 0.6;

/// A swing lands this long after it starts.
const HIT_DELAY: f32 = 0.35;

/// How quickly an engaged enemy turns to its unit, per second (exponential).
const TURN_SHARPNESS: f32 = 10.0;

/// A knocked-out body has finished falling after this long, for the shot moment.
const SETTLED: f32 = 1.2;

/// Damage to one of the player's units (the commander included).
#[derive(Message)]
pub struct UnitHit {
    pub target: Entity,
    pub amount: f32,
}

/// A unit down for now: no acting, no being targeted, until `until` (`Time` seconds).
#[derive(Component)]
pub struct KnockedOut {
    since: f32,
    until: f32,
}

/// The unit the player steers directly (the commander in commander mode): the unit AI leaves it
/// alone, and enemies still engage it.
#[derive(Component)]
pub struct PlayerControlled;

pub fn plugin(app: &mut App) {
    app.add_message::<UnitHit>().add_systems(
        Update,
        (engage, take_hits, recover, watch).chain().after(EnemyMove),
    );
}

/// A unit as an enemy sees it.
struct Prey {
    entity: Entity,
    at: Vec2,
    capacity: usize,
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn engage(
    time: Res<Time>,
    grid: Res<Grid>,
    heights: TerrainHeights,
    defs: Res<UnitDefs>,
    mut hits: MessageWriter<UnitHit>,
    mut enemies: Query<(&mut Enemy, &mut Transform, &mut CreatureAnim), Without<Unit>>,
    units: Query<(Entity, &Transform, &Unit), (Without<Enemy>, Without<KnockedOut>)>,
) {
    let now = time.elapsed_secs();
    let dt = time.delta_secs();
    let turn = 1.0 - (-TURN_SHARPNESS * dt).exp();
    let prey: Vec<Prey> = units
        .iter()
        .filter(|(_, _, u)| !u.garrisoned)
        .map(|(entity, xf, u)| Prey {
            entity,
            at: wow_xy(xf.translation),
            capacity: defs.get(u.def).blocks.max(1) as usize,
        })
        .collect();
    let find = |e: Entity| prey.iter().find(|p| p.entity == e);

    // Who is held by whom, after letting go of the dead, the downed and the far.
    let mut held: HashMap<Entity, usize> = HashMap::new();
    for (mut e, xf, _) in &mut enemies {
        let here = wow_xy(xf.translation);
        let keep = e
            .engaged
            .and_then(find)
            .is_some_and(|p| p.at.distance(here) <= e.fight.aggro_radius * LEASH);
        if keep {
            *held.entry(e.engaged.unwrap_or(Entity::PLACEHOLDER)).or_default() += 1;
        } else if e.engaged.take().is_some() {
            e.pending = None;
        }
    }

    for (mut e, mut xf, mut anim) in &mut enemies {
        let here = wow_xy(xf.translation);
        e.cooldown -= dt;
        // Pick up the nearest unit in reach that has a slot free.
        if e.engaged.is_none() {
            let pick = prey
                .iter()
                .filter(|p| p.at.distance(here) <= e.fight.aggro_radius)
                .filter(|p| held.get(&p.entity).copied().unwrap_or(0) < p.capacity)
                .min_by(|a, b| a.at.distance(here).total_cmp(&b.at.distance(here)));
            if let Some(p) = pick {
                e.engaged = Some(p.entity);
                *held.entry(p.entity).or_default() += 1;
            }
        }
        let Some(target) = e.engaged.and_then(find) else {
            continue;
        };
        let (target_entity, target_at) = (target.entity, target.at);

        // A swing whose moment has come.
        if let Some((t, land)) = e.pending {
            if now >= land {
                e.pending = None;
                if t == target_entity
                    && target_at.distance(here) <= e.fight.attack_range + BODY + 1.0
                {
                    hits.write(UnitHit {
                        target: t,
                        amount: e.fight.damage,
                    });
                }
            }
        }

        let to = target_at - here;
        let reach = e.fight.attack_range + BODY;
        let mut at = here;
        if to.length() > reach {
            // Close in, sliding along what blocks the way.
            let step = to.normalize_or_zero() * e.speed * dt;
            for s in [step, Vec2::new(step.x, 0.0), Vec2::new(0.0, step.y)] {
                if grid.walkable_at(here + s) {
                    at = here + s;
                    break;
                }
            }
            anim.set(Gait::for_speed(e.speed), e.speed);
        } else {
            anim.set(Gait::Stand, 0.0);
            if e.cooldown <= 0.0 && e.pending.is_none() {
                anim.attack(&e.fight.attack_anims);
                e.cooldown = e.fight.attack_interval;
                e.pending = Some((target_entity, now + HIT_DELAY));
            }
        }
        let flat = wow_to_bevy([at.x, at.y, 0.0]);
        let ground = heights.under(flat).unwrap_or(xf.translation.y);
        xf.translation = Vec3::new(flat.x, ground, flat.z);
        let facing = wow_to_bevy([to.x, to.y, 0.0]);
        if facing.length_squared() > 1e-4 {
            let want = Transform::IDENTITY.looking_to(facing, Vec3::Y).rotation;
            xf.rotation = xf.rotation.slerp(want, turn);
        }
    }
}

#[allow(clippy::type_complexity)]
fn take_hits(
    mut commands: Commands,
    time: Res<Time>,
    defs: Res<UnitDefs>,
    mut hits: MessageReader<UnitHit>,
    mut stats: ResMut<BattleStats>,
    mut moments: ResMut<ShotMoments>,
    mut units: Query<
        (&mut Unit, &mut CreatureAnim, Has<Commander>),
        Without<KnockedOut>,
    >,
) {
    for hit in hits.read() {
        let Ok((mut u, mut anim, commander)) = units.get_mut(hit.target) else {
            continue;
        };
        if u.health <= 0.0 {
            continue;
        }
        u.health -= hit.amount;
        // The commander falls rather than kneels; `commander::fall` takes it from here.
        if u.health > 0.0 || commander {
            continue;
        }
        let d = defs.get(u.def);
        anim.die();
        u.target = None;
        u.pending = None;
        u.path.clear();
        let now = time.elapsed_secs();
        commands.entity(hit.target).insert(KnockedOut {
            since: now,
            until: now + d.recovery_secs,
        });
        stats.knockouts += 1;
        moments.announce("knockout");
        info!("unit: {} knocked out — up again in {:.0}s", d.name, d.recovery_secs);
    }
}

/// A knocked-out unit stands up at its post with half its health.
fn recover(
    mut commands: Commands,
    time: Res<Time>,
    defs: Res<UnitDefs>,
    heights: TerrainHeights,
    mut downed: Query<(Entity, &KnockedOut, &mut Unit, &mut Transform, &mut CreatureAnim)>,
) {
    let now = time.elapsed_secs();
    for (entity, ko, mut u, mut xf, mut anim) in &mut downed {
        if now < ko.until {
            continue;
        }
        let d = defs.get(u.def);
        u.health = d.health * 0.5;
        u.cooldown = 0.0;
        let post = wow_to_bevy([u.anchor.x, u.anchor.y, 0.0]);
        let ground = heights.under(post).unwrap_or(xf.translation.y);
        xf.translation = Vec3::new(post.x, ground, post.z);
        anim.revive();
        commands.entity(entity).remove::<KnockedOut>();
        info!("unit: {} back on its feet at its post", d.name);
    }
}

/// The shot moment of a knocked-out unit, settled on the ground, with an enemy walking past it.
fn watch(
    time: Res<Time>,
    mut moments: ResMut<ShotMoments>,
    downed: Query<(&Transform, &KnockedOut)>,
    enemies: Query<(&Transform, &Enemy)>,
) {
    for (down, ko) in &downed {
        if time.elapsed_secs() - ko.since < SETTLED {
            continue;
        }
        let here = wow_xy(down.translation);
        let passing = enemies.iter().any(|(xf, e)| {
            e.engaged.is_none() && wow_xy(xf.translation).distance(here) < 8.0
        });
        if passing {
            moments.announce("knockout_pass");
        }
    }
}
