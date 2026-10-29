//! The engine boot, copied from `benilla_world::worldview::run`: the same plugins in the same
//! order, with td-game's camera and shot in place of the viewer's free-fly and timed shot. The
//! check and survey modes stay worldview's; this binary is a game, not the engine's gate.

use bevy::prelude::*;

use benilla_world::build_id::BuildId;
use benilla_world::terrain_stream::ViewFocus;

/// The arena's map: Eastern Kingdoms (`Map.dbc` id 0).
pub const MAP_ID: u32 = 0;

/// The arena's centre in WoW world coords: the road through Goldshire, north of the Lion's Pride
/// Inn. `z` is a guess the camera replaces with the terrain height once the tile is resident.
pub const ARENA_CENTER: [f32; 3] = [-9435.0, 52.0, 57.0];

/// Builds and runs the game; `build` is the launcher's compile-time stamp.
pub fn run(build: BuildId) -> AppExit {
    benilla_formats::set_project_folder(build.project_dir);
    let mut app = App::new();
    app.insert_resource(build);

    // The `mpq://` source must be registered before `AssetPlugin` (in `DefaultPlugins`) builds.
    match benilla_formats::wow_data() {
        Some(data_dir) => {
            if let Err(e) = benilla_assets::register_mpq_source(&mut app, &data_dir) {
                eprintln!("benilla-assets: mpq:// source unavailable ({e:#})");
            }
        }
        None => eprintln!(
            "td-game: no WoW install found — looked in {:?}",
            benilla_formats::candidates()
        ),
    }

    let background = benilla_world::bgwin::background_run();
    app.add_plugins(benilla_world::boot::tuned_default_plugins(Window {
        title: "benilla tower defense".into(),
        resolution: std::env::var("WOW_WIN")
            .ok()
            .and_then(|v| {
                let (w, h) = v.split_once('x')?;
                Some(UVec2::new(w.parse().ok()?, h.parse().ok()?))
            })
            .unwrap_or(UVec2::new(1600, 900))
            .into(),
        present_mode: if std::env::var("WOW_NOVSYNC").as_deref() == Ok("1") {
            bevy::window::PresentMode::AutoNoVsync
        } else {
            bevy::window::PresentMode::default()
        },
        // As in the viewer: an instrumented run (a shot) opens unfocused, below normal windows.
        focused: !background,
        window_level: if background {
            bevy::window::WindowLevel::AlwaysOnBottom
        } else {
            bevy::window::WindowLevel::Normal
        },
        ..default()
    }))
    .add_plugins(benilla_world::thread_qos::ThreadQosPlugin)
    .add_plugins(benilla_world::bgwin::BgWinPlugin)
    .add_plugins(benilla_world::mac_quit::MacQuitPlugin);

    app.add_plugins(benilla_world::world_plugins::WorldPlugins);
    // What the engine needs told: the world is live, and it streams around the arena rather than
    // wherever the camera is, so panning never re-arms a load.
    app.insert_resource(benilla_world::schedule::WorldLive(true))
        .insert_resource(ViewFocus::entry(MAP_ID, ARENA_CENTER));

    app.add_plugins((
        crate::camera::plugin,
        crate::shot::plugin,
        crate::creature::plugin,
        crate::anim::plugin,
    ));

    // After `AssetPlugin`: the loaders go into the live `AssetServer`, as in the viewer.
    benilla_assets::register_asset_loaders(&mut app);

    app.run()
}
