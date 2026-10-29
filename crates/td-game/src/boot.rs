//! The engine boot, copied from `benilla_world::worldview::run`: the same plugins in the same
//! order, with td-game's camera and shot in place of the viewer's free-fly and timed shot. The
//! check and survey modes stay worldview's; this binary is a game, not the engine's gate.

use bevy::prelude::*;

use benilla_world::build_id::BuildId;
use benilla_world::terrain_stream::ViewFocus;
use benilla_world::world_map::CurrentMap;

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

    // After `LogPlugin`, so the level's summary reaches the log.
    let level = match crate::level::load() {
        Ok(level) => level,
        Err(e) => {
            error!("level: {e}");
            return AppExit::error();
        }
    };

    app.add_plugins(benilla_world::world_plugins::WorldPlugins);
    // What the engine needs told: the world is live, and it streams around the arena rather than
    // wherever the camera is, so panning never re-arms a load.
    app.insert_resource(benilla_world::schedule::WorldLive(true))
        .insert_resource(ViewFocus::entry(level.map_id, level.center));
    // The engine seeds `CurrentMap` from `$WOW_MAP` at startup; the level's map wins, before the
    // first frame reads it, so the seed announces no map change.
    let map_id = level.map_id;
    app.add_systems(PostStartup, move |mut commands: Commands| {
        commands.insert_resource(CurrentMap(map_id));
    });
    app.insert_resource(level);

    app.add_plugins((
        crate::camera::plugin,
        crate::shot::plugin,
        crate::creature::plugin,
        crate::anim::plugin,
        crate::enemy::plugin,
        crate::waves::plugin,
        crate::grid::plugin,
        crate::flow::plugin,
        crate::test_block::plugin,
        crate::debug_draw::plugin,
    ));

    // After `AssetPlugin`: the loaders go into the live `AssetServer`, as in the viewer.
    benilla_assets::register_asset_loaders(&mut app);

    app.run()
}
