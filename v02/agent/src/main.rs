use std::collections::HashMap;
use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use routines::{BoxedRoutine, RepeatEnter, RepeatEnterJitter};

struct TaskHandle {
    stop: Arc<AtomicBool>,
}

type Registry = Arc<Mutex<HashMap<&'static str, TaskHandle>>>;

fn main() {
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
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let mut parts = line.trim().split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let args: Vec<String> = parts.map(str::to_string).collect();

        match cmd {
            "" => {}
            "repeat_enter" => {
                let ms = args.get(0).and_then(|s| s.parse().ok()).unwrap_or(1000);
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
                stop_all(&registry);
                break;
            }
            other => println!("unknown command: {other}"),
        }
    }
    println!("bye");
}

/// The agent's entire job: spawn, track, stop.
fn start_task(registry: &Registry, routine: BoxedRoutine) {
    let name = routine.name();

    let mut reg = registry.lock().unwrap();
    if reg.contains_key(name) {
        println!("task '{name}' is already running");
        return;
    }

    let stop = Arc::new(AtomicBool::new(false));
    reg.insert(name, TaskHandle { stop: Arc::clone(&stop) });
    drop(reg);

    let registry_clone = Arc::clone(registry);
    thread::spawn(move || {
        routine.run(stop);
        registry_clone.lock().unwrap().remove(name);
        println!("[task '{name}' finished]");
    });

    println!("▶ started task '{name}'");
}

fn stop_task(registry: &Registry, name: &str) {
    let reg = registry.lock().unwrap();
    match reg.get(name) {
        Some(h) => {
            h.stop.store(true, Ordering::Relaxed);
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
    for h in reg.values() {
        h.stop.store(true, Ordering::Relaxed);
    }
}