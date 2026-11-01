//! The embedded server: MariaDB, realmd and mangosd (vmangos), set up on the first launch under
//! `benilla-config/party-server/`, started hidden on every launch, bound to 127.0.0.1, and stopped
//! when the game exits. A supervisor thread does the work and then drives the party ([`party`]);
//! the app reads its [`Status`] for the overlay and carries its asks to the session.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bevy::prelude::*;

mod db;
mod fetch;
mod layout;
mod party;
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
    /// The server is up; the party plays.
    Ready,
    Failed(String),
}

/// A unit the client streams, as the party controller samples it.
#[derive(Clone, Debug)]
pub struct UnitSnap {
    pub guid: u64,
    pub entry: u32,
    /// Bevy coordinates.
    pub pos: Vec3,
    pub health: u32,
    pub is_self: bool,
}

/// Owns the server's processes for the life of the game, and is the party controller's link to
/// the app.
pub struct Supervisor {
    launched: Instant,
    status: Mutex<Status>,
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
    /// The overlay covers the client: during setup, the first entry and a switch.
    cover: AtomicBool,
    /// Asks for the session, sent by the app on its next frame.
    outbox: Mutex<Vec<benilla_app::SessionRequest>>,
    /// Our character is in the world, as the app last saw it.
    in_world: AtomicBool,
    /// When the switch key went down and its swap was sent, not yet seen through.
    switch_key: Mutex<Option<Instant>>,
    /// The member the switch key swaps to, kept by the controller; the key sends its swap at once.
    next_swap: Mutex<Option<String>>,
    /// A press of the switch key the checks made, which the next frame takes as the key.
    virtual_key: AtomicBool,
    /// The guid of the character the client plays, 0 out of the world.
    self_guid: AtomicU64,
    /// A walk forward the checks asked for: how long to hold the key, not yet begun.
    walk: Mutex<Option<Duration>>,
    /// The jump key's press, for the checks.
    jump: AtomicBool,
    /// The units the client streams, sampled by the app.
    units: Mutex<Vec<UnitSnap>>,
}

impl Supervisor {
    pub fn new(launched: Instant) -> Self {
        Self {
            launched,
            status: Mutex::default(),
            running: Mutex::default(),
            layout: Mutex::default(),
            cancel: AtomicBool::new(false),
            job: procs::Job::new(),
            timings: Mutex::default(),
            cold: AtomicBool::new(false),
            cover: AtomicBool::new(true),
            outbox: Mutex::default(),
            in_world: AtomicBool::new(false),
            switch_key: Mutex::default(),
            next_swap: Mutex::default(),
            virtual_key: AtomicBool::new(false),
            self_guid: AtomicU64::new(0),
            walk: Mutex::default(),
            jump: AtomicBool::new(false),
            units: Mutex::default(),
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

    /// Queues an ask for the session.
    fn ask(&self, request: benilla_app::SessionRequest) {
        self.outbox.lock().unwrap().push(request);
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

    /// The supervisor thread's body: recover, set up, start, then play the party.
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
        self.timed("accounts", || setup::accounts(self, layout, &db, &secrets))?;
        setup::start_mmaps(self, layout);
        info!(
            "party: server ready {:.1}s after launch ({})",
            self.launched.elapsed().as_secs_f32(),
            if cold { "cold" } else { "warm" }
        );
        party::run(self, layout, &db, &secrets)
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

/// Starts the supervisor thread, carries the party controller's asks to the session, feeds it
/// what the client sees, and reports the startup time once our character is in the world.
pub struct ServerPlugin(pub Arc<Supervisor>);

#[derive(Resource, Clone)]
pub struct Server(pub Arc<Supervisor>);

impl Plugin for ServerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(Server(Arc::clone(&self.0)))
            .add_systems(Startup, start)
            .add_systems(Update, (bridge, sample_units, report_in_world))
            .add_systems(First, walk);
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
                    error!("party: {err:#}");
                    supervisor.set(Phase::Failed(format!("{err:#}")), "");
                    supervisor.cover.store(true, Ordering::Relaxed);
                }
            }
        })
        .expect("spawn party-server thread");
}

/// The switch key.
const SWITCH_KEY: KeyCode = KeyCode::F6;

/// Sends the controller's asks, and tells it which character we play and when the switch key went
/// down.
fn bridge(
    server: Res<Server>,
    objects: benilla_app::Objects,
    keys: Res<ButtonInput<KeyCode>>,
    mut session: MessageWriter<benilla_app::SessionRequest>,
) {
    let sup = &server.0;
    for ask in sup.outbox.lock().unwrap().drain(..) {
        session.write(ask);
    }
    let me = objects.player();
    sup.in_world.store(me.is_some(), Ordering::Relaxed);
    let guid = me
        .and_then(|me| objects.iter().find(|(_, e, _)| *e == me).map(|(g, ..)| g))
        .unwrap_or(0);
    sup.self_guid.store(guid, Ordering::Relaxed);
    let pressed = keys.just_pressed(SWITCH_KEY) || sup.virtual_key.swap(false, Ordering::Relaxed);
    // The swap goes out in the key's own frame, in party chat (`SessionRequest::PartyCommand`),
    // which the server takes sooner than a say; the controller then sees it through.
    if pressed {
        if let Some(name) = sup.next_swap.lock().unwrap().clone() {
            // The client alone knows a fall: the server holds the old character for its landing.
            let air = if benilla_app::mid_air() { " air" } else { "" };
            session.write(benilla_app::SessionRequest::PartyCommand(format!(
                ".party swap {name}{air}"
            )));
            *sup.switch_key.lock().unwrap() = Some(Instant::now());
        }
    }
}

impl Supervisor {
    /// Presses the switch key from the checks: the next frame takes it as the key.
    fn press_switch(&self) {
        self.virtual_key.store(true, Ordering::Relaxed);
    }
}

/// Every quarter second, the streamed units' positions and health, for the controller's checks.
fn sample_units(
    server: Res<Server>,
    objects: benilla_app::Objects,
    transforms: Query<&GlobalTransform>,
    time: Res<Time<Real>>,
    mut next: Local<f32>,
) {
    let now = time.elapsed_secs();
    if now < *next {
        return;
    }
    // The fall checks read a body's arc, a frame at a time.
    static EVERY_FRAME: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let every_frame = *EVERY_FRAME.get_or_init(|| std::env::var_os("PARTY_FALL_TEST").is_some());
    *next = now + if every_frame { 0.0 } else { 0.25 };
    let me = objects.player();
    const TYPEMASK_UNIT: u32 = 0x8;
    let units = objects
        .iter()
        .filter(|(_, _, f)| f.object_type_mask().unwrap_or(0) & TYPEMASK_UNIT != 0)
        .filter_map(|(guid, entity, f)| {
            Some(UnitSnap {
                guid,
                entry: f.object_entry().unwrap_or(0),
                pos: transforms.get(entity).ok()?.translation(),
                health: f.unit_health().unwrap_or(0),
                is_self: Some(entity) == me,
            })
        })
        .collect();
    *server.0.units.lock().unwrap() = units;
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

/// Whether the overlay covers the client.
pub fn covering(status: &Status, server: &Server) -> bool {
    status.phase != Phase::Ready || server.0.cover.load(Ordering::Relaxed)
}

/// The movement key the checks hold, as a player would: the binding's own path.
const WALK_KEY: KeyCode = KeyCode::KeyW;

impl Supervisor {
    /// Taps the jump key, from the next frame.
    pub(super) fn jump(&self) {
        self.jump.store(true, Ordering::Relaxed);
    }

    /// Holds the movement key for `hold`, from the next frame.
    pub(super) fn walk(&self, hold: Duration) {
        *self.walk.lock().unwrap() = Some(hold);
    }
}

/// Presses and releases [`WALK_KEY`] for a walk asked for, as the keyboard does, at the frame's
/// start: the keyboard message Bevy's input pass turns into the key's state, and the window event
/// benilla's bindings read in order.
fn walk(
    server: Res<Server>,
    mut input: (
        MessageWriter<bevy::input::keyboard::KeyboardInput>,
        MessageWriter<bevy::window::WindowEvent>,
    ),
    mut until: Local<Option<Instant>>,
    mut jumping: Local<bool>,
) {
    let mut send = |key_code: KeyCode, state: bevy::input::ButtonState| {
        let ev = bevy::input::keyboard::KeyboardInput {
            key_code,
            logical_key: bevy::input::keyboard::Key::Unidentified(
                bevy::input::keyboard::NativeKey::Unidentified,
            ),
            state,
            text: None,
            repeat: false,
            window: Entity::PLACEHOLDER,
        };
        input.0.write(ev.clone());
        input.1.write(bevy::window::WindowEvent::KeyboardInput(ev));
    };
    if std::mem::take(&mut *jumping) {
        send(KeyCode::Space, bevy::input::ButtonState::Released);
    } else if server.0.jump.swap(false, Ordering::Relaxed) {
        send(KeyCode::Space, bevy::input::ButtonState::Pressed);
        *jumping = true;
    }
    if let Some(hold) = server.0.walk.lock().unwrap().take() {
        info!("party: walk key down for {hold:?}");
        send(WALK_KEY, bevy::input::ButtonState::Pressed);
        *until = Some(Instant::now() + hold);
    } else if until.is_some_and(|t| Instant::now() >= t) {
        send(WALK_KEY, bevy::input::ButtonState::Released);
        *until = None;
    }
}
