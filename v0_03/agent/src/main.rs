//! Agent v03 — console agent with basic web interactivity.
//!
//! One command surface, two front doors: stdin and a loopback HTTP server.
//! Both funnel into [`dispatch`], so a command behaves identically no matter
//! where it came from.
//!
//! New in v03: the `open_browser` subroutine, which opens a browser by trying
//! the system default first, then Chrome, then Firefox, landing on Google.

mod console;

use std::collections::HashMap;
use std::io::{self, BufRead};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use agent_web::{AgentStatus, AgentWeb, Config as WebConfig, Dispatch, Reply, StatusFn};
use routines::{BoxedRoutine, OpenBrowser, RepeatEnter, RepeatEnterJitter, GOOGLE_URL};

struct TaskHandle {
    stop: Arc<AtomicBool>,
}

type Registry = Arc<Mutex<HashMap<String, TaskHandle>>>;

/// Shared agent state handed to the dispatcher.
struct Agent {
    registry: Registry,
    allow_spawn: bool,
    started: Instant,
}

fn main() {
    let opts = console::Options::parse(std::env::args().skip(1).collect());

    if let Some(error) = &opts.error {
        if error != "help requested" {
            eprintln!("{error}\n");
        }
        print!("{}", console::USAGE);
        std::process::exit(if error == "help requested" { 0 } else { 2 });
    }

    if let Some(url) = opts.one_shot_browser.clone() {
        // `agent --open` / `agent --open https://…`: launch and exit.
        match routines::launcher::open(Some(&url)) {
            Ok(report) => {
                for line in &report.skipped {
                    println!("skipped {line}");
                }
                println!("opened {} via {}", report.url, report.route.label());
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let agent = Arc::new(Agent {
        registry: Arc::new(Mutex::new(HashMap::new())),
        allow_spawn: !opts.no_open,
        started: Instant::now(),
    });

    console::banner(&opts);

    // ---- web front door -----------------------------------------------------
    if !opts.no_web {
        let dispatch_agent = Arc::clone(&agent);
        let dispatch: Dispatch = Arc::new(move |cmd: &str| dispatch(&dispatch_agent, cmd));

        let status_agent = Arc::clone(&agent);
        let status: StatusFn = Arc::new(move || AgentStatus {
            uptime_secs: status_agent.started.elapsed().as_secs(),
            tasks: task_names(&status_agent.registry),
            spawning: false,
            allow_spawn: status_agent.allow_spawn,
        });

        match AgentWeb::start(
            WebConfig {
                port: opts.port,
                allow_spawn: !opts.no_open,
                open_command: format!("/api/cmd?cmd=browser"),
            },
            dispatch,
            status,
        ) {
            Ok(web) => {
                let addr = web.addr();
                println!("web control : http://{addr}/");
                println!("web api     : http://{addr}/api/status");
                println!();
                thread::spawn(move || {
                    if let Err(e) = web.serve() {
                        eprintln!("[web] server stopped: {e}");
                    }
                });
                if opts.open_web {
                    match routines::launcher::open(Some(&format!("http://{addr}/"))) {
                        Ok(report) => println!(
                            "opened dashboard via {} ({})",
                            report.route.label(),
                            report.cmdline
                        ),
                        Err(e) => eprintln!("could not open dashboard: {e}"),
                    }
                }
            }
            Err(e) => {
                eprintln!("[web] could not bind 127.0.0.1:{}: {e}", opts.port);
                eprintln!("[web] continuing with console only");
            }
        }
    }

    // ---- console front door -------------------------------------------------
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let cmd = line.trim();
        if cmd.is_empty() || cmd.starts_with('#') {
            continue;
        }
        let reply = dispatch(&agent, cmd);
        let is_quit = cmd == "quit" || cmd == "exit";
        // `quit` already gets its farewell from the loop's own "bye".
        if !reply.output.is_empty() && !is_quit {
            println!("{}", reply.output);
        }
        if is_quit {
            break;
        }
    }

    stop_all(&agent.registry);
    println!("bye");
}

/// The single decision point for every command, console or web.
fn dispatch(agent: &Agent, input: &str) -> Reply {
    let mut parts = input.trim().split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let args: Vec<String> = parts.map(str::to_string).collect();

    match cmd {
        "" => Reply::ok(""),
        "help" | "?" | "/help" => Reply::ok(help_text()),
        "status" => Reply::ok(status_text(agent)),

        "list" | "tasks" => Reply::ok(list_tasks(&agent.registry)),

        // ---- the v03 browser subroutine -------------------------------------
        "browser" | "browse" | "open_browser" | "open" => {
            open_browser(agent, args.first().map(String::as_str))
        }
        "browsers" | "browser_chain" => Reply::ok(format!(
            "browser fallback ladder (tried in this order):\n  {}",
            routines::launcher::describe_ladder().join("\n  ")
        )),

        // ---- existing subroutines ------------------------------------------
        "repeat_enter" => {
            let ms = args.first().and_then(|s| s.parse().ok()).unwrap_or(1000);
            start_task(
                agent,
                format!("repeat_enter {ms}"),
                Box::new(RepeatEnter { interval_ms: ms }),
            )
        }
        "repeat_enter_jitter" => {
            let base = args.first().and_then(|s| s.parse().ok()).unwrap_or(1000);
            let jitter = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100);
            start_task(
                agent,
                format!("repeat_enter_jitter {base} {jitter}"),
                Box::new(RepeatEnterJitter { base_ms: base, jitter_ms: jitter }),
            )
        }
        "stop" => stop_task(&agent.registry, args.first().map(String::as_str).unwrap_or("")),
        "stopall" | "stop_all" => {
            let n = stop_all(&agent.registry);
            Reply::ok(format!("stopping {n} task(s)"))
        }
        "quit" | "exit" => Reply::ok("bye"),

        other => Reply::err(format!("unknown command: {other}")),
    }
}

fn open_browser(agent: &Agent, url: Option<&str>) -> Reply {
    if !agent.allow_spawn {
        return Reply::err("browser launching is disabled (started with --no-open)");
    }

    let target = routines::launcher::target_url(url);

    // One browser at a time: this is what keeps a repeated click from opening
    // a dozen windows.
    {
        let mut reg = agent.registry.lock().unwrap_or_else(|e| e.into_inner());
        let busy = reg.keys().any(|k| k.starts_with("open_browser"));
        if busy {
            return Reply::err(format!(
                "a browser launch is already in flight (target {target})"
            ));
        }
        reg.insert(
            "open_browser".to_string(),
            TaskHandle { stop: Arc::new(AtomicBool::new(false)) },
        );
    }

    let registry = Arc::clone(&agent.registry);
    let requested = url.map(str::to_string);
    thread::spawn(move || {
        let stop = Arc::new(AtomicBool::new(false));
        let routine: BoxedRoutine = Box::new(OpenBrowser::new(requested));
        routine.run(Arc::clone(&stop));
        registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove("open_browser");
    });

    Reply::ok(format!(
        "opening {target}\n  ladder: default browser -> Google Chrome -> Mozilla Firefox"
    ))
}

fn start_task(agent: &Agent, label: String, routine: BoxedRoutine) -> Reply {
    let name = routine.name().to_string();
    start_named(agent, name, label, routine)
}

/// Start a routine under a unique instance name so several may run at once.
fn start_named(agent: &Agent, name: String, label: String, routine: BoxedRoutine) -> Reply {
    // `open_browser` instances dedupe on the base name; everything else gets a
    // suffix so you can run two jitter routines with different timings.
    let instance = if name == "open_browser" {
        name.clone()
    } else {
        let reg = agent.registry.lock().unwrap_or_else(|e| e.into_inner());
        if !reg.contains_key(&name) {
            name.clone()
        } else {
            let mut n = 2;
            while reg.contains_key(&format!("{name}#{n}")) {
                n += 1;
            }
            format!("{name}#{n}")
        }
    };

    if name == "open_browser" {
        return open_browser(agent, None);
    }

    let stop = Arc::new(AtomicBool::new(false));
    {
        let mut reg = agent.registry.lock().unwrap_or_else(|e| e.into_inner());
        reg.insert(instance.clone(), TaskHandle { stop: Arc::clone(&stop) });
    }

    let registry = Arc::clone(&agent.registry);
    let thread_instance = instance.clone();
    thread::spawn(move || {
        routine.run(stop);
        registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&thread_instance);
        println!("[task '{thread_instance}' finished]");
    });

    Reply::ok(format!("▶ started task '{instance}'  ({label})"))
}

fn stop_task(registry: &Registry, name: &str) -> Reply {
    if name.is_empty() {
        return Reply::err("usage: stop <task>");
    }
    let reg = registry.lock().unwrap_or_else(|e| e.into_inner());
    match reg.get(name) {
        Some(h) => {
            h.stop.store(true, Ordering::Relaxed);
            Reply::ok(format!("⏸ stopping task '{name}'"))
        }
        None => Reply::err(format!("no such running task: '{name}'")),
    }
}

fn stop_all(registry: &Registry) -> usize {
    let reg = registry.lock().unwrap_or_else(|e| e.into_inner());
    for h in reg.values() {
        h.stop.store(true, Ordering::Relaxed);
    }
    reg.len()
}

fn list_tasks(registry: &Registry) -> String {
    let reg = registry.lock().unwrap_or_else(|e| e.into_inner());
    if reg.is_empty() {
        "(no running tasks)".to_string()
    } else {
        reg.keys().map(|n| format!("- {n}")).collect::<Vec<_>>().join("\n")
    }
}

fn task_names(registry: &Registry) -> Vec<String> {
    let reg = registry.lock().unwrap_or_else(|e| e.into_inner());
    let mut names: Vec<String> = reg.keys().cloned().collect();
    names.sort();
    names
}

fn status_text(agent: &Agent) -> String {
    format!(
        "uptime      : {}s\n\
         tasks       : {}\n\
         browser open: {}\n\
         landing page: {GOOGLE_URL}",
        agent.started.elapsed().as_secs(),
        list_tasks(&agent.registry).replace('\n', ", "),
        if agent.allow_spawn { "enabled" } else { "disabled" }
    )
}

fn help_text() -> String {
    "commands:\n\
     \x20 help | ?                    this list\n\
     \x20 status                      uptime, tasks, browser state\n\
     \x20 list | tasks                running tasks\n\
     \x20 browser [url]               open default -> chrome -> firefox (default page: Google)\n\
     \x20 open_browser [url]          subroutine form of the above\n\
     \x20 browsers                    show the fallback ladder\n\
     \x20 repeat_enter [ms]           press Enter on an interval\n\
     \x20 repeat_enter_jitter [b] [j] press Enter with jitter\n\
     \x20 stop <task>                 stop one task\n\
     \x20 stopall                     stop every task\n\
     \x20 quit                        exit\n\
     \n\
     web (loopback only):\n\
     \x20 GET  /                      dashboard\n\
     \x20 GET  /api/cmd?cmd=...       run a command\n\
     \x20 POST /api/cmd               cmd=<command>\n\
     \x20 GET  /api/status            JSON status\n\
     \x20 GET  /api/browser           open a browser"
        .to_string()
}
