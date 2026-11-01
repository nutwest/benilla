//! Deviation: 1.12 has no pause. The party's vmangos fork (`.party pause`) stops its world and says
//! so in `SMSG_PARTY_PAUSE`, and the client's world stops on that notice, so the two hold nearly
//! the same state: what the server sent before it pausing lands first. The interface, the camera
//! and sound run on.
//!
//! The pause is a consumer's to offer ([`enable_world_pause`]): it adds a bindable command, and
//! without it nothing here runs but the notice. The key asks the server, whose answer is what
//! freezes us.

use std::sync::atomic::{AtomicBool, Ordering};

use benilla_protocol::{SessionEvent, SessionEventKind};
use benilla_ui::bindings_xml::Binding;
use benilla_world::world_clock;
use bevy::prelude::*;

use super::{NetHandlerApp, NetStatus, SelfGuid};
use crate::glue_session::requests::SessionRequest;

/// The commands, their section and their window labels.
const COMMAND: &str = "WORLDPAUSE";
const CAMERA_COMMAND: &str = "PARTYCAMERA";
const HEADER: &str = "PARTY";
const COMMAND_LABEL: &str = "Pause the world";
const CAMERA_LABEL: &str = "Toggle the party camera";
const HEADER_LABEL: &str = "Party";
/// The commands' keys until the player rebinds them.
const DEFAULT_KEY: &str = "F7";
const CAMERA_KEY: &str = "F8";

static OFFERED: AtomicBool = AtomicBool::new(false);
/// The pause command's press, for the next [`ask_on_key`].
static PRESSED: AtomicBool = AtomicBool::new(false);
/// The camera command's press, for [`take_camera_toggle`].
static CAMERA_PRESSED: AtomicBool = AtomicBool::new(false);

/// Offers the world pause: a Key Bindings row, default F7, that asks the fork's `.party pause`,
/// and the party camera's row, default F8, which [`take_camera_toggle`] hands to the consumer.
/// Call it before the app runs, on a server that has the command.
pub fn enable_world_pause() {
    OFFERED.store(true, Ordering::Relaxed);
}

/// Whether the world is paused, as of the server's last notice.
pub fn world_paused() -> bool {
    world_clock::is_paused()
}

/// Whether the party camera's key went down since the last call.
pub fn take_camera_toggle() -> bool {
    CAMERA_PRESSED.swap(false, Ordering::Relaxed)
}

/// The commands' rows of the key table's defaults, when offered.
pub(crate) fn default_bindings() -> Vec<benilla_ui::script::keybind::KeyBinding> {
    if !OFFERED.load(Ordering::Relaxed) {
        return Vec::new();
    }
    [(DEFAULT_KEY, COMMAND), (CAMERA_KEY, CAMERA_COMMAND)]
        .map(|(key, command)| (key.to_string(), command.to_string()))
        .into()
}

/// The commands' rows beside the core's, when offered, and the `BINDING_*` globals that label them
/// in the Key Bindings window. Their bodies are empty: 1.12 has no Lua call for these, and adding
/// one would show it to addons, so [`note_press`] takes the commands by name.
pub(crate) fn binding_rows() -> Option<(Vec<Binding>, Vec<(String, &'static str)>)> {
    OFFERED.load(Ordering::Relaxed).then(|| {
        let row = |name: &str, header: Option<&str>| Binding {
            name: name.to_string(),
            header: header.map(str::to_string),
            run_on_up: false,
            hidden: false,
            platform: None,
            body: String::new(),
        };
        let rows = vec![row(COMMAND, Some(HEADER)), row(CAMERA_COMMAND, None)];
        let labels = vec![
            (format!("BINDING_NAME_{COMMAND}"), COMMAND_LABEL),
            (format!("BINDING_NAME_{CAMERA_COMMAND}"), CAMERA_LABEL),
            (format!("BINDING_HEADER_{HEADER}"), HEADER_LABEL),
        ];
        (rows, labels)
    })
}

/// The binding dispatch's press of `command`: ours are noted for their readers.
pub(crate) fn note_press(command: &str) {
    match command {
        COMMAND => PRESSED.store(true, Ordering::Relaxed),
        CAMERA_COMMAND => CAMERA_PRESSED.store(true, Ordering::Relaxed),
        _ => {}
    }
}

pub(super) fn register(app: &mut App) {
    app.net_handler(SessionEventKind::PartyPause, on_party_pause);
    if OFFERED.load(Ordering::Relaxed) {
        app.add_systems(Update, (ask_on_key, thaw_without_a_session));
    }
}

fn apply(paused: bool, virt: &mut Time<Virtual>) {
    world_clock::set_paused(paused);
    if paused {
        virt.pause();
    } else {
        virt.unpause();
    }
}

/// The server's word: the world stops or starts here.
fn on_party_pause(In(ev): In<SessionEvent>, mut virt: ResMut<Time<Virtual>>) {
    if let SessionEvent::PartyPause { paused } = ev {
        info!("net: world {}", if paused { "paused" } else { "resumed" });
        apply(paused, &mut virt);
    }
}

/// The binding's press: asks for the state we are not in. Party chat, as a say would wait behind a
/// paused map on the server.
fn ask_on_key(mut session: MessageWriter<SessionRequest>) {
    if PRESSED.swap(false, Ordering::Relaxed) {
        let want = if world_paused() { "off" } else { "on" };
        session.write(SessionRequest::PartyCommand(format!(".party pause {want}")));
    }
}

/// A lost link or a logout ends the pause here, as the server lifts it with no client to hold it.
fn thaw_without_a_session(
    net: Option<Res<NetStatus>>,
    me: Option<Res<SelfGuid>>,
    mut virt: ResMut<Time<Virtual>>,
) {
    let in_world = net.is_some_and(|n| n.connected) && me.is_some_and(|m| m.0.is_some());
    if world_paused() && !in_world {
        info!("net: world resumed, out of the world");
        apply(false, &mut virt);
    }
}
