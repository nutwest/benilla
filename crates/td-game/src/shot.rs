//! `WOW_WORLDVIEW_SHOT=<png>`: one frame of the arena, written once the world around it has
//! loaded, then exit. Unlike the viewer's timed shot it waits for
//! [`WorldLoadProgress::presentable`] and a settle after it, so the frame is never half-streamed.
//! `WOW_WORLDVIEW_SHOT_AT=<seconds>` is the earliest it fires, not the moment.

use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use bevy::time::Real;

use benilla_world::terrain_stream::WorldLoadProgress;

/// Seconds the world must stay presentable before the shot, for textures and fades to land.
const SETTLE_SECS: f32 = 2.0;

/// Past this the shot fires anyway, with a warning, so a stuck load still leaves a picture.
const GIVE_UP_SECS: f32 = 120.0;

/// Seconds between waiting reports.
const REPORT_EVERY: f32 = 5.0;

pub fn plugin(app: &mut App) {
    if std::env::var_os("WOW_WORLDVIEW_SHOT").is_some() {
        app.add_systems(Update, shoot_when_loaded);
    }
}

#[derive(Default)]
struct ShotState {
    /// When the world last became presentable, cleared if it stops being so.
    ready_since: Option<f32>,
    /// When the screenshot was requested.
    fired_at: Option<f32>,
    last_report: f32,
}

fn shoot_when_loaded(
    time: Res<Time<Real>>,
    progress: Option<Res<WorldLoadProgress>>,
    mut commands: Commands,
    mut state: Local<ShotState>,
    mut exit: MessageWriter<AppExit>,
) {
    let now = time.elapsed_secs();
    // The write is asynchronous, and an immediate exit loses the PNG.
    if let Some(t) = state.fired_at {
        if now >= t + 2.0 {
            exit.write(AppExit::Success);
        }
        return;
    }

    let ready = progress
        .as_deref()
        .is_some_and(|p| p.focus_resident && p.presentable());
    if ready {
        state.ready_since.get_or_insert(now);
    } else {
        state.ready_since = None;
    }

    let earliest = std::env::var("WOW_WORLDVIEW_SHOT_AT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    let settled = state.ready_since.is_some_and(|t| now - t >= SETTLE_SECS);
    let give_up = now >= GIVE_UP_SECS;

    if !(settled && now >= earliest) && !give_up {
        if now - state.last_report >= REPORT_EVERY {
            state.last_report = now;
            match progress.as_deref() {
                Some(p) => info!(
                    "shot: waiting at {now:.0}s — {}/{} units, focus_resident={}, scene_ready={}, \
                     colliders {}, merge {}, gx {}",
                    p.ready,
                    p.total,
                    p.focus_resident,
                    p.scene_ready,
                    p.colliders_pending,
                    p.merge_pending,
                    p.gx_pending
                ),
                None => info!("shot: waiting at {now:.0}s — no WorldLoadProgress yet"),
            }
        }
        return;
    }

    if give_up && !settled {
        warn!("shot: world not presentable after {GIVE_UP_SECS}s — shooting anyway");
    }
    let path = std::env::var("WOW_WORLDVIEW_SHOT").unwrap_or_default();
    info!("shot: writing {path} at {now:.1}s");
    commands
        .spawn(Screenshot::primary_window())
        .observe(save_to_disk(path));
    state.fired_at = Some(now);
}
