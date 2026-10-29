//! The clip a creature plays: a gait state machine over `AnimationData.dbc` ids. The game says
//! what the body is doing ([`CreatureAnim::set`]); this module picks the clip, cross-fades into it
//! and scales a moving gait to the ground speed, so the feet do not slide.

use std::time::Duration;

use bevy::animation::transition::AnimationTransitions;
use bevy::prelude::*;

use benilla_assets::{AnimClip, ModelAnimations};
use benilla_world::world_unit::WorldUnit;

/// `AnimationData.dbc` ids.
pub const STAND: u16 = 0;
pub const WALK: u16 = 4;
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

/// A creature's wanted gait and ground speed, and the gait its player is on.
#[derive(Component)]
pub struct CreatureAnim {
    wanted: Gait,
    /// Ground speed in yards per second, which a moving gait's rate divides.
    speed: f32,
    playing: Option<Gait>,
}

impl CreatureAnim {
    pub fn new(gait: Gait) -> Self {
        Self {
            wanted: gait,
            speed: 0.0,
            playing: None,
        }
    }

    pub fn set(&mut self, gait: Gait, speed: f32) {
        self.wanted = gait;
        self.speed = speed;
    }
}

pub fn plugin(app: &mut App) {
    // Before the pose evaluator reads the player in `PostUpdate`.
    app.add_systems(Update, drive);
}

fn drive(
    mut units: Query<(
        &mut CreatureAnim,
        &mut AnimationPlayer,
        &mut AnimationTransitions,
        &ModelAnimations,
        &WorldUnit,
    )>,
) {
    for (mut anim, mut player, mut transitions, anims, unit) in &mut units {
        // A model without a run plays its walk, scaled up; one without either stands.
        let clip = anims.find(anim.wanted.anim_id()).or_else(|| match anim.wanted {
            Gait::Run => anims.find(WALK),
            _ => None,
        });
        let Some(clip) = clip.or_else(|| anims.find(STAND)) else {
            continue;
        };
        if anim.playing != Some(anim.wanted) {
            // A first arm starts at full weight; later ones fade over the clip's blend-in time.
            let fade = if anim.playing.is_some() {
                Duration::from_secs_f32(clip.blend_time.max(0.0))
            } else {
                Duration::ZERO
            };
            let active = transitions.play(&mut player, clip.node, fade);
            if clip.looping {
                active.repeat();
            }
            anim.playing = Some(anim.wanted);
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
