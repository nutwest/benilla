//! The M4 step 1 checks (`PARTY_PAUSE_TEST=1`), from the server's side: benilla does not understand
//! `SMSG_PARTY_PAUSE` yet, so every number here is the server's, from the `[PartyStatus]` and
//! `[PartyProbe]` lines of `Server.log`. Findings go to `logs/pause-checks.txt`.
//!
//! - `.party probe` arms an aura, a cooldown and a respawn of [`ARMED`].
//! - The played character jumps so the bots run, and the world is paused mid-run for [`PAUSED`].
//! - During the pause, every clock, timer and position must hold still.
//! - After resume, the bots must run on, and the three timers must finish [`ARMED`] of unpaused
//!   time after they were armed.
//! - A pause with the client's socket closed must lift itself.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use bevy::log::info;
use bevy::math::Vec3;

use benilla_app::SessionRequest;

use super::swap_test::{status, ServerLog, Status};
use super::{append, bevy_to_wow, self_pos, settle, verdict, Member};
use crate::server::layout::Layout;
use crate::server::Supervisor;

/// How long the probe's timers run.
const ARMED: Duration = Duration::from_secs(20);
/// How long the world stays paused.
const PAUSED: Duration = Duration::from_secs(10);
/// A finish this far from the expected moment fails: a respawn is held to whole seconds.
const FINISH_SLACK: f32 = 1.5;
/// How far a bot must run for the pause to have caught it moving.
const RUN_MIN: f32 = 1.0;

/// A `[PartyProbe]` line.
#[derive(Debug, Default, Clone, PartialEq)]
struct Probe {
    world_ms: u32,
    game_time: i64,
    aura_ms: i32,
    cooldown_ms: u32,
    mob_alive: i32,
    respawn_left: i64,
    watch_pos: String,
}

impl Probe {
    fn timers_done(&self) -> bool {
        self.aura_ms == 0 && self.cooldown_ms == 0 && self.mob_alive == 1
    }
}

fn gap(a: &Status, b: &Status) -> f32 {
    let d: f32 = (0..3).map(|i| (a.pos[i] - b.pos[i]).powi(2)).sum();
    d.sqrt()
}

/// `.party status`, with its probe line.
fn sample(
    sup: &Supervisor,
    log: &mut ServerLog,
    members: &[Member],
) -> anyhow::Result<(HashMap<&'static str, Status>, Probe)> {
    let s = status(sup, log, members)?;
    let line = log.wait_for(sup, Duration::from_secs(5), |l| {
        l.contains("[PartyProbe] paused=")
    })?;
    let kv: HashMap<&str, &str> = line.split(' ').filter_map(|w| w.split_once('=')).collect();
    let num = |k: &str| kv.get(k).and_then(|v| v.parse::<i64>().ok()).unwrap_or(-1);
    let probe = Probe {
        world_ms: num("world_ms") as u32,
        game_time: num("game_time"),
        aura_ms: num("aura_ms") as i32,
        cooldown_ms: num("cooldown_ms") as u32,
        mob_alive: num("mob_alive") as i32,
        respawn_left: num("respawn_left"),
        watch_pos: kv.get("watch_pos").unwrap_or(&"").to_string(),
    };
    Ok((s, probe))
}

pub(super) fn checks(
    sup: &Supervisor,
    layout: &Layout,
    members: &[Member],
    active: usize,
) -> anyhow::Result<()> {
    let fails = std::cell::Cell::new(0);
    let finding = |ok: bool, line: String| {
        if !ok {
            fails.set(fails.get() + 1);
        }
        let line = format!("{line} -> {}", verdict(ok));
        info!("party: pause check {line}");
        append(layout, "pause-checks.txt", &line);
    };
    append(
        layout,
        "pause-checks.txt",
        &format!("--- run at launch +{:?}", sup.launched.elapsed()),
    );
    let mut log = ServerLog::new(layout)?;
    settle(Duration::from_secs(3));

    sup.ask(SessionRequest::PartyCommand(format!(
        ".party probe {}",
        ARMED.as_secs()
    )));
    log.wait_for(sup, Duration::from_secs(5), |l| {
        l.contains("[PartyProbe] armed")
    })?;
    let armed = Instant::now();
    settle(Duration::from_secs(2));

    // The bots run after the jump; the pause catches them on the way.
    let start = self_pos(sup).unwrap_or_default();
    let wow = bevy_to_wow(start + Vec3::new(25.0, 0.0, 0.0));
    sup.ask(SessionRequest::Command(format!(
        ".go xyz {:.1} {:.1} {:.1}",
        wow[0], wow[1], wow[2]
    )));
    settle(Duration::from_millis(1500));
    let (s_run, _) = sample(sup, &mut log, members)?;
    settle(Duration::from_millis(500));

    let (s_before, _) = sample(sup, &mut log, members)?;
    let bot = members[(active + 1) % members.len()].name;
    finding(
        gap(&s_run[bot], &s_before[bot]) > RUN_MIN,
        format!(
            "{bot} is running before the pause: {:.1} yd in 0.5 s",
            gap(&s_run[bot], &s_before[bot])
        ),
    );

    sup.ask(SessionRequest::PartyCommand(".party pause on".into()));
    let reply = log.wait_for(sup, Duration::from_secs(5), |l| l.contains("[PartyPause] "))?;
    finding(reply.contains("[PartyPause] on"), format!("pause: {reply}"));
    let paused_at = Instant::now();

    // Held through the pause: the first sample against the rest.
    let (s0, p0) = sample(sup, &mut log, members)?;
    finding(
        p0.aura_ms > 0 && p0.cooldown_ms > 0 && p0.respawn_left > 0 && p0.mob_alive == 0,
        format!("the timers are live at the pause: {p0:?}"),
    );
    let (mut clocks_held, mut positions_held, mut samples) = (true, true, 0);
    while paused_at.elapsed() < PAUSED {
        settle(Duration::from_secs(2));
        let (s, p) = sample(sup, &mut log, members)?;
        samples += 1;
        clocks_held &= p == p0;
        positions_held &= members.iter().all(|m| gap(&s[m.name], &s0[m.name]) == 0.0);
        if p != p0 {
            finding(false, format!("sample {samples} moved: {p:?}"));
        }
    }
    finding(
        clocks_held,
        format!("{samples} samples over {PAUSED:?}: world clock, game time, aura, cooldown, respawn and creature held"),
    );
    finding(
        positions_held,
        format!("{samples} samples: every member's position held"),
    );

    sup.ask(SessionRequest::PartyCommand(".party pause off".into()));
    let reply = log.wait_for(sup, Duration::from_secs(5), |l| l.contains("[PartyPause] "))?;
    let unpaused = paused_at.elapsed();
    finding(
        reply.contains("[PartyPause] off"),
        format!("resume after {unpaused:?}: {reply}"),
    );

    // Picks up from the held values, then runs at real time.
    let resumed_at = Instant::now();
    settle(Duration::from_secs(1));
    let (s1, p1) = sample(sup, &mut log, members)?;
    let spent = resumed_at.elapsed().as_millis() as i64;
    let aura_spent = i64::from(p0.aura_ms - p1.aura_ms);
    let cooldown_spent = i64::from(p0.cooldown_ms) - i64::from(p1.cooldown_ms);
    finding(
        (aura_spent - spent).abs() < 500 && (cooldown_spent - spent).abs() < 500,
        format!(
            "{spent} ms after resume: the aura spent {aura_spent} ms, the cooldown {cooldown_spent} ms; world clock moved {} ms",
            p1.world_ms.wrapping_sub(p0.world_ms)
        ),
    );
    finding(
        gap(&s0[bot], &s1[bot]) > RUN_MIN,
        format!(
            "{bot} runs on after resume: {:.1} yd in 1 s",
            gap(&s0[bot], &s1[bot])
        ),
    );

    // Each timer's finish, against what it would be with the pause cut out.
    let expected = (ARMED + unpaused).as_secs_f32();
    let (mut aura_at, mut cooldown_at, mut mob_at) = (None, None, None);
    let limit = Instant::now() + ARMED;
    while Instant::now() < limit {
        let (_, p) = sample(sup, &mut log, members)?;
        let now = armed.elapsed().as_secs_f32();
        if p.aura_ms == 0 {
            aura_at.get_or_insert(now);
        }
        if p.cooldown_ms == 0 {
            cooldown_at.get_or_insert(now);
        }
        if p.mob_alive == 1 {
            mob_at.get_or_insert(now);
        }
        if p.timers_done() {
            break;
        }
        settle(Duration::from_millis(200));
    }
    for (name, at) in [
        ("aura", aura_at),
        ("cooldown", cooldown_at),
        ("respawn", mob_at),
    ] {
        finding(
            at.is_some_and(|t: f32| (t - expected).abs() < FINISH_SLACK),
            format!("{name} finished at {at:?} s after arming, expected {expected:.1} s"),
        );
    }

    // A pause dies with its client.
    sup.ask(SessionRequest::PartyCommand(".party pause on".into()));
    log.wait_for(sup, Duration::from_secs(5), |l| {
        l.contains("[PartyPause] on")
    })?;
    sup.ask(SessionRequest::PartyCommand(".party probe drop".into()));
    let line = log.wait_for(sup, Duration::from_secs(5), |l| {
        l.contains("[PartyPause] the client is gone")
    });
    finding(
        line.is_ok(),
        format!("socket closed while paused: {line:?}"),
    );

    let line = format!("pause checks done, {} failed", fails.get());
    info!("party: {line}");
    append(layout, "pause-checks.txt", &line);
    Ok(())
}
