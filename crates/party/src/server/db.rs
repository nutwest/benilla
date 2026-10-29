//! The database through MariaDB's own client: one statement batch or one dump file per call, as
//! root over TCP on the private port. The password travels in `MYSQL_PWD`, never on a command line.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context as _};

use super::layout::Layout;
use super::procs::hidden;
use super::setup::Secrets;
use super::DB_PORT;

pub struct Db {
    client: PathBuf,
    check: PathBuf,
    root_pass: String,
}

impl Db {
    pub fn new(layout: &Layout, secrets: &Secrets) -> Self {
        Self {
            client: layout.mariadb_bin("mariadb.exe"),
            check: layout.mariadb_bin("mariadb-check.exe"),
            root_pass: secrets.root_pass.clone(),
        }
    }

    fn command(&self, exe: &Path) -> Command {
        let mut cmd = Command::new(exe);
        cmd.args(["--protocol=TCP", "-h127.0.0.1", "-uroot", "--skip-ssl"])
            .arg(format!("-P{DB_PORT}"))
            .env("MYSQL_PWD", &self.root_pass);
        hidden(&mut cmd, 0);
        cmd
    }

    /// Runs `sql` (one or more statements) against `database`, returning the rows tab-separated.
    pub fn query(&self, database: Option<&str>, sql: &str) -> anyhow::Result<String> {
        let mut cmd = self.command(&self.client);
        cmd.args(["--batch", "--skip-column-names", "-e", sql]);
        if let Some(db) = database {
            cmd.arg(db);
        }
        let out = cmd
            .stdin(Stdio::null())
            .output()
            .context("running the database client")?;
        if !out.status.success() {
            bail!("database: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Feeds a dump file into `database`.
    pub fn import(&self, database: &str, file: &Path) -> anyhow::Result<()> {
        let input =
            std::fs::File::open(file).with_context(|| format!("opening {}", file.display()))?;
        let out = self
            .command(&self.client)
            .arg(database)
            .stdin(input)
            .output()
            .context("running the database client")?;
        if !out.status.success() {
            bail!(
                "importing {}: {}",
                file.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Checks and repairs the MyISAM tables a hard stop may have left open.
    pub fn repair(&self, databases: &[&str]) -> anyhow::Result<()> {
        let out = self
            .command(&self.check)
            .args([
                "--auto-repair",
                "--check-only-changed",
                "--silent",
                "--databases",
            ])
            .args(databases)
            .stdin(Stdio::null())
            .output()?;
        if !out.status.success() {
            bail!(
                "table check: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Of `names`, the character played most recently.
    pub fn last_played(&self, names: &[&str]) -> anyhow::Result<Option<String>> {
        let list: Vec<String> = names.iter().map(|n| format!("'{}'", sql_word(n))).collect();
        let rows = self.query(
            Some("characters"),
            &format!(
                "SELECT name FROM characters WHERE name IN ({}) \
                 ORDER BY logout_time DESC, guid DESC LIMIT 1",
                list.join(",")
            ),
        )?;
        Ok(rows
            .lines()
            .next()
            .map(str::to_string)
            .filter(|s| !s.is_empty()))
    }

    /// A character's saved row: what the server last wrote of it.
    pub fn character(&self, name: &str) -> anyhow::Result<Option<CharRow>> {
        let rows = self.query(
            Some("characters"),
            &format!(
                "SELECT c.guid, c.online, c.level, c.xp, c.money, c.map, c.position_x, \
                 c.position_y, c.position_z, \
                 (SELECT COUNT(*) FROM character_inventory i WHERE i.guid = c.guid) \
                 FROM characters c WHERE c.name = '{}'",
                sql_word(name)
            ),
        )?;
        let Some(line) = rows.lines().next() else {
            return Ok(None);
        };
        let f: Vec<&str> = line.split('\t').collect();
        let num = |i: usize| -> anyhow::Result<f64> {
            f.get(i)
                .context("short row")?
                .parse()
                .with_context(|| format!("field {i} of {name}'s row"))
        };
        Ok(Some(CharRow {
            guid: num(0)? as u64,
            online: num(1)? != 0.0,
            level: num(2)? as u32,
            xp: num(3)? as u32,
            money: num(4)? as u32,
            map: num(5)? as u32,
            pos: [num(6)? as f32, num(7)? as f32, num(8)? as f32],
            items: num(9)? as u32,
        }))
    }
}

/// A character as the database holds it.
#[derive(Clone, Debug)]
pub struct CharRow {
    pub guid: u64,
    pub online: bool,
    pub level: u32,
    pub xp: u32,
    pub money: u32,
    pub map: u32,
    /// WoW coordinates.
    pub pos: [f32; 3],
    /// Rows in `character_inventory`: equipped, bagged and banked items.
    pub items: u32,
}

/// `s` kept to the characters an account or password of ours uses, so it can sit in a quoted
/// literal.
pub fn sql_word(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).collect()
}
