//! Agent v0_02 — the console agent, now built on a `routines` library.
//!
//! This file is deliberately small. It knows how to *decide* and *track*, and
//! nothing about *how* any routine works:
//!
//!   * decide — read a line, split it, `match` the first word (see `main`).
//!   * track  — keep a map of running tasks so they can be listed and stopped
//!              (see `TaskHandle`, `Registry` and `start_task`).
//!
//! Everything a routine actually does — pressing keys — lives in the `routines`
//! crate. That split is the whole point of this version: v0_01 kept the
//! behaviour inline as closures, which made it impossible to name, store or
//! reuse.
//!
//! New in v0_02:
//!   * a `Routine` trait and the `BoxedRoutine` trait object replace v0_01's
//!     `F: FnOnce(…)` bound;
//!   * the registry key is a `&'static str` taken from the routine itself;
//!   * `sleep_interruptible` is shared from the library instead of copy-pasted.

use std::collections::HashMap;
use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

// `BoxedRoutine` is `Box<dyn Routine>`: one type that can hold any routine.
// The concrete structs are only imported here because this file *constructs*
// them — every other function below accepts the trait object instead.
use routines::{BoxedRoutine, RepeatEnter, RepeatEnterJitter};

/// What the agent knows about a running task.
///
/// Just the stop flag: the behaviour lives in the routine's own thread, so the
/// registry only needs a way to ask that thread to finish.
struct TaskHandle {
    stop: Arc<AtomicBool>,
}

/// Shared state: which tasks are running, keyed by name.
///
/// `Arc` lets several threads own the map; `Mutex` lets only one of them touch
/// it at a time. Both are needed — `Arc` answers "who owns it", `Mutex` answers
/// "who may use it right now".
///
/// The key is `&'static str` because `Routine::name` returns a string literal
/// that lives for the whole program, so looking a task up needs no allocation.
type Registry = Arc<Mutex<HashMap<&'static str, TaskHandle>>>;

fn main() {
    // One shared registry for the whole process. Cloned (not copied) into each
    // task thread so they all reach the same map.
    let registry: Registry = Arc::new(Mutex::new(HashMap::new()));

    println!("=== Agent ===");
    println!("Commands:");
    println!("  repeat_enter [ms]");
    println!("  repeat_enter_jitter [base_ms] [jitter_ms]");
    println!("  stop <name>");
    println!("  list");
    println!("  quit");
    println!();

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        // `lines()` yields `Result<String, io::Error>`. A failure here means
        // stdin is unusable (a closed pipe, for example), so stop rather than
        // spin forever on errors.
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };

        // The whole front end: split on whitespace, treat the first word as the
        // command and collect the rest as arguments.
        //
        // `parts` borrows `line`, but `args` is collected into owned `String`s,
        // so nothing in `args` keeps `line` alive.
        let mut parts = line.trim().split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let args: Vec<String> = parts.map(str::to_string).collect();

        match cmd {
            "" => {} // blank line: do nothing, do not complain
            "repeat_enter" => {
                // Optional numeric argument with a default, read as a chain:
                //   get(0)                  -> Option<&String>
                //   .and_then(parse().ok()) -> Option<u64>   (a parse error becomes None)
                //   .unwrap_or(1000)        -> u64
                let ms = args.get(0).and_then(|s| s.parse().ok()).unwrap_or(1000);
                // Construct the routine and box it. From here on, this file only
                // knows that it holds *some* `BoxedRoutine`.
                start_task(&registry, Box::new(RepeatEnter { interval_ms: ms }));
            }
            "repeat_enter_jitter" => {
                let base = args.get(0).and_then(|s| s.parse().ok()).unwrap_or(1000);
                let jitter = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100);
                start_task(
                    &registry,
                    Box::new(RepeatEnterJitter { base_ms: base, jitter_ms: jitter }),
                );
            }
            "stop" => stop_task(&registry, args.get(0).map(String::as_str).unwrap_or("")),
            "list" => list_tasks(&registry),
            "quit" | "exit" => {
                // Ask everything to stop, then leave. The task threads notice
                // their flag and exit on their own; the process ends when `main`
                // returns.
                stop_all(&registry);
                break;
            }
            other => println!("unknown command: {other}"),
        }
    }
    println!("bye");
}

/// The agent's entire job: spawn, track, stop.
///
/// `routine` is taken by value because the new thread owns it from here on.
fn start_task(registry: &Registry, routine: BoxedRoutine) {
    // Ask the routine what it is called. Because the name comes from the
    // routine itself, the registry key and the routine can never disagree —
    // which is exactly the bug v0_01 had by passing the name separately.
    let name = routine.name();

    // Hold the lock across the check *and* the insert. That is what makes "is it
    // already running?" reliable: two threads cannot both see "not present" and
    // then both insert.
    let mut reg = registry.lock().unwrap();
    if reg.contains_key(name) {
        println!("task '{name}' is already running");
        return; // the guard is dropped here, releasing the lock
    }

    // The stop flag: `false` = keep going, `true` = please stop.
    let stop = Arc::new(AtomicBool::new(false));
    // `Arc::clone` makes a second handle to the SAME flag, not a copy. One
    // handle goes into the map (so `stop_task` can reach it); the other moves
    // into the thread below (so its loop can read it).
    reg.insert(name, TaskHandle { stop: Arc::clone(&stop) });
    drop(reg); // release the lock BEFORE spawning, to avoid a deadlock

    let registry_clone = Arc::clone(registry);
    thread::spawn(move || {
        // `move` moves `routine`, `stop` and `registry_clone` into the closure,
        // because this thread may outlive the current function.
        routine.run(stop);

        // The task has finished, so remove its own entry; otherwise `list`
        // would keep showing it forever. (v0_04 moves this into the registry so
        // that a routine which ends on its own cannot leave a stale entry.)
        registry_clone.lock().unwrap().remove(name);
        println!("[task '{name}' finished]");
    });

    println!("▶ started task '{name}'");
}

/// Ask one task to stop.
///
/// This only sets the flag — the task's own thread does the actual stopping a
/// few milliseconds later, when it next checks.
fn stop_task(registry: &Registry, name: &str) {
    let reg = registry.lock().unwrap();
    match reg.get(name) {
        Some(h) => {
            // `Relaxed` is enough here: we only need "has this become true?",
            // not any ordering with other memory.
            h.stop.store(true, Ordering::Relaxed);
            println!("⏸ stopping task '{name}'");
        }
        None => println!("no such running task: '{name}'"),
    }
}

/// Print the names of the running tasks.
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

/// Ask every task to stop. Used on the way out.
fn stop_all(registry: &Registry) {
    let reg = registry.lock().unwrap();
    for h in reg.values() {
        h.stop.store(true, Ordering::Relaxed);
    }
}
