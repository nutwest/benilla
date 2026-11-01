//! The swap-in-the-air checks (`PARTY_FALL_TEST=1`). The old character jumps and is swapped away;
//! its height, sampled every frame from the client's own view of it, must keep following the arc
//! to the ground rather than drop, and the server must hear the landing. Findings go to
//! `logs/fall-checks.txt`.
//!
//! - Swapped mid-jump, the world running.
//! - Swapped mid-jump, the world paused: the body holds its height through the pause and finishes
//!   the arc after the resume.

use std::time::{Duration, Instant};

use anyhow::Context as _;
use bevy::log::info;

use benilla_app::SessionRequest;

use super::swap_test::{status, ServerLog};
use super::{append, settle, swap, verdict, Member, STEP_LIMIT};
use crate::server::layout::Layout;
use crate::server::Supervisor;

/// How long after the jump key each swap or pause goes in, on the way up.
const INTO_THE_JUMP: Duration = Duration::from_millis(150);
/// How far above the ground counts as still in the air, yd.
const IN_THE_AIR: f32 = 0.2;
/// How long the world stays paused.
const PAUSED: Duration = Duration::from_secs(3);
/// The most a body may wander while held, yd.
const HELD_SPREAD: f32 = 0.01;
/// A jump's landing, at the most, from the swap.
const LANDING_LIMIT: Duration = Duration::from_secs(3);

/// `guid`'s height in the client's view.
fn height(sup: &Supervisor, guid: u64) -> Option<f32> {
    let units = sup.units.lock().unwrap();
    units.iter().find(|u| u.guid == guid).map(|u| u.pos.y)
}

/// `guid`'s heights over `span`, about one a frame, with when each was read.
fn sample(sup: &Supervisor, guid: u64, span: Duration) -> Vec<(Duration, f32)> {
    let start = Instant::now();
    let mut out = Vec::new();
    while start.elapsed() < span {
        if let Some(y) = height(sup, guid) {
            out.push((start.elapsed(), y));
        }
        settle(Duration::from_millis(8));
    }
    out
}

/// How long `heights` stayed in the air, from their start.
fn airborne_for(heights: &[(Duration, f32)], ground: f32) -> Duration {
    heights
        .iter()
        .rev()
        .find(|(_, y)| *y > ground + IN_THE_AIR)
        .map_or(Duration::ZERO, |(at, _)| *at)
}

pub(super) fn checks(
    sup: &Supervisor,
    layout: &Layout,
    members: &[Member],
    active: &mut usize,
) -> anyhow::Result<()> {
    let fails = std::cell::Cell::new(0);
    let finding = |ok: bool, line: String| {
        if !ok {
            fails.set(fails.get() + 1);
        }
        let line = format!("{line} -> {}", verdict(ok));
        info!("party: fall check {line}");
        append(layout, "fall-checks.txt", &line);
    };
    append(
        layout,
        "fall-checks.txt",
        &format!("--- run at launch +{:?}", sup.launched.elapsed()),
    );
    let mut log = ServerLog::new(layout)?;
    settle(Duration::from_secs(3));

    // Swapped mid-jump, running.
    let (old, new) = (*active, (*active + 1) % members.len());
    let (name, guid) = (members[old].name, members[old].guid);
    let ground = height(sup, guid).context("no old body")?;
    sup.jump();
    settle(INTO_THE_JUMP);
    swap(sup, layout, members, old, new, STEP_LIMIT)?;
    *active = new;
    let arc = sample(sup, guid, Duration::from_millis(1500));
    let peak = arc.iter().map(|(_, y)| *y).fold(ground, f32::max);
    let flown = airborne_for(&arc, ground);
    finding(
        flown > Duration::from_millis(200) && peak > ground + 0.5,
        format!(
            "running: {name} flew on for {flown:?} after the swap, peak {:.2} yd up, {} samples",
            peak - ground,
            arc.len()
        ),
    );
    let last = arc.last().map_or(ground, |(_, y)| *y);
    finding(
        (last - ground).abs() < 0.3,
        format!(
            "running: {name} ended {:.2} yd from the ground",
            last - ground
        ),
    );
    let line = log.wait_for(sup, LANDING_LIMIT, |l| {
        l.contains(&format!("[PartySwap] {name}: landed")) || l.contains("no landing came")
    })?;
    finding(line.contains("landed"), format!("running: {line}"));
    let s = status(sup, &mut log, members)?;
    finding(
        s[name].ai == "PartyBotAI" && s[name].leader == members[new].name,
        format!(
            "running: {name} is a bot again: ai={} leader={}",
            s[name].ai, s[name].leader
        ),
    );

    // Swapped mid-jump, paused.
    settle(Duration::from_secs(2));
    let (old, new) = (*active, (*active + 1) % members.len());
    let (name, guid) = (members[old].name, members[old].guid);
    let ground = height(sup, guid).context("no old body")?;
    sup.jump();
    settle(INTO_THE_JUMP);
    sup.ask(SessionRequest::PartyCommand(".party pause on".into()));
    log.wait_for(sup, Duration::from_secs(5), |l| {
        l.contains("[PartyPause] on")
    })?;
    settle(Duration::from_millis(200));
    swap(sup, layout, members, old, new, STEP_LIMIT)?;
    *active = new;
    let held = sample(sup, guid, PAUSED);
    let (low, high) = held.iter().fold((f32::MAX, f32::MIN), |(lo, hi), (_, y)| {
        (lo.min(*y), hi.max(*y))
    });
    finding(
        high - low <= HELD_SPREAD && low > ground + IN_THE_AIR,
        format!(
            "paused: {name} held {:.2} yd up through {PAUSED:?}, spread {:.4} yd, {} samples",
            low - ground,
            high - low,
            held.len()
        ),
    );
    sup.ask(SessionRequest::PartyCommand(".party pause off".into()));
    log.wait_for(sup, Duration::from_secs(5), |l| {
        l.contains("[PartyPause] off")
    })?;
    let arc = sample(sup, guid, Duration::from_millis(1500));
    let flown = airborne_for(&arc, ground);
    let first = arc.first().map_or(ground, |(_, y)| *y);
    finding(
        (first - low).abs() < 0.3 && flown > Duration::from_millis(100),
        format!(
            "paused: {name} took up the arc {:.2} yd from where it held, and flew {flown:?}",
            (first - low).abs()
        ),
    );
    let last = arc.last().map_or(ground, |(_, y)| *y);
    finding(
        (last - ground).abs() < 0.3,
        format!(
            "paused: {name} ended {:.2} yd from the ground",
            last - ground
        ),
    );
    let line = log.wait_for(sup, LANDING_LIMIT, |l| {
        l.contains(&format!("[PartySwap] {name}: landed")) || l.contains("no landing came")
    })?;
    finding(line.contains("landed"), format!("paused: {line}"));
    let s = status(sup, &mut log, members)?;
    let guid_now = members[new].guid.to_string();
    finding(
        s[members[new].name].mover == guid_now && s[name].ai == "PartyBotAI",
        format!(
            "paused: the mover claim landed on resume: mover={} (want {guid_now}); {name} ai={}",
            s[members[new].name].mover, s[name].ai
        ),
    );

    let line = format!("fall checks done, {} failed", fails.get());
    info!("party: {line}");
    append(layout, "fall-checks.txt", &line);
    Ok(())
}
