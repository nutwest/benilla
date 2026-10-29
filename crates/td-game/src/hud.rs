//! A basic HUD in Bevy UI (no FrameXML): a status bar (stores, lives, wave) along the top, build
//! and recruit buttons along the bottom, and a panel for the selected unit's behaviour, target
//! priority, guard radius and anchor. The face is the game's own Friz Quadrata
//! (`Fonts\FRIZQT__.TTF`), read from the install like every other asset.
//!
//! Every HUD node takes the pointer: while it is over one, world clicks (placing, building,
//! selecting) are ignored ([`PointerOverHud`]). The keyboard shortcuts stay as they were.

use bevy::prelude::*;
use bevy::ui::{FocusPolicy, UiSystems};

use crate::build::BuildMode;
use crate::economy::{Amount, Stores};
use crate::level::Level;
use crate::structures::StructureDefs;
use crate::units::{
    Behaviour, Priority, Selection, Unit, UnitDefs, UnitMode, GUARD_RADIUS_MAX, GUARD_RADIUS_MIN,
};
use crate::waves::{Lives, WaveRunner};

const FONT: &str = "mpq://fonts/frizqt__.ttf";

const PANEL: Color = Color::srgba(0.05, 0.05, 0.07, 0.82);
const BUTTON: Color = Color::srgba(0.18, 0.16, 0.12, 0.95);
const BUTTON_HOVER: Color = Color::srgba(0.30, 0.26, 0.18, 0.95);
const BUTTON_ACTIVE: Color = Color::srgba(0.55, 0.42, 0.12, 0.95);
const BORDER: Color = Color::srgb(0.55, 0.45, 0.25);
const TEXT: Color = Color::srgb(0.95, 0.88, 0.65);
const TEXT_DIM: Color = Color::srgb(0.75, 0.72, 0.62);

/// What a HUD button does.
#[derive(Component, Clone, Copy, PartialEq)]
enum Action {
    Structure(usize),
    Unit(usize),
    Behaviour(Behaviour),
    Priority(Priority),
    Radius(f32),
    PickAnchor,
    Dismiss,
}

/// Whether the pointer is over the HUD this frame; world clicks check it.
#[derive(Resource, Default)]
pub struct PointerOverHud(pub bool);

#[derive(Component)]
struct StatusText;

#[derive(Component)]
struct UnitPanel;

#[derive(Component)]
struct UnitPanelText;

pub fn plugin(app: &mut App) {
    app.init_resource::<PointerOverHud>()
        .add_systems(Startup, spawn.after(crate::structures::load_defs))
        .add_systems(PreUpdate, track_pointer.after(UiSystems::Focus))
        .add_systems(Update, (press, paint, status, unit_panel));
}

fn spawn(
    mut commands: Commands,
    asset_server: Res<AssetServer>,
    structures: Option<Res<StructureDefs>>,
    units: Res<UnitDefs>,
) {
    let font = asset_server.load(FONT);
    let text = |size: f32, color: Color| {
        (
            TextFont {
                font: font.clone(),
                font_size: size,
                ..default()
            },
            TextColor(color),
        )
    };

    // The screen: status on top, buttons at the bottom; the root itself lets clicks through.
    commands
        .spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::SpaceBetween,
                ..default()
            },
            FocusPolicy::Pass,
            Pickable::IGNORE,
        ))
        .with_children(|root| {
            root.spawn((panel_node(), BackgroundColor(PANEL), Interaction::default()))
                .with_children(|bar| {
                    bar.spawn((Text::new(""), text(18.0, TEXT), StatusText));
                });

            root.spawn((
                Node {
                    flex_direction: FlexDirection::Column,
                    row_gap: Val::Px(6.0),
                    ..panel_node()
                },
                BackgroundColor(PANEL),
                Interaction::default(),
            ))
            .with_children(|bar| {
                if let Some(structures) = &structures {
                    bar.spawn(row()).with_children(|row| {
                        row.spawn((Text::new("Build"), text(16.0, TEXT_DIM), label_node()));
                        for (i, d) in structures.defs.iter().enumerate() {
                            button(row, &format!("{}\n{}", d.name, short_cost(&d.cost)), Action::Structure(i), &text);
                        }
                    });
                }
                bar.spawn(row()).with_children(|row| {
                    row.spawn((Text::new("Recruit"), text(16.0, TEXT_DIM), label_node()));
                    for (i, d) in units.defs.iter().enumerate() {
                        button(row, &format!("{}\n{}", d.name, short_cost(&d.cost)), Action::Unit(i), &text);
                    }
                    row.spawn((
                        Text::new(
                            "Click a button, then the ground · R turn · Esc cancel · X sell · \
                             click a unit to select · F1–F4 overlays",
                        ),
                        text(13.0, TEXT_DIM),
                        Node {
                            margin: UiRect::left(Val::Px(16.0)),
                            align_self: AlignSelf::Center,
                            ..default()
                        },
                    ));
                });
            });
        });

    // The selected unit's panel, on the right, hidden until something is selected.
    commands
        .spawn((
            Node {
                position_type: PositionType::Absolute,
                right: Val::Px(12.0),
                top: Val::Px(56.0),
                width: Val::Px(250.0),
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(6.0),
                padding: UiRect::all(Val::Px(10.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(6.0)),
                display: Display::None,
                ..default()
            },
            BackgroundColor(PANEL),
            BorderColor::all(BORDER),
            Interaction::default(),
            UnitPanel,
        ))
        .with_children(|panel| {
            panel.spawn((Text::new(""), text(16.0, TEXT), UnitPanelText));
            panel.spawn((Text::new("Behaviour"), text(13.0, TEXT_DIM)));
            panel.spawn(row()).with_children(|row| {
                button(row, "Hold", Action::Behaviour(Behaviour::Hold), &text);
                button(row, "Guard", Action::Behaviour(Behaviour::Guard), &text);
            });
            panel.spawn((Text::new("Target priority"), text(13.0, TEXT_DIM)));
            panel.spawn(row()).with_children(|row| {
                button(row, "First", Action::Priority(Priority::First), &text);
                button(row, "Nearest", Action::Priority(Priority::Nearest), &text);
            });
            panel.spawn(row()).with_children(|row| {
                button(row, "Strongest", Action::Priority(Priority::Strongest), &text);
                button(row, "Weakest", Action::Priority(Priority::Weakest), &text);
            });
            panel.spawn((Text::new("Guard radius"), text(13.0, TEXT_DIM)));
            panel.spawn(row()).with_children(|row| {
                button(row, "−", Action::Radius(-1.0), &text);
                button(row, "+", Action::Radius(1.0), &text);
                button(row, "Set post", Action::PickAnchor, &text);
            });
            panel.spawn(row()).with_children(|row| {
                button(row, "Dismiss", Action::Dismiss, &text);
            });
        });
}

fn panel_node() -> Node {
    Node {
        padding: UiRect::axes(Val::Px(12.0), Val::Px(8.0)),
        column_gap: Val::Px(6.0),
        ..default()
    }
}

fn row() -> Node {
    Node {
        flex_direction: FlexDirection::Row,
        flex_wrap: FlexWrap::Wrap,
        column_gap: Val::Px(6.0),
        row_gap: Val::Px(6.0),
        align_items: AlignItems::Center,
        ..default()
    }
}

fn label_node() -> Node {
    Node {
        width: Val::Px(64.0),
        ..default()
    }
}

fn button(
    parent: &mut ChildSpawnerCommands,
    label: &str,
    action: Action,
    text: &impl Fn(f32, Color) -> (TextFont, TextColor),
) {
    parent
        .spawn((
            Button,
            Node {
                padding: UiRect::axes(Val::Px(10.0), Val::Px(5.0)),
                border: UiRect::all(Val::Px(1.0)),
                border_radius: BorderRadius::all(Val::Px(4.0)),
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                ..default()
            },
            BackgroundColor(BUTTON),
            BorderColor::all(BORDER),
            action,
        ))
        .with_children(|b| {
            b.spawn((
                Text::new(label),
                text(14.0, TEXT),
                TextLayout::new_with_justify(Justify::Center),
            ));
        });
}

/// A cost with only its non-zero currencies, e.g. `40 lumber 60 stone`.
fn short_cost(a: &Amount) -> String {
    let parts: Vec<String> = [
        (a.gold, "gold"),
        (a.food, "food"),
        (a.lumber, "lumber"),
        (a.stone, "stone"),
    ]
    .iter()
    .filter(|(v, _)| *v > 0.0)
    .map(|(v, n)| format!("{v} {n}"))
    .collect();
    if parts.is_empty() {
        "free".into()
    } else {
        parts.join(" ")
    }
}

fn track_pointer(nodes: Query<&Interaction>, mut over: ResMut<PointerOverHud>) {
    over.0 = nodes.iter().any(|i| *i != Interaction::None);
}

#[allow(clippy::too_many_arguments)]
fn press(
    mut commands: Commands,
    buttons: Query<(&Interaction, &Action), Changed<Interaction>>,
    defs: Res<UnitDefs>,
    mut build: ResMut<BuildMode>,
    mut recruit: ResMut<UnitMode>,
    mut selection: ResMut<Selection>,
    mut units: Query<&mut Unit>,
) {
    for (interaction, action) in &buttons {
        if *interaction != Interaction::Pressed {
            continue;
        }
        match *action {
            Action::Structure(i) => {
                build.def = if build.def == Some(i) { None } else { Some(i) };
                recruit.def = None;
            }
            Action::Unit(i) => {
                recruit.def = if recruit.def == Some(i) { None } else { Some(i) };
                build.def = None;
            }
            _ => {
                let Some(entity) = selection.unit else { continue };
                let Ok(mut u) = units.get_mut(entity) else { continue };
                let name = &defs.defs[u.def].name;
                match *action {
                    Action::Behaviour(b) => {
                        u.behaviour = b;
                        u.target = None;
                        info!("unit: {name} → {b:?}");
                    }
                    Action::Priority(p) => {
                        u.priority = p;
                        u.target = None;
                        info!("unit: {name} targets {p:?}");
                    }
                    Action::Radius(step) => {
                        u.guard_radius =
                            (u.guard_radius + step).clamp(GUARD_RADIUS_MIN, GUARD_RADIUS_MAX);
                        info!("unit: {name} guard radius {:.0}", u.guard_radius);
                    }
                    Action::PickAnchor => {
                        selection.pick_anchor = true;
                        info!("unit: click the ground to set {name}'s post");
                    }
                    Action::Dismiss => {
                        info!("unit: {name} dismissed");
                        commands.entity(entity).despawn();
                        selection.unit = None;
                    }
                    Action::Structure(_) | Action::Unit(_) => {}
                }
            }
        }
    }
}

/// Button colours: pressed or hovered, else lit while its choice is the current one.
fn paint(
    build: Res<BuildMode>,
    recruit: Res<UnitMode>,
    selection: Res<Selection>,
    units: Query<&Unit>,
    mut buttons: Query<(&Interaction, &Action, &mut BackgroundColor)>,
) {
    let selected = selection.unit.and_then(|e| units.get(e).ok());
    for (interaction, action, mut bg) in &mut buttons {
        let active = match *action {
            Action::Structure(i) => build.def == Some(i),
            Action::Unit(i) => recruit.def == Some(i),
            Action::Behaviour(b) => selected.is_some_and(|u| u.behaviour == b),
            Action::Priority(p) => selected.is_some_and(|u| u.priority == p),
            Action::PickAnchor => selection.pick_anchor,
            Action::Radius(_) | Action::Dismiss => false,
        };
        let color = match (interaction, active) {
            (Interaction::Pressed, _) | (_, true) => BUTTON_ACTIVE,
            (Interaction::Hovered, false) => BUTTON_HOVER,
            (Interaction::None, false) => BUTTON,
        };
        if bg.0 != color {
            bg.0 = color;
        }
    }
}

fn status(
    stores: Res<Stores>,
    lives: Res<Lives>,
    level: Res<Level>,
    runner: Res<WaveRunner>,
    mut text: Query<&mut Text, With<StatusText>>,
) {
    let Ok(mut text) = text.single_mut() else { return };
    let a = stores.amount();
    let total = level.waves.len();
    let wave = if !runner.started {
        "waiting".to_string()
    } else {
        format!("{}/{total}", (runner.index + 1).min(total))
    };
    let line = format!(
        "Gold {}    Food {}    Lumber {}    Stone {}        Lives {}        Wave {wave}",
        a.gold.floor(),
        a.food.floor(),
        a.lumber.floor(),
        a.stone.floor(),
        lives.0,
    );
    if text.0 != line {
        text.0 = line;
    }
}

fn unit_panel(
    selection: Res<Selection>,
    defs: Res<UnitDefs>,
    units: Query<&Unit>,
    mut panel: Query<&mut Node, With<UnitPanel>>,
    mut text: Query<&mut Text, With<UnitPanelText>>,
) {
    let selected = selection.unit.and_then(|e| units.get(e).ok());
    if let Ok(mut node) = panel.single_mut() {
        let want = if selected.is_some() {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != want {
            node.display = want;
        }
    }
    let (Some(u), Ok(mut text)) = (selected, text.single_mut()) else {
        return;
    };
    let d = &defs.defs[u.def];
    let line = format!(
        "{}\nHealth {:.0}/{:.0}   Damage {:.0}\nRange {:.1}   Guard radius {:.0}",
        d.name, u.health, d.health, d.damage, d.attack_range, u.guard_radius
    );
    if text.0 != line {
        text.0 = line;
    }
}
