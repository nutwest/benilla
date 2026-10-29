//! `party`: benilla on a single-player vmangos server the game runs itself. The server is set up on
//! the first launch, started on every launch and stopped on exit; the client logs in on its own.
//! `docs/PARTY_DESIGN.md`, M0.

use std::sync::Arc;

use benilla_app::{AppExit, BuildId};

mod overlay;
mod server;

fn main() -> AppExit {
    let launched = std::time::Instant::now();
    // Read when the app builds: the realmlist dials our realmd, and the realm list takes our realm.
    std::env::set_var("WOW_HOST", server::REALMD_ADDR);
    std::env::set_var("WOW_REALM", server::REALM_NAME);

    let supervisor = Arc::new(server::Supervisor::new(launched));
    let exit = benilla_app::run_with(
        BuildId {
            version: env!("CARGO_PKG_VERSION"),
            describe: env!("BENILLA_GIT_DESCRIBE"),
            sha: env!("BENILLA_GIT_SHA"),
            short: env!("BENILLA_GIT_SHORT"),
            date: env!("BENILLA_GIT_DATE"),
            profile: env!("BENILLA_PROFILE"),
            project_dir: env!("BENILLA_PROJECT_DIR"),
            ..Default::default()
        },
        {
            let supervisor = Arc::clone(&supervisor);
            move |app| {
                app.add_plugins((server::ServerPlugin(supervisor), overlay::OverlayPlugin));
            }
        },
    );
    supervisor.shutdown();
    exit
}
