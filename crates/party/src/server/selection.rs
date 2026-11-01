//! Selecting party members in the RTS camera (M6a). A left click takes the member under it and a
//! left drag the members inside its box, Shift adds or removes, and a click on empty ground clears.
//! A double click swaps to the member. The selection is by guid, so a swap keeps it, and it reads
//! real input and live bodies, so it works over a paused world.
//!
//! Each selected member has a ring on the ground ([`PartySelection`]) and an outline over its unit
//! frame ([`PartyFrameRects`]), drawn here so the stock frames stay as they are. They show while
//! the RTS camera is on; the selection waits for it while it's off. The game's own click still
//! runs, so clicking an enemy targets it.

use benilla_app::{Objects, PartyFrameRects, PartySelection, UiFocus, WorldCamera};
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use super::rts::Rts;
use super::Server;

/// How far a press may travel and still be a click, in pixels: the reference's 2.25° of camera
/// travel (`PressGesture`), at the RTS camera's drag rate.
const CLICK_PIXELS: f32 = 10.0;
/// How long a press may be held and still be a click, in seconds, as `PressGesture`'s ceiling.
const CLICK_SECS: f32 = 0.8;
/// How near the cursor must be to a member's middle to pick it, in pixels.
const PICK_PIXELS: f32 = 48.0;
/// A member's middle above its feet, in yards.
const BODY_MIDDLE: f32 = 1.0;
/// The second click of a double click, within this many seconds.
const DOUBLE_CLICK_SECS: f32 = 0.35;
/// The interface's height, which [`PartyFrameRects`] measure in.
const SCREEN_UNITS: f32 = 768.0;
/// The most frames on screen: the player's and four party members'.
const OUTLINES: usize = 5;

const GREEN: Color = Color::srgb(0.25, 1.0, 0.35);
const BOX_FILL: Color = Color::srgba(0.25, 1.0, 0.35, 0.12);

/// The selected members, by guid, and the press under way.
#[derive(Resource, Default)]
struct Selection {
    members: Vec<u64>,
    press: Option<Press>,
    /// The last click on a member and when, for a double click.
    last_click: Option<(u64, f32)>,
}

/// A left press in the world: where and when it began, and how far it has gone.
#[derive(Clone, Copy)]
struct Press {
    from: Vec2,
    to: Vec2,
    at: f32,
    travel: f32,
}

impl Press {
    fn is_click(&self, now: f32) -> bool {
        self.travel < CLICK_PIXELS && now - self.at < CLICK_SECS
    }
}

#[derive(Component)]
struct BoxNode;

/// One frame outline, by its place in the pool.
#[derive(Component)]
struct Outline(usize);

pub(super) struct SelectionPlugin;

impl Plugin for SelectionPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Selection>()
            .add_systems(Startup, spawn_overlay)
            .add_systems(Update, (select, draw).chain());
    }
}

fn spawn_overlay(mut commands: Commands) {
    let node = || Node {
        position_type: PositionType::Absolute,
        border: UiRect::all(Val::Px(2.0)),
        ..default()
    };
    // Above the world and the interface, below the startup cover.
    let layer = GlobalZIndex(i32::MAX - 2);
    commands.spawn((
        BoxNode,
        Visibility::Hidden,
        node(),
        layer,
        BorderColor::all(GREEN),
        BackgroundColor(BOX_FILL),
    ));
    for at in 0..OUTLINES {
        commands.spawn((
            Outline(at),
            Visibility::Hidden,
            node(),
            layer,
            BorderColor::all(GREEN),
        ));
    }
}

/// The member nearest `at` on screen, within [`PICK_PIXELS`] of its middle.
fn member_at(at: Vec2, members: &[(u64, &'static str, Vec2)]) -> Option<(u64, &'static str)> {
    members
        .iter()
        .map(|&(guid, name, on_screen)| (on_screen.distance(at), guid, name))
        .filter(|(away, ..)| *away <= PICK_PIXELS)
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .map(|(_, guid, name)| (guid, name))
}

/// A click: on a member it replaces the selection, or with Shift toggles the member; on empty
/// ground it clears, or with Shift changes nothing. Whether it finished a double click, to swap.
fn click(sel: &mut Selection, under: Option<u64>, shift: bool, now: f32) -> bool {
    let Some(guid) = under else {
        if !shift {
            sel.members.clear();
        }
        sel.last_click = None;
        return false;
    };
    let double = sel
        .last_click
        .is_some_and(|(g, at)| g == guid && now - at <= DOUBLE_CLICK_SECS);
    sel.last_click = (!double).then_some((guid, now));
    if shift {
        match sel.members.iter().position(|&g| g == guid) {
            Some(at) => {
                sel.members.remove(at);
            }
            None => sel.members.push(guid),
        }
    } else {
        sel.members.clear();
        sel.members.push(guid);
    }
    double
}

/// A box: the members inside it replace the selection, or with Shift join it.
fn box_select(
    sel: &mut Selection,
    press: &Press,
    members: &[(u64, &'static str, Vec2)],
    shift: bool,
) {
    let rect = Rect::from_corners(press.from, press.to);
    if !shift {
        sel.members.clear();
    }
    for &(guid, _, on_screen) in members {
        if rect.contains(on_screen) && !sel.members.contains(&guid) {
            sel.members.push(guid);
        }
    }
    sel.last_click = None;
}

#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn select(
    rts: Res<Rts>,
    mut sel: ResMut<Selection>,
    server: Res<Server>,
    time: Res<Time<Real>>,
    objects: Objects,
    (keys, buttons): (Res<ButtonInput<KeyCode>>, Res<ButtonInput<MouseButton>>),
    focus: UiFocus,
    window: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    bodies: Query<&GlobalTransform, Without<WorldCamera>>,
) {
    let sel = &mut *sel;
    if !rts.settled() {
        sel.press = None;
        return;
    }
    let cursor = window.single().ok().and_then(Window::cursor_position);
    let now = time.elapsed_secs();

    if buttons.just_pressed(MouseButton::Left) && !focus.hovering() {
        sel.press = cursor.map(|at| Press {
            from: at,
            to: at,
            at: now,
            travel: 0.0,
        });
    }
    let Some(press) = sel.press.as_mut() else {
        return;
    };
    if let Some(at) = cursor {
        press.travel = press.travel.max(at.distance(press.from));
        press.to = at;
    }
    if buttons.pressed(MouseButton::Left) {
        return;
    }

    // The release: the members where the camera shows them now.
    let press = *press;
    sel.press = None;
    let Ok((view, view_global)) = cameras.single() else {
        return;
    };
    let party = server.0.party.lock().unwrap().clone();
    let on_screen: Vec<(u64, &'static str, Vec2)> = party
        .iter()
        .filter_map(|&(guid, name)| {
            let (_, entity, _) = objects.iter().find(|(g, ..)| *g == guid)?;
            let middle = bodies.get(entity).ok()?.translation() + Vec3::Y * BODY_MIDDLE;
            Some((
                guid,
                name,
                view.world_to_viewport(view_global, middle).ok()?,
            ))
        })
        .collect();
    let shift = keys.any_pressed([KeyCode::ShiftLeft, KeyCode::ShiftRight]);
    if press.is_click(now) {
        let under = member_at(press.from, &on_screen);
        if click(sel, under.map(|(guid, _)| guid), shift, now) {
            if let Some((guid, name)) = under {
                if server.0.self_guid() != guid {
                    server.0.swap_to(name);
                }
            }
        }
    } else {
        box_select(sel, &press, &on_screen, shift);
    }
}

/// Publishes the rings and draws the box and the frame outlines.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn draw(
    rts: Res<Rts>,
    sel: Res<Selection>,
    server: Res<Server>,
    time: Res<Time<Real>>,
    frames: Res<PartyFrameRects>,
    mut rings: ResMut<PartySelection>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut boxes: Query<(&mut Node, &mut Visibility), (With<BoxNode>, Without<Outline>)>,
    mut outlines: Query<(&Outline, &mut Node, &mut Visibility), Without<BoxNode>>,
) {
    let shown: &[u64] = if rts.on() { &sel.members } else { &[] };
    if rings.0 != shown {
        rings.0 = shown.to_vec();
    }

    if let Ok((mut node, mut visibility)) = boxes.single_mut() {
        let dragging = sel
            .press
            .filter(|p| !p.is_click(time.elapsed_secs()))
            .filter(|_| rts.settled());
        match dragging {
            Some(press) => {
                let rect = Rect::from_corners(press.from, press.to);
                node.left = Val::Px(rect.min.x);
                node.top = Val::Px(rect.min.y);
                node.width = Val::Px(rect.width());
                node.height = Val::Px(rect.height());
                visibility.set_if_neq(Visibility::Inherited);
            }
            None => {
                visibility.set_if_neq(Visibility::Hidden);
            }
        }
    }

    // The selected members' frames, by name.
    let party = server.0.party.lock().unwrap().clone();
    let names: Vec<&str> = party
        .iter()
        .filter(|(guid, _)| shown.contains(guid))
        .map(|(_, name)| *name)
        .collect();
    let scale = window.single().map_or(1.0, |w| w.height() / SCREEN_UNITS);
    let mut chosen = frames.0.iter().filter(|f| names.contains(&f.name.as_str()));
    let mut pool: Vec<_> = outlines.iter_mut().collect();
    pool.sort_by_key(|(outline, ..)| outline.0);
    for (_, node, visibility) in &mut pool {
        match chosen.next() {
            Some(frame) => {
                node.left = Val::Px(frame.left * scale);
                node.top = Val::Px(frame.top * scale);
                node.width = Val::Px(frame.width * scale);
                node.height = Val::Px(frame.height * scale);
                visibility.set_if_neq(Visibility::Inherited);
            }
            None => {
                visibility.set_if_neq(Visibility::Hidden);
            }
        }
    }
}
