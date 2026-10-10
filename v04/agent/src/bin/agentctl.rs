//! `agentctl` — drive a running agent daemon from any terminal.
//!
//! ```
//! agentctl                     interactive session against the daemon
//! agentctl status              one command, then exit
//! agentctl browser             open a browser on the daemon's machine
//! agentctl daemon-status       daemon detail: port, session file, clients
//! agentctl daemon-start        start a daemon in the background, wait for it
//! agentctl daemon-stop         stop the daemon
//! agentctl session             show where the session file is and who is there
//! ```
//!
//! Exit status is 0 when the daemon accepted the command, 1 when it refused,
//! and 2 for local problems (no daemon, bad flags).

#[path = "../agentctl_cli.rs"]
mod cli;

use std::io::{self, BufRead, Write};
use std::process::ExitCode;

use agent_core::client::Client;
use agent_core::Probe;

use cli::{Invocation, Options};

const EXIT_OK: u8 = 0;
const EXIT_REFUSED: u8 = 1;
const EXIT_LOCAL_ERROR: u8 = 2;

fn main() -> ExitCode {
    let opts = Options::parse(std::env::args().skip(1).collect());
    if let Some(error) = &opts.error {
        if error != "help requested" {
            eprintln!("{error}\n");
        }
        print!("{}", cli::usage());
        return if error == "help requested" {
            ExitCode::from(EXIT_OK)
        } else {
            ExitCode::from(EXIT_LOCAL_ERROR)
        };
    }

    match opts.invocation {
        Invocation::Version => {
            println!("agentctl v{} (api level {})", agent_core::VERSION, agent_core::API_LEVEL);
            ExitCode::from(EXIT_OK)
        }
        Invocation::Session => show_session(&opts),
        Invocation::DaemonStart => start_daemon(&opts),
        Invocation::Command(_) | Invocation::Repl => run_against_daemon(&opts),
    }
}

/// Connect, or explain why we cannot.
fn attach(opts: &Options) -> Result<Client, (u8, String)> {
    let dir = opts.session_dir();
    match Client::probe_dir(&dir) {
        Probe::Running(session) => {
            if opts.verbose {
                eprintln!(
                    "[agentctl] {} -> {} (pid {})",
                    dir.file().display(),
                    session.endpoint(),
                    session.pid
                );
            }
            Ok(Client::new(session))
        }
        Probe::Unusable(why) => Err((
            EXIT_LOCAL_ERROR,
            format!(
                "found a session file at {} but the daemon is not usable:\n  {why}\n\
                 If the daemon is gone, delete the file or restart the daemon.",
                dir.file().display()
            ),
        )),
        Probe::NotRunning => Err((
            EXIT_LOCAL_ERROR,
            format!(
                "no daemon is running (looked in {}).\n\
                 Start one with:  agent daemon\n\
                 Or point both at the same place with --session-dir.",
                dir.path().display()
            ),
        )),
    }
}

fn show_session(opts: &Options) -> ExitCode {
    let dir = opts.session_dir();
    println!("session dir : {}", dir.path().display());
    println!("source      : {}", dir.source());
    println!("writable    : {}", dir.is_writable());
    println!("session file: {}", dir.file().display());

    match dir.load() {
        Some(session) => {
            println!("port        : {}", session.port);
            println!("pid         : {}", session.pid);
            println!("version     : {}", session.version);
            println!("token       : {}...", &session.token[..8.min(session.token.len())]);
        }
        None => println!("session file: (absent or unreadable)"),
    }

    match Client::probe_dir(&dir) {
        Probe::Running(_) => println!("\n=> daemon reachable"),
        Probe::Unusable(why) => println!("\n=> daemon NOT reachable: {why}"),
        Probe::NotRunning => println!("\n=> no daemon running"),
    }
    ExitCode::from(EXIT_OK)
}

/// Start a daemon as a detached process, then wait for it to answer.
fn start_daemon(opts: &Options) -> ExitCode {
    let dir = opts.session_dir();

    if let Probe::Running(session) = Client::probe_dir(&dir) {
        println!(
            "a daemon is already running at {} (pid {})",
            session.endpoint(),
            session.pid
        );
        return ExitCode::from(EXIT_OK);
    }

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            eprintln!("cannot locate the agent executable: {e}");
            return ExitCode::from(EXIT_LOCAL_ERROR);
        }
    };
    // `agentctl` sits next to `agent`; find it rather than assuming the name.
    let agent = match find_agent_binary(&exe) {
        Some(path) => path,
        None => {
            eprintln!(
                "could not find the `agent` binary next to {}",
                exe.display()
            );
            eprintln!("start it yourself with:  agent daemon");
            return ExitCode::from(EXIT_LOCAL_ERROR);
        }
    };

    let mut command = std::process::Command::new(&agent);
    command.arg("daemon");
    command.arg("--session-dir").arg(dir.path());
    if let Some(port) = opts.port {
        command.arg("--port").arg(port.to_string());
    }
    if opts.no_open {
        command.arg("--no-open");
    }
    // The daemon must not inherit this terminal's standard handles, or the
    // shell that started it would stay attached forever. Its output goes to a
    // log file in the session directory instead.
    let log_path = dir.path().join("daemon.log");
    if let Err(e) = std::fs::create_dir_all(dir.path()) {
        eprintln!("could not create {}: {e}", dir.path().display());
        return ExitCode::from(EXIT_LOCAL_ERROR);
    }
    let log = match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(file) => file,
        Err(e) => {
            eprintln!("could not open {}: {e}", log_path.display());
            return ExitCode::from(EXIT_LOCAL_ERROR);
        }
    };
    let log_err = match log.try_clone() {
        Ok(file) => file,
        Err(e) => {
            eprintln!("could not duplicate the log handle: {e}");
            return ExitCode::from(EXIT_LOCAL_ERROR);
        }
    };

    command.stdin(std::process::Stdio::null());
    command.stdout(std::process::Stdio::from(log));

    // Detach: the daemon must outlive this terminal.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP, with stderr pointed at
        // the log so nothing keeps a handle on our console.
        command.stderr(std::process::Stdio::from(log_err));
        command.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.stderr(std::process::Stdio::from(log_err));
        unsafe {
            command.pre_exec(|| {
                // New session: survive the parent's terminal going away.
                if libc_setsid() == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            eprintln!("could not start the daemon: {e}");
            return ExitCode::from(EXIT_LOCAL_ERROR);
        }
    };

    println!("started daemon (pid {}), waiting for it to answer...", child.id());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if let Probe::Running(session) = Client::probe_dir(&dir) {
            println!("daemon ready at {} (pid {})", session.endpoint(), session.pid);
            println!("its output is in {}", log_path.display());
            return ExitCode::from(EXIT_OK);
        }
        std::thread::sleep(std::time::Duration::from_millis(150));
    }

    eprintln!("the daemon did not answer within 10s; check {}", log_path.display());
    ExitCode::from(EXIT_LOCAL_ERROR)
}

/// Find the sibling `agent` binary.
fn find_agent_binary(ctl: &std::path::Path) -> Option<std::path::PathBuf> {
    let dir = ctl.parent()?;
    let candidates = if cfg!(windows) {
        vec!["agent.exe", "agent"]
    } else {
        vec!["agent", "agent.exe"]
    };
    candidates
        .into_iter()
        .map(|name| dir.join(name))
        .find(|path| path.is_file() && *path != ctl)
}

/// Run the one-shot command, or drop into the interactive session.
fn run_against_daemon(opts: &Options) -> ExitCode {
    let client = match attach(opts) {
        Ok(client) => client,
        Err((code, message)) => {
            eprintln!("{message}");
            return ExitCode::from(code);
        }
    };

    match &opts.invocation {
        Invocation::Command(cmd) => {
            let mut since = 0;
            match send(&client, cmd, &mut since) {
                Ok(code) => ExitCode::from(code),
                Err(message) => {
                    eprintln!("{message}");
                    ExitCode::from(EXIT_LOCAL_ERROR)
                }
            }
        }
        _ => repl(&client, opts),
    }
}

/// Send one command, print the result and any events we had not seen.
///
/// Events are reported one-line to stderr so they can be piped away from the
/// command's own output. Connection bookkeeping is filtered out — it is in the
/// daemon's log and in `agentctl events`, but it is noise next to the answer.
/// The cursor advances regardless, so a one-shot `agentctl` never replays the
/// daemon's history.
fn send(client: &Client, cmd: &str, since: &mut u64) -> Result<u8, String> {
    let outcome = client.send(cmd, *since)?;

    for (seq, kind, text) in &outcome.events {
        *since = (*since).max(*seq);
        if kind == "client" {
            continue;
        }
        eprintln!("[{kind}] {text}");
    }
    if let Some((last, _, _)) = outcome.events.last() {
        *since = (*since).max(*last);
    }

    if !outcome.output.is_empty() {
        println!("{}", outcome.output);
    }

    Ok(if outcome.ok { EXIT_OK } else { EXIT_REFUSED })
}

// ----------------------------------------------------------------------- repl

fn repl(client: &Client, opts: &Options) -> ExitCode {
    let session = client.session();
    println!("agentctl v{} -> {}", agent_core::VERSION, session.endpoint());
    println!("type a command, 'help' to list them, 'quit' to disconnect");
    println!("(commands run on the daemon, not in this process)");
    println!();

    let stdin = io::stdin();
    let mut since = 0;
    let mut last_status = EXIT_OK;

    loop {
        print!("agent> ");
        let _ = io::stdout().flush();

        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(e) => {
                eprintln!("input error: {e}");
                break;
            }
        }

        let cmd = line.trim();
        if cmd.is_empty() || cmd.starts_with('#') {
            continue;
        }
        if matches!(cmd, "quit" | "exit" | ":q") {
            break;
        }

        match send(client, cmd, &mut since) {
            Ok(code) => last_status = code,
            Err(message) => {
                eprintln!("{message}");
                // The daemon may have gone away mid-session.
                if Client::probe_dir(&opts.session_dir()) != Probe::Running(session.clone()) {
                    eprintln!("the daemon is no longer reachable; leaving the session");
                    return ExitCode::from(EXIT_LOCAL_ERROR);
                }
                last_status = EXIT_LOCAL_ERROR;
            }
        }
    }

    println!("disconnected");
    ExitCode::from(last_status)
}

#[cfg(unix)]
unsafe fn libc_setsid() -> i32 {
    extern "C" {
        fn setsid() -> i32;
    }
    setsid()
}

#[cfg(not(unix))]
#[allow(dead_code)]
unsafe fn libc_setsid() -> i32 {
    -1
}
