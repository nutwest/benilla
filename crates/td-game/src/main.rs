//! `td-game`: a single-player tower defense over a streamed Azeroth. M0 is the engine boot, the
//! RTS camera over the arena and the `WOW_WORLDVIEW_SHOT` screenshot; M1 one creature that stands,
//! walks and stops; M2 a data-driven level whose waves walk the Goldshire road; M3 a pathing grid
//! and flow field the enemies steer by; M4 structures the player builds to reroute them; M5
//! units that fight them.

mod anim;
mod boot;
mod build;
mod camera;
mod combat;
mod creature;
mod debug_draw;
mod economy;
mod enemy;
mod flow;
mod grid;
mod hud;
mod level;
mod path;
mod shot;
mod structures;
mod test_block;
mod units;
mod waves;

use benilla_world::build_id::BuildId;

fn main() -> benilla_world::AppExit {
    // No build stamp: `project_dir` left empty resolves the install from benilla's own checkout
    // (`<checkout>/WoW/Data`), and the git fields only feed the startup banner.
    boot::run(BuildId {
        version: env!("CARGO_PKG_VERSION"),
        ..Default::default()
    })
}
