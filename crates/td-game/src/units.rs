//! The player's units: defs from `data/units.ron`, recruiting, selection and behaviour controls.
//! Combat and movement are [`crate::combat`]'s.
//!
//! Controls until the HUD exists (M8): 7, 8, … pick a unit to recruit (in `units.ron` order) and a
//! left-click places it on walkable ground; Esc stops. Out of any build mode a left-click selects
//! the nearest unit under the cursor. With one selected: B toggles Hold / Guard, P cycles the
//! target priority, `[` and `]` shrink and grow the guard radius, M moves the guard anchor to the
//! cursor, Delete dismisses the unit (no refund). F4 draws every unit's radius
//! (`TD_UNIT_DEBUG=1` opens with it on).
//!
//! `TD_UNITS` scripts recruiting for a shot: `;`-separated `<id> <x> <y> [hold|guard]
//! [first|nearest|strongest|weakest] [radius]`, `select <n>` (the n-th placed, from 0), `escort <n>`
//! (the commander escorts the n-th placed) or `select commander`; WoW
//! coordinates, run once everything is ready.

use bevy::prelude::*;
use bevy::window::PrimaryWindow;
use serde::Deserialize;

use benilla_assets::coords::{bevy_to_wow, wow_to_bevy};
use benilla_world::collision::WorldCollision;
use benilla_world::view::WorldCamera;
use benilla_world::world_point::TerrainHeights;

use crate::creature::CreatureSpawner;
use crate::economy::{Amount, Stores};
use crate::flow::Flow;
use crate::grid::Grid;

/// The guard radius's range, in yards.
pub const GUARD_RADIUS_MIN: f32 = 4.0;
pub const GUARD_RADIUS_MAX: f32 = 15.0;

/// Two units may not stand closer than this, in yards.
const MIN_SPACING: f32 = 1.0;

/// A click selects the nearest unit within this many yards of the cursor's ground point.
const PICK_RADIUS: f32 = 2.5;

/// An escort pick takes the nearest structure whose centre is this close to the click.
const STRUCTURE_PICK_RADIUS: f32 = 8.0;

const UNIT_KEYS: [KeyCode; 3] = [KeyCode::Digit7, KeyCode::Digit8, KeyCode::Digit9];

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Behaviour {
    /// Never moves; attacks what comes in range.
    Hold,
    /// Engages what enters the guard radius around its anchor, then walks back.
    Guard,
    /// Commander only: stays within its radius of a chosen unit or structure and defends it.
    Escort,
}

#[derive(Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Priority {
    /// Closest to the exit by the flow field.
    First,
    Nearest,
    Strongest,
    Weakest,
}

impl Priority {
    fn next(self) -> Self {
        match self {
            Priority::First => Priority::Nearest,
            Priority::Nearest => Priority::Strongest,
            Priority::Strongest => Priority::Weakest,
            Priority::Weakest => Priority::First,
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "first" => Priority::First,
            "nearest" => Priority::Nearest,
            "strongest" => Priority::Strongest,
            "weakest" => Priority::Weakest,
            _ => return None,
        })
    }
}

#[derive(Deserialize, Clone, Copy, Debug)]
pub enum AttackKind {
    Melee,
    Ranged { projectile_speed: f32 },
}

#[derive(Deserialize, Clone, Debug)]
pub struct UnitDef {
    pub id: String,
    pub name: String,
    pub display_id: u32,
    pub cost: Amount,
    pub health: f32,
    pub damage: f32,
    pub attack_range: f32,
    /// Seconds between attacks.
    pub attack_interval: f32,
    pub move_speed: f32,
    pub attack: AttackKind,
    pub attack_anims: Vec<u16>,
    pub behaviour: Behaviour,
    pub priority: Priority,
    pub guard_radius: f32,
    /// Enemies it holds in melee at once; 0 for a unit that holds none, which a single enemy may
    /// still engage.
    #[serde(default)]
    pub blocks: u32,
    /// Seconds knocked out before it stands up again at its post.
    #[serde(default = "default_recovery")]
    pub recovery_secs: f32,
}

fn default_recovery() -> f32 {
    10.0
}

/// The commander: not recruited, one per battle.
#[derive(Deserialize, Clone, Debug)]
pub struct CommanderDef {
    /// Its stats and default behaviour, as a unit's; `cost` is unused.
    pub unit: UnitDef,
    pub respawn_secs: f32,
}

#[derive(Deserialize)]
struct UnitsFile {
    units: Vec<UnitDef>,
    commander: CommanderDef,
}

#[derive(Resource)]
pub struct UnitDefs {
    pub defs: Vec<UnitDef>,
    pub commander: CommanderDef,
}

impl UnitDefs {
    /// A unit's def by its [`Unit::def`] index: the recruitable ones, then the commander's at
    /// [`Self::commander_index`].
    pub fn get(&self, i: usize) -> &UnitDef {
        self.defs.get(i).unwrap_or(&self.commander.unit)
    }

    pub fn commander_index(&self) -> usize {
        self.defs.len()
    }
}

/// Reads `data/units.ron`; at boot, so the creature library can preload every unit's display.
pub fn load() -> Result<UnitDefs, String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/data/units.ron");
    let text = std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))?;
    let file: UnitsFile = ron::from_str(&text).map_err(|e| format!("{path}: {e}"))?;
    info!("units: {} defs from {path}", file.units.len());
    Ok(UnitDefs {
        defs: file.units,
        commander: file.commander,
    })
}

/// A recruited unit and its live state.
#[derive(Component)]
pub struct Unit {
    pub def: usize,
    pub health: f32,
    pub behaviour: Behaviour,
    pub priority: Priority,
    pub guard_radius: f32,
    /// The guard post, WoW `(x, y)`.
    pub anchor: Vec2,
    pub target: Option<Entity>,
    /// Seconds until the next attack may start.
    pub cooldown: f32,
    /// A swing or a release in flight: its target and when it lands (`Time` seconds).
    pub pending: Option<(Entity, f32)>,
    /// Waypoints still to walk, WoW `(x, y)`, and the goal they lead to.
    pub path: Vec<Vec2>,
    pub path_goal: Option<Vec2>,
    pub repath_in: f32,
    /// What an Escort keeps close to: a unit or a structure.
    pub escort: Option<Entity>,
    /// On a tower (M7): out of a melee enemy's reach.
    pub garrisoned: bool,
}

impl Unit {
    /// A fresh unit of def `index` at `at` (WoW `(x, y)`), its post, with the def's defaults.
    pub fn new(index: usize, d: &UnitDef, at: Vec2) -> Self {
        Self {
            def: index,
            health: d.health,
            behaviour: d.behaviour,
            priority: d.priority,
            guard_radius: d.guard_radius,
            anchor: at,
            target: None,
            cooldown: 0.0,
            pending: None,
            path: Vec::new(),
            path_goal: None,
            repath_in: 0.0,
            escort: None,
            garrisoned: false,
        }
    }
}

/// The unit def being recruited, if in recruit mode.
#[derive(Resource, Default)]
pub struct UnitMode {
    pub def: Option<usize>,
}

/// The selected unit, and whether the next ground click sets its guard post.
#[derive(Resource, Default)]
pub struct Selection {
    pub unit: Option<Entity>,
    pub pick_anchor: bool,
    /// The next click picks the unit or structure the selected commander escorts.
    pub pick_escort: bool,
}

#[derive(Resource)]
struct RadiusOverlay(bool);

#[derive(Resource)]
struct Script {
    commands: Vec<String>,
    done: bool,
    /// `escort <n>`: the unit the commander should escort, once there is a commander.
    escort: Option<Entity>,
    /// `select commander`: select it once it exists.
    select_commander: bool,
}

pub fn plugin(app: &mut App) {
    let commands = std::env::var("TD_UNITS")
        .map(|s| {
            s.split(';')
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    app.init_resource::<UnitMode>()
        .init_resource::<Selection>()
        .insert_resource(RadiusOverlay(
            std::env::var("TD_UNIT_DEBUG").as_deref() == Ok("1"),
        ))
        .insert_resource(Script {
            commands,
            done: false,
            escort: None,
            select_commander: false,
        })
        .add_systems(
            Update,
            (
                mode_keys.run_if(crate::camera::rts_input),
                run_script,
                commander_script,
                click.run_if(crate::camera::rts_input),
                selected_keys.run_if(crate::camera::rts_input),
                draw_radii,
            )
                .chain(),
        );
}

fn mode_keys(
    keys: Res<ButtonInput<KeyCode>>,
    defs: Res<UnitDefs>,
    mut mode: ResMut<UnitMode>,
    mut build: ResMut<crate::build::BuildMode>,
    mut overlay: ResMut<RadiusOverlay>,
) {
    for (i, key) in UNIT_KEYS.iter().enumerate() {
        if keys.just_pressed(*key) && i < defs.defs.len() {
            mode.def = Some(i);
            build.def = None;
            let d = &defs.defs[i];
            info!("recruit: {} ({}) — click to place, Esc to stop", d.name, d.cost);
        }
    }
    if keys.just_pressed(KeyCode::Escape) && mode.def.is_some() {
        mode.def = None;
        info!("recruit: off");
    }
    if keys.just_pressed(KeyCode::F4) {
        overlay.0 = !overlay.0;
    }
}

/// Recruits `def` at `at` if the ground is walkable, nobody stands there and the Gold and Food
/// are there; the refusal's reason otherwise.
fn recruit(
    spawner: &mut CreatureSpawner,
    stores: &mut Stores,
    grid: &Grid,
    heights: &TerrainHeights,
    defs: &UnitDefs,
    others: &[Vec2],
    def: usize,
    at: Vec2,
) -> Result<Entity, String> {
    let d = &defs.defs[def];
    if !grid.walkable_at(at) {
        return Err("the ground there is not walkable".into());
    }
    if others.iter().any(|o| o.distance(at) < MIN_SPACING) {
        return Err("another unit stands there".into());
    }
    if !stores.can_afford(&d.cost) {
        return Err(format!("it costs {}", d.cost));
    }
    let ground = heights
        .under(wow_to_bevy([at.x, at.y, 0.0]))
        .ok_or("the ground there is not loaded")?;
    let entity = spawner
        .spawn(
            d.display_id,
            wow_to_bevy([at.x, at.y, ground]),
            Vec3::NEG_Z,
        )
        .ok_or("its model is not ready")?;
    stores.spend(d.cost, &d.name);
    spawner.commands().entity(entity).insert(Unit::new(def, d, at));
    info!(
        "recruit: {} at ({:.1}, {:.1}) — {:?}, {:?}",
        d.name, at.x, at.y, d.behaviour, d.priority
    );
    Ok(entity)
}

fn unit_positions(units: &Query<(Entity, &Transform, &mut Unit)>) -> Vec<Vec2> {
    units.iter().map(|(_, xf, _)| wow_xy(xf.translation)).collect()
}

pub fn wow_xy(bevy: Vec3) -> Vec2 {
    let w = bevy_to_wow(bevy);
    Vec2::new(w[0], w[1])
}

#[allow(clippy::too_many_arguments)]
fn run_script(
    mut script: ResMut<Script>,
    defs: Res<UnitDefs>,
    flow: Res<Flow>,
    grid: Res<Grid>,
    heights: TerrainHeights,
    mut stores: ResMut<Stores>,
    mut spawner: CreatureSpawner,
    mut selection: ResMut<Selection>,
    units: Query<(Entity, &Transform, &mut Unit)>,
) {
    if script.done || script.commands.is_empty() || !flow.ready() {
        return;
    }
    if !defs.defs.iter().all(|d| spawner.ready(d.display_id)) {
        return;
    }
    script.done = true;
    // Placed this frame, which the query cannot see yet.
    let mut others = unit_positions(&units);
    let mut placed: Vec<(Entity, Vec<String>)> = Vec::new();
    for line in script.commands.clone() {
        let words: Vec<String> = line.split_whitespace().map(String::from).collect();
        let num = |i: usize| words.get(i).and_then(|w| w.parse::<f32>().ok());
        // `select commander`, and `escort <n>`: the commander escorts the n-th unit placed.
        if words.first().map(String::as_str) == Some("select")
            && words.get(1).map(String::as_str) == Some("commander")
        {
            script.select_commander = true;
            continue;
        }
        if words.first().map(String::as_str) == Some("escort") {
            match num(1).and_then(|n| placed.get(n as usize)) {
                Some((e, _)) => script.escort = Some(*e),
                None => warn!("unit script: `{line}` names no unit placed so far"),
            }
            continue;
        }
        // `select <n>`: the n-th unit this script placed, from 0.
        if words.first().map(String::as_str) == Some("select") {
            match num(1).and_then(|n| placed.get(n as usize)) {
                Some((e, _)) => selection.unit = Some(*e),
                None => warn!("unit script: `{line}` names no unit placed so far"),
            }
            continue;
        }
        let def = words
            .first()
            .and_then(|id| defs.defs.iter().position(|d| &d.id == id));
        let (Some(def), Some(x), Some(y)) = (def, num(1), num(2)) else {
            warn!("unit script: `{line}` wants `<id> <x> <y> [behaviour] [priority] [radius]`");
            continue;
        };
        let at = Vec2::new(x, y);
        match recruit(
            &mut spawner,
            &mut stores,
            &grid,
            &heights,
            &defs,
            &others,
            def,
            at,
        ) {
            Ok(e) => {
                others.push(at);
                placed.push((e, words[3..].to_vec()));
            }
            Err(why) => warn!("unit script: refused `{line}` — {why}"),
        }
    }
    // The settings go on through commands, after the `Unit` the recruit inserted.
    for (entity, settings) in placed {
        let mut behaviour = None;
        let mut priority = None;
        let mut radius = None;
        for s in &settings {
            match s.as_str() {
                "hold" => behaviour = Some(Behaviour::Hold),
                "guard" => behaviour = Some(Behaviour::Guard),
                other => {
                    if let Some(p) = Priority::parse(other) {
                        priority = Some(p);
                    } else if let Ok(r) = other.parse::<f32>() {
                        radius = Some(r.clamp(GUARD_RADIUS_MIN, GUARD_RADIUS_MAX));
                    }
                }
            }
        }
        spawner.commands().queue(move |world: &mut World| {
            if let Some(mut u) = world.get_mut::<Unit>(entity) {
                u.behaviour = behaviour.unwrap_or(u.behaviour);
                u.priority = priority.unwrap_or(u.priority);
                u.guard_radius = radius.unwrap_or(u.guard_radius);
            }
        });
    }
}

/// The cursor's ground point, WoW `(x, y)`.
fn cursor_ground(
    window: &Query<&Window, With<PrimaryWindow>>,
    camera: &Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    collision: &WorldCollision,
) -> Option<Vec2> {
    let (window, (cam, cam_xf)) = (window.single().ok()?, camera.single().ok()?);
    let ray = cam.viewport_to_world(cam_xf, window.cursor_position()?).ok()?;
    let hit = collision.ray_body(ray.origin, ray.direction, 2_000.0)?;
    Some(wow_xy(ray.origin + *ray.direction * hit.distance))
}

#[allow(clippy::too_many_arguments)]
fn click(
    mouse: Res<ButtonInput<MouseButton>>,
    mode: Res<UnitMode>,
    build: Res<crate::build::BuildMode>,
    defs: Res<UnitDefs>,
    grid: Res<Grid>,
    heights: TerrainHeights,
    window: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    collision: WorldCollision,
    mut stores: ResMut<Stores>,
    mut spawner: CreatureSpawner,
    mut selection: ResMut<Selection>,
    over_hud: Res<crate::hud::PointerOverHud>,
    mut units: Query<(Entity, &Transform, &mut Unit)>,
    structures: Query<(Entity, &Transform), With<crate::structures::Structure>>,
) {
    if !mouse.just_pressed(MouseButton::Left) || build.def.is_some() || over_hud.0 {
        return;
    }
    let Some(at) = cursor_ground(&window, &camera, &collision) else {
        return;
    };
    // The panel's "Set post": this click places the selected unit's guard anchor.
    if selection.pick_anchor {
        selection.pick_anchor = false;
        if let Some(Ok((_, _, mut u))) = selection.unit.map(|e| units.get_mut(e)) {
            u.anchor = at;
            u.target = None;
            info!("unit: {} guards ({:.1}, {:.1})", defs.get(u.def).name, at.x, at.y);
        }
        return;
    }
    // The panel's "Escort": this click picks the unit or structure the commander keeps close to.
    if selection.pick_escort {
        selection.pick_escort = false;
        let Some(me) = selection.unit else { return };
        let unit = units
            .iter()
            .filter(|(e, _, _)| *e != me)
            .map(|(e, xf, _)| (e, wow_xy(xf.translation).distance(at)))
            .filter(|(_, d)| *d <= PICK_RADIUS)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let structure = structures
            .iter()
            .map(|(e, xf)| (e, wow_xy(xf.translation).distance(at)))
            .filter(|(_, d)| *d <= STRUCTURE_PICK_RADIUS)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let Some((charge, _)) = unit.or(structure) else {
            warn!("unit: no unit or structure there to escort");
            return;
        };
        if let Ok((_, _, mut u)) = units.get_mut(me) {
            u.behaviour = Behaviour::Escort;
            u.escort = Some(charge);
            u.target = None;
            info!("unit: {} escorts {charge:?}", defs.get(u.def).name);
        }
        return;
    }
    if let Some(def) = mode.def {
        let others = unit_positions(&units);
        if let Err(why) = recruit(
            &mut spawner,
            &mut stores,
            &grid,
            &heights,
            &defs,
            &others,
            def,
            at,
        ) {
            warn!("recruit: refused {} at ({:.1}, {:.1}) — {why}", defs.defs[def].name, at.x, at.y);
        }
        return;
    }
    let nearest = units
        .iter()
        .map(|(e, xf, u)| (e, wow_xy(xf.translation).distance(at), u))
        .filter(|(_, d, _)| *d <= PICK_RADIUS)
        .min_by(|a, b| a.1.total_cmp(&b.1));
    selection.unit = nearest.map(|(e, _, _)| e);
    if let Some((_, _, u)) = nearest {
        let d = defs.get(u.def);
        info!(
            "select: {} — {:?}, {:?}, guard radius {:.0}, health {:.0}/{:.0}",
            d.name, u.behaviour, u.priority, u.guard_radius, u.health, d.health
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn selected_keys(
    keys: Res<ButtonInput<KeyCode>>,
    defs: Res<UnitDefs>,
    window: Query<&Window, With<PrimaryWindow>>,
    camera: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    collision: WorldCollision,
    mut commands: Commands,
    mut selection: ResMut<Selection>,
    mut units: Query<(Entity, &Transform, &mut Unit)>,
) {
    let Some(entity) = selection.unit else { return };
    let Ok((_, _, mut u)) = units.get_mut(entity) else {
        selection.unit = None;
        return;
    };
    let name = defs.get(u.def).name.clone();
    if keys.just_pressed(KeyCode::KeyB) {
        u.behaviour = match u.behaviour {
            Behaviour::Hold => Behaviour::Guard,
            Behaviour::Guard | Behaviour::Escort => Behaviour::Hold,
        };
        u.target = None;
        info!("unit: {name} → {:?}", u.behaviour);
    }
    if keys.just_pressed(KeyCode::KeyP) {
        u.priority = u.priority.next();
        u.target = None;
        info!("unit: {name} targets {:?}", u.priority);
    }
    let step = if keys.just_pressed(KeyCode::BracketRight) {
        1.0
    } else if keys.just_pressed(KeyCode::BracketLeft) {
        -1.0
    } else {
        0.0
    };
    if step != 0.0 {
        u.guard_radius = (u.guard_radius + step).clamp(GUARD_RADIUS_MIN, GUARD_RADIUS_MAX);
        info!("unit: {name} guard radius {:.0}", u.guard_radius);
    }
    if keys.just_pressed(KeyCode::KeyM) {
        if let Some(at) = cursor_ground(&window, &camera, &collision) {
            u.anchor = at;
            u.target = None;
            info!("unit: {name} guards ({:.1}, {:.1})", at.x, at.y);
        }
    }
    if keys.just_pressed(KeyCode::Delete) {
        if u.def == defs.commander_index() {
            info!("unit: the commander cannot be dismissed");
            return;
        }
        commands.entity(entity).despawn();
        selection.unit = None;
        info!("unit: {name} dismissed");
    }
}

/// A circle on the terrain, sampled so it follows the ground.
pub fn ground_circle(
    gizmos: &mut Gizmos,
    heights: &TerrainHeights,
    center: Vec2,
    radius: f32,
    color: Color,
) {
    const SEGMENTS: usize = 48;
    let points: Vec<Vec3> = (0..=SEGMENTS)
        .map(|i| {
            let a = i as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            let p = center + Vec2::new(a.cos(), a.sin()) * radius;
            let flat = wow_to_bevy([p.x, p.y, 0.0]);
            let y = heights.under(flat).unwrap_or(0.0);
            Vec3::new(flat.x, y + 0.25, flat.z)
        })
        .collect();
    gizmos.linestrip(points, color);
}

fn draw_radii(
    overlay: Res<RadiusOverlay>,
    selection: Res<Selection>,
    defs: Res<UnitDefs>,
    heights: TerrainHeights,
    units: Query<(Entity, &Transform, &mut Unit)>,
    mut gizmos: Gizmos,
) {
    for (e, xf, u) in &units {
        let selected = selection.unit == Some(e);
        if !overlay.0 && !selected {
            continue;
        }
        let here = wow_xy(xf.translation);
        let d = defs.get(u.def);
        match u.behaviour {
            Behaviour::Guard => {
                ground_circle(&mut gizmos, &heights, u.anchor, u.guard_radius, Color::srgb(0.3, 0.6, 1.0));
            }
            Behaviour::Hold => {
                ground_circle(&mut gizmos, &heights, here, d.attack_range, Color::srgb(1.0, 0.6, 0.2));
            }
            Behaviour::Escort => {
                ground_circle(&mut gizmos, &heights, u.anchor, u.guard_radius, Color::srgb(0.75, 0.35, 1.0));
            }
        }
        if selected {
            ground_circle(&mut gizmos, &heights, here, 0.9, Color::srgb(1.0, 1.0, 0.3));
        }
    }
}

/// The unit script's commander lines, applied once the commander has spawned.
fn commander_script(
    mut script: ResMut<Script>,
    commander: Res<crate::commander::CommanderState>,
    mut selection: ResMut<Selection>,
    mut units: Query<&mut Unit>,
) {
    let Some(me) = commander.entity else { return };
    let Ok(mut u) = units.get_mut(me) else { return };
    if let Some(charge) = script.escort.take() {
        u.behaviour = Behaviour::Escort;
        u.escort = Some(charge);
        u.target = None;
        info!("unit script: the commander escorts {charge:?}");
    }
    if std::mem::take(&mut script.select_commander) {
        selection.unit = Some(me);
    }
}
