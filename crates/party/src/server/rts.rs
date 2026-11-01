//! The party's RTS camera, on the binding `PARTYCAMERA` (default F8), paused or not: an eye on a boom
//! over a ground focus at a fixed pitch. WASD or the arrows pan, Q and E or a right drag turn, the
//! wheel zooms, and the focus rides the terrain inside [`PARTY_RADIUS`] of the party. Picking
//! members is [`super::selection`]'s.
//!
//! The app keeps placing the third-person camera each frame, and this overwrites it after
//! [`benilla_app::CameraPlaced`], so leaving blends to wherever that view is by then: on the
//! character played now, even after a swap made here. A toggle blends position and rotation over
//! [`BLEND_SECS`], eased in and out, and no camera input is read meanwhile. All of it runs on
//! `Time<Real>`, so it works over a paused world. Ported from `td-game`'s `camera.rs`.

use benilla_app::{Objects, TerrainHeights, UiFocus, WorldCamera};
use bevy::input::mouse::{AccumulatedMouseMotion, MouseWheel};
use bevy::prelude::*;
use bevy::window::{CursorGrabMode, CursorOptions, PrimaryWindow};

use super::Server;

/// The boom's angle below the horizon, in radians (55°).
const PITCH: f32 = 0.96;
/// How far the focus may pan from the party's centre, in yards: the terrain streamed around the
/// played character reaches this far.
const PARTY_RADIUS: f32 = 100.0;
/// The boom length's range, in yards, and where it opens.
const DISTANCE_MIN: f32 = 20.0;
const DISTANCE_MAX: f32 = 160.0;
const DISTANCE_START: f32 = 40.0;
/// Pan speed in boom lengths per second, so a zoomed-out pan covers more ground.
const PAN_RATE: f32 = 0.9;
/// Turn speed for Q and E in radians per second, and for a drag in radians per pixel.
const TURN_RATE: f32 = 1.6;
const DRAG_TURN: f32 = 0.004;
/// How fast the focus height eases to the ground, per second.
const GROUND_EASE: f32 = 8.0;
/// A toggle's camera blend, in seconds.
const BLEND_SECS: f32 = 0.7;
/// A camera pose.
#[derive(Clone, Copy, Debug)]
struct CamPose {
    translation: Vec3,
    rotation: Quat,
}

impl CamPose {
    fn of(xf: &Transform) -> Self {
        Self {
            translation: xf.translation,
            rotation: xf.rotation,
        }
    }

    fn lerp(&self, to: &CamPose, t: f32) -> CamPose {
        CamPose {
            translation: self.translation.lerp(to.translation, t),
            rotation: self.rotation.slerp(to.rotation, t),
        }
    }
}

/// The camera's state.
#[derive(Resource)]
pub(super) struct Rts {
    on: bool,
    /// The pose a blend started from and when.
    blend: Option<(CamPose, f32)>,
    /// Bevy space; `y` follows the terrain.
    focus: Vec3,
    yaw: f32,
    distance: f32,
    /// The focus has taken a real terrain height, so later heights ease rather than snap.
    grounded: bool,
    dragging: bool,
}

impl Rts {
    /// Whether the camera is on, blending in or settled.
    pub(super) fn on(&self) -> bool {
        self.on
    }

    /// On with no blend running: its input may be read.
    pub(super) fn settled(&self) -> bool {
        self.on && self.blend.is_none()
    }
}

impl Default for Rts {
    fn default() -> Self {
        Self {
            on: false,
            blend: None,
            focus: Vec3::ZERO,
            yaw: 0.0,
            distance: DISTANCE_START,
            grounded: false,
            dragging: false,
        }
    }
}

pub(super) struct RtsPlugin;

impl Plugin for RtsPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Rts>()
            .add_systems(Update, camera.after(benilla_app::CameraPlaced));
    }
}

/// The eye on the boom: behind the focus along the heading, raised by the pitch, looking at it.
fn rts_pose(rts: &Rts) -> CamPose {
    let boom = Quat::from_rotation_y(rts.yaw) * Vec3::new(0.0, PITCH.sin(), PITCH.cos());
    let xf =
        Transform::from_translation(rts.focus + boom * rts.distance).looking_at(rts.focus, Vec3::Y);
    CamPose::of(&xf)
}

/// Slow out, fast through the middle, slow in.
fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) * 0.5
    }
}

/// `at`, moved in to within [`PARTY_RADIUS`] of `center` on the ground plane.
fn clamp_to_party(at: Vec3, center: Vec3) -> Vec3 {
    let offset = Vec2::new(at.x - center.x, at.z - center.z);
    if offset.length() <= PARTY_RADIUS {
        return at;
    }
    let clamped = offset.normalize() * PARTY_RADIUS;
    Vec3::new(center.x + clamped.x, at.y, center.z + clamped.y)
}

/// Pans, turns and zooms from raw input, standing aside for what the interface holds.
#[allow(clippy::too_many_arguments)]
fn steer(
    rts: &mut Rts,
    dt: f32,
    focus: &UiFocus,
    keys: &ButtonInput<KeyCode>,
    buttons: &ButtonInput<MouseButton>,
    motion: &AccumulatedMouseMotion,
    wheel: &mut MessageReader<MouseWheel>,
    cursor: Option<&mut CursorOptions>,
) {
    // A drag begun in the world holds through a frame it crosses; one can't begin over a frame.
    let dragging = buttons.pressed(MouseButton::Right) && (rts.dragging || !focus.hovering());
    if dragging != rts.dragging {
        rts.dragging = dragging;
        if let Some(cursor) = cursor {
            cursor.grab_mode = if dragging {
                CursorGrabMode::Locked
            } else {
                CursorGrabMode::None
            };
            cursor.visible = !dragging;
        }
    }
    if dragging {
        rts.yaw -= motion.delta.x * DRAG_TURN;
    }
    for ev in wheel.read() {
        if !focus.hovering() {
            rts.distance = (rts.distance * (1.0 - ev.y * 0.1)).clamp(DISTANCE_MIN, DISTANCE_MAX);
        }
    }
    if focus.typing() {
        return;
    }
    if keys.pressed(KeyCode::KeyQ) {
        rts.yaw += TURN_RATE * dt;
    }
    if keys.pressed(KeyCode::KeyE) {
        rts.yaw -= TURN_RATE * dt;
    }
    // Pan on the ground plane, relative to the heading.
    let rot = Quat::from_rotation_y(rts.yaw);
    let (forward, right) = (rot * Vec3::NEG_Z, rot * Vec3::X);
    let mut dir = Vec3::ZERO;
    if keys.any_pressed([KeyCode::KeyW, KeyCode::ArrowUp]) {
        dir += forward;
    }
    if keys.any_pressed([KeyCode::KeyS, KeyCode::ArrowDown]) {
        dir -= forward;
    }
    if keys.any_pressed([KeyCode::KeyD, KeyCode::ArrowRight]) {
        dir += right;
    }
    if keys.any_pressed([KeyCode::KeyA, KeyCode::ArrowLeft]) {
        dir -= right;
    }
    if dir != Vec3::ZERO {
        rts.focus += dir.normalize() * rts.distance * PAN_RATE * dt;
    }
}

/// Ends a right drag's hold on the cursor.
fn release_cursor(rts: &mut Rts, cursor: Option<&mut CursorOptions>) {
    if std::mem::take(&mut rts.dragging) {
        if let Some(cursor) = cursor {
            cursor.grab_mode = CursorGrabMode::None;
            cursor.visible = true;
        }
    }
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn camera(
    mut rts: ResMut<Rts>,
    server: Res<Server>,
    time: Res<Time<Real>>,
    objects: Objects,
    (keys, buttons, motion): (
        Res<ButtonInput<KeyCode>>,
        Res<ButtonInput<MouseButton>>,
        Res<AccumulatedMouseMotion>,
    ),
    mut wheel: MessageReader<MouseWheel>,
    mut window: Query<&mut CursorOptions, With<PrimaryWindow>>,
    heights: TerrainHeights,
    focus: UiFocus,
    mut cameras: Query<&mut Transform, With<WorldCamera>>,
    bodies: Query<&GlobalTransform, Without<WorldCamera>>,
) {
    let toggled = benilla_app::take_camera_toggle();
    let rts = &mut *rts;
    let Some(me) = objects.player() else {
        // Out of the world the camera goes back at once, with nothing to blend to.
        if rts.on || rts.blend.is_some() {
            rts.on = false;
            rts.blend = None;
            benilla_app::yield_camera(false);
            release_cursor(rts, window.single_mut().ok().map(|c| c.into_inner()));
        }
        return;
    };
    let Ok(mut xf) = cameras.single_mut() else {
        return;
    };
    let now = time.elapsed_secs();
    // `control` placed this: the third-person pose, on the character played now.
    let third_person = CamPose::of(&xf);
    let mut cursor = window.single_mut().ok();

    if toggled && rts.blend.is_none() {
        rts.on = !rts.on;
        rts.blend = Some((third_person, now));
        benilla_app::yield_camera(true);
        if rts.on {
            let forward = xf.forward();
            rts.yaw = (-forward.x).atan2(-forward.z);
            rts.distance = DISTANCE_START;
            rts.grounded = false;
            if let Ok(body) = bodies.get(me) {
                rts.focus = body.translation();
            }
        } else {
            release_cursor(rts, cursor.as_deref_mut());
        }
    }
    if !rts.on && rts.blend.is_none() {
        return;
    }

    // The party, for the clamp.
    let members = server.0.party.lock().unwrap().clone();
    let placed: Vec<Vec3> = members
        .iter()
        .filter_map(|&(guid, _)| {
            let (_, entity, _) = objects.iter().find(|(g, ..)| *g == guid)?;
            Some(bodies.get(entity).ok()?.translation())
        })
        .collect();

    if rts.settled() {
        let dt = time.delta_secs();
        let cursor_options = cursor.as_deref_mut();
        steer(
            rts,
            dt,
            &focus,
            &keys,
            &buttons,
            &motion,
            &mut wheel,
            cursor_options,
        );
    } else {
        wheel.clear();
    }

    if rts.on {
        if !placed.is_empty() {
            let center = placed.iter().sum::<Vec3>() / placed.len() as f32;
            rts.focus = clamp_to_party(rts.focus, center);
        }
        // Ride the ground: snap on the first real height, then ease, so a hill never jolts the eye.
        if let Some(ground) = heights.under(rts.focus) {
            if rts.grounded {
                rts.focus.y += (ground - rts.focus.y) * (GROUND_EASE * time.delta_secs()).min(1.0);
            } else {
                rts.focus.y = ground;
                rts.grounded = true;
            }
        }
    }

    let target = if rts.on { rts_pose(rts) } else { third_person };
    let pose = match rts.blend {
        Some((from, started)) => {
            let t = ((now - started) / BLEND_SECS).clamp(0.0, 1.0);
            if t >= 1.0 {
                rts.blend = None;
                // The way back is done: the camera is the app's again from the next frame.
                if !rts.on {
                    benilla_app::yield_camera(false);
                }
            }
            from.lerp(&target, ease_in_out(t))
        }
        None => target,
    };
    xf.translation = pose.translation;
    xf.rotation = pose.rotation;
}
