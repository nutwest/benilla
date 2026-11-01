//! The M1c checks (`PARTY_M1C_TEST=1`), the client's side of the swap. Each swap goes through the
//! switch key's own path ([`super::swap`]); the server's `.party status` says who holds the client,
//! who claimed which mover, who moved and who follows. Findings go to `logs/m1c-checks.txt`.
//!
//! The checks walk the new character on the movement key after each swap. What the UI shows is the
//! Lua probe's to log: with `WOW_PROBE_LUA_EACH_VM=1` and the chunk `docs/PARTY_DESIGN.md` (M1c)
//! gives, every rebuilt UI logs its player frame, spellbook, action bar and bags, takes a
//! screenshot and casts. Its `probe-log:` lines and the reload times are read from the client's
//! log after the run.
//!
//! - Six swaps out of combat, two rounds of the party.
//! - Two fights, each swapped mid-combat; the swapped-away character must fight on as a bot. Not
//!   in a `WOW_UNATTENDED=1` run.
//! - Per swap: the time from the key to playing the new character; the streamed unit count
//!   before and after (no world despawn).

use std::time::{Duration, Instant};

use anyhow::Context as _;
use bevy::log::info;

use benilla_app::SessionRequest;

use super::swap_test::{status, ServerLog, Status, PRIEST, TEST_MOB_NAME};
use super::{append, settle, swap, verdict, Member, ROSTER, TEST_MOB};
use crate::server::db::Db;
use crate::server::layout::Layout;
use crate::server::Supervisor;

/// The fight creature's health: enough to outlast a swap and its checks at the party's levels.
const FIGHT_HP: u32 = 600;

fn moved(a: &Status, b: &Status) -> f32 {
    let d: f32 = (0..3).map(|i| (a.pos[i] - b.pos[i]).powi(2)).sum();
    d.sqrt()
}

/// One swap to the next member, and what the server says of it twice: right after it, and once
/// the new character has walked. Returns the FAIL count.
fn swap_and_check(
    sup: &Supervisor,
    layout: &Layout,
    log: &mut ServerLog,
    members: &[Member],
    active: &mut usize,
    label: &str,
    report: &dyn Fn(String),
) -> anyhow::Result<usize> {
    let (old, new) = (*active, (*active + 1) % members.len());
    let (on, nn) = (members[old].name, members[new].name);
    let units_before = sup.units.lock().unwrap().len();
    let took = swap(sup, layout, members, old, new, super::STEP_LIMIT)?;
    *active = new;
    settle(Duration::from_millis(500));
    let s0 = status(sup, log, members)?;
    // Walk the new character forward on the movement key; the new UI's probe casts meanwhile.
    sup.walk(Duration::from_millis(1500));
    settle(Duration::from_millis(5500));
    let s1 = status(sup, log, members)?;
    let units_after = sup.units.lock().unwrap().len();

    let mut fails = 0;
    let mut finding = |ok: bool, line: String| {
        if !ok {
            fails += 1;
        }
        report(format!("{label}: {line} -> {}", verdict(ok)));
    };
    finding(
        true,
        format!(
            "swap {on} -> {nn}: playing {nn} {:.0} ms after the key; units streamed {units_before} -> {units_after}",
            took.as_secs_f32() * 1000.0
        ),
    );
    let n = &s1[nn];
    let guid = members[new].guid.to_string();
    finding(
        n.session == "client" && n.mover == guid && n.group_leader,
        format!(
            "{nn}: session={} mover={} (its guid {guid}) group_leader={}",
            n.session, n.mover, n.group_leader
        ),
    );
    let walked = moved(&s0[nn], &s1[nn]);
    finding(
        walked > 2.0,
        format!("{nn} moved {walked:.1} yd under the client"),
    );
    let o = &s1[on];
    let followed = moved(&s0[on], &s1[on]);
    finding(
        o.session == "bot"
            && o.ai == "PartyBotAI"
            && o.leader == nn
            && o.mover == "none"
            && o.pending_moves == 0
            && (followed > 1.0 || o.dist < 8.0),
        format!(
            "{on}: session={} ai={} leader={} mover={} pending_moves={}; moved {followed:.1} yd, {:.1} yd from {nn}",
            o.session, o.ai, o.leader, o.mover, o.pending_moves, o.dist
        ),
    );
    finding(
        units_after * 2 >= units_before,
        format!("no world despawn: {units_before} -> {units_after} units"),
    );
    Ok(fails)
}

pub(super) fn checks(
    sup: &Supervisor,
    layout: &Layout,
    _db: &Db,
    members: &[Member],
    active: &mut usize,
) -> anyhow::Result<()> {
    let report = |line: String| {
        info!("party: m1c check {line}");
        append(layout, "m1c-checks.txt", &line);
    };
    report(format!("--- run at launch +{:?}", sup.launched.elapsed()));
    let mut log = ServerLog::new(layout)?;
    // The first UI's probe: its snapshot, its screenshot, its walk.
    settle(Duration::from_secs(14));
    let mut fails = 0;

    for i in 1..=6 {
        fails += swap_and_check(
            sup,
            layout,
            &mut log,
            members,
            active,
            &format!("idle {i}"),
            &report,
        )?;
        settle(Duration::from_secs(1));
    }

    // No unattended combat probes: such a run checks the swaps alone.
    let fights = if std::env::var_os("WOW_UNATTENDED").is_some() {
        0
    } else {
        2
    };
    for fight_no in 1..=fights {
        let known: Vec<u64> = sup.units.lock().unwrap().iter().map(|u| u.guid).collect();
        sup.ask(SessionRequest::Command(format!(".npc summon {TEST_MOB}")));
        let since = Instant::now();
        let mob = loop {
            let found = sup
                .units
                .lock()
                .unwrap()
                .iter()
                .find(|u| u.entry == TEST_MOB && u.health > 0 && !known.contains(&u.guid))
                .map(|u| u.guid);
            if found.is_some() || since.elapsed() > Duration::from_secs(5) {
                break found;
            }
            settle(Duration::from_millis(100));
        };
        let mob = mob.context("the summoned creature never streamed")?;
        sup.ask(SessionRequest::Target(mob));
        settle(Duration::from_millis(150));
        sup.ask(SessionRequest::Command(format!(".modify hp {FIGHT_HP}")));
        settle(Duration::from_millis(150));
        sup.ask(SessionRequest::Command(".partybot attackstart".into()));
        settle(Duration::from_secs(2));
        let s = status(sup, &mut log, members)?;
        let old = *active;
        report(format!(
            "fight {fight_no}: before the swap {} combat={} victim={}",
            members[old].name, s[members[old].name].combat, s[members[old].name].victim
        ));
        fails += swap_and_check(
            sup,
            layout,
            &mut log,
            members,
            active,
            &format!("fight {fight_no}"),
            &report,
        )?;
        let seen: Vec<String> = sup
            .units
            .lock()
            .unwrap()
            .iter()
            .filter(|u| u.entry == TEST_MOB)
            .map(|u| format!("{:#x} hp {}", u.guid, u.health))
            .collect();
        report(format!(
            "fight {fight_no}: the client streams the creature {mob:#x}: [{}]",
            seen.join(", ")
        ));

        // The swapped-away character fights on as a bot until the creature dies.
        let start = Instant::now();
        let mut engaged = false;
        let mut dead = false;
        while start.elapsed() < Duration::from_secs(60) {
            let s = status(sup, &mut log, members)?;
            let o = &s[members[old].name];
            engaged |= o.victim == TEST_MOB_NAME || (ROSTER[old].1 == PRIEST && o.combat);
            let alive = sup
                .units
                .lock()
                .unwrap()
                .iter()
                .any(|u| u.guid == mob && u.health > 0);
            if !alive {
                dead = true;
                break;
            }
            settle(Duration::from_millis(500));
        }
        let ok = engaged;
        if !ok {
            fails += 1;
        }
        report(format!(
            "fight {fight_no}: {} fought on as a bot: {engaged}; creature dead: {dead} after {:.0} s -> {}",
            members[old].name,
            start.elapsed().as_secs_f32(),
            verdict(ok)
        ));
        if !dead {
            sup.ask(SessionRequest::Target(mob));
            settle(Duration::from_millis(150));
            sup.ask(SessionRequest::Command(".die".into()));
        }
        settle(Duration::from_secs(2));
    }
    report(format!("summary: {fails} FAIL findings"));
    Ok(())
}
