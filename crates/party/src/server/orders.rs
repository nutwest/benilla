//! The party's orders (M6b): the crate's list of them, by member. Every order goes to the server's
//! table by `.party order`, in party chat so it works while the world is paused, and that table
//! runs the bots: a swap builds a fresh AI, which reads the order there, so nothing is carried.
//!
//! The played character has no AI: its movement is the client's. So this list also runs the order
//! of whoever is played, by the app's Click to Move walk and follow ([`benilla_app::OwnOrder`]),
//! once the world is running. A swap in either direction hands over by itself: the old character's
//! order is already in the server's table, and the new one's starts here.

use std::collections::HashMap;

use benilla_app::{Interact, Objects, OwnOrder, SessionRequest, TerrainHeights};
use bevy::math::Vec3Swizzles as _;
use bevy::prelude::*;

use super::Server;

/// An order, in WoW coordinates.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Order {
    /// Walk here, then hold. A `z` of NaN takes the ground's height: the server paths to a point on
    /// the ground, and in a straight line through the hill to one that isn't.
    Move([f32; 3]),
    /// Stay here, and fight within reach.
    Hold,
    Attack(u64),
    Follow(u64),
    /// Engage what enters `radius` of the post, and walk back to it.
    Guard([f32; 3], f32),
}

impl Order {
    /// The order's line for `name`, as `.party order` reads it.
    fn line(&self, name: &str, members: &[(u64, &'static str)]) -> Option<String> {
        let verb = match *self {
            Self::Move([x, y, z]) => format!("move {x:.2} {y:.2} {z:.2}"),
            Self::Hold => "hold".to_string(),
            Self::Attack(guid) => format!("attack {guid:#x}"),
            Self::Follow(guid) => {
                let (_, followed) = members.iter().find(|(g, _)| *g == guid)?;
                format!("follow {followed}")
            }
            Self::Guard([x, y, z], radius) => format!("guard {x:.2} {y:.2} {z:.2} {radius:.1}"),
        };
        Some(format!(".party order {name} {verb}"))
    }
}

/// The members' orders, and the one the played character is running.
#[derive(Resource, Default)]
pub(super) struct Orders {
    by_member: HashMap<u64, Order>,
    /// The played character's, as it was started, so a walk it ended itself reads as ended.
    running: Option<(u64, Order)>,
}

impl Orders {
    /// `guid`'s order.
    pub(super) fn of(&self, guid: u64) -> Option<Order> {
        self.by_member.get(&guid).copied()
    }
}

pub(super) struct OrdersPlugin;

impl Plugin for OrdersPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<Orders>()
            .add_systems(Update, (take_asked, run_played).chain());
    }
}

/// Gives each member its order, from the bar, the right click and the keys through
/// [`super::Supervisor::give_order`].
fn take_asked(
    server: Res<Server>,
    mut orders: ResMut<Orders>,
    heights: TerrainHeights,
    mut session: MessageWriter<SessionRequest>,
) {
    let asked = std::mem::take(&mut *server.0.orders.lock().unwrap());
    if asked.is_empty() {
        return;
    }
    let members = server.0.party.lock().unwrap().clone();
    for (guid, order) in asked {
        let Some(&(_, name)) = members.iter().find(|(g, _)| *g == guid) else {
            continue;
        };
        let order = match order {
            Order::Move([x, y, z]) if z.is_nan() => {
                let ground = heights.under(benilla_app::wow_to_bevy([x, y, 0.0]));
                Order::Move([x, y, ground.unwrap_or(z)])
            }
            order => order,
        };
        if let Some(line) = order.line(name, &members) {
            info!("party: {line}");
            session.write(SessionRequest::PartyCommand(line));
            orders.by_member.insert(guid, order);
        }
    }
}

/// Runs the played character's order, once the world is running; a new order or a swap starts
/// over, a walk that ended near its point becomes a Hold, and one ended short, by a movement key,
/// ends the order.
fn run_played(
    server: Res<Server>,
    mut orders: ResMut<Orders>,
    objects: Objects,
    bodies: Query<&GlobalTransform>,
    mut own: MessageWriter<OwnOrder>,
    mut interact: MessageWriter<Interact>,
    mut session: MessageWriter<SessionRequest>,
) {
    if benilla_app::world_paused() {
        return;
    }
    let played = server.0.self_guid();
    let wanted = orders.of(played).map(|order| (played, order));
    if wanted != orders.running {
        orders.running = wanted;
        let body = |guid: u64| {
            let (_, entity, _) = objects.iter().find(|(g, ..)| *g == guid)?;
            Some((entity, bodies.get(entity).ok()?.translation()))
        };
        match wanted.map(|(_, order)| order) {
            Some(Order::Move(at)) => {
                own.write(OwnOrder::WalkTo(benilla_app::wow_to_bevy(at)));
            }
            Some(Order::Attack(guid)) => {
                if let Some((entity, at)) = body(guid) {
                    interact.write(Interact(entity));
                    own.write(OwnOrder::WalkTo(at));
                }
            }
            Some(Order::Follow(guid)) => {
                if let Some(&(_, name)) = server
                    .0
                    .party
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(g, _)| *g == guid)
                {
                    own.write(OwnOrder::Follow(guid, name.to_string()));
                }
            }
            // A played character can't guard, having no AI: it stands, as on a Hold.
            Some(Order::Hold | Order::Guard(..)) | None => {
                own.write(OwnOrder::Stop);
            }
        }
        return;
    }
    // A walk just started hasn't shown as active yet: the app answers the frame after.
    let Some(Order::Move(at)) = wanted.map(|(_, order)| order) else {
        return;
    };
    if benilla_app::own_order_active() || !benilla_app::own_order_settled() {
        return;
    }
    let near = objects
        .player()
        .and_then(|me| bodies.get(me).ok())
        .is_some_and(|me| {
            me.translation()
                .xz()
                .distance(benilla_app::wow_to_bevy(at).xz())
                < ARRIVED_YARDS
        });
    let members = server.0.party.lock().unwrap().clone();
    let Some(&(_, name)) = members.iter().find(|(g, _)| *g == played) else {
        return;
    };
    if near {
        orders.by_member.insert(played, Order::Hold);
        orders.running = Some((played, Order::Hold));
        if let Some(line) = Order::Hold.line(name, &members) {
            session.write(SessionRequest::PartyCommand(line));
        }
    } else {
        orders.by_member.remove(&played);
        orders.running = None;
        session.write(SessionRequest::PartyCommand(format!(
            ".party order {name} clear"
        )));
    }
}

/// A played walk counts as arrived this near its point, in yards.
const ARRIVED_YARDS: f32 = 4.0;
