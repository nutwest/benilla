//! Deviation: the camera's glide from the old character to the new on a party swap
//! (`crate::net`'s `party_swap`). 1.12 changes the played character only at a world entry, behind
//! a loading screen; a swap in place would otherwise cut the camera across in one frame. For
//! [`BLEND_SECS`] after the swap, the camera [`control`](super::control) placed behind the new
//! body is offset back toward the old one by the gap between the two bodies, eased out to none,
//! so the orbit, its collision and the player's input stay the controller's.

use std::sync::atomic::{AtomicBool, Ordering};

use benilla_world::view::WorldCamera;
use bevy::prelude::*;

/// Runs after what places the world camera each frame; a consumer that takes the camera over
/// ([`yield_camera`]) orders after it and reads the third-person pose off the `Transform`.
#[derive(SystemSet, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CameraPlaced;

static YIELDED: AtomicBool = AtomicBool::new(false);

/// Deviation: 1.12's camera always follows its unit. While yielded a consumer owns the world
/// camera and the keys and mouse it reads: `control` still places the third-person pose, which the
/// consumer overwrites, the mouse turns no body, and the bindings of the movement keys, the right
/// button and the wheel don't run.
pub fn yield_camera(yielded: bool) {
    YIELDED.store(yielded, Ordering::Relaxed);
}

/// Whether a consumer holds the camera.
pub(crate) fn camera_yielded() -> bool {
    YIELDED.load(Ordering::Relaxed)
}

/// How long the glide takes.
pub(crate) const BLEND_SECS: f32 = 0.25;

/// A swap's glide: the old body minus the new, at the swap, and when it began.
#[derive(Resource, Default)]
pub(crate) struct SwapBlend(Option<(Vec3, f32)>);

impl SwapBlend {
    /// Glide the camera from `from` (the old body) to `to` (the new) from `now`.
    pub(crate) fn start(&mut self, from: Vec3, to: Vec3, now: f32) {
        self.0 = Some((from - to, now));
    }
}

/// Ease in and out: slow off the old character and onto the new.
fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The share of the gap still applied `elapsed` seconds into the glide, none once it ends.
pub(crate) fn remaining(elapsed: f32) -> f32 {
    1.0 - smoothstep(elapsed / BLEND_SECS)
}

/// After [`super::control`]: offset the world camera by the glide's remaining gap.
pub(super) fn apply_swap_blend(
    mut blend: ResMut<SwapBlend>,
    time: Res<Time<Real>>,
    mut cameras: Query<&mut Transform, With<WorldCamera>>,
) {
    let Some((gap, start)) = blend.0 else {
        return;
    };
    // A camera on loan has nothing to glide: the consumer blends back to the new character.
    if camera_yielded() {
        blend.0 = None;
        return;
    }
    let elapsed = time.elapsed_secs() - start;
    if elapsed >= BLEND_SECS {
        blend.0 = None;
        return;
    }
    let offset = gap * remaining(elapsed);
    for mut camera in &mut cameras {
        camera.translation += offset;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_glide_starts_whole_and_ends_at_none() {
        assert_eq!(remaining(0.0), 1.0);
        assert_eq!(remaining(BLEND_SECS), 0.0);
        assert_eq!(remaining(BLEND_SECS * 2.0), 0.0);
        let half = remaining(BLEND_SECS / 2.0);
        assert!((half - 0.5).abs() < 1e-6, "half way, half the gap: {half}");
        assert!(remaining(0.02) > 0.98, "it eases off the old character");
    }
}
