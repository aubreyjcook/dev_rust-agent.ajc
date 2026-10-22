use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use rand::Rng;

pub trait Routine: Send + 'static {
    fn name(&self) -> &'static str;
    fn run(self: Box<Self>, stop: Arc<AtomicBool>);
}

pub type BoxedRoutine = Box<dyn Routine>;

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

pub fn sleep_interruptible(total_ms: u64, stop: &AtomicBool) {
    let mut slept = 0;
    while slept < total_ms && !stop.load(Ordering::Relaxed) {
        let chunk = 20.min(total_ms - slept);
        thread::sleep(Duration::from_millis(chunk));
        slept += chunk;
    }
}