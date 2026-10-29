//! Combat: each unit picks a target by its behaviour and priority, walks (Guard) or waits (Hold)
//! until it is in range, then swings or throws on its interval. A melee hit lands a beat into the
//! swing; a ranged attack releases a projectile that homes on its target. An enemy at 0 health
//! dies: it leaves the field's steering, plays Death, holds the pose, feathers out through the
//! engine's own fade and is despawned, granting its bounty in Gold. Enemies do not fight back
//! yet (M6).

use bevy::prelude::*;

use benilla_assets::coords::wow_to_bevy;
use benilla_world::model_fade::{FadeCurve, FadeMaterials, RenderFade};
use benilla_world::world_point::TerrainHeights;

use crate::anim::{CreatureAnim, Gait};
use crate::economy::{Amount, Stores};
use crate::enemy::{Enemy, EnemyMove};
use crate::flow::Flow;
use crate::grid::Grid;
use crate::shot::ShotMoments;
use crate::fight_back::{KnockedOut, PlayerControlled};
use crate::units::{wow_xy, AttackKind, Behaviour, Priority, Unit, UnitDefs};

/// A guard chases a target this far past its radius before giving it up.
const LEASH: f32 = 5.0;

/// How far into a swing or a throw the hit lands or the projectile leaves, in seconds.
const HIT_DELAY: f32 = 0.35;

/// Slack on the attack range, in yards, so a unit at the edge does not step in and out.
const RANGE_SLACK: f32 = 0.3;

/// A guard back within this of its anchor stops walking.
const HOME: f32 = 0.5;

/// An escort with nothing to fight keeps within this share of its radius of its charge.
const ESCORT_SLACK: f32 = 0.4;

/// Seconds between path searches while a goal moves.
const REPATH: f32 = 0.4;

/// How quickly a unit turns to its target or its way, per second (exponential).
const TURN_SHARPNESS: f32 = 10.0;

/// Where a projectile leaves the thrower and meets the target, above the feet, in yards.
const CHEST: f32 = 1.2;

/// A projectile closer than this to its aim has hit.
const IMPACT: f32 = 0.4;

/// The length of a projectile's drawn streak, in yards.
const STREAK: f32 = 1.6;

/// A dead enemy holds its death pose this long, then feathers out over [`FADE_SECS`].
const DEATH_HOLD: f32 = 2.5;
const FADE_SECS: f32 = 2.0;

/// Damage to an enemy, from a swing or a projectile.
#[derive(Message)]
pub struct Hit {
    pub target: Entity,
    pub amount: f32,
}

/// A projectile in flight.
#[derive(Component)]
pub struct Projectile {
    target: Entity,
    damage: f32,
    speed: f32,
    /// Where it is headed: the target's chest, refreshed while the target lives.
    aim: Vec3,
    age: f32,
}

/// A dead body's teardown clock.
#[derive(Component)]
pub struct Dying {
    since: f32,
    fading: bool,
}

impl Dying {
    /// A body that died at `now`: it holds its pose, fades and is despawned.
    pub fn new(now: f32) -> Self {
        Self {
            since: now,
            fading: false,
        }
    }
}

pub fn plugin(app: &mut App) {
    app.add_message::<Hit>().add_systems(
        Update,
        (think, fly, resolve_hits, dying)
            .chain()
            .after(EnemyMove),
    );
}

/// An enemy as targeting sees it.
struct Candidate {
    entity: Entity,
    at: Vec2,
    health: f32,
    remaining: f32,
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn think(
    mut commands: Commands,
    time: Res<Time>,
    defs: Res<UnitDefs>,
    grid: Res<Grid>,
    flow: Res<Flow>,
    heights: TerrainHeights,
    mut hits: MessageWriter<Hit>,
    mut moments: ResMut<ShotMoments>,
    mut units: Query<
        (&mut Unit, &mut Transform, &mut CreatureAnim),
        (Without<KnockedOut>, Without<PlayerControlled>),
    >,
    charges: Query<&GlobalTransform>,
    enemies: Query<(Entity, &Transform, &Enemy), Without<Unit>>,
) {
    let now = time.elapsed_secs();
    let dt = time.delta_secs();
    let turn = 1.0 - (-TURN_SHARPNESS * dt).exp();
    let candidates: Vec<Candidate> = enemies
        .iter()
        .map(|(entity, xf, e)| {
            let at = wow_xy(xf.translation);
            Candidate {
                entity,
                at,
                health: e.health,
                remaining: flow.remaining(&grid, at),
            }
        })
        .collect();
    let find = |e: Entity| candidates.iter().find(|c| c.entity == e);

    for (mut u, mut xf, mut anim) in &mut units {
        let d = defs.get(u.def);
        let here = wow_xy(xf.translation);
        u.cooldown -= dt;

        // A swing or release whose moment has come.
        if let Some((target, at)) = u.pending {
            if now >= at {
                u.pending = None;
                if let Some(c) = find(target) {
                    match d.attack {
                        AttackKind::Melee => {
                            if c.at.distance(here) <= d.attack_range + 1.0 {
                                hits.write(Hit {
                                    target,
                                    amount: d.damage,
                                });
                                moments.announce("melee");
                            }
                        }
                        AttackKind::Ranged { projectile_speed } => {
                            // Launched from the thrower's hand height.
                            let from = xf.translation + Vec3::Y * CHEST;
                            commands.spawn((
                                Projectile {
                                    target,
                                    damage: d.damage,
                                    speed: projectile_speed,
                                    aim: from,
                                    age: 0.0,
                                },
                                Transform::from_translation(from),
                            ));
                        }
                    }
                }
            }
        }

        // An escort's post moves with its charge; a charge gone (dead, sold) leaves it where it was.
        if u.behaviour == Behaviour::Escort {
            if let Some(at) = u.escort.and_then(|e| charges.get(e).ok()) {
                u.anchor = wow_xy(at.translation());
            }
        }

        // Keep the target while it stays fair game; else pick one by priority.
        let zone = |c: &Candidate, slack: f32| match u.behaviour {
            Behaviour::Hold => c.at.distance(here) <= d.attack_range + RANGE_SLACK + slack,
            Behaviour::Guard | Behaviour::Escort => {
                c.at.distance(u.anchor) <= u.guard_radius + slack
            }
        };
        let leash = if u.behaviour == Behaviour::Hold { 0.0 } else { LEASH };
        // A guard walks all the way home; an escort only back within reach of its charge.
        let home = match u.behaviour {
            Behaviour::Escort => (u.guard_radius * ESCORT_SLACK).max(HOME),
            _ => HOME,
        };
        let keep = u.target.and_then(find).is_some_and(|c| zone(c, leash));
        if !keep {
            u.target = pick(u.priority, here, candidates.iter().filter(|c| zone(c, 0.0)));
        }

        let target = u.target.and_then(find);
        let goal = match (u.behaviour, target) {
            (_, Some(c)) if c.at.distance(here) <= d.attack_range + RANGE_SLACK => {
                // In range: stop, face it and attack on the interval.
                face(&mut xf, c.at - here, turn);
                anim.set(Gait::Stand, 0.0);
                u.path.clear();
                if u.cooldown <= 0.0 && u.pending.is_none() {
                    anim.attack(&d.attack_anims);
                    u.cooldown = d.attack_interval;
                    u.pending = Some((c.entity, now + HIT_DELAY));
                }
                None
            }
            (Behaviour::Guard | Behaviour::Escort, Some(c)) => Some(c.at),
            (Behaviour::Guard | Behaviour::Escort, None) if here.distance(u.anchor) > home => {
                Some(u.anchor)
            }
            _ => {
                anim.set(Gait::Stand, 0.0);
                None
            }
        };
        if let Some(goal) = goal {
            walk(&mut u, &mut xf, &mut anim, &grid, &heights, d.move_speed, here, goal, dt, turn);
        }
    }
}

fn pick<'a>(
    priority: Priority,
    here: Vec2,
    candidates: impl Iterator<Item = &'a Candidate>,
) -> Option<Entity> {
    let key = |c: &Candidate| match priority {
        Priority::First => c.remaining,
        Priority::Nearest => c.at.distance(here),
        Priority::Strongest => -c.health,
        Priority::Weakest => c.health,
    };
    candidates
        .min_by(|a, b| key(a).total_cmp(&key(b)))
        .map(|c| c.entity)
}

fn face(xf: &mut Transform, dir: Vec2, turn: f32) {
    let facing = wow_to_bevy([dir.x, dir.y, 0.0]);
    if facing.length_squared() > 1e-4 {
        let target = Transform::IDENTITY.looking_to(facing, Vec3::Y).rotation;
        xf.rotation = xf.rotation.slerp(target, turn);
    }
}

/// One step towards `goal` over the grid: a fresh path when the goal has moved or the timer is
/// up, then along it at the unit's speed, grounded.
#[allow(clippy::too_many_arguments)]
fn walk(
    u: &mut Unit,
    xf: &mut Transform,
    anim: &mut CreatureAnim,
    grid: &Grid,
    heights: &TerrainHeights,
    speed: f32,
    here: Vec2,
    goal: Vec2,
    dt: f32,
    turn: f32,
) {
    u.repath_in -= dt;
    let moved = u.path_goal.is_none_or(|g| g.distance(goal) > 1.0);
    if u.path.is_empty() || moved || u.repath_in <= 0.0 {
        u.path = crate::path::find(grid, here, goal).unwrap_or_default();
        u.path_goal = Some(goal);
        u.repath_in = REPATH;
    }
    let Some(&next) = u.path.first() else {
        anim.set(Gait::Stand, 0.0);
        return;
    };
    let to = next - here;
    let step = (speed * dt).min(to.length());
    let at = here + to.normalize_or_zero() * step;
    if at.distance(next) < 0.05 {
        u.path.remove(0);
    }
    let flat = wow_to_bevy([at.x, at.y, 0.0]);
    let ground = heights.under(flat).unwrap_or(xf.translation.y);
    xf.translation = Vec3::new(flat.x, ground, flat.z);
    face(xf, to, turn);
    anim.set(Gait::for_speed(speed), speed);
}

fn fly(
    mut commands: Commands,
    time: Res<Time>,
    mut hits: MessageWriter<Hit>,
    targets: Query<&Transform, (With<Enemy>, Without<Projectile>)>,
    mut projectiles: Query<(Entity, &mut Projectile, &mut Transform)>,
    mut gizmos: Gizmos,
) {
    let dt = time.delta_secs();
    for (entity, mut p, mut xf) in &mut projectiles {
        p.age += dt;
        if let Ok(t) = targets.get(p.target) {
            p.aim = t.translation + Vec3::Y * CHEST;
        }
        let to = p.aim - xf.translation;
        let step = p.speed * dt;
        if to.length() <= step.max(IMPACT) || p.age > 3.0 {
            if targets.contains(p.target) {
                hits.write(Hit {
                    target: p.target,
                    amount: p.damage,
                });
            }
            commands.entity(entity).despawn();
            continue;
        }
        let dir = to.normalize();
        xf.translation += dir * step;
        // A short streak behind the head, with a bright point at it.
        gizmos.line(
            xf.translation - dir * STREAK,
            xf.translation,
            Color::srgb(1.0, 0.85, 0.4),
        );
        gizmos.sphere(
            Isometry3d::from_translation(xf.translation),
            0.12,
            Color::srgb(1.0, 1.0, 0.7),
        );
    }
}

fn resolve_hits(
    mut commands: Commands,
    time: Res<Time>,
    mut hits: MessageReader<Hit>,
    mut stores: ResMut<Stores>,
    mut stats: ResMut<crate::balance::BattleStats>,
    mut enemies: Query<(&mut Enemy, &mut CreatureAnim)>,
) {
    for hit in hits.read() {
        let Ok((mut enemy, mut anim)) = enemies.get_mut(hit.target) else {
            continue;
        };
        if anim.is_dead() {
            continue;
        }
        enemy.health -= hit.amount;
        if enemy.health > 0.0 {
            continue;
        }
        anim.die();
        commands.entity(hit.target).remove::<Enemy>().insert(Dying {
            since: time.elapsed_secs(),
            fading: false,
        });
        info!("kill: a wave {} enemy", enemy.wave + 1);
        stats.kills += 1;
        stores.grant(
            Amount {
                gold: enemy.bounty,
                ..default()
            },
            &format!("{} gold bounty", enemy.bounty),
        );
    }
}

fn dying(
    mut commands: Commands,
    time: Res<Time>,
    mut moments: ResMut<ShotMoments>,
    mut dead: Query<(Entity, &mut Dying, &Children)>,
    parts: Query<(), With<FadeMaterials>>,
    projectiles: Query<(), With<Projectile>>,
) {
    let now = time.elapsed_secs();
    for (entity, mut d, children) in &mut dead {
        let age = now - d.since;
        if age >= 0.7 {
            moments.announce("death_pose");
            // A death pose with a projectile in the air: the mid-fight shot.
            if !projectiles.is_empty() {
                moments.announce("fight");
            }
        }
        if age >= DEATH_HOLD && !d.fading {
            for child in children.iter() {
                if parts.contains(child) {
                    commands.entity(child).insert(RenderFade {
                        started: now,
                        duration: FADE_SECS,
                        from: 1.0,
                        to: 0.0,
                        curve: FadeCurve::Smoothstep,
                    });
                }
            }
            d.fading = true;
        }
        if age >= DEATH_HOLD + FADE_SECS {
            commands.entity(entity).despawn();
        }
    }
}
