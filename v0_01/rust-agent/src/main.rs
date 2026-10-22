use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::collections::HashMap;
use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// A handle to a running task, used to stop it later.
struct TaskHandle {
    stop: Arc<AtomicBool>,
}

/// Shared state: which tasks are currently running, keyed by name.
type Registry = Arc<Mutex<HashMap<String, TaskHandle>>>;

fn main() {
    let registry: Registry = Arc::new(Mutex::new(HashMap::new()));

    println!("=== Agent ===");
    println!("Commands:");
    println!("  repeat_enter [ms]  - press Enter on interval");
    println!("  stop <name>        - stop a running task");
    println!("  list               - show running tasks");
    println!("  quit               - exit");
    println!();

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        let mut parts = line.trim().split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let args: Vec<String> = parts.map(|s| s.to_string()).collect();

        match cmd {
            "" => {}
            "repeat_enter" => {
                let ms = args
                    .get(0)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1000);
                start_task(&registry, "repeat_enter", move |stop| {
                    repeat_enter_loop(ms, stop)
                });
            }
            "repeat_enter_jitter" => {
                let base = args
                    .get(0)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1000);
                let jitter = args
                    .get(1)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(100);
                start_task(&registry, "repeat_enter_jitter", move |stop| {
                    repeat_enter_jitter_loop(base, jitter, stop)
                });
            }
            "stop" => {
                let name = args.get(0).map(|s| s.as_str()).unwrap_or("");
                stop_task(&registry, name);
            }
            "list" => list_tasks(&registry),
            "quit" | "exit" => {
                stop_all(&registry);
                break;
            }
            other => println!("unknown command: {other}"),
        }
    }

    println!("bye");
}

/// Spawn a task in its own thread and register a stop signal for it.
fn start_task<F>(registry: &Registry, name: &str, body: F)
where
    F: FnOnce(Arc<AtomicBool>) -> () + Send + 'static,
{
    let mut reg = registry.lock().unwrap();
    if reg.contains_key(name) {
        println!("task '{name}' is already running");
        return;
    }

    let stop = Arc::new(AtomicBool::new(false));
    reg.insert(name.to_string(), TaskHandle { stop: Arc::clone(&stop) });
    drop(reg); // release the lock before spawning

    let name_owned = name.to_string();
    let registry_clone = Arc::clone(registry);

    thread::spawn(move || {
        body(stop);
        // When the body returns, remove ourselves from the registry.
        let mut reg = registry_clone.lock().unwrap();
        reg.remove(&name_owned);
        println!("[task '{name_owned}' finished]");
    });

    println!("▶ started task '{name}'");
}

fn stop_task(registry: &Registry, name: &str) {
    let reg = registry.lock().unwrap();
    match reg.get(name) {
        Some(handle) => {
            handle.stop.store(true, Ordering::Relaxed);
            println!("⏸ stopping task '{name}'");
        }
        None => println!("no such running task: '{name}'"),
    }
}

fn list_tasks(registry: &Registry) {
    let reg = registry.lock().unwrap();
    if reg.is_empty() {
        println!("(no running tasks)");
    } else {
        for name in reg.keys() {
            println!("- {name}");
        }
    }
}

fn stop_all(registry: &Registry) {
    let reg = registry.lock().unwrap();
    for handle in reg.values() {
        handle.stop.store(true, Ordering::Relaxed);
    }
}

// ---------- Subroutines ----------

/// The "repeat enter" subroutine we already had, now taking a stop flag.
fn repeat_enter_loop(interval_ms: u64, stop: Arc<AtomicBool>) {
    let mut enigo = match Enigo::new(&Settings::default()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[repeat_enter] failed to init Enigo: {e}");
            return;
        }
    };

    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = enigo.key(Key::Return, Direction::Click) {
            eprintln!("[repeat_enter] key press error: {e}");
        }
        // Sleep in small chunks so we react to `stop` quickly.
        let mut slept = 0;
        while slept < interval_ms && !stop.load(Ordering::Relaxed) {
            let chunk = 20.min(interval_ms - slept);
            thread::sleep(Duration::from_millis(chunk));
            slept += chunk;
        }
    }
    println!("[repeat_enter] stopped");
}

use rand::Rng;

/// Press Enter at `base_ms` ± up to `jitter_ms` between presses.
fn repeat_enter_jitter_loop(base_ms: u64, jitter_ms: u64, stop: Arc<AtomicBool>) {
    let mut enigo = match Enigo::new(&Settings::default()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[repeat_enter_jitter] failed to init Enigo: {e}");
            return;
        }
    };

    let mut rng = rand::thread_rng();

    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = enigo.key(Key::Return, Direction::Click) {
            eprintln!("[repeat_enter_jitter] key press error: {e}");
        }

        // Compute a random sleep duration: base ± jitter.
        let delta: i64 = if jitter_ms == 0 {
            0
        } else {
            rng.gen_range(-(jitter_ms as i64)..=(jitter_ms as i64))
        };

        // Clamp to avoid negative sleep (which would panic).
        let sleep_ms = ((base_ms as i64) + delta).max(1) as u64;

        // Sleep in small chunks so we react to `stop` quickly.
        let mut slept = 0;
        while slept < sleep_ms && !stop.load(Ordering::Relaxed) {
            let chunk = 20.min(sleep_ms - slept);
            thread::sleep(Duration::from_millis(chunk));
            slept += chunk;
        }
    }
    println!("[repeat_enter_jitter] stopped");
}