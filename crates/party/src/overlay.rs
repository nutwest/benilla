//! The full-screen cover while the server is set up and started: what it is doing, a progress bar,
//! and the error if it fails. It hides the login screen the client logs in through, and lifts once
//! our character is in the world, or once the login goes out on an account with no character, so
//! the stock create screen shows.

use bevy::prelude::*;

use crate::server::{covering, Phase, Server};

/// The game's own Friz Quadrata, read from the install.
const FONT: &str = "mpq://fonts/frizqt__.ttf";

pub struct OverlayPlugin;

impl Plugin for OverlayPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, spawn).add_systems(Update, update);
    }
}

#[derive(Component)]
struct Cover;

#[derive(Component)]
struct StepText;

#[derive(Component)]
struct DetailText;

#[derive(Component)]
struct BarFill;

#[derive(Component)]
struct Bar;

const GOLD: Color = Color::srgb(1.0, 0.82, 0.0);
const GREY: Color = Color::srgb(0.75, 0.75, 0.75);

fn spawn(mut commands: Commands, assets: Res<AssetServer>) {
    let font = assets.load(FONT);
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
    commands
        .spawn((
            Cover,
            Node {
                position_type: PositionType::Absolute,
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(14.0),
                ..default()
            },
            BackgroundColor(Color::srgb(0.03, 0.03, 0.04)),
            GlobalZIndex(i32::MAX),
        ))
        .with_children(|root| {
            root.spawn((Text::new("Preparing your world"), text(34.0, GOLD)));
            root.spawn((StepText, Text::new("Starting"), text(22.0, Color::WHITE)));
            root.spawn((
                Bar,
                Node {
                    width: Val::Px(420.0),
                    height: Val::Px(10.0),
                    ..default()
                },
                BackgroundColor(Color::srgb(0.15, 0.15, 0.17)),
            ))
            .with_children(|bar| {
                bar.spawn((
                    BarFill,
                    Node {
                        width: Val::Percent(0.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    BackgroundColor(GOLD),
                ));
            });
            root.spawn((DetailText, Text::new(""), text(16.0, GREY)));
        });
}

/// One of the overlay's three nodes, disjoint from the other two.
type NodeOf<'w, 's, T, A, B> = Query<'w, 's, &'static mut Node, (With<T>, Without<A>, Without<B>)>;
/// One of the overlay's two text lines, disjoint from the other.
type TextOf<'w, 's, T, O> =
    Query<'w, 's, (&'static mut Text, &'static mut TextColor), (With<T>, Without<O>)>;

fn update(
    server: Res<Server>,
    objects: benilla_app::Objects,
    mut cover: NodeOf<Cover, Bar, BarFill>,
    mut step: TextOf<StepText, DetailText>,
    mut detail: TextOf<DetailText, StepText>,
    mut bar: NodeOf<Bar, Cover, BarFill>,
    mut fill: NodeOf<BarFill, Cover, Bar>,
) {
    let status = server.0.status();
    let show = covering(&status, objects.player().is_some(), &server);
    for mut node in &mut cover {
        let display = if show { Display::Flex } else { Display::None };
        if node.display != display {
            node.display = display;
        }
    }
    if !show {
        return;
    }
    let (step_line, detail_line, failed) = match &status.phase {
        Phase::Failed(why) => ("The server could not start".to_string(), why.clone(), true),
        Phase::Starting => ("Starting".to_string(), String::new(), false),
        _ => (status.step.clone(), status.detail.clone(), false),
    };
    for (mut text, _) in &mut step {
        if text.0 != step_line {
            text.0 = step_line.clone();
        }
    }
    for (mut text, mut color) in &mut detail {
        if text.0 != detail_line {
            text.0 = detail_line.clone();
        }
        color.0 = if failed {
            Color::srgb(1.0, 0.3, 0.3)
        } else {
            GREY
        };
    }
    for mut node in &mut bar {
        let display = if status.fraction.is_some() {
            Display::Flex
        } else {
            Display::None
        };
        if node.display != display {
            node.display = display;
        }
    }
    if let Some(f) = status.fraction {
        for mut node in &mut fill {
            node.width = Val::Percent(f * 100.0);
        }
    }
}
