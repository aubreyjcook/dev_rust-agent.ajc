//! Agent subroutines.
//!
//! A [`Routine`] is a self-contained unit of work the agent can start, track
//! and stop. Long-running routines poll an [`AtomicBool`] stop flag so the
//! agent stays responsive.

pub mod launcher;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use rand::Rng;

pub use launcher::{LaunchError, LaunchReport, Route, GOOGLE_URL};

pub trait Routine: Send + 'static {
    fn name(&self) -> &'static str;
    fn run(self: Box<Self>, stop: Arc<AtomicBool>);
}

pub type BoxedRoutine = Box<dyn Routine>;

// ---------------------------------------------------------------- input bots

pub struct RepeatEnter {
    pub interval_ms: u64,
}

impl Routine for RepeatEnter {
    fn name(&self) -> &'static str { "repeat_enter" }

    fn run(self: Box<Self>, stop: Arc<AtomicBool>) {
        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => { eprintln!("[repeat_enter] Enigo init failed: {e}"); return; }
        };

        while !stop.load(Ordering::Relaxed) {
            if let Err(e) = enigo.key(Key::Return, Direction::Click) {
                eprintln!("[repeat_enter] key error: {e}");
            }
            sleep_interruptible(self.interval_ms, &stop);
        }
        println!("[repeat_enter] stopped");
    }
}

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
        let mut rng = rand::thread_rng();

        while !stop.load(Ordering::Relaxed) {
            if let Err(e) = enigo.key(Key::Return, Direction::Click) {
                eprintln!("[repeat_enter_jitter] key error: {e}");
            }

            let delta: i64 = if self.jitter_ms == 0 {
                0
            } else {
                rng.gen_range(-(self.jitter_ms as i64)..=(self.jitter_ms as i64))
            };
            let sleep_ms = ((self.base_ms as i64) + delta).max(1) as u64;

            sleep_interruptible(sleep_ms, &stop);
        }
        println!("[repeat_enter_jitter] stopped");
    }
}

// ------------------------------------------------------------------- browser

/// Opens a browser on the system: default first, then Chrome, then Firefox.
///
/// Runs once per invocation, so it finishes on its own but is still tracked and
/// therefore stoppable like any other routine. Concurrent calls are refused by
/// the agent, which is what stops a flood of tabs.
pub struct OpenBrowser {
    /// `None` means "the default landing page", which is Google.
    pub url: Option<String>,
}

impl OpenBrowser {
    pub fn new(url: Option<String>) -> Self {
        Self { url }
    }
}

impl Routine for OpenBrowser {
    fn name(&self) -> &'static str { "open_browser" }

    fn run(self: Box<Self>, stop: Arc<AtomicBool>) {
        if stop.load(Ordering::Relaxed) {
            println!("[open_browser] cancelled before launch");
            return;
        }

        let requested = launcher::target_url(self.url.as_deref());
        println!("[open_browser] opening {requested}");

        match launcher::open(self.url.as_deref()) {
            Ok(report) => {
                for line in &report.skipped {
                    println!("[open_browser] skipped {line}");
                }
                println!(
                    "[open_browser] opened via {} ({})",
                    report.route.label(),
                    report.cmdline
                );
            }
            Err(e) => eprintln!("[open_browser] {e}"),
        }
    }
}

// --------------------------------------------------------------------- utils

pub fn sleep_interruptible(total_ms: u64, stop: &AtomicBool) {
    let mut slept = 0;
    while slept < total_ms && !stop.load(Ordering::Relaxed) {
        let chunk = 20.min(total_ms - slept);
        thread::sleep(Duration::from_millis(chunk));
        slept += chunk;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_browser_defaults_to_google() {
        let routine = OpenBrowser::new(None);
        assert_eq!(routine.name(), "open_browser");
        assert_eq!(launcher::target_url(None), GOOGLE_URL);
    }

    #[test]
    fn sleep_interruptible_returns_when_stopped() {
        let stop = AtomicBool::new(true);
        let start = std::time::Instant::now();
        sleep_interruptible(10_000, &stop);
        assert!(start.elapsed() < Duration::from_millis(500));
    }
}
