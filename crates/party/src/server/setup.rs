//! First-run setup, step by step, each step leaving a marker so an interrupted run resumes:
//! download and unpack, extract the client data, initialise and import the database, apply
//! missing migrations, create the account. The configs are rewritten on every launch.

use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher as _, Hasher as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context as _};
use bevy::log::{info, warn};

use super::db::{sql_word, Db};
use super::fetch::{self, Asset};
use super::layout::{forward, Layout};
use super::procs::{self, BELOW_NORMAL};
use super::{Phase, Supervisor, DB_PORT, REALMD_PORT, REALM_NAME, WORLD_PORT};

const VMANGOS: &str = "vmangos/core";
const MARIADB_VERSION: &str = "11.4.5";
/// The client build the extracted DBCs are filed under (`DataDir/5875/dbc`).
const CLIENT_BUILD: u32 = 5875;
/// The account's GM level: `.partybot` needs `SEC_ADMINISTRATOR`.
const GM_LEVEL: u32 = 6;

/// How many local accounts the party has: one per character, since `.partybot load` needs the
/// bot's account offline.
pub const PARTY_SIZE: usize = 3;

/// The passwords this install made for itself, and the party's local accounts.
pub struct Secrets {
    pub root_pass: String,
    pub mangos_pass: String,
    /// `(account, password)`, one per party member.
    pub accounts: Vec<(String, String)>,
}

impl Secrets {
    pub fn load_or_create(layout: &Layout) -> anyhow::Result<Self> {
        let path = layout.state().join("secrets.txt");
        let mut kv = read_kv(&path);
        let before = kv.len();
        kv.entry("root".into()).or_insert_with(|| random_word(16));
        kv.entry("mangos".into()).or_insert_with(|| random_word(16));
        for i in 0..PARTY_SIZE {
            let (key, name) = account_key(i);
            kv.entry(key.clone()).or_insert(name);
            kv.entry(format!("{key}_pass"))
                .or_insert_with(|| random_word(12));
        }
        if kv.len() != before {
            write_kv(&path, &kv)?;
        }
        let get = |k: &str| {
            kv.get(k)
                .cloned()
                .with_context(|| format!("secrets.txt lacks {k}"))
        };
        let accounts = (0..PARTY_SIZE)
            .map(|i| {
                let (key, _) = account_key(i);
                Ok((get(&key)?, get(&format!("{key}_pass"))?))
            })
            .collect::<anyhow::Result<_>>()?;
        Ok(Self {
            root_pass: get("root")?,
            mangos_pass: get("mangos")?,
            accounts,
        })
    }
}

/// The secrets key and account name of party member `i`: `account`/`PARTY`, then
/// `account2`/`PARTY2`, and so on.
fn account_key(i: usize) -> (String, String) {
    match i {
        0 => ("account".into(), "PARTY".into()),
        _ => (format!("account{}", i + 1), format!("PARTY{}", i + 1)),
    }
}

/// An uppercase alphanumeric word from the OS-seeded hasher: local passwords, never shown.
fn random_word(len: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let state = std::collections::hash_map::RandomState::new();
    (0..len)
        .map(|i| {
            let mut h = state.build_hasher();
            h.write_usize(i);
            h.write_u128(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos()),
            );
            ALPHABET[(h.finish() % ALPHABET.len() as u64) as usize] as char
        })
        .collect()
}

fn read_kv(path: &Path) -> HashMap<String, String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect()
}

fn write_kv(path: &Path, kv: &HashMap<String, String>) -> anyhow::Result<()> {
    let mut keys: Vec<_> = kv.keys().collect();
    keys.sort();
    let body: String = keys.iter().map(|k| format!("{k}={}\n", kv[*k])).collect();
    std::fs::write(path, body)?;
    Ok(())
}

/// A step run once: skipped when its marker exists, marked when it succeeds, timed either way.
fn step(
    sup: &Supervisor,
    layout: &Layout,
    name: &str,
    title: &str,
    f: impl FnOnce() -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    if layout.marker(name).exists() {
        return Ok(());
    }
    sup.set(Phase::Setup, title);
    sup.timed(name, f)?;
    layout.mark(name)?;
    Ok(())
}

/// The steps that need no database running.
pub fn files(sup: &Supervisor, layout: &Layout) -> anyhow::Result<()> {
    layout.make_dirs()?;
    step(sup, layout, "fetch", "Downloading the server", || {
        fetch_all(sup, layout)
    })?;
    let data_dir = benilla_formats::wow_data()
        .context("no WoW 1.12.1 install found; set WOW_DATA to its Data folder")?;
    let wow_root = data_dir.parent().context("the Data folder has no parent")?;
    step(sup, layout, "maps", "Extracting maps", || {
        extract_maps(sup, layout, wow_root)
    })?;
    step(sup, layout, "vmaps", "Extracting collision", || {
        extract_vmaps(sup, layout, &data_dir)
    })?;
    let secrets = Secrets::load_or_create(layout)?;
    step(sup, layout, "dbinit", "Creating the database", || {
        init_db(sup, layout, &secrets)
    })?;
    Ok(())
}

/// The server build this game is made against: an exact vmangos commit, its Windows build and
/// database snapshot, and an exact MariaDB, each archive checked against its SHA-256.
const VMANGOS_SHA: &str = "9b7fb4dfdb1d863ef93c5cf24d57aff92124f41f";
const PINNED: [Pinned; 3] = [
    Pinned {
        name: "mariadb-11.4.5-winx64.zip",
        url: "https://archive.mariadb.org/mariadb-11.4.5/winx64-packages/mariadb-11.4.5-winx64.zip",
        size: 90_835_771,
        sha256: "b7c11d38657f16b837e68199d73670510aadb78f42dfa5d5fdea31a7aab342e3",
    },
    Pinned {
        name: "dev-windows-amd64-9b7fb4d.zip",
        url: "https://github.com/vmangos/core/releases/download/latest/dev-windows-amd64-9b7fb4d.zip",
        size: 5_805_146,
        sha256: "f287c9f4ca426b13480c59a4109ba8e45f27c35b56c52f1f6e6f59dd153a0351",
    },
    Pinned {
        name: "db-4641790.zip",
        url: "https://github.com/vmangos/core/releases/download/db_latest/db-4641790.zip",
        size: 30_511_447,
        sha256: "9e178f1d70363e92a7b8c259000102dc9739333d79ca00c99ea32dd1fb43e904",
    },
];

/// One archive of the pinned server.
struct Pinned {
    name: &'static str,
    url: &'static str,
    size: u64,
    sha256: &'static str,
}

fn fetch_all(sup: &Supervisor, layout: &Layout) -> anyhow::Result<()> {
    let [mariadb, build, dump] = PINNED;
    let sha = VMANGOS_SHA.to_string();
    let assets = PINNED.map(|p| Asset {
        url: p.url.to_string(),
        name: p.name.to_string(),
        size: Some(p.size),
    });
    let assets = assets.iter().collect::<Vec<_>>();
    let total: u64 = assets.iter().map(|a| a.size.unwrap_or(1)).sum();
    let mut before = 0u64;
    for (asset, pin) in assets.iter().zip(&PINNED) {
        let size = asset.size.unwrap_or(1);
        let dest = layout.downloads().join(&asset.name);
        sup.progress(format!("{} ({} MB)", asset.name, size / 1_000_000), None);
        if dest.exists() && fetch::sha256(&dest)? != pin.sha256 {
            warn!("party: {} does not match its checksum; fetching it again", asset.name);
            std::fs::remove_file(&dest)?;
        }
        fetch::download(asset, &dest, &sup.cancel, |f| {
            let done = before as f32 + f * size as f32;
            sup.progress(
                format!("{} ({} MB)", asset.name, size / 1_000_000),
                Some(done / total as f32),
            );
        })
        .with_context(|| {
            format!(
                "{} could not be downloaded; vmangos replaces its rolling builds, so place \
                 that exact file in {} and start again",
                asset.name,
                layout.downloads().display()
            )
        })?;
        let got = fetch::sha256(&dest)?;
        if got != pin.sha256 {
            std::fs::remove_file(&dest)?;
            bail!("{} has SHA-256 {got}, expected {}", asset.name, pin.sha256);
        }
        before += size;
    }

    sup.progress("Unpacking", None);
    let unpack = layout.state().join("unpack");
    let _ = std::fs::remove_dir_all(&unpack);
    fetch::unzip(&layout.downloads().join(mariadb.name), &unpack)?;
    replace_dir(
        &unpack.join(mariadb.name.trim_end_matches(".zip")),
        &layout.mariadb(),
    )?;
    fetch::unzip(&layout.downloads().join(build.name), &unpack)?;
    replace_dir(&unpack.join("Release"), &layout.vmangos())?;
    fetch::unzip(&layout.downloads().join(dump.name), &unpack)?;
    replace_dir(&unpack.join("mysql-dump"), &layout.sql())?;
    let _ = std::fs::remove_dir_all(&unpack);

    sup.progress("Fetching the server's settings", None);
    for (path, dest) in [
        (
            "src/mangosd/mangosd.conf.dist.in",
            layout.vmangos().join("mangosd.conf.dist"),
        ),
        (
            "src/realmd/realmd.conf.dist.in",
            layout.vmangos().join("realmd.conf.dist"),
        ),
        (
            "contrib/mmap/offmesh.txt",
            layout.data().join("offmesh.txt"),
        ),
        (
            "contrib/mmap/config.json",
            layout.data().join("config.json"),
        ),
    ] {
        std::fs::write(dest, fetch::raw(VMANGOS, &sha, path)?)?;
    }
    let mut installed = HashMap::new();
    installed.insert("vmangos_build".to_string(), build.name.to_string());
    installed.insert("vmangos_sha".to_string(), sha);
    installed.insert("vmangos_db".to_string(), dump.name.to_string());
    installed.insert("mariadb".to_string(), MARIADB_VERSION.to_string());
    write_kv(&layout.state().join("installed.txt"), &installed)?;
    info!("party: installed vmangos {} with {}", build.name, dump.name);
    Ok(())
}

/// Moves `from` to `to`, replacing what was there.
fn replace_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    let _ = std::fs::remove_dir_all(to);
    std::fs::rename(from, to)
        .with_context(|| format!("moving {} to {}", from.display(), to.display()))
}

/// `(a/b)` or `(a / b)` in a tool's progress line, as a fraction.
fn ratio(line: &str) -> Option<f32> {
    let open = line.rfind('(')?;
    let close = open + line[open..].find(')')?;
    let (a, b) = line[open + 1..close].split_once('/')?;
    let (a, b) = (a.trim().parse::<f32>().ok()?, b.trim().parse::<f32>().ok()?);
    (b > 0.0).then(|| (a / b).min(1.0))
}

fn tool(layout: &Layout, exe: &str, cwd: &Path) -> Command {
    let mut cmd = Command::new(layout.vmangos_bin(exe));
    cmd.current_dir(cwd);
    cmd
}

/// Terrain heights, areas and liquids per map tile, and the DBCs the server reads, from the
/// client's MPQs. The install is only read: everything is written under `data/`.
fn extract_maps(sup: &Supervisor, layout: &Layout, wow_root: &Path) -> anyhow::Result<()> {
    let data = layout.data();
    let _ = std::fs::remove_dir_all(data.join("maps"));
    let _ = std::fs::remove_dir_all(data.join("dbc"));
    let mut cmd = tool(layout, "MapExtractor.exe", &data);
    cmd.arg("-i")
        .arg(wow_root)
        .arg("-o")
        .arg(&data)
        .arg("--silent");
    procs::run_tool(&sup.job, &mut cmd, 0, &sup.cancel, |line| {
        if line.starts_with("Extract") {
            sup.progress(line.trim().to_string(), ratio(line));
        }
    })?;
    let dbc = data.join(CLIENT_BUILD.to_string());
    std::fs::create_dir_all(&dbc)?;
    replace_dir(&data.join("dbc"), &dbc.join("dbc"))?;
    Ok(())
}

/// Collision models from the client's WMOs and doodads, assembled into vmaps.
fn extract_vmaps(sup: &Supervisor, layout: &Layout, data_dir: &Path) -> anyhow::Result<()> {
    let data = layout.data();
    let buildings = data.join("Buildings");
    let vmaps = data.join("vmaps");
    let _ = std::fs::remove_dir_all(&buildings);
    let _ = std::fs::remove_dir_all(&vmaps);
    let mut cmd = tool(layout, "VMapExtractor.exe", &data);
    cmd.arg("-d").arg(data_dir).arg("--silent");
    let mut seen = 0u32;
    procs::run_tool(&sup.job, &mut cmd, 0, &sup.cancel, |line| {
        if line.starts_with("Extract") || line.contains(".wmo") || line.starts_with("Opening") {
            seen += 1;
            sup.progress(format!("{seen} files read"), None);
        }
    })?;
    sup.progress("Assembling", None);
    std::fs::create_dir_all(&vmaps)?;
    let mut cmd = tool(layout, "VMapAssembler.exe", &data);
    cmd.arg("Buildings").arg("vmaps");
    procs::run_tool(&sup.job, &mut cmd, 0, &sup.cancel, |_| {})?;
    let _ = std::fs::remove_dir_all(&buildings);
    Ok(())
}

/// A fresh MariaDB data directory with our root password, listening on 127.0.0.1 only.
fn init_db(sup: &Supervisor, layout: &Layout, secrets: &Secrets) -> anyhow::Result<()> {
    let _ = std::fs::remove_dir_all(layout.db());
    let mut cmd = Command::new(layout.mariadb_bin("mariadb-install-db.exe"));
    cmd.arg(format!("--datadir={}", layout.db().display()))
        .arg(format!("--port={DB_PORT}"))
        .arg(format!("--password={}", secrets.root_pass));
    procs::run_tool(&sup.job, &mut cmd, 0, &sup.cancel, |_| {})?;
    let ini = layout.db().join("my.ini");
    let body = std::fs::read_to_string(&ini)?.replacen(
        "[mysqld]",
        "[mysqld]\nbind-address=127.0.0.1\nskip-name-resolve",
        1,
    );
    std::fs::write(ini, body)?;
    Ok(())
}

/// The settings files, from the build's `.conf.dist` with our keys set, on every launch.
pub fn write_configs(layout: &Layout, secrets: &Secrets) -> anyhow::Result<()> {
    let info = |db: &str| {
        format!(
            "\"127.0.0.1;{DB_PORT};mangos;{};{db}\"",
            secrets.mangos_pass
        )
    };
    let quoted = |p: PathBuf| format!("\"{}\"", forward(&p));
    let mmaps = mmaps_done(layout);
    let mangosd = [
        ("DataDir", quoted(layout.data())),
        ("LogsDir", quoted(layout.logs())),
        ("LoginDatabase.Info", info("realmd")),
        ("WorldDatabase.Info", info("mangos")),
        ("CharacterDatabase.Info", info("characters")),
        ("LogsDatabase.Info", info("logs")),
        ("WorldServerPort", WORLD_PORT.to_string()),
        ("BindIP", "\"127.0.0.1\"".into()),
        ("PidFile", "\"\"".into()),
        ("WaitAtStartupError", "0".into()),
        ("Console.Enable", "1".into()),
        ("Ra.Enable", "0".into()),
        ("SOAP.Enabled", "0".into()),
        ("mmap.enabled", if mmaps { "1" } else { "0" }.into()),
        // A loaded party bot keeps what it earns.
        ("PlayerBot.AllowSaving", "1".into()),
        // A hard stop loses at most two minutes.
        ("PlayerSave.Interval", "120000".into()),
        // The account is a GM only so it may run `.partybot`; it plays as a player.
        ("GM.LoginState", "0".into()),
        ("GM.Visible", "1".into()),
        ("GM.AcceptTickets", "0".into()),
        ("GM.Chat", "0".into()),
        ("GM.WhisperingTo", "1".into()),
        ("GM.CheatGod", "0".into()),
        ("GM.AllowPublicChannels", "1".into()),
    ];
    let realmd = [
        // realmd's key has no dot, unlike mangosd's.
        ("LoginDatabaseInfo", info("realmd")),
        ("LogsDir", quoted(layout.logs())),
        ("RealmServerPort", REALMD_PORT.to_string()),
        ("BindIP", "\"127.0.0.1\"".into()),
        ("PidFile", "\"\"".into()),
        ("WaitAtStartupError", "0".into()),
    ];
    for (name, keys) in [("mangosd", &mangosd[..]), ("realmd", &realmd[..])] {
        let dir = layout.vmangos();
        let dist = std::fs::read_to_string(dir.join(format!("{name}.conf.dist")))
            .with_context(|| format!("{name}.conf.dist is missing"))?;
        std::fs::write(dir.join(format!("{name}.conf")), with_keys(&dist, keys))?;
    }
    Ok(())
}

/// `conf` with each key's line replaced, or appended when absent.
fn with_keys(conf: &str, keys: &[(&str, String)]) -> String {
    let mut seen = HashSet::new();
    let mut out: Vec<String> = conf
        .lines()
        .map(|line| {
            let key = line.split('=').next().unwrap_or("").trim();
            match keys
                .iter()
                .find(|(k, _)| *k == key && !line.starts_with('#'))
            {
                Some((k, v)) => {
                    seen.insert(*k);
                    format!("{k} = {v}")
                }
                None => line.to_string(),
            }
        })
        .collect();
    for (k, v) in keys {
        if !seen.contains(k) {
            out.push(format!("{k} = {v}"));
        }
    }
    out.join("\n") + "\n"
}

/// The steps that need the database: import, migrations, and a check after a crash.
pub fn database(sup: &Supervisor, layout: &Layout, db: &Db, recovered: bool) -> anyhow::Result<()> {
    if recovered && layout.marker("dbimport").exists() {
        sup.set(Phase::Booting, "Checking the database");
        sup.timed("table check", || {
            db.repair(&["realmd", "characters", "logs"])
        })?;
    }
    let secrets = Secrets::load_or_create(layout)?;
    step(
        sup,
        layout,
        "dbimport",
        "Importing the world database",
        || import(sup, layout, db, &secrets),
    )?;
    step(
        sup,
        layout,
        "migrations",
        "Updating the world database",
        || migrate(sup, layout, db),
    )?;
    Ok(())
}

const DATABASES: [(&str, &str); 4] = [
    ("realmd", "logon"),
    ("characters", "characters"),
    ("logs", "logs"),
    ("mangos", "mangos"),
];

fn import(sup: &Supervisor, layout: &Layout, db: &Db, secrets: &Secrets) -> anyhow::Result<()> {
    let pass = sql_word(&secrets.mangos_pass);
    let mut sql = String::new();
    for (name, _) in DATABASES {
        sql += &format!("DROP DATABASE IF EXISTS {name}; CREATE DATABASE {name};");
    }
    for host in ["localhost", "127.0.0.1"] {
        sql += &format!(
            "CREATE USER IF NOT EXISTS 'mangos'@'{host}'; ALTER USER 'mangos'@'{host}' IDENTIFIED BY '{pass}';"
        );
        for (name, _) in DATABASES {
            sql += &format!("GRANT ALL ON {name}.* TO 'mangos'@'{host}';");
        }
    }
    db.query(None, &sql)?;
    let files: Vec<(&str, PathBuf)> = DATABASES
        .iter()
        .map(|(name, dump)| (*name, layout.sql().join(format!("{dump}.sql"))))
        .collect();
    let sizes: Vec<u64> = files
        .iter()
        .map(|(_, f)| std::fs::metadata(f).map_or(1, |m| m.len()))
        .collect();
    let total: u64 = sizes.iter().sum();
    let mut done = 0u64;
    for ((name, file), size) in files.iter().zip(&sizes) {
        sup.progress(
            format!("{name} ({} MB)", size / 1_000_000),
            Some(done as f32 / total as f32),
        );
        db.import(name, file)?;
        done += size;
    }
    db.query(
        Some("realmd"),
        &format!(
            "REPLACE INTO realmlist (id, name, address, localAddress, port, icon, realmflags, \
             timezone, allowedSecurityLevel, gamebuild_min, gamebuild_max) VALUES \
             (1, '{REALM_NAME}', '127.0.0.1', '127.0.0.1', {WORLD_PORT}, 0, 0, 1, 0, 5875, 5875)"
        ),
    )?;
    Ok(())
}

/// Applies the build's migrations the snapshot lacks, each `sql/migrations/<id>_<db>.sql` whose id
/// is not in that database's `migrations` table; mangosd refuses to start while one is missing.
fn migrate(sup: &Supervisor, layout: &Layout, db: &Db) -> anyhow::Result<()> {
    let installed = read_kv(&layout.state().join("installed.txt"));
    let sha = installed
        .get("vmangos_sha")
        .context("installed.txt lacks the build's commit")?;
    sup.progress("Comparing with the server build", None);
    let names = fetch::list_dir(VMANGOS, sha, "sql/migrations")?;
    let target = |suffix: &str| match suffix {
        "world" => Some("mangos"),
        "logon" => Some("realmd"),
        "characters" => Some("characters"),
        "logs" => Some("logs"),
        _ => None,
    };
    let mut applied: HashMap<&str, HashSet<String>> = HashMap::new();
    for (name, _) in DATABASES {
        let rows = db.query(Some(name), "SELECT id FROM migrations")?;
        applied.insert(name, rows.lines().map(str::to_string).collect());
    }
    let mut missing: Vec<(&str, &str, &str)> = names
        .iter()
        .filter_map(|file| {
            let (id, rest) = file.strip_suffix(".sql")?.split_once('_')?;
            let database = target(rest)?;
            (!applied[database].contains(id)).then_some((id, database, file.as_str()))
        })
        .collect();
    missing.sort();
    for (i, (id, database, file)) in missing.iter().enumerate() {
        sup.progress(
            format!("migration {id}"),
            Some(i as f32 / missing.len() as f32),
        );
        let body = fetch::raw(VMANGOS, sha, &format!("sql/migrations/{file}"))?;
        let tmp = layout.state().join("migration.sql");
        std::fs::write(&tmp, body)?;
        db.import(database, &tmp)?;
    }
    info!("party: {} migrations applied", missing.len());
    Ok(())
}

/// The party's local accounts, each created on mangosd's console and given the GM level
/// `.partybot` needs.
pub fn accounts(
    sup: &Supervisor,
    layout: &Layout,
    db: &Db,
    secrets: &Secrets,
) -> anyhow::Result<()> {
    for (i, (name, pass)) in secrets.accounts.iter().enumerate() {
        // The first account's marker predates the party.
        let marker = if i == 0 {
            "account".to_string()
        } else {
            format!("account-{name}")
        };
        if !layout.marker(&marker).exists() {
            account(sup, db, name, pass)?;
            layout.mark(&marker)?;
        }
    }
    Ok(())
}

fn account(sup: &Supervisor, db: &Db, name: &str, pass: &str) -> anyhow::Result<()> {
    sup.set(Phase::Setup, &format!("Creating account {name}"));
    let name = sql_word(name);
    let console = |line: String| -> anyhow::Result<()> {
        let mut running = sup.running.lock().unwrap();
        running
            .as_mut()
            .context("the server is not running")?
            .console(&line)
    };
    let first = |sql: String| -> anyhow::Result<Option<String>> {
        let rows = db.query(Some("realmd"), &sql)?;
        Ok(rows.lines().next().map(|l| l.trim().to_string()))
    };
    let exists = || first(format!("SELECT id FROM account WHERE username = '{name}'"));
    // vmangos keeps the level per realm, in `account_access`.
    let level = || {
        first(format!(
            "SELECT aa.gmlevel FROM account_access aa JOIN account a ON a.id = aa.id \
             WHERE a.username = '{name}' AND aa.RealmID IN (1, -1)"
        ))
    };
    // The account is written asynchronously, so its level is set only once its row exists.
    if exists()?.is_none() {
        console(format!("account create {name} {pass}"))?;
        wait_for(|| Ok(exists()?.is_some()), "create the account")?;
    }
    console(format!("account set gmlevel {name} {GM_LEVEL}"))?;
    wait_for(
        || Ok(level()?.as_deref() == Some(&GM_LEVEL.to_string())),
        "set the account's GM level",
    )?;
    Ok(())
}

/// Polls `done` until it holds, for at most 20 s.
pub fn wait_for(
    mut done: impl FnMut() -> anyhow::Result<bool>,
    what: &str,
) -> anyhow::Result<()> {
    let start = Instant::now();
    while !done()? {
        if start.elapsed() > Duration::from_secs(20) {
            bail!("the world server did not {what}; see logs/mangosd.out");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn mmaps_done(layout: &Layout) -> bool {
    layout.marker("mmaps").exists()
}

/// Builds the navigation meshes in the background, at low priority and one map at a time so an
/// exit loses at most the map under way. mangosd uses them from the next launch on.
pub fn start_mmaps(sup: &Arc<Supervisor>, layout: &Layout) {
    if mmaps_done(layout) {
        return;
    }
    let (sup, layout) = (Arc::clone(sup), layout.clone());
    let _ = std::thread::Builder::new()
        .name("party-mmaps".into())
        .spawn(move || {
            if let Err(err) = build_mmaps(&sup, &layout) {
                if !sup.cancelled() {
                    warn!("party: navmesh build stopped: {err:#}");
                }
            }
        });
}

fn build_mmaps(sup: &Supervisor, layout: &Layout) -> anyhow::Result<()> {
    let data = layout.data();
    std::fs::create_dir_all(data.join("mmaps"))?;
    let mut maps: Vec<u32> = std::fs::read_dir(data.join("maps"))?
        .filter_map(|e| e.ok()?.file_name().to_str()?.get(..3)?.parse().ok())
        .collect();
    maps.sort_unstable();
    maps.dedup();
    let threads = std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).max(1));
    let start = Instant::now();
    for map in &maps {
        let marker = format!("mmaps-{map:03}");
        if layout.marker(&marker).exists() {
            continue;
        }
        let mut cmd = tool(layout, "MoveMapGenerator.exe", &data);
        cmd.arg(map.to_string())
            .args(["--threads", &threads.to_string(), "--silent"]);
        procs::run_tool(&sup.job, &mut cmd, BELOW_NORMAL, &sup.cancel, |_| {})?;
        layout.mark(&marker)?;
    }
    layout.mark("mmaps")?;
    info!(
        "party: navmeshes built in {:.0}s; the server uses them from the next launch",
        start.elapsed().as_secs_f32()
    );
    Ok(())
}
