//! The commander: one creature per battle that the player can take direct control of. It stands
//! by the exit at the start; V switches between the RTS view and commander mode, blending the
//! camera ([`crate::camera`]).
//!
//! Commander mode plays like WoW: W and S run and backpedal, A and D turn (strafe while the right
//! button is held), Q and E strafe, a right drag turns the commander and the camera, a left drag
//! orbits the camera alone, both buttons run forward, the wheel zooms and Space jumps. The camera
//! follows behind, rides above the terrain, and pulls in when world geometry comes between it and
//! the commander. Tab cycles targets among nearby enemies, and the commander auto-attacks its
//! target in range.
//!
//! Left to itself it is a unit ([`Unit`]) like any other, selectable in RTS mode, with Hold, Guard
//! or the commander-only Escort (stay within its radius of a chosen unit or structure and defend
//! it); leaving commander mode drops its post where it stands. Either way it holds up to `blocks`
//! enemies in melee and enemies fight it. If it dies it plays Death, the view is forced back to
//! RTS, and it respawns by the exit after `respawn_secs`, keeping its behaviour settings.
//!
//! For shots: `TD_COMMANDER_AT=<x>,<y>` places it elsewhere, and `TD_COMMANDER` takes
//! comma-separated `enter@<s>` (switch to commander mode at that time), `auto` (in commander mode,
//! target the nearest enemy and close on it), `face@<deg>` (the first spawn's facing, WoW yaw) and
//! `kill@<s>` (drop it to 0 health). In a debug build F9 does the same kill by hand.

use bevy::input::mouse::{AccumulatedMouseMotion, MouseWheel};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, PrimaryWindow};

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_world::collision::WorldCollision;
use benilla_world::world_point::TerrainHeights;

use crate::anim::{CreatureAnim, Gait};
use crate::camera::{current_pose, CamPose, CameraApply, CommanderPose, ControlMode, Mode, RtsCam};
use crate::combat::{Dying, Hit};
use crate::creature::CreatureSpawner;
use crate::enemy::Enemy;
use crate::flow::Flow;
use crate::grid::Grid;
use crate::level::Level;
use crate::shot::ShotMoments;
use crate::fight_back::PlayerControlled;
use crate::units::{ground_circle, wow_xy, Behaviour, Priority, Unit, UnitDefs};

/// Where the commander stands at the start, yards from the exit back along the last road.
const HOME_FROM_EXIT: f32 = 6.0;

/// Backpedal speed as a share of the run, and keyboard turn rate (WoW's 180°/s).
const BACKPEDAL: f32 = 0.6;
const TURN_RATE: f32 = std::f32::consts::PI;

/// Mouse look, radians per pixel, and the camera pitch's range above the horizon.
const LOOK: f32 = 0.005;
const PITCH_MIN: f32 = -0.45;
const PITCH_MAX: f32 = 1.35;

/// The follow camera's zoom range and opening distance, in yards, and its field of view.
const ZOOM_MIN: f32 = 2.0;
const ZOOM_MAX: f32 = 30.0;
const ZOOM_START: f32 = 9.0;
const FOV: f32 = 1.15;

/// The camera's pivot above the commander's feet, per unit of scale; and how far it stays off
/// whatever it collides with and above the ground.
const EYE: f32 = 1.7;
const CAMERA_SKIN: f32 = 0.25;
const CAMERA_ABOVE_GROUND: f32 = 0.4;

/// How quickly the boom grows back after a collision pulled it in, and the camera swings back
/// behind a running commander after an orbit, per second.
const BOOM_RECOVER: f32 = 4.0;
const SWING_BACK: f32 = 3.0;

/// Jump take-off speed and gravity (WoW's 19.29 yd/s²).
const JUMP_SPEED: f32 = 8.0;
const GRAVITY: f32 = 19.291;

/// Tab looks for enemies this close; a target farther than [`TARGET_DROP`] is let go.
const TAB_RANGE: f32 = 30.0;
const TARGET_DROP: f32 = 40.0;

/// A swing lands this long after it starts.
const HIT_DELAY: f32 = 0.35;

#[derive(Component)]
pub struct Commander {
    pub target: Option<Entity>,
    /// Facing, Bevy yaw: forward is `rot_y(yaw) · −Z`.
    yaw: f32,
    cooldown: f32,
    pending: Option<(Entity, f32)>,
    /// Height above the ground while jumping, and its vertical speed.
    lift: f32,
    lift_speed: f32,
}

/// The commander's life and home, across deaths.
#[derive(Resource, Default)]
pub struct CommanderState {
    pub entity: Option<Entity>,
    /// When a fallen commander comes back (`Time` seconds).
    pub respawn_at: Option<f32>,
    home: Option<Vec2>,
    /// The behaviour, priority and radius it had when it fell, for the next body.
    settings: Option<(Behaviour, Priority, f32)>,
}

/// The follow camera: its orbit around the commander and its boom.
#[derive(Resource)]
struct FollowCam {
    yaw: f32,
    pitch: f32,
    /// The zoom the player asked for.
    distance: f32,
    /// The boom as drawn, shorter while something is in the way.
    boom: f32,
}

#[derive(Resource, Default)]
struct Script {
    enter_at: Option<f32>,
    kill_at: Option<f32>,
    auto: bool,
    /// A first spawn's facing, degrees of WoW yaw (0 north, 90 west).
    face: Option<f32>,
}

pub fn plugin(app: &mut App) {
    let mut script = Script::default();
    if let Ok(v) = std::env::var("TD_COMMANDER") {
        for part in v.split(',').map(str::trim) {
            if let Some(t) = part.strip_prefix("enter@") {
                script.enter_at = t.parse().ok();
            } else if let Some(t) = part.strip_prefix("kill@") {
                script.kill_at = t.parse().ok();
            } else if let Some(d) = part.strip_prefix("face@") {
                script.face = d.parse().ok();
            } else if part == "auto" {
                script.auto = true;
            }
        }
    }
    app.init_resource::<CommanderState>()
        .insert_resource(FollowCam {
            yaw: 0.0,
            pitch: 0.35,
            distance: ZOOM_START,
            boom: ZOOM_START,
        })
        .insert_resource(script)
        .add_systems(
            Update,
            (spawn, toggle, control, fall, follow_pose, draw)
                .chain()
                .before(CameraApply),
        );
}

/// The commander's home: `TD_COMMANDER_AT`, else a few yards from the exit back along the last
/// road, on the nearest walkable cell.
fn home(level: &Level, grid: &Grid) -> Option<Vec2> {
    if let Ok(v) = std::env::var("TD_COMMANDER_AT") {
        let c: Vec<f32> = v.split(',').filter_map(|p| p.trim().parse().ok()).collect();
        if let [x, y] = c[..] {
            return Some(Vec2::new(x, y));
        }
    }
    let back = level
        .roads
        .first()
        .and_then(|r| r.points.iter().rev().nth(1).copied())
        .map_or(Vec2::X, |p| (p - level.exit_center).normalize_or(Vec2::X));
    let want = level.exit_center + back * HOME_FROM_EXIT;
    (0..grid.len())
        .filter(|&i| !grid.blocked(i))
        .min_by(|&a, &b| {
            grid.center(a)
                .distance(want)
                .total_cmp(&grid.center(b).distance(want))
        })
        .map(|i| grid.center(i))
}

#[allow(clippy::too_many_arguments)]
fn spawn(
    time: Res<Time>,
    level: Res<Level>,
    grid: Res<Grid>,
    flow: Res<Flow>,
    defs: Res<UnitDefs>,
    heights: TerrainHeights,
    mut state: ResMut<CommanderState>,
    script: Res<Script>,
    mut spawner: CreatureSpawner,
) {
    if state.entity.is_some() || !flow.ready() {
        return;
    }
    if state.respawn_at.is_some_and(|t| time.elapsed_secs() < t) {
        return;
    }
    let d = &defs.commander.unit;
    if !spawner.ready(d.display_id) {
        return;
    }
    if state.home.is_none() {
        state.home = home(&level, &grid);
    }
    let Some(at) = state.home else { return };
    let Some(ground) = heights.under(wow_to_bevy([at.x, at.y, 0.0])) else {
        return;
    };
    // Facing up the road, towards what is coming.
    let up_road = (-flow
        .steer(&grid, at)
        .unwrap_or((level.exit_center - at).normalize_or(Vec2::X)))
    .normalize_or(Vec2::X);
    let up_road = script
        .face
        .map_or(up_road, |deg| Vec2::from_angle(deg.to_radians()));
    let facing = wow_to_bevy([up_road.x, up_road.y, 0.0]);
    let Some(entity) = spawner.spawn(d.display_id, wow_to_bevy([at.x, at.y, ground]), facing)
    else {
        return;
    };
    let yaw = facing.x.atan2(facing.z) + std::f32::consts::PI;
    // As a unit it guards its post until told otherwise; a respawn keeps the player's settings.
    let mut unit = Unit::new(defs.commander_index(), d, at);
    if let Some((behaviour, priority, radius)) = state.settings {
        unit.behaviour = behaviour;
        unit.priority = priority;
        unit.guard_radius = radius;
    }
    spawner.commands().entity(entity).insert((unit, Commander {
        target: None,
        yaw,
        cooldown: 0.0,
        pending: None,
        lift: 0.0,
        lift_speed: 0.0,
    }));
    let again = state.respawn_at.take().is_some();
    state.entity = Some(entity);
    info!(
        "commander: {} at ({:.1}, {:.1}){}",
        d.name,
        at.x,
        at.y,
        if again { " — back from the dead" } else { "" }
    );
}

/// Leaves for RTS, the view centred on where the commander stands.
fn to_rts(
    mode: &mut ControlMode,
    rts: &mut RtsCam,
    follow: &FollowCam,
    at: Vec3,
    from: CamPose,
    now: f32,
) {
    rts.focus = at;
    rts.yaw = follow.yaw;
    rts.distance = crate::camera::DISTANCE_START;
    mode.switch(Mode::Rts, from, now);
}

#[allow(clippy::too_many_arguments)]
fn toggle(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut script: ResMut<Script>,
    state: Res<CommanderState>,
    mut mode: ResMut<ControlMode>,
    mut follow: ResMut<FollowCam>,
    mut build: ResMut<crate::build::BuildMode>,
    mut recruit: ResMut<crate::units::UnitMode>,
    mut selection: ResMut<crate::units::Selection>,
    mut commands: Commands,
    mut camera: Query<(&Transform, &Projection, &mut RtsCam)>,
    mut commanders: Query<(&Transform, &mut Commander, &mut Unit)>,
) {
    let now = time.elapsed_secs();
    let scripted = script.enter_at.is_some_and(|t| now >= t);
    if !(keys.just_pressed(KeyCode::KeyV) || scripted) {
        return;
    }
    // No switch mid-blend, and none into commander mode without a living commander; a scripted
    // switch waits for both.
    if mode.blend_progress(now).is_some() {
        return;
    }
    if scripted {
        if state.entity.is_none() {
            return;
        }
        script.enter_at = None;
    }
    let Ok((cam_xf, projection, mut rts)) = camera.single_mut() else {
        return;
    };
    let from = current_pose(cam_xf, projection);
    let Some(entity) = state.entity else {
        if mode.mode == Mode::Rts {
            info!("commander: none to take over yet");
        }
        return;
    };
    let Ok((xf, mut c, mut unit)) = commanders.get_mut(entity) else {
        return;
    };
    match mode.mode {
        Mode::Rts => {
            // It may have walked and turned on its own since; steer on from how it stands.
            let fwd = xf.rotation * Vec3::NEG_Z;
            c.yaw = (-fwd.x).atan2(-fwd.z);
            c.target = unit.target;
            follow.yaw = c.yaw;
            follow.pitch = 0.35;
            follow.boom = follow.distance;
            // Building and recruiting stop; the ghost goes with the build mode.
            build.def = None;
            recruit.def = None;
            selection.unit = None;
            selection.pick_anchor = false;
            selection.pick_escort = false;
            unit.target = None;
            unit.pending = None;
            commands.entity(entity).insert(PlayerControlled);
            mode.switch(Mode::Commander, from, now);
        }
        Mode::Commander => {
            // Back to its own judgement, its post where it was left.
            unit.anchor = wow_xy(xf.translation);
            unit.target = None;
            unit.path.clear();
            commands.entity(entity).remove::<PlayerControlled>();
            to_rts(&mut mode, &mut rts, &follow, xf.translation, from, now);
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn control(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    mut window: Query<&mut bevy::window::CursorOptions, With<PrimaryWindow>>,
    mode: Res<ControlMode>,
    script: Res<Script>,
    defs: Res<UnitDefs>,
    grid: Res<Grid>,
    heights: TerrainHeights,
    mut follow: ResMut<FollowCam>,
    mut moments: ResMut<ShotMoments>,
    mut hits: MessageWriter<Hit>,
    // Only while the player steers it; otherwise it is a unit like any other (`combat::think`).
    mut commanders: Query<
        (&mut Commander, &mut Transform, &mut CreatureAnim),
        With<PlayerControlled>,
    >,
    enemies: Query<(Entity, &Transform), (With<Enemy>, Without<Commander>)>,
) {
    let Ok((mut c, mut xf, mut anim)) = commanders.single_mut() else {
        return;
    };
    if anim.is_dead() {
        return;
    }
    let d = &defs.commander.unit;
    let now = time.elapsed_secs();
    let dt = time.delta_secs();
    let here = wow_xy(xf.translation);
    let enemy_at = |e: Entity| enemies.get(e).ok().map(|(_, t)| wow_xy(t.translation));
    let steering = mode.settled(Mode::Commander);

    // The target: dropped when dead or far; Tab picks the next one, nearest first.
    if c.target.and_then(enemy_at).is_none_or(|t| t.distance(here) > TARGET_DROP) {
        c.target = None;
    }
    let mut nearby: Vec<(Entity, f32)> = enemies
        .iter()
        .map(|(e, t)| (e, wow_xy(t.translation).distance(here)))
        .filter(|(_, dist)| *dist <= TAB_RANGE)
        .collect();
    nearby.sort_by(|a, b| a.1.total_cmp(&b.1));
    let tab = steering && keys.just_pressed(KeyCode::Tab);
    if tab || (steering && script.auto && c.target.is_none()) {
        let next = match c.target.and_then(|t| nearby.iter().position(|(e, _)| *e == t)) {
            Some(i) => nearby.get((i + 1) % nearby.len().max(1)),
            None => nearby.first(),
        };
        c.target = next.map(|(e, _)| *e);
        if tab {
            info!("commander: target {:?}", c.target);
        }
    }

    // Movement, read only while settled in commander mode.
    let mut forward = 0.0;
    let mut strafe = 0.0;
    let mut turn = 0.0;
    if steering {
        let right = buttons.pressed(MouseButton::Right);
        let left = buttons.pressed(MouseButton::Left);
        if let Ok(mut cursor) = window.single_mut() {
            let dragging = right || left;
            let want = if dragging {
                CursorGrabMode::Locked
            } else {
                CursorGrabMode::None
            };
            if cursor.grab_mode != want {
                cursor.grab_mode = want;
                cursor.visible = !dragging;
            }
        }
        if right || left {
            follow.yaw -= motion.delta.x * LOOK;
            follow.pitch = (follow.pitch + motion.delta.y * LOOK).clamp(PITCH_MIN, PITCH_MAX);
        }
        if right {
            // The right drag turns the commander with the camera.
            c.yaw = follow.yaw;
        }
        for ev in wheel.read() {
            follow.distance = (follow.distance * (1.0 - ev.y * 0.12)).clamp(ZOOM_MIN, ZOOM_MAX);
        }
        if keys.pressed(KeyCode::KeyW) || (right && left) {
            forward += 1.0;
        }
        if keys.pressed(KeyCode::KeyS) {
            forward -= 1.0;
        }
        if keys.pressed(KeyCode::KeyQ) {
            strafe -= 1.0;
        }
        if keys.pressed(KeyCode::KeyE) {
            strafe += 1.0;
        }
        // A and D turn, or strafe while the right button steers.
        let a = keys.pressed(KeyCode::KeyA);
        let dkey = keys.pressed(KeyCode::KeyD);
        if right {
            strafe += f32::from(dkey) - f32::from(a);
        } else {
            turn += f32::from(a) - f32::from(dkey);
        }
        if keys.just_pressed(KeyCode::Space) && c.lift <= 0.0 {
            c.lift_speed = JUMP_SPEED;
        }
        // The shot script's stand-in for a player: close on the target.
        if script.auto {
            if let Some(t) = c.target.and_then(enemy_at) {
                let to = t - here;
                if to.length() > d.attack_range * 0.8 {
                    let dir = wow_to_bevy([to.x, to.y, 0.0]);
                    c.yaw = dir.x.atan2(dir.z) + std::f32::consts::PI;
                    forward = 1.0;
                }
                follow.yaw += (c.yaw - follow.yaw) * (SWING_BACK * dt).min(1.0);
            }
        }
        c.yaw += turn * TURN_RATE * dt;
        if turn != 0.0 && !left {
            follow.yaw = c.yaw;
        }
    }

    // Move on the grid, sliding along what blocks it.
    let rot = Quat::from_rotation_y(c.yaw);
    let fwd = rot * Vec3::NEG_Z;
    let side = rot * Vec3::X;
    let dir3 = fwd * forward + side * strafe;
    let moving = dir3.length_squared() > 1e-4;
    let backpedal = forward < 0.0 && strafe == 0.0;
    let speed = d.move_speed * if backpedal { BACKPEDAL } else { 1.0 };
    let mut at = here;
    if moving {
        let step = dir3.normalize() * speed * dt;
        let w = bevy_to_wow(step);
        let delta = Vec2::new(w[0], w[1]);
        for try_step in [delta, Vec2::new(delta.x, 0.0), Vec2::new(0.0, delta.y)] {
            if grid.walkable_at(here + try_step) {
                at = here + try_step;
                break;
            }
        }
        // Running swings an orbited camera back behind.
        if !buttons.pressed(MouseButton::Left) {
            follow.yaw += (c.yaw - follow.yaw) * (SWING_BACK * dt).min(1.0);
        }
    }

    // Grounded, or in the air on a jump.
    c.lift += c.lift_speed * dt;
    c.lift_speed -= GRAVITY * dt;
    if c.lift <= 0.0 {
        c.lift = 0.0;
        c.lift_speed = 0.0;
    }
    let flat = wow_to_bevy([at.x, at.y, 0.0]);
    let ground = heights.under(flat).unwrap_or(xf.translation.y);
    xf.translation = Vec3::new(flat.x, ground + c.lift, flat.z);

    // Auto-attack: a swing on the interval while the target is in reach; standing, it faces it.
    c.cooldown -= dt;
    if let Some((t, land)) = c.pending {
        if now >= land {
            c.pending = None;
            if enemy_at(t).is_some_and(|p| p.distance(at) <= d.attack_range + 1.0) {
                hits.write(Hit {
                    target: t,
                    amount: d.damage,
                });
                moments.announce("commander_hit");
            }
        }
    }
    let target_at = c.target.and_then(enemy_at);
    if let Some(t) = target_at {
        if !moving {
            let dir = wow_to_bevy([t.x - at.x, t.y - at.y, 0.0]);
            if dir.length_squared() > 1e-4 {
                let want = dir.x.atan2(dir.z) + std::f32::consts::PI;
                c.yaw = want;
            }
        }
        if t.distance(at) <= d.attack_range && c.cooldown <= 0.0 && c.pending.is_none() {
            anim.attack(&d.attack_anims);
            c.cooldown = d.attack_interval;
            c.pending = c.target.map(|e| (e, now + HIT_DELAY));
        }
    }
    xf.rotation = Quat::from_rotation_y(c.yaw);

    let gait = match (moving, backpedal) {
        (false, _) => Gait::Stand,
        (true, true) => Gait::Walk,
        (true, false) => Gait::Run,
    };
    anim.set(gait, if moving { speed } else { 0.0 });
}

/// Death: the clip, the body handed to the dying lane, the view forced back to RTS, and the
/// respawn timer. `F9` and `kill@` drop it to 0 health.
#[allow(clippy::too_many_arguments)]
fn fall(
    mut commands: Commands,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mut script: ResMut<Script>,
    defs: Res<UnitDefs>,
    follow: Res<FollowCam>,
    mut state: ResMut<CommanderState>,
    mut mode: ResMut<ControlMode>,
    mut camera: Query<(&Transform, &Projection, &mut RtsCam), Without<Commander>>,
    mut stats: ResMut<crate::balance::BattleStats>,
    mut commanders: Query<(&mut Unit, &Transform, &mut CreatureAnim), With<Commander>>,
) {
    let now = time.elapsed_secs();
    let Some(entity) = state.entity else { return };
    let Ok((mut u, xf, mut anim)) = commanders.get_mut(entity) else {
        return;
    };
    // F9 is a debug-build key only; `kill@` is the scripted equivalent.
    let debug_kill = cfg!(debug_assertions) && keys.just_pressed(KeyCode::F9);
    if debug_kill || script.kill_at.is_some_and(|t| now >= t) {
        script.kill_at = None;
        u.health = 0.0;
    }
    if u.health > 0.0 {
        return;
    }
    anim.die();
    state.settings = Some((u.behaviour, u.priority, u.guard_radius));
    // No longer a unit: enemies let go of it, and nothing selects or steers the body.
    commands
        .entity(entity)
        .remove::<(Commander, Unit, PlayerControlled)>()
        .insert(Dying::new(now));
    stats.commander_deaths += 1;
    state.entity = None;
    state.respawn_at = Some(now + defs.commander.respawn_secs);
    info!(
        "commander: fallen — back in {:.0}s",
        defs.commander.respawn_secs
    );
    if mode.mode == Mode::Commander {
        if let Ok((cam_xf, projection, mut rts)) = camera.single_mut() {
            let from = current_pose(cam_xf, projection);
            to_rts(&mut mode, &mut rts, &follow, xf.translation, from, now);
        }
    }
}

/// The follow camera's pose, for commander mode and for a blend into it: behind and above the
/// commander, pulled in by world collision, kept above the ground.
fn follow_pose(
    time: Res<Time>,
    state: Res<CommanderState>,
    mut follow: ResMut<FollowCam>,
    collision: WorldCollision,
    heights: TerrainHeights,
    mut pose: ResMut<CommanderPose>,
    mut moments: ResMut<ShotMoments>,
    mode: Res<ControlMode>,
    bodies: Query<(&Transform, &benilla_world::world_unit::WorldUnit)>,
) {
    if mode.mode == Mode::Commander
        && mode
            .blend_progress(time.elapsed_secs())
            .is_some_and(|t| (0.4..0.7).contains(&t))
    {
        moments.announce("transition_mid");
    }
    let Some((xf, unit)) = state.entity.and_then(|e| bodies.get(e).ok()) else {
        pose.0 = None;
        return;
    };
    let pivot = xf.translation + Vec3::Y * EYE * unit.scale;
    let boom_dir =
        Quat::from_rotation_y(follow.yaw) * Vec3::new(0.0, follow.pitch.sin(), follow.pitch.cos());
    let wanted = follow.distance;
    let clear = collision
        .cast_camera(pivot, boom_dir * wanted, false)
        .map_or(wanted, |hit| (hit - CAMERA_SKIN).max(ZOOM_MIN * 0.5));
    // In at once when something cuts in; back out smoothly once it is clear.
    follow.boom = if clear < follow.boom {
        clear
    } else {
        follow.boom + (clear - follow.boom) * (BOOM_RECOVER * time.delta_secs()).min(1.0)
    };
    if follow.boom < wanted - 1.0 {
        moments.announce("camera_pulled");
    }
    let mut eye = pivot + boom_dir * follow.boom;
    if let Some(ground) = heights.under(eye) {
        eye.y = eye.y.max(ground + CAMERA_ABOVE_GROUND);
    }
    let look = Transform::from_translation(eye).looking_at(pivot, Vec3::Y);
    pose.0 = Some(CamPose {
        translation: eye,
        rotation: look.rotation,
        fov: FOV,
    });
}

/// A ring under the commander in RTS mode, and under its target in either.
fn draw(
    mode: Res<ControlMode>,
    state: Res<CommanderState>,
    heights: TerrainHeights,
    commanders: Query<(&Transform, &Commander)>,
    enemies: Query<&Transform, With<Enemy>>,
    mut gizmos: Gizmos,
) {
    let Some((xf, c)) = state.entity.and_then(|e| commanders.get(e).ok()) else {
        return;
    };
    if mode.mode == Mode::Rts {
        ground_circle(&mut gizmos, &heights, wow_xy(xf.translation), 1.4, Color::srgb(1.0, 0.8, 0.2));
    }
    if let Some(t) = c.target.and_then(|e| enemies.get(e).ok()) {
        ground_circle(&mut gizmos, &heights, wow_xy(t.translation), 1.0, Color::srgb(1.0, 0.2, 0.15));
    }
}
