//! The M1b checks (`PARTY_SWAP_TEST=1`), from the server's side: benilla does not understand
//! `SMSG_PARTY_SWAP` yet, so after the first swap the client's own view is stale. Each check reads
//! the server's: the `[PartySwap]` and `[PartyStatus]` lines of `Server.log` (`.party swap`,
//! `.party status`) and the characters database. Findings go to `logs/swap-checks.txt`.
//!
//! - A follow: the played character jumps 25 yd, then is swapped away at once; it must run back
//!   to the new leader as a party bot.
//! - Idle swaps around the party, twice.
//! - Two fights, each swapped mid-combat; the swapped-away character must join the fight.
//! - After every swap, for each member: who holds the client (`session=client`), who runs
//!   PartyBotAI and under whom, who leads the group, and that the old character has no client
//!   mover and no movement change or spline waiting on an ack.
//! - Back to the first character: XP, items, level and account kept.
//! - The server log's errors during the test.

use std::collections::{HashMap, VecDeque};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _};
use bevy::log::info;
use bevy::math::Vec3;

use benilla_app::SessionRequest;

use super::{append, bevy_to_wow, row, self_pos, settle, verdict, Member, ROSTER, TEST_MOB};
use crate::server::db::Db;
use crate::server::layout::Layout;
use crate::server::Supervisor;

/// Kobold Vermin's name, as `.party status` prints a victim.
pub(super) const TEST_MOB_NAME: &str = "Kobold Vermin";
/// Its health for the test, set with `.modify hp`.
pub(super) const TEST_MOB_HP: u32 = 200;
/// The priest's `ChrClasses` id: PartyBotAI makes it a healer, which takes no victim.
pub(super) const PRIEST: u8 = 5;

/// New lines of `Server.log` since the last read.
pub(super) struct ServerLog {
    path: PathBuf,
    offset: u64,
    partial: String,
    /// Read and not yet passed over.
    queue: VecDeque<String>,
}

impl ServerLog {
    pub(super) fn new(layout: &Layout) -> anyhow::Result<Self> {
        let path = layout.logs().join("Server.log");
        let offset = std::fs::metadata(&path)
            .with_context(|| format!("no {}", path.display()))?
            .len();
        Ok(Self {
            path,
            offset,
            partial: String::new(),
            queue: VecDeque::new(),
        })
    }

    fn read(&mut self) -> anyhow::Result<Vec<String>> {
        let mut file = std::fs::File::open(&self.path)?;
        file.seek(SeekFrom::Start(self.offset))?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        self.offset += buf.len() as u64;
        self.partial.push_str(&String::from_utf8_lossy(&buf));
        let Some(end) = self.partial.rfind('\n') else {
            return Ok(Vec::new());
        };
        let done: Vec<String> = self.partial[..end]
            .lines()
            .map(|l| l.trim_end().to_string())
            .collect();
        self.partial.drain(..=end);
        Ok(done)
    }

    /// The first new line `pick` accepts, within `limit`; lines before it are passed over.
    pub(super) fn wait_for(
        &mut self,
        sup: &Supervisor,
        limit: Duration,
        mut pick: impl FnMut(&str) -> bool,
    ) -> anyhow::Result<String> {
        let start = Instant::now();
        loop {
            while let Some(line) = self.queue.pop_front() {
                if pick(&line) {
                    return Ok(line);
                }
            }
            let more = self.read()?;
            self.queue.extend(more);
            if !self.queue.is_empty() {
                continue;
            }
            if sup.cancelled() {
                bail!("cancelled");
            }
            if start.elapsed() > limit {
                bail!("no matching server log line in {limit:?}");
            }
            settle(Duration::from_millis(20));
        }
    }
}

/// A member as `.party status` prints it.
#[derive(Debug, Default, Clone)]
pub(super) struct Status {
    pub(super) session: String,
    pub(super) mover: String,
    pub(super) ai: String,
    pub(super) leader: String,
    pub(super) dist: f32,
    pub(super) group_leader: bool,
    pub(super) combat: bool,
    pub(super) victim: String,
    /// The victim's health, 0 without one.
    pub(super) victim_hp: u32,
    pub(super) pending_moves: u32,
    pub(super) spline_ack: u32,
    pub(super) raw: String,
    /// Where it stands, WoW coordinates.
    pub(super) pos: [f32; 3],
}

/// Asks `.party status` and reads one line per member.
pub(super) fn status(
    sup: &Supervisor,
    log: &mut ServerLog,
    members: &[Member],
) -> anyhow::Result<HashMap<&'static str, Status>> {
    sup.ask(SessionRequest::PartyCommand(".party status".into()));
    let mut out = HashMap::new();
    while out.len() < members.len() {
        let line = log.wait_for(sup, Duration::from_secs(5), |l| {
            l.contains("[PartyStatus] ")
        })?;
        let body = &line[line.find("[PartyStatus] ").unwrap() + 14..];
        let Some(member) = members
            .iter()
            .find(|m| body.starts_with(&format!("{} ", m.name)))
        else {
            continue;
        };
        let kv: HashMap<&str, &str> = body.split(' ').filter_map(|w| w.split_once('=')).collect();
        let get = |k: &str| kv.get(k).copied().unwrap_or("").to_string();
        let num = |k: &str| {
            kv.get(k)
                .and_then(|v| v.parse::<f32>().ok())
                .unwrap_or(-1.0)
        };
        out.insert(
            member.name,
            Status {
                session: get("session"),
                mover: get("mover"),
                ai: get("ai"),
                leader: get("leader"),
                dist: num("dist"),
                group_leader: get("group_leader") == "1",
                combat: get("combat") == "1",
                victim: body
                    .split(" victim=")
                    .nth(1)
                    .and_then(|v| v.split(" victim_hp=").next())
                    .unwrap_or("")
                    .to_string(),
                victim_hp: num("victim_hp").max(0.0) as u32,
                pending_moves: num("pending_moves") as u32,
                spline_ack: num("spline_ack") as u32,
                pos: kv.get("pos").map_or([0.0; 3], |p| {
                    let mut c = p.split(',').map(|v| v.parse().unwrap_or(0.0));
                    [
                        c.next().unwrap_or(0.0),
                        c.next().unwrap_or(0.0),
                        c.next().unwrap_or(0.0),
                    ]
                }),
                raw: body.to_string(),
            },
        );
    }
    Ok(out)
}

/// Swaps from `old` to `new` and waits for the server's verdict: the time from the command to
/// the `done` line, and the server's own time for the swap.
fn swap(
    sup: &Supervisor,
    log: &mut ServerLog,
    old: &str,
    new: &str,
) -> anyhow::Result<(Duration, String)> {
    let start = Instant::now();
    sup.ask(SessionRequest::Command(format!(".party swap {new}")));
    let done = format!("[PartySwap] {old} -> {new}: done");
    let line = log.wait_for(sup, Duration::from_secs(5), |l| {
        l.contains(&done) || l.contains("[PartySwap]") && l.contains("refused")
    })?;
    if !line.contains(&done) {
        bail!("swap {old} -> {new}: {line}");
    }
    let took = line
        .split("swap took ")
        .nth(1)
        .unwrap_or("?")
        .trim()
        .to_string();
    Ok((start.elapsed(), took))
}

/// The members' state right after a swap to `new` from `old`: findings, each `ok` or `FAIL`.
fn swap_findings(
    s: &HashMap<&'static str, Status>,
    members: &[Member],
    old: &str,
    new: &str,
) -> Vec<String> {
    let mut out = Vec::new();
    let n = &s[new];
    out.push(format!(
        "{new}: session={} ai={} group_leader={} mover={} -> {}",
        n.session,
        n.ai,
        n.group_leader,
        n.mover,
        verdict(n.session == "client" && n.ai == "none" && n.group_leader)
    ));
    for m in members.iter().filter(|m| m.name != new) {
        let b = &s[m.name];
        let mut ok = b.session == "bot" && b.ai == "PartyBotAI" && b.leader == new;
        let mut line = format!(
            "{}: session={} ai={} leader={} mover={} pending_moves={} spline_ack={}",
            m.name, b.session, b.ai, b.leader, b.mover, b.pending_moves, b.spline_ack
        );
        if m.name == old {
            // Nothing may wait on the old character's client. A spline-done flag is not a wait:
            // a bot's own movement sets it, and only client movement reads it.
            ok &= b.mover == "none" && b.pending_moves == 0;
            line.push_str(" (swapped away)");
        }
        out.push(format!("{line} -> {}", verdict(ok)));
    }
    out
}

pub(super) fn swap_checks(
    sup: &Supervisor,
    layout: &Layout,
    db: &Db,
    members: &[Member],
    active: &mut usize,
) -> anyhow::Result<()> {
    let report = |line: String| {
        info!("party: swap check {line}");
        append(layout, "swap-checks.txt", &line);
    };
    report(format!("--- run at launch +{:?}", sup.launched.elapsed()));
    let first = *active;
    let mut log = ServerLog::new(layout)?;
    let names: Vec<&str> = members.iter().map(|m| m.name).collect();
    settle(Duration::from_secs(3));

    // Persistence baseline: an item to carry, then the saved rows and accounts.
    sup.ask(SessionRequest::Command(format!(
        ".additem {} 1",
        super::TEST_ITEM
    )));
    settle(Duration::from_secs(1));
    sup.ask(SessionRequest::Command(".saveall".into()));
    settle(Duration::from_secs(2));
    let accounts = |db: &Db| -> anyhow::Result<String> {
        let list = names
            .iter()
            .map(|n| format!("'{n}'"))
            .collect::<Vec<_>>()
            .join(",");
        Ok(db
            .query(
                Some("characters"),
                &format!(
                    "SELECT name, account FROM characters WHERE name IN ({list}) ORDER BY name"
                ),
            )?
            .replace('\t', "=")
            .lines()
            .collect::<Vec<_>>()
            .join(" "))
    };
    let accounts_before = accounts(db)?;
    let mut before = HashMap::new();
    for m in members {
        let r = db.character(m.name)?.context("member gone")?;
        report(format!("{} before: {}", m.name, row(&r)));
        before.insert(m.name, r);
    }
    report(format!("accounts before: {accounts_before}"));
    let s = status(sup, &mut log, members)?;
    for m in members {
        report(format!("start: {}", s[m.name].raw));
    }

    let mut times: Vec<(Duration, String)> = Vec::new();
    let mut fails = 0;
    // Reports each finding and counts the failed ones.
    let tally = |findings: Vec<String>, report: &dyn Fn(String)| -> usize {
        findings
            .into_iter()
            .map(|f| {
                let failed = f.ends_with("FAIL");
                report(f);
                usize::from(failed)
            })
            .sum()
    };

    // 1. Follow: jump 25 yd while the client still knows its character, then swap away at once.
    let start = self_pos(sup).context("no self position")?;
    let wow = bevy_to_wow(start + Vec3::new(25.0, 0.0, 0.0));
    sup.ask(SessionRequest::Command(format!(
        ".go xyz {:.1} {:.1} {:.1}",
        wow[0], wow[1], wow[2]
    )));
    settle(Duration::from_secs(1));
    let (old, new) = (*active, (*active + 1) % members.len());
    let t = swap(sup, &mut log, names[old], names[new])?;
    report(format!(
        "swap {} -> {} (after the jump): {:.0} ms to the done line, server {}",
        names[old],
        names[new],
        t.0.as_secs_f32() * 1000.0,
        t.1
    ));
    times.push(t);
    *active = new;
    let s = status(sup, &mut log, members)?;
    fails += tally(swap_findings(&s, members, names[old], names[new]), &report);
    let dist0 = s[names[old]].dist;
    settle(Duration::from_secs(6));
    let s = status(sup, &mut log, members)?;
    let dist1 = s[names[old]].dist;
    fails += tally(
        vec![format!(
            "follow: {} as a bot, {:.1} yd from {} right after the swap, {:.1} yd 6 s later -> {}",
            names[old],
            dist0,
            names[new],
            dist1,
            verdict((0.0..8.0).contains(&dist1) && (dist0 > dist1 || dist0 < 8.0))
        )],
        &report,
    );

    // 2. Idle swaps: around the party twice more.
    for _ in 0..(2 * members.len() - 1) {
        let (old, new) = (*active, (*active + 1) % members.len());
        let t = match swap(sup, &mut log, names[old], names[new]) {
            Ok(t) => t,
            Err(err) => {
                fails += 1;
                report(format!("{err:#} -> FAIL"));
                settle(Duration::from_secs(2));
                continue;
            }
        };
        report(format!(
            "swap {} -> {}: {:.0} ms to the done line, server {}",
            names[old],
            names[new],
            t.0.as_secs_f32() * 1000.0,
            t.1
        ));
        times.push(t);
        *active = new;
        settle(Duration::from_millis(1500));
        let s = status(sup, &mut log, members)?;
        fails += tally(swap_findings(&s, members, names[old], names[new]), &report);
    }

    // 3. Two fights, each swapped mid-combat.
    for fight_no in 1..=2 {
        let known: Vec<u64> = sup.units.lock().unwrap().iter().map(|u| u.guid).collect();
        sup.ask(SessionRequest::Command(format!(".npc summon {TEST_MOB}")));
        let mob = {
            let since = Instant::now();
            loop {
                let found = sup
                    .units
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|u| u.entry == TEST_MOB && u.health > 0 && !known.contains(&u.guid))
                    .map(|u| u.guid);
                if let Some(g) = found {
                    break Some(g);
                }
                if since.elapsed() > Duration::from_secs(5) {
                    break None;
                }
                settle(Duration::from_millis(100));
            }
        };
        let Some(mob) = mob else {
            fails += 1;
            report(format!(
                "fight {fight_no}: the summoned creature never streamed -> FAIL"
            ));
            continue;
        };
        sup.ask(SessionRequest::Target(mob));
        settle(Duration::from_millis(150));
        // Enough health for a fight with two swaps in it.
        sup.ask(SessionRequest::Command(format!(".modify hp {TEST_MOB_HP}")));
        settle(Duration::from_millis(150));
        sup.ask(SessionRequest::Command(".partybot attackstart".into()));
        let fight = Instant::now();
        settle(Duration::from_millis(1500));
        let s = status(sup, &mut log, members)?;
        let fighting: Vec<String> = members
            .iter()
            .filter(|m| s[m.name].victim == TEST_MOB_NAME)
            .map(|m| m.name.to_string())
            .collect();
        report(format!(
            "fight {fight_no}: at {:.1}s attacking the {TEST_MOB_NAME}: [{}]",
            fight.elapsed().as_secs_f32(),
            fighting.join(", ")
        ));

        let (old, new) = (*active, (*active + 1) % members.len());
        let mob_alive = |sup: &Supervisor| {
            sup.units
                .lock()
                .unwrap()
                .iter()
                .find(|u| u.guid == mob)
                .is_some_and(|u| u.health > 0)
        };
        let alive_at_swap = mob_alive(sup);
        let in_combat = s[names[old]].combat;
        let t = swap(sup, &mut log, names[old], names[new])?;
        report(format!(
            "fight {fight_no}: swap {} -> {} at {:.1}s, {} in combat={in_combat}, creature alive={alive_at_swap}: \
             {:.0} ms to the done line, server {}",
            names[old],
            names[new],
            fight.elapsed().as_secs_f32(),
            names[old],
            t.0.as_secs_f32() * 1000.0,
            t.1
        ));
        times.push(t);
        *active = new;
        let s = status(sup, &mut log, members)?;
        fails += tally(swap_findings(&s, members, names[old], names[new]), &report);

        // Again, off the character that took the client while it fought as a bot: it is in
        // combat as it goes back to being a bot.
        settle(Duration::from_millis(800));
        let third = (new + 1) % members.len();
        let s = status(sup, &mut log, members)?;
        let (combat2, victim2) = (s[names[new]].combat, s[names[new]].victim.clone());
        let alive_at_swap2 = mob_alive(sup);
        let t = swap(sup, &mut log, names[new], names[third])?;
        report(format!(
            "fight {fight_no}: swap {} -> {} at {:.1}s, {} in combat={combat2} victim={victim2}, creature alive={alive_at_swap2}: \
             {:.0} ms to the done line, server {}",
            names[new],
            names[third],
            fight.elapsed().as_secs_f32(),
            names[new],
            t.0.as_secs_f32() * 1000.0,
            t.1
        ));
        times.push(t);
        *active = third;
        let s = status(sup, &mut log, members)?;
        fails += tally(
            swap_findings(&s, members, names[new], names[third]),
            &report,
        );

        // Both swapped-away characters, bots now, must fight on until the creature dies: a damage
        // dealer with the creature as its victim, a healer (the priest) in combat, as it heals.
        let engaged = |s: &HashMap<&'static str, Status>, i: usize| {
            let m = &s[names[i]];
            m.victim == TEST_MOB_NAME || (ROSTER[i].1 == PRIEST && m.combat)
        };
        // The creature's health as its attackers see it, at the second swap and since.
        let mob_hp = |s: &HashMap<&'static str, Status>| {
            s.values()
                .filter(|m| m.victim == TEST_MOB_NAME)
                .map(|m| m.victim_hp)
                .min()
        };
        let hp_at_swap2 = mob_hp(&s);
        let mut hp_low = hp_at_swap2;
        let mut joined = None;
        let mut joined2 = None;
        let mut dead_at = None;
        while fight.elapsed() < Duration::from_secs(60) {
            let s = status(sup, &mut log, members)?;
            if let Some(h) = mob_hp(&s) {
                hp_low = Some(hp_low.map_or(h, |low| low.min(h)));
            }
            let at = Some(fight.elapsed().as_secs_f32());
            if joined.is_none() && engaged(&s, old) {
                joined = at;
            }
            if joined2.is_none() && engaged(&s, new) {
                joined2 = at;
            }
            if !mob_alive(sup) {
                dead_at = at;
                break;
            }
            settle(Duration::from_millis(500));
        }
        // Fighting on is the creature dying, or its health falling after the second swap: the
        // client's own character, unplayed here, never turns to face it.
        let hurt = matches!((hp_at_swap2, hp_low), (Some(a), Some(b)) if b < a);
        let seen = |t: Option<f32>| t.map_or("never seen".into(), |t| format!("at {t:.1}s"));
        fails += tally(
            vec![format!(
                "fight {fight_no}: attacking the creature as bots after their swaps: {} {}, {} {}; creature health {} at the second swap, lowest {} after; dead: {} -> {}",
                names[old],
                seen(joined),
                names[new],
                seen(joined2),
                hp_at_swap2.map_or("?".into(), |h| h.to_string()),
                hp_low.map_or("?".into(), |h| h.to_string()),
                dead_at.map_or("no, after 60 s".into(), |t| format!("at {t:.1}s")),
                verdict(
                    (dead_at.is_some() || hurt)
                        && (joined.is_some() || !alive_at_swap)
                        && (joined2.is_some() || !alive_at_swap2)
                )
            )],
            &report,
        );
        if dead_at.is_none() {
            sup.ask(SessionRequest::Target(mob));
            settle(Duration::from_millis(150));
            sup.ask(SessionRequest::Command(".die".into()));
        }
        settle(Duration::from_secs(2));
    }

    // 4. Back to the first character; what each kept.
    while *active != first {
        let (old, new) = (*active, (*active + 1) % members.len());
        let t = swap(sup, &mut log, names[old], names[new])?;
        times.push(t);
        *active = new;
        settle(Duration::from_millis(1500));
        let s = status(sup, &mut log, members)?;
        fails += tally(swap_findings(&s, members, names[old], names[new]), &report);
    }
    sup.ask(SessionRequest::Command(".saveall".into()));
    settle(Duration::from_secs(2));
    for m in members {
        let b = &before[m.name];
        let a = db.character(m.name)?.context("member gone")?;
        fails += tally(
            vec![format!(
                "{} after: {} (xp {} -> {}, items {} -> {}) -> {}",
                m.name,
                row(&a),
                b.xp,
                a.xp,
                b.items,
                a.items,
                verdict(a.online && (a.level > b.level || a.xp >= b.xp) && a.items >= b.items)
            )],
            &report,
        );
    }
    let accounts_after = accounts(db)?;
    fails += tally(
        vec![format!(
            "accounts after: {accounts_after} -> {}",
            verdict(accounts_after == accounts_before)
        )],
        &report,
    );

    // 5. What the server logged as an error while the test ran.
    let all = std::fs::read_to_string(layout.logs().join("Server.log")).unwrap_or_default();
    let errors: Vec<&str> = all
        .lines()
        .filter(|l| {
            let u = l.to_ascii_uppercase();
            (u.contains("ERROR") || u.contains("[CRASH]") || u.contains("ASSERT"))
                && !l.contains("Gameobject (Entry: 148500")
        })
        .collect();
    for e in &errors {
        report(format!("server error: {e}"));
    }
    let server_ms: Vec<String> = times.iter().map(|t| t.1.clone()).collect();
    let round_ms: Vec<u32> = times
        .iter()
        .map(|t| (t.0.as_secs_f32() * 1000.0).round() as u32)
        .collect();
    report(format!(
        "summary: {} swaps; server swap times [{}]; command to done line [{}] ms; {} server errors; {} FAIL findings",
        times.len(),
        server_ms.join(", "),
        round_ms
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        errors.len(),
        fails
    ));
    Ok(())
}
