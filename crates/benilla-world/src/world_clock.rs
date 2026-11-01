//! Deviation: 1.12 has no pause. A consumer that adds one (the party crate) stops this clock, and
//! everything that keeps world time with it: splines, casts, cooldowns, auras and transports read
//! [`now`] instead of `Instant::now()`, and seconds stores read `Res<Time<WorldClock>>` instead of
//! `Res<Time<Real>>`.
//!
//! It equals the wall clock until the first pause, holds still while paused, and after that runs
//! behind the wall clock by the total paused. So a stored deadline stays good across a pause, with
//! nothing to shift on resume. It is global because many readers are free functions with no
//! resource access. `Time<Virtual>` stops separately, for `Res<Time>`'s readers.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The `Time` context of the world's seconds: [`crate::frame_pace::FrameStep`]'s world step, so a
/// seconds stamp and a [`FrameStep::world_elapsed`](crate::frame_pace::FrameStep) reading agree.
#[derive(Default, Clone, Copy, Debug)]
pub struct WorldClock;

/// [`PauseState::frozen_ns`] while running.
const RUNNING: u64 = u64::MAX;

/// The pause, in nanoseconds of wall clock since an origin. Two words, read without a lock: the
/// world thread writes them in an order that keeps every reader on a good value.
struct PauseState {
    /// The world reading the clock holds at, [`RUNNING`] while it runs.
    frozen_ns: AtomicU64,
    /// The time paused so far, by finished pauses.
    paused_ns: AtomicU64,
}

impl PauseState {
    const fn new() -> Self {
        Self {
            frozen_ns: AtomicU64::new(RUNNING),
            paused_ns: AtomicU64::new(0),
        }
    }

    /// The world reading at `real_ns` of wall clock.
    fn world_ns(&self, real_ns: u64) -> u64 {
        match self.frozen_ns.load(Ordering::SeqCst) {
            RUNNING => real_ns.saturating_sub(self.paused_ns.load(Ordering::SeqCst)),
            frozen => frozen,
        }
    }

    fn is_paused(&self) -> bool {
        self.frozen_ns.load(Ordering::SeqCst) != RUNNING
    }

    /// Stops or starts the clock at `real_ns` of wall clock; no change when already so.
    fn set_paused(&self, paused: bool, real_ns: u64) {
        if paused == self.is_paused() {
            return;
        }
        if paused {
            let at = self.world_ns(real_ns);
            self.frozen_ns.store(at, Ordering::SeqCst);
        } else {
            // The total first: a reader that sees the clock running must see the new total.
            let frozen = self.frozen_ns.load(Ordering::SeqCst);
            self.paused_ns
                .store(real_ns.saturating_sub(frozen), Ordering::SeqCst);
            self.frozen_ns.store(RUNNING, Ordering::SeqCst);
        }
    }
}

static PAUSE: PauseState = PauseState::new();

/// The clock's origin, this module's first use.
fn origin() -> Instant {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    *ORIGIN.get_or_init(Instant::now)
}

fn real_ns(at: Instant) -> u64 {
    at.saturating_duration_since(origin()).as_nanos() as u64
}

/// How far the world clock stands behind the wall-clock instant `at`.
pub fn lag_at(at: Instant) -> Duration {
    let real = real_ns(at);
    Duration::from_nanos(real.saturating_sub(PAUSE.world_ns(real)))
}

/// The world clock's reading of a wall-clock stamp, as the IO thread takes one on a packet.
pub fn of(at: Instant) -> Instant {
    at.checked_sub(lag_at(at)).unwrap_or(at)
}

/// The world clock now, for everything that keeps time with the world.
pub fn now() -> Instant {
    origin() + Duration::from_nanos(PAUSE.world_ns(real_ns(Instant::now())))
}

/// Whether the world is paused.
pub fn is_paused() -> bool {
    PAUSE.is_paused()
}

/// Stops or starts the world clock. The caller also pauses `Time<Virtual>`.
pub fn set_paused(paused: bool) {
    PAUSE.set_paused(paused, real_ns(Instant::now()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_reads_the_wall_clock_until_a_pause() {
        let p = PauseState::new();
        assert_eq!(p.world_ns(100), 100);
        assert_eq!(p.world_ns(250), 250);
        assert!(!p.is_paused());
    }

    #[test]
    fn it_holds_through_a_pause_and_then_runs_behind_by_it() {
        let p = PauseState::new();
        p.set_paused(true, 100);
        assert!(p.is_paused());
        assert_eq!(p.world_ns(100), 100);
        assert_eq!(p.world_ns(900), 100, "held whatever the wall clock does");
        p.set_paused(false, 900);
        assert_eq!(p.world_ns(900), 100, "resumes from where it stopped");
        assert_eq!(p.world_ns(1000), 200, "and runs at the wall clock's rate");
    }

    #[test]
    fn pauses_add_up() {
        let p = PauseState::new();
        p.set_paused(true, 100);
        p.set_paused(false, 300);
        p.set_paused(true, 400);
        assert_eq!(p.world_ns(1000), 200);
        p.set_paused(false, 1000);
        assert_eq!(p.world_ns(1100), 300);
    }

    #[test]
    fn asking_for_the_state_it_has_changes_nothing() {
        let p = PauseState::new();
        p.set_paused(false, 100);
        assert_eq!(p.world_ns(200), 200);
        p.set_paused(true, 200);
        p.set_paused(true, 500);
        assert_eq!(p.world_ns(800), 200);
    }
}
