//! Routines: the "do the work" half of the agent.
//!
//! A [`Routine`] is a self-contained unit of work the agent can start, track and
//! stop. In v0_01 this behaviour was an anonymous closure written at the call
//! site; here it is a **type**, so it can be named, stored in a `Box` and
//! dispatched through a trait.
//!
//! The contract every routine must honour:
//!
//! * it runs on its own thread;
//! * it polls the `stop` flag and returns promptly when it is set;
//! * it never sleeps for long in one call (see [`sleep_interruptible`]).
//!
//! This crate knows nothing about the `agent` crate — the dependency arrow
//! points the other way. That is what makes the routines reusable and testable.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

// `enigo` turns these calls into real operating-system input events.
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use rand::Rng;

/// A unit of work the agent can start and stop.
///
/// The `Send + 'static` supertrait bound says every implementor must be safe to
/// move to another thread and must own its data. The agent needs both, because
/// it moves routines onto threads — stating them here means `start_task` does
/// not have to repeat them.
pub trait Routine: Send + 'static {
    /// The task's name, also used as its registry key.
    ///
    /// `&'static str` means a string literal, which lives for the whole program
    /// and therefore needs no allocation or lifetime tracking.
    fn name(&self) -> &'static str;

    /// Run until the stop flag is set, then return.
    ///
    /// `self: Box<Self>` rather than `self`: through a `Box<dyn Routine>` there
    /// is no sized value to move, so the routine takes ownership of its own box.
    fn run(self: Box<Self>, stop: Arc<AtomicBool>);
}

/// A boxed trait object — "any routine". This is the type the agent stores and
/// dispatches, and the reason a single registry can hold several kinds of work.
pub type BoxedRoutine = Box<dyn Routine>;

// ---------------------------------------------------------------------------
// repeat_enter: press Enter on a fixed interval
// ---------------------------------------------------------------------------

/// Press Enter every `interval_ms` milliseconds.
///
/// The struct *is* the configuration: `interval_ms` used to be a function
/// parameter threaded through a free function, and is now owned by the routine
/// for as long as it runs.
pub struct RepeatEnter {
    // `pub` because the `agent` crate constructs this from outside the crate.
    // Removing it produces a helpful error (E0451: field is private).
    pub interval_ms: u64,
}

impl Routine for RepeatEnter {
    fn name(&self) -> &'static str { "repeat_enter" }

    fn run(self: Box<Self>, stop: Arc<AtomicBool>) {
        // Creating an `Enigo` can fail (no desktop, no input permission), so
        // report and give up rather than panicking on a background thread.
        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => { eprintln!("[repeat_enter] Enigo init failed: {e}"); return; }
        };

        // The main loop: check the flag, press, wait, repeat.
        while !stop.load(Ordering::Relaxed) {
            if let Err(e) = enigo.key(Key::Return, Direction::Click) {
                // A single failed keypress is not a reason to abandon the task.
                eprintln!("[repeat_enter] key error: {e}");
            }
            sleep_interruptible(self.interval_ms, &stop);
        }
        println!("[repeat_enter] stopped");
    }
}

// ---------------------------------------------------------------------------
// repeat_enter_jitter: press Enter on a *varying* interval
// ---------------------------------------------------------------------------

/// Press Enter every `base_ms` ± up to `jitter_ms` milliseconds.
///
/// The randomness makes the timing look less mechanical than a fixed interval.
pub struct RepeatEnterJitter {
    pub base_ms: u64,
    pub jitter_ms: u64,
}

impl Routine for RepeatEnterJitter {
    fn name(&self) -> &'static str { "repeat_enter_jitter" }

    fn run(self: Box<Self>, stop: Arc<AtomicBool>) {
        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => { eprintln!("[repeat_enter_jitter] Enigo init failed: {e}"); return; }
        };
        // One RNG per routine: cheap to create, and keeps the state local.
        let mut rng = rand::thread_rng();

        while !stop.load(Ordering::Relaxed) {
            if let Err(e) = enigo.key(Key::Return, Direction::Click) {
                eprintln!("[repeat_enter_jitter] key error: {e}");
            }

            // "base ± jitter" cannot be computed in `u64`, which has no negative
            // values. Cast to `i64`, do the signed arithmetic, then clamp and
            // cast back.
            let delta: i64 = if self.jitter_ms == 0 {
                0
            } else {
                // Inclusive range: -jitter ..= +jitter
                rng.gen_range(-(self.jitter_ms as i64)..=(self.jitter_ms as i64))
            };
            // `.max(1)` clamps zero and negatives away, because `sleep(0)` would
            // spin the CPU. `as` casts are unchecked, so the clamp matters.
            let sleep_ms = ((self.base_ms as i64) + delta).max(1) as u64;

            sleep_interruptible(sleep_ms, &stop);
        }
        println!("[repeat_enter_jitter] stopped");
    }
}

// ---------------------------------------------------------------------------
// shared helper
// ---------------------------------------------------------------------------

/// Sleep for `total_ms`, but return early if `stop` becomes set.
///
/// A single `thread::sleep(total_ms)` cannot be interrupted, so a `stop` would
/// appear to do nothing for up to the whole interval. Sleeping in 20 ms slices
/// and re-checking means a stop is noticed within about 20 ms.
///
/// The argument is `&AtomicBool`, not `Arc<AtomicBool>`: the caller owns the
/// `Arc`, and this function only needs to *look* at the flag, so it borrows.
pub fn sleep_interruptible(total_ms: u64, stop: &AtomicBool) {
    let mut slept = 0;
    while slept < total_ms && !stop.load(Ordering::Relaxed) {
        // Never overshoot: the last slice is whatever remains.
        let chunk = 20.min(total_ms - slept);
        thread::sleep(Duration::from_millis(chunk));
        slept += chunk;
    }
}
