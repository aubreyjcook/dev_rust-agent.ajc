//! End-to-end tests for the daemon/client split.
//!
//! These run the real binaries: a daemon process, then `agentctl` against it.
//! That is the whole point of v04 — the transport, the session file, the
//! handshake and the task registry are only proven together.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// The agent binary under test.
fn agent_bin() -> &'static str {
    env!("CARGO_BIN_EXE_agent")
}

/// The client binary under test.
fn agentctl_bin() -> &'static str {
    env!("CARGO_BIN_EXE_agentctl")
}

/// A scratch session directory plus a daemon we promise to kill.
struct Harness {
    root: PathBuf,
    session_dir: PathBuf,
    daemon: Option<Child>,
}

impl Harness {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "v04-it-{tag}-{}-{}",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
        ));
        let session_dir = root.join("session");
        std::fs::create_dir_all(&session_dir).expect("create session dir");
        Self { root, session_dir, daemon: None }
    }

    /// Start `agent daemon` and wait until it answers.
    fn start_daemon(&mut self) {
        let child = Command::new(agent_bin())
            .arg("daemon")
            .arg("--session-dir")
            .arg(&self.session_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn daemon");
        self.daemon = Some(child);
        self.wait_ready();
    }

    /// Poll with `agentctl session` until a daemon answers.
    fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            let probe = self.session_cmd(&["session"]);
            if String::from_utf8_lossy(&probe.stdout).contains("daemon reachable") {
                return;
            }
            // If the daemon process died, stop waiting and say why.
            if let Some(child) = self.daemon.as_mut() {
                if let Some(status) = child.try_wait().expect("try_wait") {
                    let mut stderr = String::new();
                    if let Some(mut pipe) = child.stderr.take() {
                        use std::io::Read;
                        let _ = pipe.read_to_string(&mut stderr);
                    }
                    panic!(
                        "the daemon exited with {status} before becoming reachable.\n\
                         session dir: {}\nstderr: {stderr}",
                        self.session_dir.display()
                    );
                }
            }
            std::thread::sleep(Duration::from_millis(150));
        }
        panic!(
            "the daemon never became reachable in {}",
            self.session_dir.display()
        );
    }

    fn session_cmd(&self, args: &[&str]) -> Output {
        Command::new(agentctl_bin())
            .args(args)
            .arg("--session-dir")
            .arg(&self.session_dir)
            .output()
            .expect("run agentctl")
    }

    /// Run an `agentctl <command>` and return stdout+stderr as one string.
    fn ctl(&self, args: &[&str]) -> String {
        let output = self.session_cmd(args);
        let mut text = String::from_utf8_lossy(&output.stdout).to_string();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        text
    }

    fn ctl_status(&self, args: &[&str]) -> i32 {
        self.session_cmd(args)
            .status
            .code()
            .expect("agentctl should exit with a status")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        // Ask nicely, then make sure.
        let _ = self.session_cmd(&["daemon-stop"]);
        if let Some(mut child) = self.daemon.take() {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn a_daemon_publishes_a_session_that_a_client_can_use() {
    let mut harness = Harness::new("session");
    harness.start_daemon();

    // The session file is the contract between the two processes.
    let session_file = harness.session_dir.join("session.json");
    assert!(session_file.is_file(), "daemon must publish {}", session_file.display());
    let raw = std::fs::read_to_string(&session_file).expect("read session file");
    assert!(raw.contains("\"port\""), "session file should carry a port: {raw}");
    assert!(raw.contains("\"token\""), "session file should carry a token: {raw}");
    assert!(raw.contains("\"pid\""), "session file should carry a pid: {raw}");

    let session = harness.ctl(&["session"]);
    assert!(session.contains("daemon reachable"), "got: {session}");
    assert!(session.contains("writable    : true"), "got: {session}");

    let status = harness.ctl(&["status"]);
    assert!(status.contains("(daemon"), "status should name the mode: {status}");
    assert!(status.contains("api level"), "status should name the api level: {status}");

    let detail = harness.ctl(&["daemon-status"]);
    assert!(detail.contains("mode       : daemon"), "got: {detail}");
    assert!(detail.contains("session dir:"), "got: {detail}");
    assert!(detail.contains("endpoint   : 127.0.0.1:"), "got: {detail}");
}

#[test]
fn tasks_started_by_one_client_are_visible_to_another() {
    let mut harness = Harness::new("tasks");
    harness.start_daemon();

    let started = harness.ctl(&["repeat_enter", "30000"]);
    assert!(started.contains("started task 'repeat_enter'"), "got: {started}");
    assert!(
        started.contains("agentctl stop repeat_enter"),
        "daemon mode should point at the remote stop command: {started}"
    );

    // A second, independent client sees the same registry.
    let listing = harness.ctl(&["list"]);
    assert!(listing.contains("repeat_enter"), "second client should see the task: {listing}");
    assert!(listing.contains("repeat_enter 30000ms"), "label should carry parameters: {listing}");

    // And can stop it, by name or by unique prefix.
    let stopped = harness.ctl(&["stop", "repeat_enter"]);
    assert!(stopped.contains("stopping task 'repeat_enter'"), "got: {stopped}");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if harness.ctl(&["list"]).contains("(no running tasks)") {
            break;
        }
        assert!(Instant::now() < deadline, "the task never disappeared from `list`");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn refused_commands_exit_nonzero_and_explain_themselves() {
    let mut harness = Harness::new("refused");
    harness.start_daemon();

    assert_eq!(harness.ctl_status(&["nonsense"]), 1, "an unknown command is a refusal");
    let unknown = harness.ctl(&["nonsense"]);
    assert!(unknown.contains("unknown command: nonsense"), "got: {unknown}");

    assert_eq!(harness.ctl_status(&["stop", "ghost"]), 1, "stopping nothing is a refusal");
    let missing = harness.ctl(&["stop", "ghost"]);
    assert!(missing.contains("no such running task"), "got: {missing}");

    // A completed command exits zero.
    assert_eq!(harness.ctl_status(&["list"]), 0);
}

#[test]
fn a_stale_session_file_is_reported_rather_than_looking_alive() {
    let harness = Harness::new("stale");
    // Write a session that points at a port nobody is listening on.
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().expect("addr").port();
    drop(listener);

    let session = format!(
        "{{\"port\":{port},\"token\":\"0123456789abcdef\",\"pid\":999999,\
          \"started_unix\":0,\"version\":\"0.4.0\"}}"
    );
    std::fs::write(harness.session_dir.join("session.json"), session).expect("write");

    let session_out = harness.ctl(&["session"]);
    assert!(session_out.contains("NOT reachable"), "got: {session_out}");

    // A command against a dead daemon fails locally, not silently.
    assert_eq!(
        harness.ctl_status(&["status"]),
        2,
        "no reachable daemon should be a local error"
    );
    let failed = harness.ctl(&["status"]);
    assert!(failed.contains("not usable") || failed.contains("no daemon"), "got: {failed}");
}

#[test]
fn no_daemon_running_is_a_clear_local_error() {
    let harness = Harness::new("none");

    let out = harness.ctl(&["status"]);
    assert!(out.contains("no daemon is running"), "got: {out}");
    assert!(out.contains("agent daemon"), "the error should say how to fix it: {out}");
    assert_eq!(harness.ctl_status(&["status"]), 2);
}

#[test]
fn daemon_stop_shuts_down_and_cleans_up() {
    let mut harness = Harness::new("stop");
    harness.start_daemon();

    let out = harness.ctl(&["daemon-stop"]);
    assert!(out.contains("shutting the daemon down"), "got: {out}");

    let child = harness.daemon.as_mut().expect("daemon child");
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    while Instant::now() < deadline {
        if child.try_wait().expect("try_wait").is_some() {
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(exited, "the daemon should exit after daemon-stop");

    assert!(
        !harness.session_dir.join("session.json").exists(),
        "the session file should be removed on shutdown"
    );
}

#[test]
fn standalone_mode_needs_no_daemon() {
    // `agent` with no mode word: commands come from stdin and nothing is
    // published, so a stray `agentctl` has nothing to talk to.
    let harness = Harness::new("standalone");
    let mut child = Command::new(agent_bin())
        .arg("--session-dir")
        .arg(&harness.session_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn standalone agent");

    {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("stdin");
        stdin.write_all(b"status\nlist\nquit\n").expect("write commands");
    }

    let output = child.wait_with_output().expect("wait for standalone agent");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("(standalone"), "got: {text}");
    assert!(text.contains("not published (standalone mode)"), "got: {text}");
    assert!(text.contains("bye"), "got: {text}");
    assert!(
        !text.contains("unknown command: quit"),
        "quitting the console is not an unknown command: {text}"
    );
    assert!(
        !harness.session_dir.join("session.json").exists(),
        "standalone mode must not publish a session"
    );
}

/// The binaries must exist side by side, which is what `daemon-start` relies on.
#[test]
fn the_two_binaries_live_next_to_each_other() {
    let agent = Path::new(agent_bin());
    let ctl = Path::new(agentctl_bin());
    assert_eq!(agent.parent(), ctl.parent(), "binaries should be siblings");
}
