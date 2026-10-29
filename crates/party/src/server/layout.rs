//! Where the embedded server lives: one folder, `benilla-config/party-server/`, with the
//! downloads, the unpacked binaries, the extracted data, the database, logs and our own state.

use std::io::Write as _;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Layout {
    root: PathBuf,
}

impl Layout {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Archives as downloaded, kept so a re-run of setup does not fetch them again.
    pub fn downloads(&self) -> PathBuf {
        self.root.join("downloads")
    }

    pub fn mariadb(&self) -> PathBuf {
        self.root.join("mariadb")
    }

    pub fn mariadb_bin(&self, exe: &str) -> PathBuf {
        self.mariadb().join("bin").join(exe)
    }

    /// The vmangos build: mangosd, realmd, the extractors and their configs.
    pub fn vmangos(&self) -> PathBuf {
        self.root.join("vmangos")
    }

    pub fn vmangos_bin(&self, exe: &str) -> PathBuf {
        self.vmangos().join(exe)
    }

    /// The database dumps of the snapshot.
    pub fn sql(&self) -> PathBuf {
        self.root.join("sql")
    }

    /// mangosd's `DataDir`: maps, vmaps, mmaps, and the DBCs under `5875/dbc`.
    pub fn data(&self) -> PathBuf {
        self.root.join("data")
    }

    /// MariaDB's data directory.
    pub fn db(&self) -> PathBuf {
        self.root.join("db")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// Our own state: step markers, secrets, the run file.
    pub fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    /// A first-run step's done marker.
    pub fn marker(&self, step: &str) -> PathBuf {
        self.state().join(format!("{step}.done"))
    }

    pub fn mark(&self, step: &str) -> std::io::Result<()> {
        std::fs::write(self.marker(step), b"")
    }

    /// The PIDs of the processes this launch started; present only while they run.
    pub fn run_file(&self) -> PathBuf {
        self.state().join("running.txt")
    }

    pub fn make_dirs(&self) -> std::io::Result<()> {
        for dir in [self.downloads(), self.data(), self.logs(), self.state()] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }

    /// Appends a line to the startup report, `logs/startup.txt`.
    pub fn append_report(&self, line: &str) {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.logs().join("startup.txt"));
        if let Ok(mut file) = file {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// A path as the server's config files want it: forward slashes.
pub fn forward(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}
