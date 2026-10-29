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

    /// The account's most recently played character.
    pub fn last_character(&self, account: &str) -> anyhow::Result<Option<String>> {
        let rows = self.query(
            None,
            &format!(
                "SELECT c.name FROM characters.characters c JOIN realmd.account a ON a.id = c.account \
                 WHERE a.username = '{}' ORDER BY c.logout_time DESC, c.guid DESC LIMIT 1",
                sql_word(account)
            ),
        )?;
        Ok(rows
            .lines()
            .next()
            .map(str::to_string)
            .filter(|s| !s.is_empty()))
    }
}

/// `s` kept to the characters an account or password of ours uses, so it can sit in a quoted
/// literal.
pub fn sql_word(s: &str) -> String {
    s.chars().filter(char::is_ascii_alphanumeric).collect()
}
