//! Agent v04 — one program, two ways to run.
//!
//! * `agent` / `agent run` — **standalone**: exactly the v03 experience, a
//!   process that reads commands from its own console and exits when you quit.
//! * `agent daemon` — **daemon**: publishes a loopback control socket and a
//!   session file, then serves commands from any terminal on the machine via
//!   `agentctl`, until it is told to stop.
//!
//! The command semantics live in `agent_core::Runtime`, so the two modes cannot
//! drift apart: same commands, same tasks, same output.

mod console;

use std::io::{self, BufRead};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use agent_core::protocol::VERSION;
use agent_core::server::Server;
use agent_core::{client, Probe, Runtime, RuntimeOptions, Session};
use console::{Options, Role};

/// How long shutdown waits for running tasks to notice their stop flag.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    let mut args: Vec<String> = std::env::args().skip(1).collect();

    // The first word may select a mode; anything else is a flag.
    let role = match args.first().map(String::as_str) {
        Some("run") => {
            args.remove(0);
            Role::Standalone
        }
        Some("daemon") | Some("serve") | Some("daemonize") => {
            args.remove(0);
            Role::Daemon
        }
        Some("standalone") => {
            args.remove(0);
            Role::Standalone
        }
        _ => Role::Standalone,
    };

    // `--foreground` is accepted (and is already the behaviour) so the flag can
    // be written down explicitly in scripts and docs.
    args.retain(|a| a != "--foreground");

    let opts = Options::parse(role, args);
    if let Some(error) = &opts.error {
        if error != "help requested" {
            eprintln!("{error}\n");
        }
        print!("{}", console::usage(Some(role)));
        return if error == "help requested" { ExitCode::SUCCESS } else { ExitCode::from(2) };
    }

    // A daemon is supervised and redirected; a panic message matters more than
    // a tidy unwind, so make sure it reaches stderr whole.
    std::panic::set_hook(Box::new(|info| {
        eprintln!("[agent] panic: {info}");
    }));

    match opts.role {
        Role::Standalone => run_standalone(&opts),
        Role::Daemon => run_daemon(&opts),
    }
}

// --------------------------------------------------------------- standalone

fn run_standalone(opts: &Options) -> ExitCode {
    let session_dir = opts.resolve_session_dir();
    console::banner(opts, &session_dir);

    let runtime = Arc::new(Runtime::standalone(!opts.no_open));
    runtime.record("agent", format!("standalone mode, v{VERSION}"));

    let shutdown = Arc::new(AtomicBool::new(false));
    install_ctrlc(Arc::clone(&shutdown));

    println!("type 'help' for commands, 'quit' to exit");
    println!();

    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => break,
        };
        let cmd = line.trim().to_string();
        if cmd.is_empty() || cmd.starts_with('#') {
            continue;
        }

        // Leaving the console is a property of this front end, not a command
        // the shared runtime implements: in daemon mode there is no console to
        // quit, and `daemon_stop` is the equivalent.
        if matches!(cmd.as_str(), "quit" | "exit") {
            break;
        }

        let response = {
            let _guard = agent_core::state::lock(runtime.gate());
            runtime.handle("console", &agent_core::Request::new("", &cmd))
        };
        print!("{}", response.reply.output);
        if !response.reply.output.ends_with('\n') {
            println!();
        }

        // `daemon_stop` also ends a standalone session.
        if response.reply.shutdown {
            break;
        }
    }

    shutdown_and_exit(&runtime);
    ExitCode::SUCCESS
}

// ------------------------------------------------------------------- daemon

fn run_daemon(opts: &Options) -> ExitCode {
    let session_dir = opts.resolve_session_dir();
    console::banner(opts, &session_dir);

    let shutdown = Arc::new(AtomicBool::new(false));
    let runtime = Arc::new(Runtime::new(RuntimeOptions {
        mode: agent_core::runtime::Mode::Daemon,
        allow_spawn: !opts.no_open,
        shutdown: Arc::clone(&shutdown),
    }));

    // Bind first: if the port is unusable we want to fail before publishing a
    // session file that points at nothing.
    let server = match Server::bind(Arc::clone(&runtime), opts.port) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("[agent] could not bind the control socket: {e}");
            return ExitCode::FAILURE;
        }
    };

    // If a session file was already here, say so: it may belong to a daemon
    // that is still alive, which we would rather report than silently shadow
    // (both would answer on different ports, which is confusing).
    let previous = session_dir.load();
    if let Some(previous) = previous {
        match client::Client::probe_dir(&session_dir) {
            Probe::Running(live) => {
                eprintln!(
                    "[agent] WARNING: another daemon looks alive at {} (pid {}).",
                    live.endpoint(),
                    live.pid
                );
                eprintln!(
                    "[agent] WARNING: this daemon is taking over {}; stop the old one, \
                     or use a different --session-dir.",
                    session_dir.file().display()
                );
            }
            _ => {
                if previous.pid != 0 {
                    console::note(
                        opts.role,
                        &format!("[agent] replacing stale session file (old pid {})", previous.pid),
                    );
                }
            }
        }
    }

    let session = Session::new(server.port());
    if let Err(e) = session_dir.save(&session) {
        eprintln!(
            "[agent] could not write the session file in {}: {e}",
            session_dir.path().display()
        );
        eprintln!("[agent] clients will not be able to find this daemon; pass --session-dir");
        return ExitCode::FAILURE;
    }
    runtime.set_session(session.clone(), session_dir.clone());

    runtime.record("agent", format!("daemon started on port {}", server.port()));
    console::note(opts.role, &format!("=== daemon listening ==="));
    console::note(opts.role, &format!("session file : {}", session_dir.file().display()));
    console::note(opts.role, &format!("endpoint     : {}", session.endpoint()));
    console::note(opts.role, &format!("pid          : {}", session.pid));
    console::note(opts.role, &format!("version      : {VERSION}"));
    console::note(opts.role, "");
    console::note(opts.role, "run commands from any terminal, for example:");
    console::note(opts.role, "  agentctl status");
    console::note(opts.role, "  agentctl browser");
    console::note(opts.role, "  agentctl daemon-stop");
    console::note(opts.role, "");
    console::note(opts.role, "press Ctrl-C here to stop the daemon");

    install_ctrlc(Arc::clone(&shutdown));

    // When a shutdown is requested, tell the tasks to stop. The server loop
    // notices the same flag and unwinds, so this is the only place tasks are
    // asked to finish.
    {
        let watcher_runtime = Arc::clone(&runtime);
        let watcher_flag = Arc::clone(&shutdown);
        thread::spawn(move || {
            while !watcher_flag.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_millis(100));
            }
            let n = watcher_runtime.tasks().stop_all();
            if n > 0 {
                watcher_runtime.record("daemon", format!("asking {n} task(s) to stop"));
            }
        });
    }

    let serve_result = server.serve();

    // Best-effort teardown: stop tasks, remove the session file so a later
    // `agentctl` does not chase a dead port.
    runtime.tasks().stop_all();
    let joined = runtime.tasks().join_all(SHUTDOWN_GRACE);
    runtime.record("daemon", format!("stopped with {joined} task(s) joined"));
    if let Err(e) = session_dir.clear() {
        eprintln!("[agent] could not remove {}: {e}", session_dir.file().display());
    }

    if let Err(e) = serve_result {
        eprintln!("[agent] server error: {e}");
        return ExitCode::FAILURE;
    }
    console::note(opts.role, "daemon stopped");
    ExitCode::SUCCESS
}

// -------------------------------------------------------------------- shared

/// Stop tasks and wait a bounded time for them.
fn shutdown_and_exit(runtime: &Runtime) {
    let n = runtime.tasks().stop_all();
    if n > 0 {
        runtime.record("agent", format!("asking {n} task(s) to stop"));
    }
    runtime.tasks().join_all(SHUTDOWN_GRACE);
    println!("bye");
}

/// Set the shutdown flag on Ctrl-C, in addition to whatever the runtime does.
fn install_ctrlc(shutdown: Arc<AtomicBool>) {
    if let Err(e) = ctrlc_flag(shutdown) {
        // Not fatal: `daemon_stop` still works, and the process can be killed.
        eprintln!("[agent] could not install a Ctrl-C handler ({e}); use 'daemon_stop' to stop");
    }
}

/// Minimal Ctrl-C handling with no third-party dependency.
#[cfg(windows)]
fn ctrlc_flag(shutdown: Arc<AtomicBool>) -> Result<(), String> {
    use std::sync::OnceLock;

    // A control handler must be a plain `extern "system"` fn with no captured
    // state, so the flag is parked in a static for it to reach.
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();

    extern "system" fn handler(_ctrl_type: u32) -> i32 {
        if let Some(flag) = FLAG.get() {
            flag.store(true, Ordering::SeqCst);
        }
        1 // handled: do not run the default terminate handler
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32)
            -> i32;
    }

    FLAG.set(shutdown).map_err(|_| "handler already installed".to_string())?;
    let ok = unsafe { SetConsoleCtrlHandler(Some(handler), 1) };
    if ok == 0 {
        return Err("SetConsoleCtrlHandler failed".to_string());
    }
    Ok(())
}

#[cfg(unix)]
fn ctrlc_flag(shutdown: Arc<AtomicBool>) -> Result<(), String> {
    use std::sync::OnceLock;

    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    const SIG_ERR: usize = usize::MAX;

    /// Async-signal-safe: only flips an atomic.
    extern "C" fn handler(_signum: i32) {
        if let Some(flag) = FLAG.get() {
            flag.store(true, Ordering::SeqCst);
        }
    }

    extern "C" {
        fn signal(signum: i32, handler: usize) -> usize;
    }

    FLAG.set(shutdown).map_err(|_| "handler already installed".to_string())?;
    for signum in [SIGINT, SIGTERM] {
        let previous = unsafe { signal(signum, handler as usize) };
        if previous == SIG_ERR {
            return Err(format!("signal({signum}) failed"));
        }
    }
    Ok(())
}
