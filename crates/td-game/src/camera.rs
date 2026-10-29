//! The world camera and its two modes. In RTS mode it is an eye on a boom over a ground focus, at
//! a fixed pitch: WASD or the arrows pan, Q and E or a right drag turn, the wheel zooms, and the
//! focus rides the terrain inside the arena. In commander mode [`crate::commander`] computes the
//! pose ([`CommanderPose`]) and this module only applies it.
//!
//! A mode switch blends position, rotation and field of view from where the camera was to the new
//! mode's live pose over [`BLEND_SECS`], eased in and out; no camera input is read meanwhile
//! ([`ControlMode::settled`]).

use bevy::camera::{PerspectiveProjection, Projection};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::mouse::{AccumulatedMouseMotion, MouseWheel};
use bevy::prelude::*;
use bevy::render::view::Hdr;
use bevy::window::{CursorGrabMode, PrimaryWindow};

use benilla_assets::coords::wow_to_bevy;
use benilla_world::world_point::TerrainHeights;

use crate::level::Level;

/// The boom's angle below the horizon, in radians (55°).
const PITCH: f32 = 0.96;

/// How far the focus may pan from the arena centre, in yards.
const ARENA_RADIUS: f32 = 200.0;

/// The boom length's range, in yards, and where it opens.
const DISTANCE_MIN: f32 = 20.0;
const DISTANCE_MAX: f32 = 160.0;
pub const DISTANCE_START: f32 = 40.0;

/// Pan speed in boom lengths per second, so a zoomed-out pan covers more ground.
const PAN_RATE: f32 = 0.9;

/// Turn speed for Q and E in radians per second, and for a drag in radians per pixel.
const TURN_RATE: f32 = 1.6;
const DRAG_TURN: f32 = 0.004;

/// How fast the focus height eases to the ground, per second.
const GROUND_EASE: f32 = 8.0;

/// A mode switch's camera blend, in seconds.
pub const BLEND_SECS: f32 = 0.7;

/// The RTS view's vertical field of view.
const RTS_FOV: f32 = benilla_world::view::CAM_FOVY;

/// Which controller owns the camera.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Rts,
    Commander,
}

/// A camera pose: where the eye is, where it looks, and its vertical field of view.
#[derive(Clone, Copy, Debug)]
pub struct CamPose {
    pub translation: Vec3,
    pub rotation: Quat,
    pub fov: f32,
}

impl CamPose {
    fn lerp(&self, to: &CamPose, t: f32) -> CamPose {
        CamPose {
            translation: self.translation.lerp(to.translation, t),
            rotation: self.rotation.slerp(to.rotation, t),
            fov: self.fov + (to.fov - self.fov) * t,
        }
    }
}

/// The current mode and any blend into it.
#[derive(Resource)]
pub struct ControlMode {
    pub mode: Mode,
    /// The pose the blend started from and when (`Time` seconds).
    blend: Option<(CamPose, f32)>,
}

impl ControlMode {
    /// In `mode` with no blend running: its input may be read.
    pub fn settled(&self, mode: Mode) -> bool {
        self.mode == mode && self.blend.is_none()
    }

    /// How far a running blend has got, `0..1`.
    pub fn blend_progress(&self, now: f32) -> Option<f32> {
        self.blend
            .map(|(_, started)| ((now - started) / BLEND_SECS).clamp(0.0, 1.0))
    }

    /// Switches to `to`, blending from `from`, the camera's pose now.
    pub fn switch(&mut self, to: Mode, from: CamPose, now: f32) {
        self.mode = to;
        self.blend = Some((from, now));
        info!("camera: → {to:?}");
    }
}

/// A run condition: the RTS controls (building, recruiting, selecting) take input.
pub fn rts_input(mode: Res<ControlMode>) -> bool {
    mode.settled(Mode::Rts)
}

/// The commander controller's pose this frame, written before [`CameraApply`].
#[derive(Resource, Default)]
pub struct CommanderPose(pub Option<CamPose>);

/// The system that writes the camera's transform and field of view.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CameraApply;

/// The RTS camera's state: the ground point it looks at, its heading, pitch and boom length.
#[derive(Component)]
pub struct RtsCam {
    /// Bevy space; `y` follows the terrain.
    pub focus: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub distance: f32,
    /// The focus has taken a real terrain height, so later heights ease rather than snap.
    grounded: bool,
}

/// `TD_CAM=<x>,<y>[,<distance>[,<pitch°>[,<yaw°>]]]`: the opening pose for a shot, the focus in
/// WoW coords. Pitch and distance outside the player's range are allowed (a top-down survey).
fn pose_from_env(cam: &mut RtsCam) {
    let Ok(v) = std::env::var("TD_CAM") else {
        return;
    };
    let c: Vec<f32> = v.split(',').filter_map(|p| p.trim().parse().ok()).collect();
    if c.len() < 2 {
        warn!("camera: TD_CAM wants `x,y[,distance[,pitch[,yaw]]]` — ignored");
        return;
    }
    cam.focus = wow_to_bevy([c[0], c[1], cam.focus.y]);
    if let Some(&d) = c.get(2) {
        cam.distance = d;
    }
    if let Some(&p) = c.get(3) {
        cam.pitch = p.to_radians();
    }
    if let Some(&y) = c.get(4) {
        cam.yaw = y.to_radians();
    }
}

pub fn plugin(app: &mut App) {
    app.insert_resource(ControlMode {
        mode: Mode::Rts,
        blend: None,
    })
    .init_resource::<CommanderPose>()
    .add_systems(
        Startup,
        spawn_camera.after(benilla_assets::AssetSet::Open),
    )
    .add_systems(
        Update,
        (rts_control, apply.in_set(CameraApply))
            .chain()
            .in_set(benilla_world::schedule::WorldStage::Input),
    );
}

/// The world camera bundle `benilla_world::worldview` spawns, under the RTS controller.
fn spawn_camera(
    mut commands: Commands,
    msaa: Res<benilla_world::view::MsaaSetting>,
    level: Res<Level>,
) {
    let mut cam = RtsCam {
        focus: level.center_bevy(),
        yaw: 0.0,
        pitch: PITCH,
        distance: DISTANCE_START,
        grounded: false,
    };
    pose_from_env(&mut cam);
    commands.spawn((
        Camera3d::default(),
        benilla_world::view::WorldCamera,
        msaa.level(),
        Projection::from(PerspectiveProjection {
            far: benilla_world::view::CAM_FAR,
            near: benilla_world::view::NEARCLIP_DEFAULT,
            fov: RTS_FOV,
            ..default()
        }),
        Hdr,
        Tonemapping::None,
        benilla_world::ffx_glow::FfxGlow::WORLD,
        rts_pose(&cam).transform(),
        cam,
    ));
}

impl CamPose {
    pub fn transform(&self) -> Transform {
        Transform::from_translation(self.translation).with_rotation(self.rotation)
    }
}

/// The eye on the boom: behind the focus along the heading, raised by the pitch, looking at it.
fn rts_pose(cam: &RtsCam) -> CamPose {
    let boom = Quat::from_rotation_y(cam.yaw) * Vec3::new(0.0, cam.pitch.sin(), cam.pitch.cos());
    let xf = Transform::from_translation(cam.focus + boom * cam.distance)
        .looking_at(cam.focus, Vec3::Y);
    CamPose {
        translation: xf.translation,
        rotation: xf.rotation,
        fov: RTS_FOV,
    }
}

/// The camera's pose as it stands, for a blend's start.
pub fn current_pose(xf: &Transform, projection: &Projection) -> CamPose {
    let fov = match projection {
        Projection::Perspective(p) => p.fov,
        _ => RTS_FOV,
    };
    CamPose {
        translation: xf.translation,
        rotation: xf.rotation,
        fov,
    }
}

#[allow(clippy::too_many_arguments)]
fn rts_control(
    time: Res<Time>,
    mode: Res<ControlMode>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    mut window: Query<&mut bevy::window::CursorOptions, With<PrimaryWindow>>,
    heights: TerrainHeights,
    level: Res<Level>,
    mut cams: Query<&mut RtsCam>,
) {
    let Ok(mut cam) = cams.single_mut() else {
        return;
    };
    let dt = time.delta_secs();

    // Input only while settled in RTS mode; the focus keeps riding the ground regardless.
    if mode.settled(Mode::Rts) {
        let dragging = buttons.pressed(MouseButton::Right);
        if let Ok(mut cursor) = window.single_mut() {
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
        if dragging {
            cam.yaw -= motion.delta.x * DRAG_TURN;
        }
        if keys.pressed(KeyCode::KeyQ) {
            cam.yaw += TURN_RATE * dt;
        }
        if keys.pressed(KeyCode::KeyE) {
            cam.yaw -= TURN_RATE * dt;
        }

        for ev in wheel.read() {
            cam.distance = (cam.distance * (1.0 - ev.y * 0.1)).clamp(DISTANCE_MIN, DISTANCE_MAX);
        }

        // Pan on the ground plane, relative to the heading.
        let rot = Quat::from_rotation_y(cam.yaw);
        let forward = rot * Vec3::NEG_Z;
        let right = rot * Vec3::X;
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
            let step = dir.normalize() * cam.distance * PAN_RATE * dt;
            cam.focus += step;
        }
    }

    // Keep the focus inside the arena, measured on the ground plane.
    let center = level.center_bevy();
    let offset = Vec2::new(cam.focus.x - center.x, cam.focus.z - center.z);
    if offset.length() > ARENA_RADIUS {
        let clamped = offset.normalize() * ARENA_RADIUS;
        cam.focus.x = center.x + clamped.x;
        cam.focus.z = center.z + clamped.y;
    }

    // Ride the ground: snap on the first real height, then ease, so a hill never jolts the eye.
    if let Some(ground) = heights.under(cam.focus) {
        if cam.grounded {
            cam.focus.y += (ground - cam.focus.y) * (GROUND_EASE * dt).min(1.0);
        } else {
            cam.focus.y = ground;
            cam.grounded = true;
        }
    }
}

/// `smoothstep`'s steeper cousin: slow out, fast through the middle, slow in.
fn ease_in_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 {
        4.0 * t * t * t
    } else {
        1.0 - (-2.0 * t + 2.0).powi(3) * 0.5
    }
}

/// Writes the mode's pose, or the blend into it, to the camera.
fn apply(
    time: Res<Time>,
    mut mode: ResMut<ControlMode>,
    commander: Res<CommanderPose>,
    mut cams: Query<(&RtsCam, &mut Transform, &mut Projection)>,
) {
    let Ok((rts, mut xf, mut projection)) = cams.single_mut() else {
        return;
    };
    let target = match mode.mode {
        Mode::Rts => rts_pose(rts),
        Mode::Commander => commander.0.unwrap_or_else(|| rts_pose(rts)),
    };
    let now = time.elapsed_secs();
    let pose = match (mode.blend, mode.blend_progress(now)) {
        (Some((from, _)), Some(t)) => {
            if t >= 1.0 {
                mode.blend = None;
            }
            from.lerp(&target, ease_in_out(t))
        }
        _ => target,
    };
    *xf = pose.transform();
    if let Projection::Perspective(p) = &mut *projection {
        if p.fov != pose.fov {
            p.fov = pose.fov;
        }
    }
}
