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
    // One shared map for the whole process. Every task thread gets a clone of
    // this handle, so they all reach the same registry.
    let registry: Registry = Arc::new(Mutex::new(HashMap::new()));

    // Keep this banner in sync with the `match` below by hand — v0_01 has no
    // single source of truth for "what commands exist", which is one of the
    // things v0_02 fixes.
    println!("=== Agent ===");
    println!("Commands:");
    println!("  repeat_enter [ms]  - press Enter on interval");
    println!("  stop <name>        - stop a running task");
    println!("  list               - show running tasks");
    println!("  quit               - exit");
    println!();

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        // `lines()` yields `Result<String, io::Error>`: report by stopping,
        // because a broken stdin will never recover.
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        // Split the line into a command word plus its arguments. `parts`
        // borrows `line`, so the arguments are copied into owned `String`s
        // before `line` is reused on the next iteration.
        let mut parts = line.trim().split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let args: Vec<String> = parts.map(|s| s.to_string()).collect();

        match cmd {
            "" => {} // blank line: ignore it rather than reporting an error
            "repeat_enter" => {
                // Optional number with a default. Read the chain out loud:
                //   get(0)                  -> Option<&String>
                //   and_then(parse().ok())  -> Option<u64>  (parse error -> None)
                //   unwrap_or(1000)         -> u64
                let ms = args
                    .get(0)
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1000);
                // The behaviour is supplied here, as a closure. `move` copies
                // `ms` into the closure so it does not borrow the loop's locals.
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
                // Set every flag and leave the loop; each task thread notices
                // and returns, then this function ends and the process exits.
                stop_all(&registry);
                break;
            }
            other => println!("unknown command: {other}"),
        }
    }

    println!("bye");
}

/// Spawn a task in its own thread and register a stop signal for it.
///
/// `body` is "the work": any closure that accepts the stop flag. The three
/// bounds are what `thread::spawn` requires:
///
/// * `FnOnce(Arc<AtomicBool>)` — callable once, taking the stop flag;
/// * `Send` — safe to move to another thread;
/// * `'static` — owns its data, so it cannot borrow from this function's stack.
///
/// The `-> ()` is just the explicit way of writing "returns nothing".
fn start_task<F>(registry: &Registry, name: &str, body: F)
where
    F: FnOnce(Arc<AtomicBool>) -> () + Send + 'static,
{
    // Hold the lock across the check and the insert so two tasks cannot both
    // pass the "is it already running?" test.
    let mut reg = registry.lock().unwrap();
    if reg.contains_key(name) {
        println!("task '{name}' is already running");
        return;
    }

    let stop = Arc::new(AtomicBool::new(false));
    // `Arc::clone` = a second handle to the SAME flag, not a copy of its value.
    // One handle is stored in the registry so `stop_task` can reach it; the
    // other is moved into the thread below so the loop can read it.
    reg.insert(name.to_string(), TaskHandle { stop: Arc::clone(&stop) });
    drop(reg); // release the lock BEFORE spawning, or the new thread could deadlock

    // `name` is a `&str` borrowed from the caller, but the thread may outlive
    // that borrow, so take an owned copy for it to use.
    let name_owned = name.to_string();
    let registry_clone = Arc::clone(registry);

    thread::spawn(move || {
        // Run the closure supplied by the caller. Everything it needs was
        // captured when the closure was created.
        body(stop);
        // When the body returns, remove ourselves from the registry, otherwise
        // `list` would keep showing a task that has already stopped.
        let mut reg = registry_clone.lock().unwrap();
        reg.remove(&name_owned);
        println!("[task '{name_owned}' finished]");
    });

    println!("▶ started task '{name}'");
}

/// Ask one task to stop by setting its flag.
///
/// This does not wait: the task's own thread notices the flag the next time it
/// checks, which is why the message says "stopping", not "stopped".
fn stop_task(registry: &Registry, name: &str) {
    let reg = registry.lock().unwrap();
    match reg.get(name) {
        Some(handle) => {
            // `Relaxed` is enough: we only care whether the flag became true,
            // not how it orders against other memory.
            handle.stop.store(true, Ordering::Relaxed);
            println!("⏸ stopping task '{name}'");
        }
        None => println!("no such running task: '{name}'"),
    }
}

/// Print one line per running task.
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

/// Ask every task to stop. Used when the user quits.
fn stop_all(registry: &Registry) {
    let reg = registry.lock().unwrap();
    for handle in reg.values() {
        handle.stop.store(true, Ordering::Relaxed);
    }
}

// ---------- Subroutines ----------

/// The "repeat enter" subroutine we already had, now taking a stop flag.
///
/// Note what this function needs: the interval and the stop flag. There is no
/// struct holding them and no trait naming it — that is the gap v0_02 fills.
fn repeat_enter_loop(interval_ms: u64, stop: Arc<AtomicBool>) {
    // Creating an `Enigo` can fail (no desktop, no input permission). Report it
    // and return: a background thread should not panic.
    let mut enigo = match Enigo::new(&Settings::default()) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("[repeat_enter] failed to init Enigo: {e}");
            return;
        }
    };

    // Main loop: check the flag, press Enter, wait, repeat.
    while !stop.load(Ordering::Relaxed) {
        if let Err(e) = enigo.key(Key::Return, Direction::Click) {
            // One failed keypress is not a reason to give up entirely.
            eprintln!("[repeat_enter] key press error: {e}");
        }
        // Sleep in small chunks so we react to `stop` quickly: a single
        // `sleep(interval_ms)` could make `stop` appear dead for the whole
        // interval.
        let mut slept = 0;
        while slept < interval_ms && !stop.load(Ordering::Relaxed) {
            // Never overshoot: the final chunk is whatever remains.
            let chunk = 20.min(interval_ms - slept);
            thread::sleep(Duration::from_millis(chunk));
            slept += chunk;
        }
    }
    println!("[repeat_enter] stopped");
}

// (v0_02 moves this to the top of the file with the other imports. It works
// here, but mid-file imports are unusual style.)
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
        //
        // `u64` cannot be negative, so the arithmetic is done in `i64` and cast
        // back at the end. `gen_range` with `..=` is an *inclusive* range.
        let delta: i64 = if jitter_ms == 0 {
            0
        } else {
            rng.gen_range(-(jitter_ms as i64)..=(jitter_ms as i64))
        };

        // Clamp to avoid a negative or zero sleep: `sleep(0)` would busy-loop, and
        // the cast back to `u64` needs a non-negative value.
        let sleep_ms = ((base_ms as i64) + delta).max(1) as u64;

        // Sleep in small chunks so we react to `stop` quickly — the same loop as
        // `repeat_enter_loop`. v0_02 extracts this into `sleep_interruptible`.
        let mut slept = 0;
        while slept < sleep_ms && !stop.load(Ordering::Relaxed) {
            let chunk = 20.min(sleep_ms - slept);
            thread::sleep(Duration::from_millis(chunk));
            slept += chunk;
        }
    }
    println!("[repeat_enter_jitter] stopped");
}