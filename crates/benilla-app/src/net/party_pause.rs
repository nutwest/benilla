//! Deviation: 1.12 has no pause. The party's vmangos fork (`.party pause`) stops its world and says
//! so in `SMSG_PARTY_PAUSE`, and the client's world stops on that notice, so the two hold nearly
//! the same state: what the server sent before it pausing lands first. The interface, the camera
//! and sound run on.
//!
//! The pause is a consumer's to offer ([`enable_world_pause`]): it adds a bindable command, and
//! without it nothing here runs. The key asks the server, whose answer is what freezes us.

use std::sync::atomic::{AtomicBool, Ordering};

use benilla_protocol::{SessionEvent, SessionEventKind};
use benilla_ui::bindings_xml::Binding;
use benilla_ui::script::UiScript;
use benilla_world::world_clock;
use bevy::prelude::*;

use super::{NetHandlerApp, NetStatus, SelfGuid};
use crate::bindings::{BindingsState, Input};
use crate::glue_session::requests::SessionRequest;

/// The command, its section and their window labels.
const COMMAND: &str = "WORLDPAUSE";
const HEADER: &str = "PARTY";
const COMMAND_LABEL: &str = "Pause the world";
const HEADER_LABEL: &str = "Party";
/// The command's key until the player rebinds it.
const DEFAULT_KEY: &str = "F7";

static OFFERED: AtomicBool = AtomicBool::new(false);

/// Offers the world pause: a Key Bindings row, default F7, that asks the fork's `.party pause`.
/// Call it before the app runs, on a server that has the command.
pub fn enable_world_pause() {
    OFFERED.store(true, Ordering::Relaxed);
}

/// Whether the world is paused, as of the server's last notice.
pub fn world_paused() -> bool {
    world_clock::is_paused()
}

/// The command's row of the key table's defaults, when offered.
pub(crate) fn default_binding() -> Option<benilla_ui::script::keybind::KeyBinding> {
    OFFERED
        .load(Ordering::Relaxed)
        .then(|| (DEFAULT_KEY.to_string(), COMMAND.to_string()))
}

/// Registers the command beside the core's, when offered: a row of the Key Bindings window, whose
/// labels are the `BINDING_*` globals.
pub(crate) fn register_binding(script: &UiScript) {
    if !OFFERED.load(Ordering::Relaxed) {
        return;
    }
    script.register_bindings(&[Binding {
        name: COMMAND.to_string(),
        header: Some(HEADER.to_string()),
        run_on_up: false,
        hidden: false,
        platform: None,
        body: "ToggleWorldPause()".to_string(),
    }]);
    let globals = script.lua().globals();
    let labels = [
        (format!("BINDING_NAME_{COMMAND}"), COMMAND_LABEL),
        (format!("BINDING_HEADER_{HEADER}"), HEADER_LABEL),
    ];
    for (name, label) in labels {
        if let Err(e) = globals.set(name, label) {
            error!("net: the pause binding's label: {e}");
        }
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
fn ask_on_key(binds: Res<BindingsState>, mut session: MessageWriter<SessionRequest>) {
    if binds.fired(Input::WorldPause) {
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
