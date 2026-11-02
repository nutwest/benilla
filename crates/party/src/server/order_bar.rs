//! Giving orders in the RTS camera (M6b): the right click, the H, G and F keys, and a bar of Hold,
//! Guard and Follow buttons above the action bar while members are selected. A right click is by
//! what's under it: an enemy is attacked, a member followed, the ground walked to. Guard picks a
//! post: a circle follows the cursor, the wheel or `[` and `]` set its radius, a left click places
//! it, and Esc or a right click cancels.
//!
//! The bar is the crate's own Bevy nodes, sized and placed here so it can be hit-tested here: the
//! stock interface is untouched, and its own frames still take the mouse first.

use benilla_app::{
    Hostility, Objects, PartyMarks, TerrainHeights, UiFocus, WorldCamera, WorldCollision,
};
use bevy::input::mouse::MouseWheel;
use bevy::prelude::*;
use bevy::window::PrimaryWindow;

use super::orders::{Order, Orders};
use super::rts::Rts;
use super::selection::{member_at, party_on_screen, Selection, PICK_PIXELS};
use super::Server;

/// The game's own Friz Quadrata, read from the install.
const FONT: &str = "mpq://fonts/frizqt__.ttf";
/// A button's side and the gap between two, in pixels.
const BUTTON: f32 = 40.0;
const GAP: f32 = 6.0;
/// The bar's bottom edge above the window's, in pixels: clear of the action bar at the stock scale.
const LIFT: f32 = 96.0;
/// Guard's radius, in yards, as td-game's: its range, where it opens, and a step of the keys.
const RADIUS_MIN: f32 = 4.0;
const RADIUS_MAX: f32 = 15.0;
const RADIUS_START: f32 = 8.0;
const RADIUS_STEP: f32 = 1.0;
/// How far a right click's ray looks for the ground, in yards.
const RAY_REACH: f32 = 2000.0;
/// The type mask's unit bit, on an object's descriptor.
const TYPEMASK_UNIT: u32 = 0x8;

const GOLD: Color = Color::srgb(1.0, 0.82, 0.0);
const DIM_GOLD: Color = Color::srgb(0.55, 0.45, 0.05);
const EDGE: Color = Color::srgb(0.32, 0.32, 0.32);
const TOOLTIP_BACK: Color = Color::srgba(0.03, 0.03, 0.10, 0.9);
/// How a button's icon shows, by whether its order is in force.
const ICON_LIT: Color = Color::WHITE;
const ICON_PART: Color = Color::srgb(0.75, 0.75, 0.75);
const ICON_OFF: Color = Color::srgb(0.5, 0.5, 0.5);

/// A button of the bar, in its order.
#[derive(Component, Clone, Copy, PartialEq, Debug)]
enum Kind {
    Hold,
    Guard,
    Follow,
}

impl Kind {
    const ALL: [Kind; 3] = [Kind::Hold, Kind::Guard, Kind::Follow];

    fn icon(self) -> &'static str {
        match self {
            Kind::Hold => "mpq://interface/icons/ability_warrior_defensivestance.blp",
            Kind::Guard => "mpq://interface/icons/ability_defend.blp",
            Kind::Follow => "mpq://interface/icons/ability_tracking.blp",
        }
    }

    fn tooltip(self) -> &'static str {
        match self {
            Kind::Hold => "Hold (H)\nStay here and fight within reach.",
            Kind::Guard => "Guard (G)\nPlace a post: fight what enters its radius, then return.",
            Kind::Follow => "Follow me (F)\nFollow the character you play.",
        }
    }

    /// Whether `order` is this button's. A Move ends in a Hold, so it reads as one.
    fn matches(self, order: Option<Order>) -> bool {
        matches!(
            (self, order),
            (Kind::Hold, Some(Order::Hold | Order::Move(_)))
                | (Kind::Guard, Some(Order::Guard(..)))
                | (Kind::Follow, Some(Order::Follow(_)))
        )
    }
}

/// The bar's left edge and top, for a window, in pixels.
fn bar_origin(window: Vec2) -> Vec2 {
    let width = BUTTON * 3.0 + GAP * 2.0;
    Vec2::new((window.x - width) * 0.5, window.y - LIFT - BUTTON)
}

/// The button at `at`, in a window of that size.
fn button_at(at: Vec2, window: Vec2) -> Option<Kind> {
    let origin = bar_origin(window);
    Kind::ALL.into_iter().enumerate().find_map(|(i, kind)| {
        let left = origin.x + i as f32 * (BUTTON + GAP);
        let inside =
            (left..left + BUTTON).contains(&at.x) && (origin.y..origin.y + BUTTON).contains(&at.y);
        inside.then_some(kind)
    })
}

/// The bar's mode: whether a Guard post is being placed, and the button under the cursor.
#[derive(Resource, Default)]
struct OrderUi {
    /// Guard's radius, while placing its post.
    placing: Option<f32>,
    hovered: Option<Kind>,
}

#[derive(Component)]
struct BarRoot;

#[derive(Component)]
struct Tooltip;

pub(super) struct OrderBarPlugin;

impl Plugin for OrderBarPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<OrderUi>()
            .add_systems(Startup, spawn_bar)
            .add_systems(Update, (give, draw).chain());
    }
}

fn spawn_bar(mut commands: Commands, assets: Res<AssetServer>) {
    let layer = GlobalZIndex(i32::MAX - 2);
    commands
        .spawn((
            BarRoot,
            Visibility::Hidden,
            layer,
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(LIFT),
                width: Val::Percent(100.0),
                justify_content: JustifyContent::Center,
                column_gap: Val::Px(GAP),
                ..default()
            },
        ))
        .with_children(|bar| {
            for kind in Kind::ALL {
                bar.spawn((
                    kind,
                    ImageNode::new(assets.load(kind.icon())),
                    BorderColor::all(EDGE),
                    Node {
                        width: Val::Px(BUTTON),
                        height: Val::Px(BUTTON),
                        border: UiRect::all(Val::Px(2.0)),
                        ..default()
                    },
                ));
            }
        });
    commands
        .spawn((
            Tooltip,
            Visibility::Hidden,
            layer,
            BackgroundColor(TOOLTIP_BACK),
            BorderColor::all(EDGE),
            Node {
                position_type: PositionType::Absolute,
                bottom: Val::Px(LIFT + BUTTON + 8.0),
                border: UiRect::all(Val::Px(1.0)),
                padding: UiRect::all(Val::Px(6.0)),
                ..default()
            },
        ))
        .with_children(|tip| {
            tip.spawn((
                Text::new(""),
                TextFont {
                    font: assets.load(FONT),
                    font_size: 13.0,
                    ..default()
                },
                TextColor(Color::WHITE),
            ));
        });
}

/// What a right click at `at` means, by what's under it.
#[allow(clippy::too_many_arguments)]
fn right_click_order(
    at: Vec2,
    selected: &[u64],
    (server, objects): (&Server, &Objects),
    (view, view_global): (&Camera, &GlobalTransform),
    bodies: &Query<&GlobalTransform, Without<WorldCamera>>,
    collision: &WorldCollision,
    hostility: &Hostility,
) -> Vec<(u64, Order)> {
    let on_screen = party_on_screen(server, objects, view, view_global, bodies);
    let everyone = |order: Order| selected.iter().map(|&g| (g, order)).collect::<Vec<_>>();

    if let Some((member, _)) = member_at(at, &on_screen) {
        return selected
            .iter()
            .filter(|&&g| g != member)
            .map(|&g| (g, Order::Follow(member)))
            .collect();
    }
    // The nearest enemy on screen near the cursor.
    let enemy = objects
        .iter()
        .filter(|(_, _, fields)| fields.object_type_mask().unwrap_or(0) & TYPEMASK_UNIT != 0)
        .filter(|(guid, ..)| hostility.hostile(*guid))
        .filter_map(|(guid, entity, _)| {
            let middle = bodies.get(entity).ok()?.translation() + Vec3::Y;
            let shown = view.world_to_viewport(view_global, middle).ok()?;
            Some((shown.distance(at), guid))
        })
        .filter(|(away, _)| *away <= PICK_PIXELS)
        .min_by(|a, b| a.0.total_cmp(&b.0));
    if let Some((_, guid)) = enemy {
        return everyone(Order::Attack(guid));
    }
    match ground_at(at, view, view_global, collision) {
        Some(point) => everyone(Order::Move(benilla_app::bevy_to_wow(point))),
        None => Vec::new(),
    }
}

/// The world under the cursor.
fn ground_at(
    at: Vec2,
    view: &Camera,
    view_global: &GlobalTransform,
    collision: &WorldCollision,
) -> Option<Vec3> {
    let ray = view.viewport_to_world(view_global, at).ok()?;
    let hit = collision.ray_body(ray.origin, ray.direction, RAY_REACH)?;
    Some(ray.origin + *ray.direction * hit.distance)
}

/// Takes the keys, the bar and the right click, and places a Guard post.
#[allow(clippy::too_many_arguments, clippy::type_complexity)]
fn give(
    mut rts: ResMut<Rts>,
    mut ui: ResMut<OrderUi>,
    selection: Res<Selection>,
    server: Res<Server>,
    (keys, buttons, focus): (
        Res<ButtonInput<KeyCode>>,
        Res<ButtonInput<MouseButton>>,
        UiFocus,
    ),
    mut wheel: MessageReader<MouseWheel>,
    (objects, hostility, collision, heights): (Objects, Hostility, WorldCollision, TerrainHeights),
    window: Query<&Window, With<PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<WorldCamera>>,
    bodies: Query<&GlobalTransform, Without<WorldCamera>>,
    mut marks: ResMut<PartyMarks>,
) {
    let selected: Vec<u64> = selection.members().to_vec();
    if !rts.settled() || selected.is_empty() {
        ui.placing = None;
        ui.hovered = None;
        rts.placing = false;
        rts.over_bar = false;
        benilla_app::yield_escape(false);
        if !marks.0.is_empty() {
            marks.0.clear();
        }
        wheel.clear();
        return;
    }
    let Ok(win) = window.single() else { return };
    let size = Vec2::new(win.width(), win.height());
    let cursor = win.cursor_position();
    ui.hovered = cursor.and_then(|at| button_at(at, size));
    rts.over_bar = ui.hovered.is_some();

    let played = server.0.self_guid();
    let give_all = |order: Order| {
        for &guid in &selected {
            server.0.give_order(guid, order);
        }
    };
    let follow_me = || {
        for guid in selected.iter().copied().filter(|&g| g != played) {
            server.0.give_order(guid, Order::Follow(played));
        }
    };

    let typing = focus.typing();
    let mut press = None;
    if !typing && keys.just_pressed(KeyCode::KeyH) {
        press = Some(Kind::Hold);
    }
    if !typing && keys.just_pressed(KeyCode::KeyG) {
        press = Some(Kind::Guard);
    }
    if !typing && keys.just_pressed(KeyCode::KeyF) {
        press = Some(Kind::Follow);
    }
    if buttons.just_pressed(MouseButton::Left) && ui.placing.is_none() {
        press = press.or(ui.hovered);
    }
    match press {
        Some(Kind::Hold) => give_all(Order::Hold),
        Some(Kind::Follow) => follow_me(),
        Some(Kind::Guard) => ui.placing = Some(ui.placing.unwrap_or(RADIUS_START)),
        None => {}
    }

    let camera = cameras.single().ok();
    let right_click = rts.take_right_click();
    let Some(radius) = ui.placing else {
        rts.placing = false;
        benilla_app::yield_escape(false);
        if !marks.0.is_empty() {
            marks.0.clear();
        }
        wheel.clear();
        if let (true, Some(at), Some(view)) = (right_click, cursor, camera) {
            if !focus.hovering() && ui.hovered.is_none() {
                for (guid, order) in right_click_order(
                    at,
                    &selected,
                    (&server, &objects),
                    view,
                    &bodies,
                    &collision,
                    &hostility,
                ) {
                    server.0.give_order(guid, order);
                }
            }
        }
        return;
    };

    // Placing a Guard post.
    rts.placing = true;
    benilla_app::yield_escape(true);
    if right_click || keys.just_pressed(KeyCode::Escape) {
        ui.placing = None;
        marks.0.clear();
        return;
    }
    let mut radius = radius;
    for ev in wheel.read() {
        radius += ev.y.signum() * RADIUS_STEP;
    }
    if !typing && keys.just_pressed(KeyCode::BracketLeft) {
        radius -= RADIUS_STEP;
    }
    if !typing && keys.just_pressed(KeyCode::BracketRight) {
        radius += RADIUS_STEP;
    }
    let radius = radius.clamp(RADIUS_MIN, RADIUS_MAX);
    ui.placing = Some(radius);

    let post = cursor.zip(camera).and_then(|(at, (view, view_global))| {
        let hit = ground_at(at, view, view_global, &collision)?;
        Some(Vec3::new(
            hit.x,
            heights.under(hit).unwrap_or(hit.y).max(hit.y),
            hit.z,
        ))
    });
    marks.0.clear();
    if let Some(post) = post {
        marks.0.push((post, radius));
        if buttons.just_pressed(MouseButton::Left) && !focus.hovering() {
            give_all(Order::Guard(benilla_app::bevy_to_wow(post), radius));
            ui.placing = None;
            marks.0.clear();
        }
    }
}

/// How many of `selected` have `kind`'s order, as none, some or all.
fn lit(kind: Kind, selected: &[u64], orders: &Orders) -> (Color, Color) {
    let having = selected
        .iter()
        .filter(|&&guid| kind.matches(orders.of(guid)))
        .count();
    if having == 0 {
        (EDGE, ICON_OFF)
    } else if having == selected.len() {
        (GOLD, ICON_LIT)
    } else {
        (DIM_GOLD, ICON_PART)
    }
}

/// Shows the bar while members are selected, each button lit by the orders in force.
#[allow(clippy::type_complexity)]
fn draw(
    rts: Res<Rts>,
    ui: Res<OrderUi>,
    selection: Res<Selection>,
    orders: Res<Orders>,
    window: Query<&Window, With<PrimaryWindow>>,
    mut root: Query<&mut Visibility, (With<BarRoot>, Without<Tooltip>)>,
    mut buttons: Query<(&Kind, &mut BorderColor, &mut ImageNode)>,
    mut tooltip: Query<(&mut Node, &mut Visibility, &Children), With<Tooltip>>,
    mut texts: Query<&mut Text>,
) {
    let shown = rts.on() && !selection.members().is_empty();
    if let Ok(mut visibility) = root.single_mut() {
        visibility.set_if_neq(if shown {
            Visibility::Inherited
        } else {
            Visibility::Hidden
        });
    }
    for (kind, mut border, mut image) in &mut buttons {
        let placing = *kind == Kind::Guard && ui.placing.is_some();
        let (edge, tint) = if placing {
            (GOLD, ICON_LIT)
        } else {
            lit(*kind, selection.members(), &orders)
        };
        border.set_all(edge);
        image.color = tint;
    }
    let Ok((mut node, mut visibility, children)) = tooltip.single_mut() else {
        return;
    };
    match ui.hovered.filter(|_| shown) {
        Some(kind) => {
            if let (Some(&child), Ok(win)) = (children.first(), window.single()) {
                if let Ok(mut text) = texts.get_mut(child) {
                    if text.0 != kind.tooltip() {
                        text.0 = kind.tooltip().to_string();
                    }
                }
                let origin = bar_origin(Vec2::new(win.width(), win.height()));
                node.left = Val::Px(origin.x);
            }
            visibility.set_if_neq(Visibility::Inherited);
        }
        None => {
            visibility.set_if_neq(Visibility::Hidden);
        }
    }
}
