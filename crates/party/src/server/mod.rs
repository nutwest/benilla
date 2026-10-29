//! The embedded server: MariaDB, realmd and mangosd (vmangos), set up on the first launch under
//! `benilla-config/party-server/`, started hidden on every launch, bound to 127.0.0.1, and stopped
//! when the game exits. A supervisor thread does the work; the app reads its [`Status`] for the
//! overlay and logs in through [`benilla_app::AutoLogin`] once the world port answers.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::prelude::*;

mod db;
mod fetch;
mod layout;
mod procs;
mod setup;

use layout::Layout;

/// The realmd address the client dials, and the realm it takes off the list.
pub const REALMD_ADDR: &str = "127.0.0.1:13724";
pub const REALM_NAME: &str = "Party";

/// Private ports, so the embedded server never meets another one on this machine.
const DB_PORT: u16 = 13306;
const REALMD_PORT: u16 = 13724;
const WORLD_PORT: u16 = 18085;

/// What the supervisor is doing, for the overlay.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub phase: Phase,
    /// The step under way, e.g. "Extracting maps".
    pub step: String,
    /// A line under it: a file, a count.
    pub detail: String,
    /// Done fraction of the step, when it is known.
    pub fraction: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Phase {
    #[default]
    Starting,
    /// First-run setup.
    Setup,
    /// Starting the processes.
    Booting,
    /// The world port answers; the client logs in.
    Ready,
    Failed(String),
}

/// The account the client logs in with and the character it enters, once the server is up.
#[derive(Clone, Debug)]
pub struct Login {
    pub user: String,
    pub pass: String,
    /// The most recently played character, `None` on a fresh account.
    pub character: Option<String>,
}

/// Owns the server's processes for the life of the game.
pub struct Supervisor {
    launched: Instant,
    status: Mutex<Status>,
    login: Mutex<Option<Login>>,
    running: Mutex<Option<procs::Running>>,
    layout: Mutex<Option<Layout>>,
    /// The game is exiting: setup stops at its next check.
    cancel: AtomicBool,
    /// Takes every process we start down with the game, should it die without [`Self::shutdown`].
    job: procs::Job,
    /// Step timings of this launch, for the startup report.
    timings: Mutex<Vec<(String, Duration)>>,
    /// Whether this launch ran first-run setup.
    cold: AtomicBool,
}

impl Supervisor {
    pub fn new(launched: Instant) -> Self {
        Self {
            launched,
            status: Mutex::default(),
            login: Mutex::default(),
            running: Mutex::default(),
            layout: Mutex::default(),
            cancel: AtomicBool::new(false),
            job: procs::Job::new(),
            timings: Mutex::default(),
            cold: AtomicBool::new(false),
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().unwrap().clone()
    }

    fn set(&self, phase: Phase, step: &str) {
        let mut s = self.status.lock().unwrap();
        s.phase = phase;
        s.step = step.to_string();
        s.detail.clear();
        s.fraction = None;
    }

    fn progress(&self, detail: impl Into<String>, fraction: Option<f32>) {
        let mut s = self.status.lock().unwrap();
        s.detail = detail.into();
        s.fraction = fraction;
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    /// Runs `f` as one timed step.
    fn timed<T>(&self, name: &str, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
        let start = Instant::now();
        let out = f()?;
        let took = start.elapsed();
        info!("party: {name} took {:.1}s", took.as_secs_f32());
        self.timings.lock().unwrap().push((name.to_string(), took));
        Ok(out)
    }

    /// The supervisor thread's body: recover, set up, start, and hand the client its login.
    fn run(self: &Arc<Self>, layout: &Layout) -> anyhow::Result<()> {
        let recovered = procs::recover_stale(layout)?;
        if recovered {
            warn!("party: the last session did not shut down cleanly; recovering");
        }
        let cold = !layout.marker("account").exists();
        self.cold.store(cold, Ordering::Relaxed);
        setup::files(self, layout)?;
        let secrets = setup::Secrets::load_or_create(layout)?;
        setup::write_configs(layout, &secrets)?;

        self.set(Phase::Booting, "Starting the database");
        let mariadb = self.timed("start database", || {
            procs::start_mariadb(&self.job, layout, &self.cancel)
        })?;
        let db = db::Db::new(layout, &secrets);
        let running = procs::Running::with_db(mariadb);
        procs::write_run_file(layout, &running)?;
        *self.running.lock().unwrap() = Some(running);
        setup::database(self, layout, &db, recovered)?;

        self.set(Phase::Booting, "Starting the world server");
        let (realmd, mangosd) = self.timed("start servers", || {
            procs::start_servers(&self.job, layout, &self.cancel)
        })?;
        if let Some(running) = self.running.lock().unwrap().as_mut() {
            running.add_servers(realmd, mangosd);
            procs::write_run_file(layout, running)?;
        }
        self.timed("account", || setup::account(self, layout, &db, &secrets))?;
        setup::start_mmaps(self, layout);

        let character = db.last_character(&secrets.account)?;
        *self.login.lock().unwrap() = Some(Login {
            user: secrets.account.clone(),
            pass: secrets.account_pass.clone(),
            character,
        });
        self.set(Phase::Ready, "Entering the world");
        info!(
            "party: server ready {:.1}s after launch ({})",
            self.launched.elapsed().as_secs_f32(),
            if cold { "cold" } else { "warm" }
        );
        Ok(())
    }

    /// Stops the server cleanly: mangosd saves and exits from its console, realmd stops, then
    /// MariaDB shuts down. Each waits a bounded time, then is killed.
    pub fn shutdown(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        let Some(layout) = self.layout.lock().unwrap().clone() else {
            return;
        };
        let Some(running) = self.running.lock().unwrap().take() else {
            return;
        };
        let start = Instant::now();
        let secrets = setup::Secrets::load_or_create(&layout).ok();
        running.stop(&layout, secrets.as_ref());
        procs::remove_run_file(&layout);
        info!(
            "party: server stopped in {:.1}s",
            start.elapsed().as_secs_f32()
        );
    }
}

/// Starts the supervisor thread, logs the client in when the server is ready, and reports the
/// startup time once our character is in the world.
pub struct ServerPlugin(pub Arc<Supervisor>);

#[derive(Resource, Clone)]
pub struct Server(pub Arc<Supervisor>);

impl Plugin for ServerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Server(Arc::clone(&self.0)))
            .add_systems(Startup, start)
            .add_systems(Update, (log_in, report_in_world));
    }
}

fn start(server: Res<Server>) {
    let supervisor = Arc::clone(&server.0);
    let Some(root) = benilla_app::config_dir() else {
        supervisor.set(
            Phase::Failed("this run keeps no local state (capture mode)".into()),
            "",
        );
        return;
    };
    let layout = Layout::new(root.join("party-server"));
    *supervisor.layout.lock().unwrap() = Some(layout.clone());
    std::thread::Builder::new()
        .name("party-server".into())
        .spawn(move || {
            if let Err(err) = supervisor.run(&layout) {
                if !supervisor.cancelled() {
                    error!("party: server setup failed: {err:#}");
                    supervisor.set(Phase::Failed(format!("{err:#}")), "");
                }
            }
        })
        .expect("spawn party-server thread");
}

/// Sends the login once, naming the character to enter from the first roster.
fn log_in(
    server: Res<Server>,
    mut sent: Local<bool>,
    mut login: MessageWriter<benilla_app::AutoLogin>,
) {
    if *sent {
        return;
    }
    let Some(creds) = server.0.login.lock().unwrap().clone() else {
        return;
    };
    *sent = true;
    login.write(benilla_app::AutoLogin {
        user: creds.user,
        pass: creds.pass,
        character: creds.character,
    });
}

/// The startup report: launch to our character in the world, with the step timings.
fn report_in_world(server: Res<Server>, objects: benilla_app::Objects, mut done: Local<bool>) {
    if *done || objects.player().is_none() {
        return;
    }
    *done = true;
    let sup = &server.0;
    let total = sup.launched.elapsed();
    let cold = sup.cold.load(Ordering::Relaxed);
    let steps: Vec<String> = sup
        .timings
        .lock()
        .unwrap()
        .iter()
        .map(|(name, d)| format!("{name} {:.1}s", d.as_secs_f32()))
        .collect();
    let line = format!(
        "{} start: in the world {:.1}s after launch [{}]",
        if cold { "cold" } else { "warm" },
        total.as_secs_f32(),
        steps.join(", ")
    );
    info!("party: {line}");
    if let Some(layout) = sup.layout.lock().unwrap().as_ref() {
        layout.append_report(&line);
    }
}

/// Whether the overlay should cover the client: until our character is in the world, or, on an
/// account with no character, until the login goes out, so the create screen shows.
pub fn covering(status: &Status, in_world: bool, server: &Server) -> bool {
    if in_world {
        return false;
    }
    match status.phase {
        Phase::Ready => server
            .0
            .login
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|l| l.character.is_some()),
        _ => true,
    }
}
