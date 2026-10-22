//! Agent subroutines.
//!
//! A [`Routine`] is a self-contained unit of work the agent can start, track
//! and stop. Two things matter in v04:
//!
//! * Long-running routines poll an [`AtomicBool`] stop flag so the agent stays
//!   responsive.
//! * Every routine reports through a [`Log`] sink instead of printing. In
//!   standalone mode the sink is the process's stdout; in daemon mode it is the
//!   per-task buffer that remote clients read.

pub mod launcher;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use rand::Rng;

pub use launcher::{LaunchError, LaunchReport, Route, GOOGLE_URL};

/// Where a routine sends progress. Implemented by the daemon's per-task buffer
/// and by the standalone console sink.
pub trait Log: Send + Sync + 'static {
    fn line(&self, text: &str);
}

impl Log for std::io::Stdout {
    fn line(&self, text: &str) {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }
}

/// A sink that drops everything, for tests.
#[derive(Debug, Default, Clone, Copy)]
pub struct NullLog;

impl Log for NullLog {
    fn line(&self, _text: &str) {}
}

/// A cheap, cloneable handle to a log sink.
pub type LogHandle = Arc<dyn Log>;

/// Build a handle around any sink.
pub fn log_handle<L: Log>(log: L) -> LogHandle {
    Arc::new(log)
}

pub trait Routine: Send + 'static {
    fn name(&self) -> &'static str;
    fn run(self: Box<Self>, stop: Arc<AtomicBool>, log: LogHandle);
}

pub type BoxedRoutine = Box<dyn Routine>;

// ---------------------------------------------------------------- input bots

pub struct RepeatEnter {
    pub interval_ms: u64,
}

impl Routine for RepeatEnter {
    fn name(&self) -> &'static str { "repeat_enter" }

    fn run(self: Box<Self>, stop: Arc<AtomicBool>, log: LogHandle) {
        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => {
                log.line(&format!("[repeat_enter] Enigo init failed: {e}"));
                return;
            }
        };

        log.line(&format!("[repeat_enter] pressing Enter every {}ms", self.interval_ms));
        let mut presses: u64 = 0;
        while !stop.load(Ordering::Relaxed) {
            if let Err(e) = enigo.key(Key::Return, Direction::Click) {
                log.line(&format!("[repeat_enter] key error: {e}"));
            }
            presses += 1;
            sleep_interruptible(self.interval_ms, &stop);
        }
        log.line(&format!("[repeat_enter] stopped after {presses} presses"));
    }
}

pub struct RepeatEnterJitter {
    pub base_ms: u64,
    pub jitter_ms: u64,
}

impl Routine for RepeatEnterJitter {
    fn name(&self) -> &'static str { "repeat_enter_jitter" }

    fn run(self: Box<Self>, stop: Arc<AtomicBool>, log: LogHandle) {
        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => {
                log.line(&format!("[repeat_enter_jitter] Enigo init failed: {e}"));
                return;
            }
        };
        let mut rng = rand::thread_rng();

        log.line(&format!(
            "[repeat_enter_jitter] pressing Enter every {}ms +/- {}ms",
            self.base_ms, self.jitter_ms
        ));
        let mut presses: u64 = 0;
        while !stop.load(Ordering::Relaxed) {
            if let Err(e) = enigo.key(Key::Return, Direction::Click) {
                log.line(&format!("[repeat_enter_jitter] key error: {e}"));
            }
            presses += 1;

            let delta: i64 = if self.jitter_ms == 0 {
                0
            } else {
                rng.gen_range(-(self.jitter_ms as i64)..=(self.jitter_ms as i64))
            };
            let sleep_ms = ((self.base_ms as i64) + delta).max(1) as u64;

            sleep_interruptible(sleep_ms, &stop);
        }
        log.line(&format!("[repeat_enter_jitter] stopped after {presses} presses"));
    }
}

// ------------------------------------------------------------------- browser

/// Opens a browser on the system: default first, then Chrome, then Firefox.
///
/// Runs once per invocation, so it finishes on its own but is still tracked and
/// therefore stoppable like any other routine.
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

    fn run(self: Box<Self>, stop: Arc<AtomicBool>, log: LogHandle) {
        if stop.load(Ordering::Relaxed) {
            log.line("[open_browser] cancelled before launch");
            return;
        }

        let requested = launcher::target_url(self.url.as_deref());
        log.line(&format!("[open_browser] opening {requested}"));

        match launcher::open(self.url.as_deref()) {
            Ok(report) => {
                for line in &report.skipped {
                    log.line(&format!("[open_browser] skipped {line}"));
                }
                log.line(&format!(
                    "[open_browser] opened via {} ({})",
                    report.route.label(),
                    report.cmdline
                ));
            }
            Err(e) => log.line(&format!("[open_browser] ERROR {e}")),
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

    /// A routine must be able to report through any sink without printing.
    #[test]
    fn routines_log_through_the_sink() {
        struct Recorder(std::sync::Mutex<Vec<String>>);
        impl Log for Recorder {
            fn line(&self, text: &str) {
                self.0.lock().unwrap().push(text.to_string());
            }
        }

        let recorder = Arc::new(Recorder(std::sync::Mutex::new(Vec::new())));
        let sink: LogHandle = recorder.clone();
        let stop = Arc::new(AtomicBool::new(true));

        // Already-stopped flag: the browser routine reports cancellation and
        // must not touch the real launcher.
        Box::new(OpenBrowser::new(Some("https://example.test".into())))
            .run(stop, sink);

        let lines = recorder.0.lock().unwrap().clone();
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("cancelled"), "got: {lines:?}");
    }

    #[test]
    fn null_log_swallows_everything() {
        let log = log_handle(NullLog);
        log.line("nobody hears this");
    }
}
