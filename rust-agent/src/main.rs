use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn main() {
    println!("=== Auto-Enter Agent ===");
    println!("Commands:");
    println!("  start [ms]  - start pressing Enter every <ms> milliseconds (default 1000)");
    println!("  stop        - stop pressing Enter");
    println!("  status      - show current state");
    println!("  quit        - exit");
    println!();

    let running = Arc::new(AtomicBool::new(false));
    let interval_ms = Arc::new(AtomicU64::new(1000));

    // Worker thread: presses Enter on interval when `running` is true
    {
        let running = Arc::clone(&running);
        let interval_ms = Arc::clone(&interval_ms);
        thread::spawn(move || {
            let mut enigo = match Enigo::new(&Settings::default()) {
                Ok(e) => e,
                Err(e) => {
                    eprintln!("[worker] failed to init Enigo: {e}");
                    return;
                }
            };
            loop {
                if running.load(Ordering::Relaxed) {
                    if let Err(e) = enigo.key(Key::Return, Direction::Click) {
                        eprintln!("[worker] key press error: {e}");
                    }
                    let ms = interval_ms.load(Ordering::Relaxed);
                    thread::sleep(Duration::from_millis(ms));
                } else {
                    thread::sleep(Duration::from_millis(50));
                }
            }
        });
    }

    // Main loop: read commands from stdin
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let mut parts = line.trim().split_whitespace();
        let cmd = parts.next().unwrap_or("");

        match cmd {
            "start" => {
                let ms = parts
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(1000);
                interval_ms.store(ms, Ordering::Relaxed);
                running.store(true, Ordering::Relaxed);
                println!("▶ started, pressing Enter every {ms} ms");
            }
            "stop" => {
                running.store(false, Ordering::Relaxed);
                println!("⏸ stopped");
            }
            "status" => {
                let state = if running.load(Ordering::Relaxed) {
                    "running"
                } else {
                    "stopped"
                };
                println!("state: {state}, interval: {} ms", interval_ms.load(Ordering::Relaxed));
            }
            "quit" | "exit" => {
                running.store(false, Ordering::Relaxed);
                println!("bye");
                break;
            }
            "" => {}
            other => println!("unknown command: {other}"),
        }
    }
}