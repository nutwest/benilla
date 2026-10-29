//! The RTS camera: an eye on a boom over a ground focus, at a fixed pitch. WASD or the arrows pan,
//! Q and E or a right drag turn, the wheel zooms. The focus rides the terrain height and stays
//! inside the arena.

use bevy::camera::{PerspectiveProjection, Projection};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::input::mouse::{AccumulatedMouseMotion, MouseWheel};
use bevy::prelude::*;
use bevy::render::view::Hdr;
use bevy::window::{CursorGrabMode, PrimaryWindow};

use benilla_assets::coords::wow_to_bevy;
use benilla_world::world_point::TerrainHeights;

use crate::boot::ARENA_CENTER;

/// The boom's angle below the horizon, in radians (55°).
const PITCH: f32 = 0.96;

/// How far the focus may pan from the arena centre, in yards.
const ARENA_RADIUS: f32 = 200.0;

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

/// The camera's pose: the ground point it looks at, its heading and its boom length.
#[derive(Component)]
pub struct RtsCam {
    /// Bevy space; `y` follows the terrain.
    pub focus: Vec3,
    pub yaw: f32,
    pub distance: f32,
    /// The focus has taken a real terrain height, so later heights ease rather than snap.
    grounded: bool,
}

pub fn plugin(app: &mut App) {
    app.add_systems(
        Startup,
        spawn_camera.after(benilla_assets::AssetSet::Open),
    )
    .add_systems(
        Update,
        control.in_set(benilla_world::schedule::WorldStage::Input),
    );
}

/// The world camera bundle `benilla_world::worldview` spawns, under the RTS controller.
fn spawn_camera(mut commands: Commands, msaa: Res<benilla_world::view::MsaaSetting>) {
    let cam = RtsCam {
        focus: wow_to_bevy(ARENA_CENTER),
        yaw: 0.0,
        distance: DISTANCE_START,
        grounded: false,
    };
    commands.spawn((
        Camera3d::default(),
        benilla_world::view::WorldCamera,
        msaa.level(),
        Projection::from(PerspectiveProjection {
            far: benilla_world::view::CAM_FAR,
            near: benilla_world::view::NEARCLIP_DEFAULT,
            fov: benilla_world::view::CAM_FOVY,
            ..default()
        }),
        Hdr,
        Tonemapping::None,
        benilla_world::ffx_glow::FfxGlow::WORLD,
        eye_transform(&cam),
        cam,
    ));
}

/// The eye on the boom: behind the focus along the heading, raised by the pitch, looking at it.
fn eye_transform(cam: &RtsCam) -> Transform {
    let boom = Quat::from_rotation_y(cam.yaw) * Vec3::new(0.0, PITCH.sin(), PITCH.cos());
    Transform::from_translation(cam.focus + boom * cam.distance).looking_at(cam.focus, Vec3::Y)
}

fn control(
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    buttons: Res<ButtonInput<MouseButton>>,
    motion: Res<AccumulatedMouseMotion>,
    mut wheel: MessageReader<MouseWheel>,
    mut window: Query<&mut bevy::window::CursorOptions, With<PrimaryWindow>>,
    heights: TerrainHeights,
    mut cams: Query<(&mut Transform, &mut RtsCam)>,
) {
    let Ok((mut xf, mut cam)) = cams.single_mut() else {
        return;
    };
    let dt = time.delta_secs();

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

    // Keep the focus inside the arena, measured on the ground plane.
    let center = wow_to_bevy(ARENA_CENTER);
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

    *xf = eye_transform(&cam);
}
