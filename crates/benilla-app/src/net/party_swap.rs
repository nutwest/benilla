//! Deviation: `SMSG_PARTY_SWAP` is not 1.12. The party's vmangos fork (`.party swap`) moves this
//! client's connection to another member of its party in place, with no logout and no world
//! entry, then sends the new character's login burst. 1.12 changes the active player only at a
//! world entry. Here the identity moves to a unit the client already streams:
//!
//! - [`SelfGuid`] and the [`SelfPlayer`] tag go to the new character's entity. Its in-place self
//!   create runs our arrival (`super::objects::arrival`), and [`crate::player`]'s embodiment
//!   re-seats the body, the camera and [`super::ActiveMover`] by guid, as possession does.
//! - The mover is claimed (`CMSG_SET_ACTIVE_MOVER`), as the arrival would.
//! - A roster row for the new character, made from its descriptor and name on the pick's model
//!   (a party member is often on another account), goes to the rebind for the player record
//!   ([`crate::ui_world_enter::PartyRow`]). The roster and the per-character files stay as the
//!   login set them: a party shares one UI configuration
//!   ([`crate::local_state::share_character_state`]).
//! - The UI rebinds by events, no reload: the new character's arrival fires the world-enter
//!   cascade, and [`crate::ui_world_enter::PartyRebindOwed`] adds the "player" unit's events.
//!
//! The per-character stores the burst refills are reset by their own modules on the same event
//! (`crate::spell`, `crate::ui_action`). The world, terrain and objects stay.

use benilla_protocol::{SessionEvent, SessionEventKind};
use bevy::prelude::*;

use super::{GuidIndex, MoverClaim, NetCommands, NetHandlerApp, ObjectStore, SelfGuid, SelfPlayer};

pub(super) fn register(app: &mut App) {
    app.init_resource::<PartyArrivalOwed>()
        .init_resource::<crate::lanes::LaneHold>()
        .net_handler(SessionEventKind::PartySwap, on_party_swap);
}

/// The new character a swap owes an arrival: its self create in the burst runs our create hook,
/// though it is shown already (`super::objects::arrival`).
#[derive(Resource, Default)]
pub(crate) struct PartyArrivalOwed(pub(crate) Option<u64>);

/// The swap's own state. The old character's entity stays, now a remote unit the server moves.
fn on_party_swap(
    In(ev): In<SessionEvent>,
    mut commands: Commands,
    mut self_guid: ResMut<SelfGuid>,
    index: Res<GuidIndex>,
    net: Res<NetCommands>,
    mover: Option<ResMut<MoverClaim>>,
    own_control: Option<ResMut<crate::player::OwnControlLost>>,
    tagged: Query<Entity, With<SelfPlayer>>,
    stores: Query<&ObjectStore>,
    names: Res<crate::names::NameCache>,
    roster: Option<Res<crate::glue_session::roster::Roster>>,
    (rebind, rebind_row): (
        Option<ResMut<crate::ui_world_enter::PartyRebindOwed>>,
        Option<ResMut<crate::ui_world_enter::PartyRow>>,
    ),
    mut arrival: ResMut<PartyArrivalOwed>,
    // The camera's glide from the old body to the new ([`crate::player::SwapBlend`]).
    mut glide: (
        Query<&Transform>,
        Option<ResMut<crate::player::SwapBlend>>,
        Res<Time<Real>>,
    ),
) {
    let SessionEvent::PartySwap { old, new } = ev else {
        return;
    };
    let Some(&body) = index.0.get(&new) else {
        // The server swaps only within the map, where the party streams; a body we do not hold
        // cannot be ours to play.
        error!("net: party swap to {new:#x}, which this client does not stream");
        return;
    };
    if self_guid.0 != Some(old) {
        warn!(
            "net: party swap from {old:#x}, but this client plays {:?}",
            self_guid.0
        );
    }
    info!("net: party swap {old:#x} -> {new:#x}");
    if benilla_assets::trace::enabled_for("pty") {
        benilla_assets::trace::line("pty", &format!("swap {old:#x} -> {new:#x}"));
    }

    self_guid.0 = Some(new);
    arrival.0 = Some(new);
    let (poses, blend, time) = (&glide.0, &mut glide.1, &glide.2);
    if let (Some(from), Ok(to), Some(blend)) = (
        tagged.iter().next().and_then(|e| poses.get(e).ok()),
        poses.get(body),
        blend.as_deref_mut(),
    ) {
        blend.start(from.translation, to.translation, time.elapsed_secs());
    }
    for e in &tagged {
        commands.entity(e).remove::<SelfPlayer>();
    }
    commands.entity(body).insert(SelfPlayer);
    if let Some(mut mover) = mover {
        mover.claim(new, &net);
    }
    // The new character is ours to move; a fear or stun on it comes back as its control update.
    if let Some(mut lost) = own_control {
        lost.0 = false;
    }

    // The new character's row, modelled on the pick's (a party member is often on another account,
    // so the login's roster has none of its own), for the rebind's player record.
    let template = roster
        .as_deref()
        .and_then(|r| r.chars.iter().find(|c| c.guid == new).or(r.pending_row()))
        .cloned();
    match template {
        Some(mut row) => {
            row.guid = new;
            if let Some(name) = names.resolve_player(new, crate::query_cache::Ask::Read) {
                row.name = name.to_string();
            }
            if let Some(f) = stores.get(body).ok().map(|s| &s.0) {
                row.race = f.unit_race().unwrap_or(row.race);
                row.class = f.unit_class().unwrap_or(row.class);
                row.gender = f.unit_gender().unwrap_or(row.gender);
                row.level = f.unit_level().map_or(row.level, |l| l.min(255) as u8);
            }
            if let Some(mut slot) = rebind_row {
                slot.0 = Some(row);
            }
        }
        None => warn!("net: party swap with no roster row to model the new character on"),
    }
    if let Some(mut rebind) = rebind {
        rebind.0 = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glue_session::roster::{test_character, Roster};
    use crate::net::{ClientCommand, Guid};
    use benilla_protocol::{messages::ObjectType, ObjectFields};

    const OLD: u64 = 3;
    const NEW: u64 = 1;

    /// Our old character tagged and played, the new one streamed as a level 4 human (race 1)
    /// warrior (class 1) woman, with a roster that has only the old one's row, as a login on the
    /// old one's account leaves it.
    fn harness() -> (
        App,
        Entity,
        Entity,
        crossbeam_channel::Receiver<ClientCommand>,
    ) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let mut app = App::new();
        app.insert_resource(NetCommands(tx))
            .insert_resource(SelfGuid(Some(OLD)))
            .init_resource::<GuidIndex>()
            .init_resource::<MoverClaim>()
            .init_resource::<crate::names::NameCache>()
            .init_resource::<crate::player::OwnControlLost>()
            .init_resource::<crate::ui_world_enter::PartyRebindOwed>()
            .init_resource::<crate::ui_world_enter::PartyRow>()
            .init_resource::<crate::player::SwapBlend>()
            .init_resource::<Time<Real>>();
        let mut roster = Roster::with_chars(vec![test_character(OLD, "Partythree")]);
        roster.pending_pick = Some(OLD);
        app.insert_resource(roster);
        register(&mut app);
        let old = app
            .world_mut()
            .spawn((Guid(OLD), SelfPlayer, Transform::from_xyz(10.0, 0.0, 0.0)))
            .id();
        let fields = ObjectFields::from_pairs(&[(34, 4), (36, 1 | (1 << 8) | (1 << 16))])
            .into_created(ObjectType::Player);
        let new = app
            .world_mut()
            .spawn((Guid(NEW), ObjectStore(fields), Transform::default()))
            .id();
        let mut index = app.world_mut().resource_mut::<GuidIndex>();
        index.0.insert(OLD, old);
        index.0.insert(NEW, new);
        app.world_mut()
            .resource_mut::<crate::names::NameCache>()
            .insert_player(NEW, "Partyone".into(), None);
        (app, old, new, rx)
    }

    #[test]
    fn the_identity_moves_to_the_streamed_unit_and_its_mover_is_claimed() {
        let (mut app, old, new, rx) = harness();
        crate::net::handlers::dispatch(
            app.world_mut(),
            vec![SessionEvent::PartySwap { old: OLD, new: NEW }],
        );
        app.world_mut().flush();
        assert_eq!(app.world().resource::<SelfGuid>().0, Some(NEW));
        assert!(app.world().get::<SelfPlayer>(new).is_some());
        assert!(app.world().get::<SelfPlayer>(old).is_none());
        assert!(rx
            .try_iter()
            .any(|c| matches!(c, ClientCommand::SetActiveMover { guid: NEW })));
        assert!(
            app.world()
                .resource::<crate::ui_world_enter::PartyRebindOwed>()
                .0
        );
        assert_eq!(
            app.world().resource::<PartyArrivalOwed>().0,
            Some(NEW),
            "the new character's create in place is owed our arrival"
        );
    }

    #[test]
    fn the_rebind_gets_the_new_characters_row_from_its_descriptor() {
        let (mut app, ..) = harness();
        crate::net::handlers::dispatch(
            app.world_mut(),
            vec![SessionEvent::PartySwap { old: OLD, new: NEW }],
        );
        let roster = app
            .world()
            .resource::<crate::glue_session::roster::Roster>();
        assert_eq!(roster.pending_pick, Some(OLD), "the login's roster stays");
        let row = app
            .world()
            .resource::<crate::ui_world_enter::PartyRow>()
            .0
            .clone()
            .expect("a row for the new character");
        assert_eq!(
            (
                row.name.as_str(),
                row.race,
                row.class,
                row.gender,
                row.level
            ),
            ("Partyone", 1, 1, 1, 4)
        );
    }

    #[test]
    fn a_swap_to_a_unit_not_streamed_changes_nothing() {
        let (mut app, old, ..) = harness();
        crate::net::handlers::dispatch(
            app.world_mut(),
            vec![SessionEvent::PartySwap { old: OLD, new: 99 }],
        );
        app.world_mut().flush();
        assert_eq!(app.world().resource::<SelfGuid>().0, Some(OLD));
        assert!(app.world().get::<SelfPlayer>(old).is_some());
        assert!(
            !app.world()
                .resource::<crate::ui_world_enter::PartyRebindOwed>()
                .0
        );
    }
}
