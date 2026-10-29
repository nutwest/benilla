//! Building: pick a structure with the number keys (1–6, in `structures.ron` order), aim with the
//! mouse, turn it with R (Shift+R back) in 15° steps, and left-click to build; Esc leaves build
//! mode. Out of build mode, X sells the structure under the cursor. A translucent ghost shows the
//! model and the footprint's cells are drawn green (valid) or red (refused, with the reason
//! logged on a click).
//!
//! `TD_BUILD` scripts the same path for a shot: `;`-separated commands run once the grid, the
//! flow field and the models are ready. `place <id> <x> <y> <yaw°>` builds through the same
//! validation as a click, `ghost <id> <x> <y> <yaw°>` pins the ghost there, `sell <x> <y>` sells
//! what stands there. Positions are WoW coordinates.

use std::f32::consts::FRAC_PI_2;

use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use benilla_assets::coords::bevy_to_wow;
use benilla_world::collision::WorldCollision;
use benilla_world::view::WorldCamera;

use crate::flow::Flow;
use crate::enemy::Enemy;
use crate::structures::{
    footprint_cells, placement_transform, Placement, StructureBuilder, INFLATE,
};

/// One turn step, in degrees.
const TURN_STEP: f32 = 15.0;

/// Placements snap to this grid, in yards, so footprints rasterize the same way each time.
const SNAP: f32 = 0.5;

/// The ghost's alpha.
const GHOST_ALPHA: f32 = 0.45;

const NUMBER_KEYS: [KeyCode; 6] = [
    KeyCode::Digit1,
    KeyCode::Digit2,
    KeyCode::Digit3,
    KeyCode::Digit4,
    KeyCode::Digit5,
    KeyCode::Digit6,
];

#[derive(Resource, Default)]
struct BuildMode {
    /// The structure being placed, if in build mode.
    def: Option<usize>,
    yaw: f32,
    /// The ghost's root and the def it shows.
    ghost: Option<(Entity, usize)>,
    /// A scripted ghost's fixed position, which the cursor does not move.
    pinned: Option<Vec2>,
}

#[derive(Resource)]
struct Script {
    commands: Vec<String>,
    done: bool,
}

pub fn plugin(app: &mut App) {
    let commands = std::env::var("TD_BUILD")
        .map(|s| {
            s.split(';')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    app.init_resource::<BuildMode>()
        .insert_resource(Script {
            commands,
            done: false,
        })
        .add_systems(
            Update,
            (keys, run_script, aim_and_build, sell_under_cursor)
                .chain()
                .in_set(crate::grid::GridSet)
                .after(crate::grid::bake),
        );
}

fn keys(
    keys: Res<ButtonInput<KeyCode>>,
    defs: Option<Res<crate::structures::StructureDefs>>,
    mut mode: ResMut<BuildMode>,
) {
    let Some(defs) = defs else { return };
    for (i, key) in NUMBER_KEYS.iter().enumerate() {
        if keys.just_pressed(*key) && i < defs.defs.len() {
            mode.def = Some(i);
            mode.pinned = None;
            info!("build: {} — click to build, R to turn, Esc to stop", defs.defs[i].name);
        }
    }
    if keys.just_pressed(KeyCode::Escape) && mode.def.is_some() {
        mode.def = None;
        info!("build: off");
    }
    if keys.just_pressed(KeyCode::KeyR) {
        let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
        let step = TURN_STEP.to_radians();
        mode.yaw += if shift { -step } else { step };
    }
}

/// The ground point under the cursor, WoW `(x, y)`, snapped.
fn cursor_ground(
    window: &Window,
    camera: &Camera,
    cam_xf: &GlobalTransform,
    collision: &WorldCollision,
) -> Option<Vec2> {
    let cursor = window.cursor_position()?;
    let ray = camera.viewport_to_world(cam_xf, cursor).ok()?;
    let hit = collision.ray_body(ray.origin, ray.direction, 2_000.0)?;
    let w = bevy_to_wow(ray.origin + *ray.direction * hit.distance);
    Some(snap(Vec2::new(w[0], w[1])))
}

fn snap(v: Vec2) -> Vec2 {
    (v / SNAP).round() * SNAP
}

fn run_script(
    mut script: ResMut<Script>,
    mut mode: ResMut<BuildMode>,
    flow: Res<Flow>,
    mut builder: StructureBuilder,
) {
    if script.done || script.commands.is_empty() || !flow.ready() || !builder.ready() {
        return;
    }
    script.done = true;
    for line in script.commands.clone() {
        let words: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize| words.get(i).and_then(|w| w.parse::<f32>().ok());
        let def = |builder: &StructureBuilder| {
            words
                .get(1)
                .and_then(|id| builder.defs.as_ref().and_then(|d| d.find(id)))
        };
        match words.first().copied() {
            Some(verb @ ("place" | "ghost")) => {
                let (Some(d), Some(x), Some(y), Some(yaw)) =
                    (def(&builder), num(2), num(3), num(4))
                else {
                    warn!("build script: `{line}` wants `{verb} <id> <x> <y> <yaw°>`");
                    continue;
                };
                let p = Placement {
                    def: d,
                    at: snap(Vec2::new(x, y)),
                    yaw: yaw.to_radians(),
                };
                if verb == "ghost" {
                    mode.def = Some(d);
                    mode.yaw = p.yaw;
                    mode.pinned = Some(p.at);
                    continue;
                }
                if let Err(why) = builder.place(p) {
                    warn!("build script: refused `{line}` — {why}");
                }
            }
            Some("sell") => {
                let (Some(x), Some(y)) = (num(1), num(2)) else {
                    warn!("build script: `{line}` wants `sell <x> <y>`");
                    continue;
                };
                match builder.structure_at(Vec2::new(x, y)) {
                    Some(e) => builder.sell(e),
                    None => warn!("build script: nothing to sell at ({x}, {y})"),
                }
            }
            _ => warn!("build script: unknown command `{line}`"),
        }
    }
}

fn aim_and_build(
    mut mode: ResMut<BuildMode>,
    mouse: Res<ButtonInput<MouseButton>>,
    window: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    collision: WorldCollision,
    mut builder: StructureBuilder,
    mut ghosts: Query<&mut Transform, Without<Enemy>>,
    mut gizmos: Gizmos,
) {
    // Out of build mode, or switched structure: the old ghost goes.
    if let Some((root, shown)) = mode.ghost {
        if mode.def != Some(shown) {
            builder.commands.entity(root).despawn();
            mode.ghost = None;
        }
    }
    let Some(def) = mode.def else { return };
    if !builder.ready() || !builder.grid.ready {
        return;
    }
    let target = mode.pinned.or_else(|| {
        let (Ok(window), Ok((cam, cam_xf))) = (window.single(), camera.single()) else {
            return None;
        };
        cursor_ground(window, cam, cam_xf, &collision)
    });
    let Some(at) = target else { return };
    let p = Placement {
        def,
        at,
        yaw: mode.yaw,
    };
    let Some(defs) = builder.defs.as_ref() else {
        return;
    };
    let d = defs.defs[def].clone();
    let verdict = builder.validate(&p);

    // The ghost: spawned once per def, moved every frame.
    let xf = placement_transform(&d, &p, builder.ground(at));
    match mode.ghost {
        Some((root, _)) => {
            if let Ok(mut t) = ghosts.get_mut(root) {
                *t = xf;
            }
        }
        None => {
            let root = builder.commands.spawn((xf, Visibility::default())).id();
            builder.spawn_parts(root, def, Some(GHOST_ALPHA));
            mode.ghost = Some((root, def));
        }
    }

    // The footprint's blocking cells, green or red.
    let color = if verdict.is_ok() {
        Color::srgb(0.2, 1.0, 0.3)
    } else {
        Color::srgb(1.0, 0.2, 0.2)
    };
    let flat = Quat::from_rotation_x(FRAC_PI_2);
    let grid = &builder.grid;
    if let Some(cells) = footprint_cells(grid, &d, &p, INFLATE) {
        for i in cells {
            gizmos.rect(
                Isometry3d::new(grid.center_bevy(i) + Vec3::Y * 0.3, flat),
                Vec2::splat(0.8),
                color,
            );
        }
    }

    if mode.pinned.is_none() && mouse.just_pressed(MouseButton::Left) {
        match builder.place(p) {
            Ok(_) => {}
            Err(why) => warn!("build: refused {} at ({:.1}, {:.1}) — {why}", d.name, at.x, at.y),
        }
    }
}

fn sell_under_cursor(
    mode: Res<BuildMode>,
    keys: Res<ButtonInput<KeyCode>>,
    window: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    collision: WorldCollision,
    mut builder: StructureBuilder,
) {
    if mode.def.is_some() || !keys.just_pressed(KeyCode::KeyX) {
        return;
    }
    let (Ok(window), Ok((cam, cam_xf))) = (window.single(), camera.single()) else {
        return;
    };
    let Some(at) = cursor_ground(window, cam, cam_xf, &collision) else {
        return;
    };
    match builder.structure_at(at) {
        Some(e) => builder.sell(e),
        None => info!("build: nothing to sell at ({:.1}, {:.1})", at.x, at.y),
    }
}
