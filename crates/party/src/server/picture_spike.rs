//! The M4 step 2 spike (`PARTY_PICTURE_SPIKE=<dir>`), to be removed once M4's clock work settles
//! it: which of what the picture needs stops when `Time<Virtual>` stops. With the server paused and
//! `Time<Virtual>` paused, the world camera flies [`FLIGHT_YD`] yd out, holds, and sees the world
//! resume; it flies back, and flies the same line unpaused as the control. The census and a
//! screenshot at each stage go to `<dir>`.

use std::path::PathBuf;

use benilla_app::SessionRequest;
use benilla_world::model_fade::{DoodadFade, RenderFade};
use benilla_world::view::WorldCamera;
use bevy::prelude::*;
use bevy::render::view::screenshot::{save_to_disk, Screenshot};
use bevy::transform::TransformSystems;

/// How far the camera flies, and how long it takes.
const FLIGHT_YD: f32 = 100.0;
const FLIGHT_SECS: f32 = 8.0;
/// Seconds in the world before the spike starts, so the first login's fades have finished.
const LEAD_SECS: f32 = 15.0;
/// How far above its start the camera flies, to clear the ground.
const LIFT_YD: f32 = 12.0;

/// Stage starts in seconds from the pause, each `(at, name, how far out, shot)`.
const STAGES: [(f32, &str, Leg, bool); 8] = [
    (1.0, "paused-start", Leg::Home, true),
    (3.0, "paused-out", Leg::Out, false),
    (11.0, "paused-far", Leg::Far, true),
    (15.0, "resumed-far", Leg::Far, true),
    (19.0, "back", Leg::Back, false),
    (27.0, "control-start", Leg::Home, true),
    (31.0, "control-out", Leg::Out, false),
    (39.0, "control-far", Leg::Far, true),
];
/// The resume, between `paused-far` and `resumed-far`.
const RESUME_AT: f32 = 14.0;
/// The end, the camera handed back.
const END_AT: f32 = 44.0;

#[derive(Clone, Copy, PartialEq)]
enum Leg {
    Home,
    Out,
    Far,
    Back,
}

#[derive(Resource)]
struct Spike {
    dir: PathBuf,
    in_world_at: Option<f32>,
    paused_at: Option<f32>,
    resumed: bool,
    done: bool,
    stage: usize,
    /// Where the flight starts and which way it goes.
    home: Vec3,
    heading: Vec3,
    /// The last census line's time, and what came since it.
    last_census: f32,
    frames: u32,
    spawned: u32,
}

pub(super) struct PictureSpikePlugin;

impl Plugin for PictureSpikePlugin {
    fn build(&self, app: &mut App) {
        let Some(dir) = std::env::var_os("PARTY_PICTURE_SPIKE") else {
            return;
        };
        app.insert_resource(Spike {
            dir: PathBuf::from(dir),
            in_world_at: None,
            paused_at: None,
            resumed: false,
            done: false,
            stage: 0,
            home: Vec3::ZERO,
            heading: Vec3::ZERO,
            last_census: 0.0,
            frames: 0,
            spawned: 0,
        })
        .add_systems(PostUpdate, fly.before(TransformSystems::Propagate));
    }
}

fn log_line(spike: &Spike, line: &str) {
    use std::io::Write as _;
    info!("party: spike {line}");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(spike.dir.join("spike.txt"))
    {
        let _ = writeln!(f, "{line}");
    }
}

#[allow(clippy::too_many_arguments)]
fn fly(
    mut spike: ResMut<Spike>,
    objects: benilla_app::Objects,
    real: Res<Time<Real>>,
    mut virt: ResMut<Time<Virtual>>,
    mut session: MessageWriter<SessionRequest>,
    mut commands: Commands,
    mut cameras: Query<&mut Transform, With<WorldCamera>>,
    fades: Query<(&RenderFade, Has<Mesh3d>)>,
    doodads: Query<&ViewVisibility, With<DoodadFade>>,
    meshes: Query<&ViewVisibility, With<Mesh3d>>,
    added: Query<(), Added<Mesh3d>>,
) {
    let now = real.elapsed_secs();
    if objects.player().is_none() || spike.done {
        return;
    }
    let in_world_at = *spike.in_world_at.get_or_insert(now);
    let Ok(mut camera) = cameras.single_mut() else {
        return;
    };
    let Some(paused_at) = spike.paused_at else {
        if now - in_world_at >= LEAD_SECS {
            let forward = camera.forward();
            spike.home = camera.translation + Vec3::Y * LIFT_YD;
            spike.heading = Vec3::new(forward.x, 0.0, forward.z).normalize_or_zero();
            spike.paused_at = Some(now);
            session.write(SessionRequest::PartyCommand(".party pause on".into()));
            virt.pause();
            let line = format!(
                "paused: from {:?} heading {:?}, Time<Virtual> elapsed {:.2}",
                spike.home,
                spike.heading,
                virt.elapsed_secs()
            );
            log_line(&spike, &line);
        }
        return;
    };
    let t = now - paused_at;

    if !spike.resumed && t >= RESUME_AT {
        spike.resumed = true;
        session.write(SessionRequest::PartyCommand(".party pause off".into()));
        virt.unpause();
        log_line(&spike, "resumed");
    }

    // The stage and the camera's place on the line.
    while spike.stage + 1 < STAGES.len() && t >= STAGES[spike.stage + 1].0 {
        spike.stage += 1;
    }
    let (at, name, leg, shot) = STAGES[spike.stage];
    let next = STAGES.get(spike.stage + 1).map_or(END_AT, |s| s.0);
    let flown = ((t - at) / FLIGHT_SECS).clamp(0.0, 1.0);
    let out = match leg {
        Leg::Home => 0.0,
        Leg::Out => flown,
        Leg::Far => 1.0,
        Leg::Back => 1.0 - flown,
    };
    camera.translation = spike.home + spike.heading * FLIGHT_YD * out;

    // The census, once a second.
    spike.frames += 1;
    spike.spawned += added.iter().count() as u32;
    if now - spike.last_census >= 1.0 {
        let vnow = virt.elapsed_secs();
        let mid_fades = fades
            .iter()
            .filter(|(f, _)| (vnow - f.started) / f.duration < 1.0)
            .count();
        let line = format!(
            "t={t:.1} {name} out={:.0}yd frames={} new_meshes={} meshes_drawn={}/{} doodads_drawn={}/{} render_fades={} mid={} virt={vnow:.2} delta={:.4}",
            out * FLIGHT_YD,
            spike.frames,
            spike.spawned,
            meshes.iter().filter(|v| v.get()).count(),
            meshes.iter().count(),
            doodads.iter().filter(|v| v.get()).count(),
            doodads.iter().count(),
            fades.iter().count(),
            mid_fades,
            virt.delta_secs(),
        );
        log_line(&spike, &line);
        spike.last_census = now;
        spike.frames = 0;
        spike.spawned = 0;
    }

    // Each stage's shot, a little before the next: the view has settled.
    if shot && t >= next - 0.5 {
        let path = spike.dir.join(format!("{name}.png"));
        if !path.exists() {
            let _ = std::fs::write(&path, b"");
            commands
                .spawn(Screenshot::primary_window())
                .observe(save_to_disk(path));
        }
    }

    if t >= END_AT {
        spike.done = true;
        log_line(&spike, "done");
    }
}
