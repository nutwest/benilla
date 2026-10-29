//! The clip a creature plays: a state machine over `AnimationData.dbc` ids. The game says what the
//! body is doing: a gait ([`CreatureAnim::set`]), a one-shot attack ([`CreatureAnim::attack`]) or
//! death ([`CreatureAnim::die`]). This module picks the clip, cross-fades into it, scales a moving
//! gait to the ground speed so the feet do not slide, returns to the gait when an attack ends, and
//! holds Death's last frame.

use std::time::Duration;

use bevy::animation::transition::AnimationTransitions;
use bevy::prelude::*;

use benilla_assets::{AnimClip, ModelAnimations};
use benilla_world::world_unit::WorldUnit;

/// `AnimationData.dbc` ids.
pub const STAND: u16 = 0;
pub const DEATH: u16 = 1;
pub const WALK: u16 = 4;
pub const ATTACK_1H: u16 = 17;
pub const RUN: u16 = 5;

/// Above this ground speed (yd/s) a mover runs rather than walks: past the 2.5 yd/s walk and
/// well under the 7 yd/s run, so neither gait plays far from its authored speed.
const RUN_ABOVE: f32 = 4.0;

/// What the body is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Gait {
    Stand,
    Walk,
    Run,
}

impl Gait {
    fn anim_id(self) -> u16 {
        match self {
            Gait::Stand => STAND,
            Gait::Walk => WALK,
            Gait::Run => RUN,
        }
    }

    /// The gait for a ground speed: still, walking or running.
    pub fn for_speed(speed: f32) -> Self {
        if speed <= 0.0 {
            Gait::Stand
        } else if speed <= RUN_ABOVE {
            Gait::Walk
        } else {
            Gait::Run
        }
    }
}

/// What the player is on.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Playing {
    Gait(Gait),
    /// A one-shot attack, until this many seconds of [`Time`].
    Attack { until: f32 },
    Dead,
}

/// A creature's wanted gait and ground speed, any requested one-shot, and what is playing.
#[derive(Component)]
pub struct CreatureAnim {
    wanted: Gait,
    /// Ground speed in yards per second, which a moving gait's rate divides.
    speed: f32,
    /// An attack asked for since the last drive: the ids to try, first owned wins.
    attack: Option<Vec<u16>>,
    dead: bool,
    playing: Option<Playing>,
}

impl CreatureAnim {
    pub fn new(gait: Gait) -> Self {
        Self {
            wanted: gait,
            speed: 0.0,
            attack: None,
            dead: false,
            playing: None,
        }
    }

    pub fn set(&mut self, gait: Gait, speed: f32) {
        self.wanted = gait;
        self.speed = speed;
    }

    /// Plays one attack: the first of `ids` the model has, else its Attack1H, else nothing.
    pub fn attack(&mut self, ids: &[u16]) {
        if !self.dead {
            self.attack = Some(ids.to_vec());
        }
    }

    /// Plays Death and holds its last frame; nothing after it plays.
    pub fn die(&mut self) {
        self.dead = true;
    }

    pub fn is_dead(&self) -> bool {
        self.dead
    }
}

pub fn plugin(app: &mut App) {
    // Before the pose evaluator reads the player in `PostUpdate`.
    app.add_systems(Update, drive);
}

fn drive(
    time: Res<Time>,
    mut units: Query<(
        &mut CreatureAnim,
        &mut AnimationPlayer,
        &mut AnimationTransitions,
        &ModelAnimations,
        &WorldUnit,
    )>,
) {
    let now = time.elapsed_secs();
    for (mut anim, mut player, mut transitions, anims, unit) in &mut units {
        let fade = |clip: &AnimClip| Duration::from_secs_f32(clip.blend_time.clamp(0.0, 0.3));

        if anim.dead {
            if anim.playing != Some(Playing::Dead) {
                anim.playing = Some(Playing::Dead);
                if let Some(clip) = anims.find(DEATH) {
                    // Not repeating: a finished clip holds its last pose.
                    transitions.play(&mut player, clip.node, fade(clip));
                }
            }
            continue;
        }

        if let Some(ids) = anim.attack.take() {
            let clip = ids
                .iter()
                .find_map(|&id| anims.find(id))
                .or_else(|| anims.find(ATTACK_1H));
            if let Some(clip) = clip {
                transitions.play(&mut player, clip.node, fade(clip)).replay();
                anim.playing = Some(Playing::Attack {
                    until: now + clip.duration,
                });
                continue;
            }
        }
        if let Some(Playing::Attack { until }) = anim.playing {
            if now < until {
                continue;
            }
        }

        // A model without a run plays its walk, scaled up; one without either stands.
        let clip = anims.find(anim.wanted.anim_id()).or_else(|| match anim.wanted {
            Gait::Run => anims.find(WALK),
            _ => None,
        });
        let Some(clip) = clip.or_else(|| anims.find(STAND)) else {
            continue;
        };
        let want = Playing::Gait(anim.wanted);
        if anim.playing != Some(want) {
            // A first arm starts at full weight; later ones fade over the clip's blend-in time.
            let blend = if anim.playing.is_some() {
                fade(clip)
            } else {
                Duration::ZERO
            };
            let active = transitions.play(&mut player, clip.node, blend);
            if clip.looping {
                active.repeat();
            }
            anim.playing = Some(want);
            debug!(
                "anim: {:?} (clip {}, {:.2}s, authored {:.2} yd/s, rate {:.2}x)",
                anim.wanted,
                clip.anim_id,
                clip.duration,
                clip.move_speed,
                playback_rate(clip, anim.speed, unit.scale)
            );
        }
        let rate = playback_rate(clip, anim.speed, unit.scale);
        if let Some(active) = player.animation_mut(clip.node) {
            active.set_speed(rate);
        }
    }
}

/// A moving gait plays at ground speed over its authored speed at this scale; a gait with no
/// authored speed, or standing, plays at 1×. The client's rule (`0x5fe2f0`): `moveSpeed` is
/// signed and a backwards gait stays at 1×.
fn playback_rate(clip: &AnimClip, speed: f32, scale: f32) -> f32 {
    let divisor = clip.move_speed * scale.abs();
    if clip.anim_id != STAND && divisor > 0.0 && speed > 0.0 {
        speed / divisor
    } else {
        1.0
    }
}
