//! The party: three local accounts, one character each, grouped. The character you play is a
//! real login; the others are vmangos party bots (`.partybot load`), which group under you and
//! follow and fight with their own AI. The switch key logs the played character out and the
//! target in, and the server's bots change hands:
//!
//! 1. The target's bot is removed (`.partybot remove` on it), so its account is free to log in.
//! 2. We relog as the target. A bot whose leader leaves removes itself (`PartyBotAI::UpdateAI`),
//!    so the others drop out as the old character logs out.
//! 3. The new character loads every other member as its bot; each joins its group.
//!
//! Each step is timed, and a switch's line goes to `logs/switch.txt`. `PARTY_TEST=1` runs the
//! M1 checks after the first entry: switches, a follow and a fight, and the saved state of a
//! character across them (`docs/PARTY_DESIGN.md`, M1).

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

/// The party's characters, by account: name, `ChrClasses` id, class name. All human, so a new
/// party starts together in Northshire.
const ROSTER: [(&str, u8, &str); 3] = [
    ("Partyone", 1, "warrior"),
    ("Partytwo", 5, "priest"),
    ("Partythree", 8, "mage"),
];
const HUMAN: u8 = 1;

/// How long a server-side step may take before the switch gives up.
const STEP_LIMIT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
struct Member {
    account: String,
    pass: String,
    name: &'static str,
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
/// as bots, then switches on the key until the game exits.
pub fn run(sup: &Supervisor, layout: &Layout, db: &Db, secrets: &Secrets) -> anyhow::Result<()> {
    let members: Vec<Member> = secrets
        .accounts
        .iter()
        .zip(ROSTER)
        .map(|((account, pass), (name, _, _))| Member {
            account: account.clone(),
            pass: pass.clone(),
            name,
        })
        .collect();
    create_missing(sup, db, &members)?;

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
    load_bots(sup, db, &members, active)?;
    record_played(layout, members[active].name);
    sup.cover.store(false, Ordering::Relaxed);
    info!(
        "party: {} in the world with {} bots",
        members[active].name,
        members.len() - 1
    );

    if std::env::var_os("PARTY_TEST").is_some() {
        if let Err(err) = checks(sup, layout, db, &members, &mut active) {
            warn!("party: M1 checks failed: {err:#}");
        }
    }
    while !sup.cancelled() {
        if sup.switch_key.swap(false, Ordering::Relaxed) {
            let next = (active + 1) % members.len();
            match switch(sup, layout, db, &members, active, next) {
                Ok(_) => active = next,
                Err(err) => {
                    warn!("party: switch failed: {err:#}");
                    sup.cover.store(false, Ordering::Relaxed);
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
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

/// One switch's step timings.
#[derive(Debug, Default)]
struct SwitchTimes {
    unload_target: Duration,
    logout: Duration,
    login_to_world: Duration,
    others_released: Duration,
    bots_loaded: Duration,
    total: Duration,
}

/// Plays `members[to]` in place of `members[from]`, which stays on as a bot.
fn switch(
    sup: &Supervisor,
    layout: &Layout,
    db: &Db,
    members: &[Member],
    from: usize,
    to: usize,
) -> anyhow::Result<SwitchTimes> {
    let (old, new) = (&members[from], &members[to]);
    let start = Instant::now();
    let mut t = SwitchTimes::default();
    sup.cover.store(true, Ordering::Relaxed);
    sup.set(Phase::Ready, &format!("Switching to {}", new.name));
    info!("party: switch {} -> {} begins", old.name, new.name);

    // 1. The target's bot goes, so its account may log in.
    let step = Instant::now();
    let target = db.character(new.name)?.context("no target character")?;
    if target.online {
        // `.partybot remove` acts on the selection, which must reach the server first.
        sup.ask(SessionRequest::Target(target.guid));
        std::thread::sleep(Duration::from_millis(150));
        sup.ask(SessionRequest::Command(".partybot remove".into()));
        wait(sup, "remove the target's bot", || {
            Ok(!db.character(new.name)?.context("gone")?.online)
        })?;
    }
    t.unload_target = step.elapsed();

    // 2. Out of the world as the old character.
    let step = Instant::now();
    sup.ask(SessionRequest::Relog(new.login()));
    wait(sup, "log out", || Ok(!sup.in_world.load(Ordering::Relaxed)))?;
    t.logout = step.elapsed();
    info!("party: {} logged out", old.name);

    // 3. In as the new one.
    let step = Instant::now();
    wait(sup, "enter the world", || {
        Ok(sup.in_world.load(Ordering::Relaxed))
    })?;
    t.login_to_world = step.elapsed();
    info!("party: {} in the world", new.name);
    record_played(layout, new.name);
    sup.cover.store(false, Ordering::Relaxed);

    // 4. The others: released by the old leader's logout, loaded under the new one.
    let step = Instant::now();
    let others: Vec<&Member> = members
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != to)
        .map(|(_, m)| m)
        .collect();
    wait(sup, "release the other bots", || {
        others.iter().try_fold(true, |all, m| {
            Ok(all && !db.character(m.name)?.context("gone")?.online)
        })
    })?;
    t.others_released = step.elapsed();
    let step = Instant::now();
    for m in &others {
        sup.ask(SessionRequest::Command(format!(
            ".partybot load {}",
            m.name
        )));
    }
    wait(sup, "load the party bots", || {
        others.iter().try_fold(true, |all, m| {
            Ok(all && db.character(m.name)?.context("gone")?.online)
        })
    })?;
    t.bots_loaded = step.elapsed();
    t.total = start.elapsed();

    let line = format!(
        "switch {} -> {}: total {:.2}s = unload target bot {:.2}s + logout {:.2}s + login and \
         enter world {:.2}s + old bots released {:.2}s + bots loaded {:.2}s",
        old.name,
        new.name,
        t.total.as_secs_f32(),
        t.unload_target.as_secs_f32(),
        t.logout.as_secs_f32(),
        t.login_to_world.as_secs_f32(),
        t.others_released.as_secs_f32(),
        t.bots_loaded.as_secs_f32(),
    );
    info!("party: {line}");
    append(layout, "switch.txt", &line);
    Ok(t)
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

// ── The M1 checks (`PARTY_TEST=1`) ───────────────────────────────────────────────────────────────

/// Linen Cloth, the item given to the played character before it is switched away.
const TEST_ITEM: u32 = 2589;
/// Kobold Vermin, the level 1 hostile summoned to start a fight.
const TEST_MOB: u32 = 6;

/// Switches through the party and back, checking follow, fight and the saved state of the first
/// character. Each finding is logged as `party: check ...` and written to `logs/checks.txt`.
fn checks(
    sup: &Supervisor,
    layout: &Layout,
    db: &Db,
    members: &[Member],
    active: &mut usize,
) -> anyhow::Result<()> {
    let report = |line: String| {
        info!("party: check {line}");
        append(layout, "checks.txt", &line);
    };
    let me = *active;
    let first = members[me].name;
    settle(Duration::from_secs(3));

    // A's own state, then an item to carry across the switches.
    sup.ask(SessionRequest::Command(format!(".additem {TEST_ITEM} 1")));
    settle(Duration::from_secs(2));
    sup.ask(SessionRequest::Command(".saveall".into()));
    settle(Duration::from_secs(2));
    let before = db.character(first)?.context("no first character")?;
    report(format!("{first} before: {}", row(&before)));

    // Away from A: A becomes a bot.
    let other = (me + 1) % members.len();
    switch(sup, layout, db, members, me, other)?;
    *active = other;
    settle(Duration::from_secs(4));

    // Follow: the leader moves 25 yd, the bots should come after it.
    let start = self_pos(sup).context("no self position")?;
    let moved = start + Vec3::new(25.0, 0.0, 0.0);
    let wow = bevy_to_wow(moved);
    sup.ask(SessionRequest::Command(format!(
        ".go xyz {:.1} {:.1} {:.1}",
        wow[0], wow[1], wow[2]
    )));
    settle(Duration::from_secs(1));
    let at = self_pos(sup).context("no self position")?;
    let bot_guids: Vec<u64> = members
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != other)
        .filter_map(|(_, m)| db.character(m.name).ok().flatten().map(|r| r.guid))
        .collect();
    let spread = |sup: &Supervisor, at: Vec3| -> Vec<f32> {
        let units = sup.units.lock().unwrap();
        bot_guids
            .iter()
            .map(|g| {
                units
                    .iter()
                    .find(|u| u.guid == *g)
                    .map_or(f32::INFINITY, |u| u.pos.distance(at))
            })
            .collect()
    };
    report(format!(
        "follow: moved 25 yd; bots at {:?} yd",
        spread(sup, at)
    ));
    settle(Duration::from_secs(10));
    let after = spread(sup, at);
    report(format!(
        "follow: after 10 s bots at {after:?} yd -> {}",
        verdict(after.iter().all(|d| *d < 12.0))
    ));

    // Fight: a hostile at the leader's feet; the bots, A among them, should kill it.
    sup.ask(SessionRequest::Command(format!(".npc summon {TEST_MOB}")));
    settle(Duration::from_secs(1));
    let mob = sup
        .units
        .lock()
        .unwrap()
        .iter()
        .find(|u| u.entry == TEST_MOB && u.health > 0)
        .map(|u| u.guid);
    match mob {
        None => report("fight: the summoned creature never streamed".into()),
        Some(guid) => {
            let fight = Instant::now();
            let mut dead = false;
            // Which bots selected the creature, and when each first did.
            let mut engaged: Vec<(u64, f32)> = Vec::new();
            let mut ordered = false;
            while fight.elapsed() < Duration::from_secs(60) {
                // Left alone it may never aggro a player standing still; then the leader
                // orders the attack, as a player would with `.partybot attackstart`.
                if !ordered && engaged.is_empty() && fight.elapsed() > Duration::from_secs(8) {
                    ordered = true;
                    sup.ask(SessionRequest::Target(guid));
                    settle(Duration::from_millis(150));
                    sup.ask(SessionRequest::Command(".partybot attackstart".into()));
                    report(format!(
                        "fight: no bot engaged on its own in 8 s; ordered the attack at {:.1}s",
                        fight.elapsed().as_secs_f32()
                    ));
                }
                let units = sup.units.lock().unwrap().clone();
                for u in units.iter().filter(|u| bot_guids.contains(&u.guid)) {
                    if u.target == guid && !engaged.iter().any(|(g, _)| *g == u.guid) {
                        engaged.push((u.guid, fight.elapsed().as_secs_f32()));
                    }
                }
                let hp = units.iter().find(|u| u.guid == guid).map(|u| u.health);
                if hp.is_none_or(|h| h == 0) {
                    dead = true;
                    break;
                }
                settle(Duration::from_millis(250));
            }
            let names: Vec<String> = engaged
                .iter()
                .map(|(g, t)| {
                    let name = members
                        .iter()
                        .find(|m| db.character(m.name).ok().flatten().map(|r| r.guid) == Some(*g))
                        .map_or("?", |m| m.name);
                    format!("{name} at {t:.1}s")
                })
                .collect();
            report(format!(
                "fight: creature {} after {:.1}s; bots that targeted it: [{}] -> {}",
                if dead { "dead" } else { "alive" },
                fight.elapsed().as_secs_f32(),
                names.join(", "),
                verdict(dead && engaged.len() == bot_guids.len())
            ));
        }
    }
    settle(Duration::from_secs(3));
    let as_bot = db.character(first)?.context("gone")?;
    report(format!("{first} as a bot (last save): {}", row(&as_bot)));

    // Back to A, then around the party and back once more.
    switch(sup, layout, db, members, other, me)?;
    *active = me;
    settle(Duration::from_secs(3));
    sup.ask(SessionRequest::Command(".saveall".into()));
    settle(Duration::from_secs(2));
    let back = db.character(first)?.context("gone")?;
    report(format!("{first} back: {}", row(&back)));
    report(format!(
        "{first} kept: xp {} -> {} ({}), items {} -> {} ({}), level {} -> {}",
        before.xp,
        back.xp,
        verdict(back.level > before.level || back.xp >= before.xp),
        before.items,
        back.items,
        verdict(back.items >= before.items),
        before.level,
        back.level
    ));
    let pos = self_pos(sup).map(bevy_to_wow);
    report(format!(
        "{first} logged in at {:?}; its bot was last saved at {:?}",
        pos.map(|p| p.map(|c| c.round())),
        as_bot.pos.map(|c| c.round())
    ));

    for step in 0..members.len() {
        let next = (*active + 1) % members.len();
        switch(sup, layout, db, members, *active, next)?;
        *active = next;
        let _ = step;
        settle(Duration::from_secs(3));
    }
    report(format!(
        "round trip: {} switches done, playing {}",
        members.len(),
        members[*active].name
    ));
    Ok(())
}

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
