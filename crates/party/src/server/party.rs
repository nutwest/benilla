//! The party: three local accounts, one character each, grouped. The character you play is a
//! real login; the others are vmangos party bots (`.partybot load`), which group under you and
//! follow and fight with their own AI. The switch key (F6) plays the next member in place: our
//! vmangos fork's `.party swap <name>` hands the connection to that member's session and the bot
//! role to the old one's, and the client takes the new character over on `SMSG_PARTY_SWAP`
//! (`docs/PARTY_DESIGN.md`, §2.1). Each swap's line goes to `logs/swap.txt`.
//!
//! `PARTY_SWAP_TEST=1` runs the server-side swap checks (M1b), `PARTY_M1C_TEST=1` the client's
//! (M1c), after the first entry.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _};
use bevy::log::{info, warn};
use bevy::math::Vec3;

use benilla_app::{AutoLogin, SessionRequest};

use super::db::{CharRow, Db};
use super::layout::Layout;
use super::setup::Secrets;
use super::{Phase, Supervisor};

mod fall_test;
mod m1c_test;
mod order_test;
mod pause_test;
mod swap_test;

/// The party's characters, by account: name, `ChrClasses` id, class name. All human, so a new
/// party starts together in Northshire.
const ROSTER: [(&str, u8, &str); 3] = [
    ("Partyone", 1, "warrior"),
    ("Partytwo", 5, "priest"),
    ("Partythree", 8, "mage"),
];
const HUMAN: u8 = 1;

/// How long a server-side step may take before the controller gives up.
const STEP_LIMIT: Duration = Duration::from_secs(30);
/// How long a played swap may take: a refused one holds the key until this.
const SWAP_LIMIT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug)]
struct Member {
    account: String,
    pass: String,
    name: &'static str,
    /// The character's guid, once it exists.
    guid: u64,
}

impl Member {
    fn login(&self) -> AutoLogin {
        AutoLogin {
            user: self.account.clone(),
            pass: self.pass.clone(),
            character: Some(self.name.to_string()),
        }
    }
}

/// Creates the party if it is missing, enters the world as its last played member with the others
/// as bots, then swaps on the key until the game exits.
pub fn run(sup: &Supervisor, layout: &Layout, db: &Db, secrets: &Secrets) -> anyhow::Result<()> {
    let mut members: Vec<Member> = secrets
        .accounts
        .iter()
        .zip(ROSTER)
        .map(|((account, pass), (name, _, _))| Member {
            account: account.clone(),
            pass: pass.clone(),
            name,
            guid: 0,
        })
        .collect();
    create_missing(sup, db, &members)?;
    for m in &mut members {
        m.guid = db.character(m.name)?.context("member gone")?.guid;
    }

    // The member last played, as we recorded it: the database's logout time also moves when a
    // bot logs out, so it names a bot as often as not.
    let names: Vec<&str> = members.iter().map(|m| m.name).collect();
    let last = match std::fs::read_to_string(played_file(layout)) {
        Ok(name) => Some(name.trim().to_string()),
        Err(_) => db.last_played(&names)?,
    };
    let mut active = members
        .iter()
        .position(|m| Some(m.name) == last.as_deref())
        .unwrap_or(0);
    sup.set(Phase::Ready, "Entering the world");
    sup.cover.store(true, Ordering::Relaxed);
    sup.ask(SessionRequest::Relog(members[active].login()));
    wait(sup, "enter the world", || {
        Ok(sup.in_world.load(Ordering::Relaxed))
    })?;
    // A swap moves the group's lead and the group keeps it; a party bot that leads its group
    // removes itself.
    sup.ask(SessionRequest::Command(".party lead".into()));
    load_bots(sup, db, &members, active)?;
    warm_up(sup, layout, &members, active)?;
    record_played(layout, members[active].name);
    sup.cover.store(false, Ordering::Relaxed);
    info!(
        "party: {} in the world with {} bots",
        members[active].name,
        members.len() - 1
    );

    if std::env::var_os("PARTY_SWAP_TEST").is_some() {
        if let Err(err) = swap_test::swap_checks(sup, layout, db, &members, &mut active) {
            warn!("party: swap checks failed: {err:#}");
        }
        record_played(layout, members[active].name);
    }
    if std::env::var_os("PARTY_FALL_TEST").is_some() {
        if let Err(err) = fall_test::checks(sup, layout, &members, &mut active) {
            warn!("party: fall checks failed: {err:#}");
        }
        record_played(layout, members[active].name);
    }
    if std::env::var_os("PARTY_ORDER_TEST").is_some() {
        *sup.party.lock().unwrap() = members.iter().map(|m| (m.guid, m.name)).collect();
        if let Err(err) = order_test::checks(sup, layout, &members, &mut active) {
            warn!("party: order checks failed: {err:#}");
        }
        record_played(layout, members[active].name);
    }
    if std::env::var_os("PARTY_PAUSE_TEST").is_some() {
        if let Err(err) = pause_test::checks(sup, layout, &members, active) {
            warn!("party: pause checks failed: {err:#}");
        }
    }
    if std::env::var_os("PARTY_M1C_TEST").is_some() {
        if let Err(err) = m1c_test::checks(sup, layout, db, &members, &mut active) {
            warn!("party: M1c checks failed: {err:#}");
        }
        record_played(layout, members[active].name);
    }
    *sup.party.lock().unwrap() = members.iter().map(|m| (m.guid, m.name)).collect();
    loop {
        let next = (active + 1) % members.len();
        *sup.next_swap.lock().unwrap() = Some(members[next].name.to_string());
        let pressed = loop {
            if sup.cancelled() {
                return Ok(());
            }
            if let Some(pressed) = sup.switch_key.lock().unwrap().take() {
                break pressed;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        // The key's swap goes to the next member, the RTS camera's to the one it picked.
        let picked = sup.next_swap.lock().unwrap().clone();
        let next = members
            .iter()
            .position(|m| Some(m.name) == picked.as_deref())
            .unwrap_or(next);
        match see_through(sup, layout, &members, active, next, pressed, SWAP_LIMIT) {
            Ok(_) => active = next,
            Err(err) => warn!("party: swap failed: {err:#}"),
        }
    }
}

/// The preload: behind the startup cover, once through every member and back to `active`. The
/// client learns a member's spells, bar, bags and model only by playing it (the server sends them in
/// its burst), so playing each once loads its icons, item records, textures and portrait where a
/// later swap finds them.
fn warm_up(
    sup: &Supervisor,
    layout: &Layout,
    members: &[Member],
    active: usize,
) -> anyhow::Result<()> {
    let start = Instant::now();
    sup.set(Phase::Ready, "Preparing the party");
    let mut at = active;
    for _ in 0..members.len() {
        let next = (at + 1) % members.len();
        // A party bot just loaded is not ready to take over until its first update: retried.
        let mut tries = 0;
        while let Err(err) = swap(sup, layout, members, at, next, Duration::from_millis(1500)) {
            tries += 1;
            if tries >= 10 {
                return Err(err);
            }
        }
        // The new character's rebind and its asset requests, a frame or two after the takeover.
        std::thread::sleep(WARM_UP_SETTLE);
        at = next;
    }
    info!(
        "party: warm-up through {} members in {:.0} ms",
        members.len(),
        start.elapsed().as_secs_f32() * 1000.0
    );
    Ok(())
}

/// How long the warm-up rests on each member: its rebind and the asset loads it starts.
const WARM_UP_SETTLE: Duration = Duration::from_millis(250);

/// Presses the switch key for `members[to]`, as a player would, and sees the swap through: the
/// checks' path to the key's.
fn swap(
    sup: &Supervisor,
    layout: &Layout,
    members: &[Member],
    from: usize,
    to: usize,
    limit: Duration,
) -> anyhow::Result<Duration> {
    *sup.next_swap.lock().unwrap() = Some(members[to].name.to_string());
    sup.press_switch();
    let start = Instant::now();
    let pressed = loop {
        if let Some(pressed) = sup.switch_key.lock().unwrap().take() {
            break pressed;
        }
        if start.elapsed() > STEP_LIMIT {
            bail!("the switch key was never taken");
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    see_through(sup, layout, members, from, to, pressed, limit)
}

/// Waits for the client to play `members[to]` after the key the bridge sent its `.party swap`
/// on, and logs the time from that frame to the takeover.
fn see_through(
    sup: &Supervisor,
    layout: &Layout,
    members: &[Member],
    from: usize,
    to: usize,
    pressed: Instant,
    limit: Duration,
) -> anyhow::Result<Duration> {
    let (old, new) = (&members[from], &members[to]);
    info!("party: swap {} -> {} on the key", old.name, new.name);
    // Polled finely: the takeover is timed in tens of milliseconds.
    while sup.self_guid.load(Ordering::Relaxed) != new.guid {
        if sup.cancelled() {
            bail!("cancelled");
        }
        if pressed.elapsed() > limit {
            bail!("timed out waiting to play {}", new.name);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let took = pressed.elapsed();
    let line = format!(
        "swap {} -> {}: playing {} {:.0} ms after the key",
        old.name,
        new.name,
        new.name,
        took.as_secs_f32() * 1000.0
    );
    info!("party: {line}");
    append(layout, "swap.txt", &line);
    record_played(layout, new.name);
    Ok(took)
}

/// Each member without a character gets one, created on the stock path at character select.
fn create_missing(sup: &Supervisor, db: &Db, members: &[Member]) -> anyhow::Result<()> {
    for (member, (_, class, class_name)) in members.iter().zip(ROSTER) {
        if db.character(member.name)?.is_some() {
            continue;
        }
        sup.set(
            Phase::Setup,
            &format!("Creating {} the {class_name}", member.name),
        );
        sup.ask(SessionRequest::Relog(AutoLogin {
            character: None,
            ..member.login()
        }));
        sup.ask(SessionRequest::CreateCharacter {
            name: member.name.to_string(),
            race: HUMAN,
            class,
            gender: 0,
        });
        wait(sup, &format!("create {}", member.name), || {
            Ok(db.character(member.name)?.is_some())
        })?;
        info!("party: created {} the {class_name}", member.name);
    }
    Ok(())
}

/// Loads every member but `active` as a party bot of the active character, and waits for them.
fn load_bots(sup: &Supervisor, db: &Db, members: &[Member], active: usize) -> anyhow::Result<()> {
    let others: Vec<&Member> = members
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != active)
        .map(|(_, m)| m)
        .collect();
    // A bot of the last leader may still be on its way out.
    wait(sup, "release the other bots", || {
        others.iter().try_fold(true, |all, m| {
            Ok(all && !db.character(m.name)?.context("member gone")?.online)
        })
    })?;
    for m in &others {
        sup.ask(SessionRequest::Command(format!(
            ".partybot load {}",
            m.name
        )));
    }
    wait(sup, "load the party bots", || {
        others.iter().try_fold(true, |all, m| {
            Ok(all && db.character(m.name)?.context("member gone")?.online)
        })
    })
}

/// Polls `done` every 50 ms until it holds, for at most [`STEP_LIMIT`].
fn wait(
    sup: &Supervisor,
    what: &str,
    mut done: impl FnMut() -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    let start = Instant::now();
    while !done()? {
        if sup.cancelled() {
            bail!("cancelled");
        }
        if start.elapsed() > STEP_LIMIT {
            bail!("timed out waiting to {what}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Where the member last played is kept.
fn played_file(layout: &Layout) -> std::path::PathBuf {
    layout.state().join("played.txt")
}

fn record_played(layout: &Layout, name: &str) {
    let _ = std::fs::write(played_file(layout), name);
}

fn append(layout: &Layout, file: &str, line: &str) {
    use std::io::Write as _;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(layout.logs().join(file))
    {
        let _ = writeln!(f, "{line}");
    }
}

// ── What the checks share (`PARTY_SWAP_TEST`, `PARTY_M1C_TEST`) ──────────────────────────────────────

/// Linen Cloth, the item given to the played character before it is switched away.
const TEST_ITEM: u32 = 2589;
/// Kobold Vermin, the level 1 hostile summoned to start a fight.
const TEST_MOB: u32 = 6;

fn settle(d: Duration) {
    std::thread::sleep(d);
}

fn verdict(ok: bool) -> &'static str {
    if ok {
        "ok"
    } else {
        "FAIL"
    }
}

fn row(r: &CharRow) -> String {
    format!(
        "level {} xp {} money {} items {} map {} at [{:.0}, {:.0}, {:.0}] online {}",
        r.level, r.xp, r.money, r.items, r.map, r.pos[0], r.pos[1], r.pos[2], r.online
    )
}

fn self_pos(sup: &Supervisor) -> Option<Vec3> {
    sup.units
        .lock()
        .unwrap()
        .iter()
        .find(|u| u.is_self)
        .map(|u| u.pos)
}

/// Bevy space back to WoW's: the inverse of `wow_to_bevy`'s `(x, y, z) -> (-y, z, -x)`.
fn bevy_to_wow(p: Vec3) -> [f32; 3] {
    [-p.z, -p.x, p.y]
}
