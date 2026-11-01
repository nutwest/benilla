//! Our own player's arrival (`OnActiveArrival 0x5dea50`) and the world-enter cascade
//! (`EnterWorldUI 0x4908c0`), as one item on the arrival lane each. The reference runs the arrival
//! inside the update packet, from our create's `PostInit` (`0x5debe0`, at `0x5dec7b`), and every
//! listener of its signals runs before it goes on (`0x703e50`); a `/reload` runs the cascade alone
//! from the interface's initializer (`0x490168`). Here the create's handler writes what the next
//! packet's handlers read ([`crate::net::objects`]'s arrival) and pushes the item, stamped at the
//! create; its firing runs the sequence, its legs in the reference's order, with the host lent
//! and each signal in place:
//!
//! - the book's reset and the initial spells learned one at a time in `SMSG_INITIAL_SPELLS`' order
//!   (`0x5dea86` → `0x4b23a0`, `0x5dea96`-`0x5deab9` → `0x5e9c20` → `0x4b25b0`): each language
//!   spell's `LANGUAGE_LIST_CHANGED` (`0x4b2659`), each form's add to the stored form list and its
//!   `UPDATE_SHAPESHIFT_FORMS` (`0x4b2904`), so the k-th listener counts k forms;
//! - the book's sort and its `SPELLS_CHANGED` (`0x5deac2` → `0x4b2fd0`), the macros' derivation
//!   (`0x5deacc` → `0x4f0ca0`), then, with the manager's `+0xc8` clear (`0x5dead1`, always at a
//!   create), the bar's 120 slots and `ACTIONBAR_SLOT_CHANGED` with 0 (`0x5deae2`-`0x5deb20`);
//! - the ghost state (`0x5deb2a` → `0x5de9c0`): `PLAYER_UNGHOST`, or a ghost's tutorial;
//! - with `+0xc8` clear (`0x5deb49`): the control flag's rise (`0x5deb56` → `0x5fa600`), then the
//!   cascade (`0x5deb60`), then the death state (`0x5deb65` → `0x495850`);
//! - `SetInWorld(1)` and its `LOGOUT_CANCEL` (`0x5deb73` → `0x5aae70`).
//!
//! The cascade's legs are its calls that benilla models, in its order (`0x490927`-`0x490a56`).

use std::collections::VecDeque;

use bevy::ecs::query::{FilteredAccess, FilteredAccessSet};
use bevy::prelude::*;

use crate::lanes::{Stamp, Until};
use crate::net::{Guid, ObjectStore, SelfPlayer};

/// A window's silent reset in the cascade (`0x4909ab`, `0x4909b0`, `0x4909b5`, `0x490a0a`), owed
/// from where its item was pushed. The reference runs it inside our create's packet, after the
/// cascade's login events, so a getter there reads what the close left; and before the packets
/// after it, so a later list survives it. It lands at its cascade place, or ahead of a later
/// packet's write to its window's records, whichever comes first.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct OwedReset(bool);

impl OwedReset {
    /// The reset owed, from the item's push.
    pub(crate) fn owe(&mut self) {
        self.0 = true;
    }

    /// Whether the reset was owed, now settled.
    pub(crate) fn take(&mut self) -> bool {
        std::mem::take(&mut self.0)
    }
}

/// The cascade's silent window resets owed, as our create (`crate::net::objects`' arrival) and a
/// `/reload` push its item ([`OwedReset`]).
pub(crate) fn owe_window_resets(world: &mut World) {
    if let Some(mut trainer) = world.get_resource_mut::<crate::ui_trainer::TrainerOpen>() {
        trainer.world_reset.owe();
    }
    if let Some(mut merchant) = world.get_resource_mut::<crate::ui_merchant::MerchantOpen>() {
        merchant.world_reset.owe();
    }
    if let Some(mut trade) = world.get_resource_mut::<crate::ui_trade::TradeSession>() {
        trade.world_reset.owe();
    }
    if let Some(mut stable) = world.get_resource_mut::<crate::ui_stable::StableOpen>() {
        stable.world_reset.owe();
    }
}

/// What one item on the lane runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Entry {
    /// Our player's arrival (`0x5dea50`), the cascade among its steps. `control_gained`: the
    /// create raised the player-control flag, whose signals are owed (`0x5deb56`).
    Arrival { control_gained: bool },
    /// A `/reload`'s cascade (`0x490168`), which no arrival precedes.
    Reload,
}

/// The arrival lane: each item stamped where its create, or its reload, put it.
#[derive(Resource, Default)]
pub(crate) struct ArrivalLane(VecDeque<(Stamp, Entry)>);

impl ArrivalLane {
    /// `entry` at the clock's next stamp.
    pub(crate) fn push(&mut self, entry: Entry) {
        self.0.push_back((crate::lanes::stamp(), entry));
    }

    /// The items queued, in order, for a test.
    #[cfg(test)]
    pub(crate) fn entries(&self) -> Vec<Entry> {
        self.0.iter().map(|(_, e)| *e).collect()
    }
}

/// Registers the lane. Its items wait for the in-game interface, which the reference loads before
/// the create arrives and benilla a frame or two later, behind the loading screen's cover; the
/// session's end drops what never fired.
pub(crate) struct WorldEnterPlugin;

impl Plugin for WorldEnterPlugin {
    fn build(&self, app: &mut App) {
        use crate::lanes::LaneApp;
        app.init_resource::<ArrivalLane>()
            .init_resource::<PartyRebindOwed>()
            .init_resource::<PartyRow>()
            .add_exclusive_lane("arrival", peek, fire, reads)
            .add_systems(
                OnExit(crate::client_state::ClientState::InWorld),
                drop_arrivals.in_set(crate::ui_script::SessionEnd::Clears),
            );
    }
}

/// An arrival is the session's: a later one's interface never hears it.
fn drop_arrivals(mut lane: ResMut<ArrivalLane>) {
    lane.0.clear();
}

/// The lane's head.
fn peek(world: &World) -> Option<Stamp> {
    world
        .get_resource::<ArrivalLane>()?
        .0
        .front()
        .map(|(at, _)| *at)
}

/// Each item `until` admits, run whole as the firing of its stamp, so what a leg pushes onto the
/// other lanes fires straight after it; the lane stops after an item whose firing stamped
/// anything, so what it pushed fires next.
fn fire(In(until): In<Until>, world: &mut World) {
    loop {
        let Some((at, entry)) = world
            .resource::<ArrivalLane>()
            .0
            .front()
            .copied()
            .filter(|(at, _)| until.admits(*at))
        else {
            return;
        };
        world.resource_mut::<ArrivalLane>().0.pop_front();
        let ((), moved) = crate::lanes::firing(at, || run(world, entry));
        if moved {
            return;
        }
    }
}

/// One item's sequence.
fn run(world: &mut World, entry: Entry) {
    match entry {
        Entry::Arrival { control_gained } => arrival(world, control_gained),
        Entry::Reload => cascade(world, false),
    }
}

/// `0x5dea50` from the book's reset on (`0x5dea86`); its watchers, the error speech table and the
/// item cache's no-op (`0x5dea58`-`0x5dea81`) signal nothing.
fn arrival(world: &mut World, control_gained: bool) {
    // A party swap's arrival: the rebind follows, and the target stays.
    let party = world
        .get_resource::<PartyRebindOwed>()
        .is_some_and(|owed| owed.0);
    leg(world, seat_player);
    replay_book(world);
    // `0x5deac2` → `0x4b2fd0(1, 0)`: the book sorted, `SPELLS_CHANGED` (`0x4b302a`).
    signal(world, Signal::SpellsChanged);
    leg(world, crate::ui_macro::derive_every);
    // `0x5dead1`: the slot pass with the manager's `+0xc8` clear, as at every create.
    leg(world, crate::ui_action::recount::arrival_slot_pass);
    ghost_state(world);
    // `0x5deb49`: `+0xc8` clear, as at every create.
    if control_gained {
        leg(world, control_rose);
    }
    cascade(world, party);
    crate::death::signal_death_state(world);
    leg(world, crate::ui_logout::set_in_world);
    // `0x5debcc` → `0x6e6ca0`: the class family the spell modifiers gate on.
    leg(world, class_family);
    apply_form_edits(world);
    // Deviation: a party swap's arrival (`crate::net`'s `party_swap`) rebinds the UI to the new
    // character by its events, where the reference enters the world anew.
    let rebind = world
        .get_resource_mut::<PartyRebindOwed>()
        .is_some_and(|mut owed| std::mem::take(&mut owed.0));
    if rebind {
        // The player record `UnitName`/`UnitClass("player")` answer, made as a load makes it from
        // the pick's roster row: here the new character's row, which the swap made.
        let row = world
            .get_resource_mut::<PartyRow>()
            .and_then(|mut row| row.0.take());
        let record = row.and_then(|row| {
            let mut roster = crate::glue_session::roster::Roster::default();
            roster.chars = vec![row.clone()];
            roster.pending_pick = Some(row.guid);
            crate::ui_script::record_from_roster(&roster, crate::ui_unit::world_unit_names(world))
        });
        world.insert_resource(PartyRecord(record));
        leg(world, party_rebind);
    }
}

/// Owed by a party swap (`crate::net`'s `party_swap`): the next arrival, the new character's
/// self create, rebinds the UI with [`party_rebind`].
#[derive(Resource, Default)]
pub(crate) struct PartyRebindOwed(pub(crate) bool);

/// The new character's roster row as the swap made it (name, race, class and sex from its
/// descriptor), for the rebind's player record.
#[derive(Resource, Default)]
pub(crate) struct PartyRow(pub(crate) Option<benilla_protocol::messages::Character>);

/// The new character's player record, for [`party_rebind`] to seat.
#[derive(Resource, Default)]
struct PartyRecord(Option<benilla_ui::script::PlayerRecord>);

/// `UNIT_FLAG_IN_COMBAT`, for the combat state's edge.
const UNIT_FLAG_IN_COMBAT: u32 = 0x0008_0000;

/// The party swap's rebind: every frame showing "player" is told it changed, in one pass, after the
/// new character's own arrival seated it.
///
/// What it fires is what the stock frames need beyond the cascade, which fires
/// `PLAYER_ENTERING_WORLD`, `SPELLS_CHANGED`, the bar's slots, the bags' rescan, the quest log's and
/// the lists' rebuilds. On `PLAYER_ENTERING_WORLD` the stock FrameXML already redraws the player
/// frame (`PlayerFrame.lua`, `UnitFrame_Update`: name, portrait, health, power), every action
/// button (`ActionButton.lua`), the paper doll's model (`PaperDollFrame.lua`), experience
/// (`MainMenuBar.lua`), honor (`HonorFrame.lua`) and the shapeshift bar (`BonusActionBarFrame.lua`).
/// What it leaves is fired here, each event with the reference's arguments. `CHARACTER_POINTS_CHANGED`
/// is left out: it carries the points gained, which `ChatFrame.lua` announces.
fn party_rebind(
    mut vm: crate::script_calls::HostedVm,
    me: Query<&ObjectStore, With<SelfPlayer>>,
    mut record: ResMut<PartyRecord>,
) {
    use benilla_ui::script::ScriptValue;
    let start = std::time::Instant::now();
    let in_combat = me
        .iter()
        .next()
        .is_some_and(|s| s.0.unit_flags() & UNIT_FLAG_IN_COMBAT != 0);
    let record = record.0.take();
    vm.run(|script| {
        let Some(script) = script else { return };
        if let Some(record) = record {
            script.set_player_record(record);
        }
        // The character frame's name and portrait (`CharacterFrame.lua`), faction, auras, the bag
        // slot buttons and the pet.
        script.fire_event("UNIT_NAME_UPDATE", vec![ScriptValue::Str("player".into())]);
        script.fire_event(
            "UNIT_PORTRAIT_UPDATE",
            vec![ScriptValue::Str("player".into())],
        );
        script.fire_event("UNIT_FACTION", vec![ScriptValue::Str("player".into())]);
        script.fire_event("UNIT_AURA", vec![ScriptValue::Str("player".into())]);
        script.fire_event(
            "UNIT_INVENTORY_CHANGED",
            vec![ScriptValue::Str("player".into())],
        );
        script.fire_event("UNIT_PET", vec![ScriptValue::Str("player".into())]);
        // The buff frame (`BuffFrame.lua`), resting (`PlayerFrame.lua`), money, the character
        // sheet's guild, the spell book's and bags' cooldowns, the pet bar, and a cast bar left
        // from the old character (`CastingBarFrame.lua`).
        script.fire_event("PLAYER_AURAS_CHANGED", Vec::new());
        script.fire_event("PLAYER_UPDATE_RESTING", Vec::new());
        script.fire_event("PLAYER_MONEY", Vec::new());
        script.fire_event("PLAYER_GUILD_UPDATE", Vec::new());
        script.fire_event("SPELL_UPDATE_COOLDOWN", Vec::new());
        script.fire_event("BAG_UPDATE_COOLDOWN", Vec::new());
        script.fire_event("PET_BAR_UPDATE", Vec::new());
        script.fire_event("SPELLCAST_STOP", Vec::new());
        // The player frame's combat state.
        let regen = if in_combat {
            "PLAYER_REGEN_DISABLED"
        } else {
            "PLAYER_REGEN_ENABLED"
        };
        script.fire_event(regen, Vec::new());
    });
    info!(
        "ui: party rebind fired in {:.1} ms",
        start.elapsed().as_secs_f32() * 1000.0
    );
    if benilla_assets::trace::enabled_for("pty") {
        benilla_assets::trace::line("pty", "rebind");
    }
}

/// The cascade `0x4908c0` past its once-per-world latch (`0x4908c7`, the sweep's
/// [`crate::ui_script::LeavingWorldArmed`]), its legs in call order.
fn cascade(world: &mut World, party: bool) {
    leg(world, seat_player);
    // `0x490927` `SelectTarget(0)`, the collection, `PLAYER_LOGIN` if armed, `PLAYER_ENTERING_WORLD`
    // and the minimap initializer (`0x49096f`).
    if party {
        leg(world, open_cascade_keeping_target);
    } else {
        leg(world, open_cascade);
    }
    // `0x490974` → `0x4989c0`: the chat display latch released, its lines shown.
    leg(world, crate::ui_chat::release_display);
    // `0x490979` → `0x4e4f90`: the bar's usable cache, then the bonus bar.
    leg(world, action_bar);
    // `0x490983` → `0x4e4100`: the buff cache's set-up.
    leg(world, crate::ui_aura::enter_world_in_place);
    // `0x49098d` → `0x4f8cc0`: the bags' rescan.
    leg(world, |mut vm: crate::script_calls::HostedVm| {
        vm.run(|script| {
            if let Some(script) = script {
                script.fire_world_enter_bags();
            }
        })
    });
    // `0x49099c` → `0x500af0`: the questgiver's reset.
    leg(world, crate::ui_quest::world_enter_cascade);
    // `0x4909a1` → `0x4de430`: `CMSG_QUERY_TIME` (`0x4de45b`), then the quest log's rebuild.
    leg(world, crate::net::query_time_at_world_enter);
    leg(world, |mut vm: crate::script_calls::HostedVm| {
        vm.run(|script| {
            if let Some(script) = script {
                script.fire_world_enter_quest_log();
            }
        })
    });
    // `0x4909ab`, `0x4909b0`, `0x4909b5`: the trainer's, the merchant's and the trade's resets,
    // which signal nothing, each owed since the item's push ([`OwedReset`]).
    leg(world, crate::ui_trainer::reset_on_world_enter);
    leg(world, crate::ui_merchant::reset_on_world_enter);
    leg(world, crate::ui_trade::reset_on_world_enter);
    // `0x4909bf`, `0x4909c4`: the profession windows' resets.
    leg(world, crate::ui_tradeskill::reset_crafts_on_world_enter);
    // `0x4909c9` → `0x4bc6f0`: the pet's.
    leg(world, crate::ui_pet::reset_pet_on_world_enter);
    // `0x4909d3`, `0x4909f1`: the reputation and skill lists' rebuilds.
    leg(world, |mut vm: crate::script_calls::HostedVm| {
        vm.run(|script| {
            if let Some(script) = script {
                script.fire_world_enter_rebuilds();
            }
        })
    });
    // `0x4909f6` → `0x4acb10`: the mail init, its papers' lookups ahead of its query
    // (`0x4acbba`, `0x4acbdc`).
    leg(world, crate::ui_mail::count_papers_on_enter);
    leg(world, crate::ui_mail::send_query_next_mail_time_on_enter);
    // `0x4909fb` → `0x4a9db0`: the battlefield init.
    leg(world, crate::ui_battlefield::reset_on_world_enter);
    // `0x490a05` → `0x4cc280`: the auction reset.
    leg(world, crate::ui_auction::reset_auction_on_world_enter);
    // `0x490a0a` → `0x4caab0`: the stable's, all engine.
    leg(world, crate::ui_stable::reset_on_world_enter);
    // `0x490a14` → `0x4c9f40`: the meeting stone's.
    leg(world, crate::ui_dialog_verbs::meeting_stone_enter_world);
    // `0x490a28` → `0x491f50`: the corpse cache reset, and a ghost's `MSG_CORPSE_QUERY`.
    leg(world, crate::death::query_corpse_on_world_enter);
    // `0x490a2d`-`0x490a3c`: a dead body released.
    leg(world, crate::death::repop_on_world_enter);
    // `0x490a48`, `0x490a51`: Welcome, then Questgivers.
    tutorial(world, crate::tutorial::id::WELCOME);
    tutorial(world, crate::tutorial::id::QUESTGIVERS);
    // `0x490a56`: the bracket closed.
    leg(world, |mut vm: crate::script_calls::HostedVm| {
        vm.run(|script| {
            if let Some(script) = script {
                script.close_world_enter();
            }
        })
    });
}

/// Run one leg as a one-shot, its unit edits applied as it returns; a harness without a leg's
/// state runs nothing of it.
fn leg<M>(world: &mut World, system: impl IntoSystem<(), (), M> + 'static) {
    if let Err(err) = crate::script_calls::run_cached(world, system) {
        debug!("world_enter: a leg did not run: {err}");
    }
}

/// The arrival's argumentless signals that fire outside a leg.
#[derive(Clone, Copy, Debug)]
enum Signal {
    /// `0x4b2659`.
    LanguageListChanged,
    /// `0x4b2904`.
    ShapeshiftForms,
    /// `0x4b2fd0`.
    SpellsChanged,
    /// `0x5dea2a`.
    Unghost,
}

/// `signal` fired with no arguments, in place, with the host lent.
fn signal(world: &mut World, signal: Signal) {
    if let Err(err) = world.run_system_cached_with(fire_signal, signal) {
        debug!("world_enter: {signal:?} did not signal: {err}");
    }
    crate::creature_anim::apply_unit_edits(world);
}

/// [`signal`]'s one-shot.
fn fire_signal(In(signal): In<Signal>, mut vm: crate::script_calls::HostedVm) {
    let event = match signal {
        Signal::LanguageListChanged => "LANGUAGE_LIST_CHANGED",
        Signal::ShapeshiftForms => "UPDATE_SHAPESHIFT_FORMS",
        Signal::SpellsChanged => "SPELLS_CHANGED",
        Signal::Unghost => "PLAYER_UNGHOST",
    };
    vm.run(|script| {
        if let Some(script) = script {
            script.fire_event(event, Vec::new());
        }
    })
}

/// `FireTutorial(id, 0)` in place ([`crate::tutorial::fire_now`]).
fn tutorial(world: &mut World, id: u32) {
    if let Err(err) = world.run_system_cached_with(crate::tutorial::fire_now, id) {
        debug!("world_enter: tutorial {id} did not fire: {err}");
    }
}

/// `"player"` names us for the sequence's listeners: the stock windows bind their model panes to
/// it at `PLAYER_ENTERING_WORLD` (`PaperDollFrame.lua:39`), ahead of the resolver feed's full push
/// (`feed_unit_guids`).
fn seat_player(mut vm: crate::script_calls::VmWith<()>, me: Query<&Guid, With<SelfPlayer>>) {
    let Some(me) = me.iter().next().map(|g| g.0) else {
        return;
    };
    vm.turn(|script, ()| {
        if let Some(script) = script {
            script.seat_player_guid(me);
        }
    });
}

/// `ResetSpellBook 0x4b23a0`'s form-list clear (`0x4b2493`), then `LearnSpell(id, x, 0, 1)` for each
/// initial spell in wire order (`0x5dea96`-`0x5deab9`), whose notify flag runs the registrar
/// `0x4b25b0` (`0x5e9c5a`): a language spell's `LANGUAGE_LIST_CHANGED` (`0x4b2659`) ahead of its
/// form leg, whose add to the stored list ([`crate::ui_shapeshift::form_learned`]) signals
/// `UPDATE_SHAPESHIFT_FORMS` (`0x4b2904`). Each signals in place, its listeners reading the list
/// as this learn left it. The book's other lists are the `SMSG_INITIAL_SPELLS` handler's.
fn replay_book(world: &mut World) {
    let Some(initial) = world
        .get_resource_mut::<crate::ui_action::PlayerActions>()
        .map(|mut actions| {
            actions.forms.clear();
            actions.initial.clone()
        })
    else {
        return;
    };
    for spell_id in initial {
        let language = world
            .get_resource::<crate::ui_action::Spells>()
            .and_then(|s| s.catalog.get(spell_id))
            .is_some_and(crate::ui_spellbook::teaches_language);
        if language {
            signal(world, Signal::LanguageListChanged);
        }
        let moved =
            world.resource_scope(|world, mut actions: Mut<crate::ui_action::PlayerActions>| {
                world
                    .get_resource::<crate::ui_action::Spells>()
                    .is_some_and(|s| {
                        crate::ui_shapeshift::form_learned(
                            &mut actions.forms,
                            spell_id,
                            None,
                            &s.catalog,
                        )
                    })
            });
        if moved {
            signal(world, Signal::ShapeshiftForms);
        }
    }
}

/// `0x6e6ca0` at the arrival's end (`0x5debcc`): the spell modifiers' class family, `ChrClasses`
/// field 15 of our class, byte 1 of `UNIT_FIELD_BYTES_0`. Written only on a move, as the
/// item-tooltip feed rebuilds whenever the tables are marked changed.
fn class_family(
    me: Query<&ObjectStore, With<SelfPlayer>>,
    mods: Option<ResMut<crate::spell::SpellModifiers>>,
    classes: Option<Res<crate::chr_classes::ChrClassTable>>,
) {
    let (Some(mut mods), Some(classes), Some(class)) = (
        mods,
        classes,
        me.iter().next().and_then(|s| s.0.unit_class()),
    ) else {
        return;
    };
    let family = classes.0.spell_family(u32::from(class));
    if mods.class_family() != family {
        mods.set_class_family(family);
    }
}

/// The form-list edits the packets after our create made while its item waited
/// ([`crate::ui_action::PlayerActions::form_edits`]), applied after the replay, signalling
/// nothing: their own signals fire at their packets' places, after this item. While another
/// arrival waits behind this one, its edits stay for it.
fn apply_form_edits(world: &mut World) {
    if !world.contains_resource::<crate::ui_action::PlayerActions>() {
        return;
    }
    let another = world
        .get_resource::<ArrivalLane>()
        .is_some_and(|l| l.0.iter().any(|(_, e)| matches!(e, Entry::Arrival { .. })));
    world.resource_scope(|world, mut actions: Mut<crate::ui_action::PlayerActions>| {
        let edits = if another {
            actions.form_edits.clone().unwrap_or_default()
        } else {
            actions.form_edits.take().unwrap_or_default()
        };
        if let Some(spells) = world.get_resource::<crate::ui_action::Spells>() {
            for edit in edits {
                edit.apply(&mut actions.forms, &spells.catalog);
            }
        }
    });
}

/// `UpdateGhostState 0x5de9c0` at the arrival: a ghost's `PLAYER_FLAGS` bit 0x10 fires the Death
/// tutorial (`0x5de9f0`); any other body signals `PLAYER_UNGHOST` (`0x5dea2a`).
fn ghost_state(world: &mut World) {
    let ghost = crate::script_calls::run_cached(world, ghost_of)
        .ok()
        .flatten();
    match ghost {
        Some(true) => tutorial(world, crate::tutorial::id::DEATH),
        Some(false) => signal(world, Signal::Unghost),
        None => {}
    }
}

/// Whether our player is a ghost, `PLAYER_FLAGS` bit 0x10 (`0x5de9cc`); `None` with no player.
fn ghost_of(me: Query<&ObjectStore, With<SelfPlayer>>) -> Option<bool> {
    me.iter().next().map(|s| s.0.player_flags() & 0x10 != 0)
}

/// `SetClientControl(1)`'s rise of the player-control flag (`0x5fa6b1` → `0x4958e0`):
/// `PLAYER_CONTROL_GAINED` (`0x4958f7`), then the usable refresh (`0x4958fc`), as the control
/// packet's handler signals it.
fn control_rose(mut vm: crate::script_calls::VmWith<()>) {
    vm.run(|script| {
        if let Some(script) = script {
            script.set_player_control(true);
            script.fire_event("PLAYER_CONTROL_GAINED", Vec::new());
        }
    });
    vm.engine_usable_refresh();
}

/// The cascade's opening: the sound bracket (`0x4908d5`), then `SelectTarget(0)` (`0x490927`)
/// inside it as an engine turn on the selection, so its handlers' sounds are dropped, then the
/// collection, `PLAYER_LOGIN` if armed, `PLAYER_ENTERING_WORLD` and the minimap's (`0x49096a`,
/// `0x49096f`).
fn open_cascade(mut vm: crate::script_calls::VmWith<()>) {
    vm.run(|script| {
        if let Some(script) = script {
            script.open_world_enter();
        }
    });
    vm.engine_clear_target(crate::lanes::mark());
    vm.run(|script| {
        if let Some(script) = script {
            script.fire_world_enter_events();
        }
    });
}

/// Deviation: [`open_cascade`] for a party swap's arrival, without its `SelectTarget(0)`. A world
/// entry starts a session with nothing selected; a swap mid-fight keeps what the player was
/// fighting, and clearing it re-signals every frame that reads the target, which in combat cost
/// the swap's frame over 150 ms.
fn open_cascade_keeping_target(mut vm: crate::script_calls::VmWith<()>) {
    vm.run(|script| {
        if let Some(script) = script {
            script.open_world_enter();
            script.fire_world_enter_events();
        }
    });
}

/// `0x4e4f90`: the bar's usable cache refreshed in place (`0x4e5c00`), then the bonus bar's offset
/// from our form (`0x4e4fc0`), each with its signal.
fn action_bar(
    mut vm: crate::script_calls::VmWith<Option<Res<'static, crate::ui_action::Spells>>>,
    me: Query<&ObjectStore, With<SelfPlayer>>,
) {
    let changed = vm.engine_enter_world_refresh();
    let offset = crate::ui_action::bonus_bar_offset(me.iter().next(), vm.own().as_deref());
    vm.run(|script| {
        if let Some(script) = script {
            script.fire_world_enter_action_bar(offset, changed);
        }
    });
}

/// What the lane's firing reads beside the call host and its own lane: every leg's declared
/// access with the host's taken out, and the book the replay reads.
fn reads(world: &mut World) -> FilteredAccessSet {
    use crate::lanes::system_access;
    let host: Vec<FilteredAccess> =
        system_access(world, |_: crate::script_calls::ScriptCallHost| {})
            .filtered_accesses()
            .to_vec();
    let legs = [
        system_access(world, seat_player),
        system_access(world, crate::ui_macro::derive_every),
        system_access(world, crate::ui_action::recount::arrival_slot_pass),
        system_access(world, control_rose),
        system_access(world, open_cascade),
        system_access(world, crate::ui_chat::release_display),
        system_access(world, action_bar),
        system_access(world, crate::ui_aura::enter_world_in_place),
        system_access(world, crate::ui_quest::world_enter_cascade),
        system_access(world, crate::net::query_time_at_world_enter),
        system_access(world, crate::ui_tradeskill::reset_crafts_on_world_enter),
        system_access(world, crate::ui_pet::reset_pet_on_world_enter),
        system_access(world, crate::ui_mail::count_papers_on_enter),
        system_access(world, crate::ui_mail::send_query_next_mail_time_on_enter),
        system_access(world, crate::ui_battlefield::reset_on_world_enter),
        system_access(world, crate::ui_auction::reset_auction_on_world_enter),
        system_access(world, crate::ui_dialog_verbs::meeting_stone_enter_world),
        system_access(world, crate::ui_trainer::reset_on_world_enter),
        system_access(world, crate::ui_merchant::reset_on_world_enter),
        system_access(world, crate::ui_trade::reset_on_world_enter),
        system_access(world, crate::ui_stable::reset_on_world_enter),
        system_access(world, crate::death::query_corpse_on_world_enter),
        system_access(world, crate::death::repop_on_world_enter),
        system_access(world, crate::ui_logout::set_in_world),
        system_access(world, class_family),
        system_access(world, party_rebind),
        system_access(world, open_cascade_keeping_target),
        system_access(world, fire_signal),
        system_access(world, crate::tutorial::fire_now),
        system_access(world, ghost_of),
        system_access(world, crate::death::signal_death),
    ];
    let mut set = FilteredAccessSet::default();
    for access in legs {
        for filtered in access.filtered_accesses() {
            if !host.contains(filtered) {
                set.add(filtered.clone());
            }
        }
    }
    // The replay's book and records, which it reads holding the world.
    for id in [
        world
            .components()
            .get_resource_id(std::any::TypeId::of::<crate::ui_action::PlayerActions>()),
        world
            .components()
            .get_resource_id(std::any::TypeId::of::<crate::ui_action::Spells>()),
    ]
    .into_iter()
    .flatten()
    {
        set.add_unfiltered_resource_read(id);
    }
    set
}

/// Our player's arrival pushed, as a create's handler does, for a test that seats the player by
/// hand.
#[cfg(test)]
pub(crate) fn push_arrival(world: &mut World) {
    world
        .get_resource_or_init::<ArrivalLane>()
        .push(Entry::Arrival {
            control_gained: false,
        });
}

/// The cascade alone, as a `/reload`'s item fires it, for a test of its legs.
#[cfg(test)]
pub(crate) fn run_cascade(world: &mut World) {
    crate::lanes::firing(crate::lanes::stamp(), || cascade(world, false));
}

/// Every item queued, fired alone with the host lent, for a test that drives the arrival without
/// the other lanes.
#[cfg(test)]
pub(crate) fn fire_lane(world: &mut World) {
    while world
        .get_resource::<ArrivalLane>()
        .is_some_and(|l| !l.0.is_empty())
    {
        world
            .run_system_cached_with(fire, Until::all())
            .expect("the arrival lane fires");
    }
}
