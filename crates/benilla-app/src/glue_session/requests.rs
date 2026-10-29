//! What a crate on top of benilla asks of the session besides a login ([`super::login::AutoLogin`]):
//! a chat line (a `.` server command included), a target, a relog as another account or character,
//! and a character created at select. Each goes through the paths the screens use.

use bevy::prelude::*;

use super::login::AutoLogin;
use crate::client_state::ClientState;
use crate::net::{
    CharListMessage, CharPick, CharRequest, ChatKind, ClientCommand, LoggedOutMessage, NetCommands,
};

/// A crate's ask of the session ([`crate::run_with`]).
#[derive(Message, Debug, Clone)]
pub enum SessionRequest {
    /// Says `line` in the world, as typed into the chat box; a `.` line is a server command.
    Command(String),
    /// Selects the unit `guid` (`CMSG_SET_SELECTION`), as a click does; 0 clears.
    Target(u64),
    /// Logs out (`CMSG_LOGOUT_REQUEST`), leaves character select for the login park as select's
    /// Back does, then logs in as the [`AutoLogin`] says. At select it skips the logout; at the
    /// login screen it only logs in.
    Relog(AutoLogin),
    /// Creates a character (`CMSG_CHAR_CREATE`) with the default appearance, once the session is
    /// at character select with no relog under way.
    CreateCharacter {
        name: String,
        race: u8,
        class: u8,
        gender: u8,
    },
}

/// A relog under way and creates waiting for select.
#[derive(Resource, Default)]
pub(crate) struct Relog {
    login: Option<AutoLogin>,
    stage: RelogStage,
    creates: Vec<benilla_protocol::CharCreateReq>,
    /// A roster of a session newer than the last relog arrived: the IO thread is parked at select
    /// and takes a create. The client state reaches select before the roster does, and the logged
    /// out session's own roster can arrive after the relog.
    roster: bool,
    /// The newest world session a roster came from, and the newest at the last relog.
    last_epoch: u64,
    relog_epoch: u64,
    /// After a logout, the select screen asks for the roster (`GetCharacterListUpdate`); Back
    /// waits for that answer, until this deadline, so it cannot land after the next login and
    /// put the screen back at select. `None` once it came.
    select_roster_until: Option<f32>,
}

/// How long Back waits for the select screen's roster after a logout.
const SELECT_ROSTER_WAIT: f32 = 2.0;

#[derive(Default, PartialEq, Eq, Debug)]
enum RelogStage {
    #[default]
    Idle,
    /// The logout request is out; `SMSG_LOGOUT_COMPLETE` brings us to select.
    LoggingOut,
    /// At select: Back to the login park.
    AtSelect,
    /// Back sent; the login goes once the client is at the login park.
    Disconnecting,
}

/// Turns the crate's asks into the session's own commands, and walks a relog out of the world.
/// Before the login policy, so the Back and the login it writes are read in order.
pub(crate) fn drive_requests(
    mut asks: MessageReader<SessionRequest>,
    mut logged_out: MessageReader<LoggedOutMessage>,
    mut rosters: MessageReader<CharListMessage>,
    mut relog: ResMut<Relog>,
    state: Res<State<ClientState>>,
    net: Option<Res<NetCommands>>,
    pick: Option<Res<CharPick>>,
    mut glue: MessageWriter<super::login::GlueLogin>,
    mut login: MessageWriter<AutoLogin>,
    time: Res<Time<Real>>,
) {
    let now = time.elapsed_secs();
    let state = *state.get();
    let send = |command: ClientCommand| {
        if let Some(net) = &net {
            let _ = net.0.send(command);
        }
    };
    for ask in asks.read() {
        match ask {
            SessionRequest::Command(line) => send(ClientCommand::Chat {
                kind: ChatKind::Say,
                target: None,
                text: line.clone(),
                language: None,
            }),
            SessionRequest::Target(guid) => send(ClientCommand::SetSelection { guid: *guid }),
            SessionRequest::Relog(next) => {
                relog.login = Some(next.clone());
                relog.roster = false;
                relog.relog_epoch = relog.last_epoch;
                relog.stage = match state {
                    ClientState::InWorld => {
                        send(ClientCommand::Logout);
                        RelogStage::LoggingOut
                    }
                    ClientState::CharSelect => RelogStage::AtSelect,
                    ClientState::Login => RelogStage::Disconnecting,
                };
                info!("session: relog as {} requested", next.user);
            }
            SessionRequest::CreateCharacter {
                name,
                race,
                class,
                gender,
            } => relog.creates.push(benilla_protocol::CharCreateReq {
                name: name.clone(),
                race: *race,
                class: *class,
                gender: *gender,
                skin: 0,
                face: 0,
                hair_style: 0,
                hair_color: 0,
                facial_hair: 0,
            }),
        }
    }
    if logged_out.read().count() > 0 && relog.stage == RelogStage::LoggingOut {
        info!("session: logged out");
        relog.stage = RelogStage::AtSelect;
        relog.select_roster_until = Some(now + SELECT_ROSTER_WAIT);
    }
    for roster in rosters.read() {
        relog.last_epoch = relog.last_epoch.max(roster.epoch);
        if roster.epoch > relog.relog_epoch {
            relog.roster = true;
        }
        if relog.stage == RelogStage::AtSelect {
            relog.select_roster_until = None;
        }
    }
    let roster_settled = relog.select_roster_until.is_none_or(|until| now >= until);
    match relog.stage {
        RelogStage::AtSelect if state == ClientState::CharSelect && roster_settled => {
            relog.select_roster_until = None;
            glue.write(super::login::GlueLogin::Disconnect);
            relog.stage = RelogStage::Disconnecting;
        }
        RelogStage::Disconnecting if state == ClientState::Login => {
            if let Some(next) = relog.login.take() {
                login.write(next);
            }
            relog.stage = RelogStage::Idle;
        }
        RelogStage::Idle
            if state == ClientState::CharSelect && relog.roster && !relog.creates.is_empty() =>
        {
            if let Some(pick) = &pick {
                for req in relog.creates.drain(..) {
                    info!("session: creating {}", req.name);
                    let _ = pick.0.send(CharRequest::Create(req));
                }
            }
        }
        _ => {}
    }
}
