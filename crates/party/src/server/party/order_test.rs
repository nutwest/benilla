//! The order handover checks (`PARTY_ORDER_TEST=1`), M6b's riskiest case: a member mid-walk on a
//! Move order is swapped, in both directions, with the world running and paused. Its position,
//! sampled every frame from the client's view, must carry on to the point with no snap and no lost
//! order. Findings go to `logs/order-checks.txt`.

use std::time::{Duration, Instant};

use anyhow::{bail, Context as _};
use bevy::log::info;
use bevy::math::{Vec3, Vec3Swizzles as _};

use benilla_app::SessionRequest;

use super::swap_test::{status, ServerLog};
use super::{append, bevy_to_wow, settle, swap, verdict, Member, STEP_LIMIT};
use crate::server::layout::Layout;
use crate::server::orders::Order;
use crate::server::Supervisor;

/// How far each walk goes, in yards.
const WALK: f32 = 24.0;
/// How far into a walk the swap comes.
const INTO_THE_WALK: Duration = Duration::from_millis(1200);
/// How long a paused case stays paused after its swap.
const PAUSED: Duration = Duration::from_millis(1500);
/// The longest a walk may take from the swap.
const ARRIVAL_LIMIT: Duration = Duration::from_secs(12);
/// A frame's move past this is a snap, in yards.
const SNAP: f32 = 1.5;
/// A stop longer than this breaks the handover.
const STOP_LIMIT: Duration = Duration::from_millis(1500);
/// The most a body may shift at a paused swap: the server's position is up to a map tick (100 ms
/// at 7 yd/s) behind the client's frozen view, and the swap's re-create corrects it once.
const SWAP_SHIFT: f32 = 0.75;
/// How near the point counts as arrived, in yards: the server's own arrival, and a slot's reach.
const ARRIVED: f32 = 4.5;

fn position(sup: &Supervisor, guid: u64) -> Option<Vec3> {
    let units = sup.units.lock().unwrap();
    units.iter().find(|u| u.guid == guid).map(|u| u.pos)
}

/// `guid`'s walk from here to `to` or the limit: the biggest jump, the longest stop, how near it
/// came.
fn follow_walk(sup: &Supervisor, guid: u64, to: Vec3) -> (f32, Duration, f32) {
    let start = Instant::now();
    let mut last = position(sup, guid).unwrap_or(to);
    let (mut snap, mut stop, mut still_since) = (0.0f32, Duration::ZERO, Instant::now());
    let mut nearest = last.xz().distance(to.xz());
    while start.elapsed() < ARRIVAL_LIMIT {
        settle(Duration::from_millis(8));
        let Some(at) = position(sup, guid) else {
            continue;
        };
        let step = at.distance(last);
        let across = at.xz().distance(to.xz());
        snap = snap.max(step);
        if step > 0.001 {
            stop = stop.max(still_since.elapsed());
            still_since = Instant::now();
        }
        last = at;
        nearest = nearest.min(across);
        if nearest < ARRIVED {
            break;
        }
    }
    (snap, stop, nearest)
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
        info!("party: order check {line}");
        append(layout, "order-checks.txt", &line);
    };
    append(
        layout,
        "order-checks.txt",
        &format!("--- run at launch +{:?}", sup.launched.elapsed()),
    );
    let mut log = ServerLog::new(layout)?;
    settle(Duration::from_secs(3));

    let cases = [
        ("bot to played, running", true, false),
        ("played to bot, running", false, false),
        ("bot to played, paused", true, true),
        ("played to bot, paused", false, true),
    ];
    let mut way = 1.0f32;
    for (label, walker_is_bot, paused) in cases {
        let next = (*active + 1) % members.len();
        // The walker: the member swapped to, or the one swapped away.
        let walker = if walker_is_bot {
            &members[next]
        } else {
            &members[*active]
        };
        let from = position(sup, walker.guid).context("no walker")?;
        let to = from + Vec3::new(WALK * way, 0.0, 0.0);
        way = -way;
        let [x, y, _] = bevy_to_wow(to);
        sup.give_order(walker.guid, Order::Move([x, y, f32::NAN]));
        settle(INTO_THE_WALK);
        let moved = position(sup, walker.guid).map_or(0.0, |at| at.distance(from));
        finding(
            moved > 2.0,
            format!("{label}: {} is mid-walk, {moved:.1} yd in", walker.name),
        );
        if paused {
            sup.ask(SessionRequest::PartyCommand(".party pause on".into()));
            log.wait_for(sup, Duration::from_secs(5), |l| {
                l.contains("[PartyPause] on")
            })?;
        }
        swap(sup, layout, members, *active, next, STEP_LIMIT)?;
        *active = next;
        if paused {
            let held_at = position(sup, walker.guid).context("no walker")?;
            settle(PAUSED);
            let drift = position(sup, walker.guid).map_or(f32::MAX, |at| at.distance(held_at));
            finding(
                drift < SWAP_SHIFT,
                format!(
                    "{label}: {} held through the pause, drift {drift:.3} yd",
                    walker.name
                ),
            );
            sup.ask(SessionRequest::PartyCommand(".party pause off".into()));
            log.wait_for(sup, Duration::from_secs(5), |l| {
                l.contains("[PartyPause] off")
            })?;
        }
        let (snap, stop, nearest) = follow_walk(sup, walker.guid, to);
        finding(
            nearest < ARRIVED,
            format!(
                "{label}: {} came within {nearest:.1} yd of its point",
                walker.name
            ),
        );
        finding(
            snap < SNAP && stop < STOP_LIMIT,
            format!("{label}: biggest step {snap:.2} yd, longest stop {stop:?}"),
        );
        settle(Duration::from_millis(2500));
        let s = status(sup, &mut log, members)?;
        let order = s[walker.name].order;
        finding(
            order == 1,
            format!("{label}: {} ends on Hold, order={order}", walker.name),
        );
    }

    // A bot on Hold stays when the leader walks away.
    let bot = &members[(*active + 1) % members.len()];
    let held = position(sup, bot.guid).context("no bot")?;
    sup.walk(Duration::from_millis(2500));
    settle(Duration::from_secs(4));
    let drift = position(sup, bot.guid).map_or(f32::MAX, |at| at.distance(held));
    finding(
        drift < 0.5,
        format!(
            "{} held its place as the leader walked off, drift {drift:.2} yd",
            bot.name
        ),
    );

    if fails.get() > 0 {
        let line = format!("order checks done, {} failed", fails.get());
        append(layout, "order-checks.txt", &line);
        bail!(line);
    }
    append(layout, "order-checks.txt", "order checks done, 0 failed");
    info!("party: order checks done, 0 failed");
    Ok(())
}
