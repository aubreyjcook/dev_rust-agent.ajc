//! The command runtime: what the agent can do, independent of how a command
//! reached it.
//!
//! Standalone mode reads commands from stdin and hands them here. Daemon mode
//! receives them over the control socket and hands them here. That is the whole
//! point of the split — there is exactly one implementation of `browser`,
//! `repeat_enter`, `stop`, and the rest.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use routines::launcher::{self, Route};
use routines::{OpenBrowser, RepeatEnter, RepeatEnterJitter, GOOGLE_URL};

use crate::protocol::{now_unix_ms, Event, Reply, Request, Response, VERSION};
use crate::session::{Session, SessionDir};
use crate::state::{lock, Agent};

/// How many events the daemon remembers for clients that were not listening.
const EVENT_BACKLOG: usize = 500;
/// How many recent client connections to remember for `daemon_status`.
const CLIENT_MEMORY: usize = 20;
/// How long a browser launch blocks repeat launches.
const SPAWN_GUARD: Duration = Duration::from_millis(1500);

/// How the process is being hosted. Only affects reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Standalone,
    Daemon,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Standalone => "standalone",
            Mode::Daemon => "daemon",
        }
    }
}

pub struct RuntimeOptions {
    pub mode: Mode,
    pub allow_spawn: bool,
    /// Reset after a refused command, so a flood of requests is refused once.
    pub shutdown: Arc<AtomicBool>,
}

impl Default for RuntimeOptions {
    fn default() -> Self {
        Self {
            mode: Mode::Standalone,
            allow_spawn: true,
            shutdown: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// One recorded client connection.
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub peer: String,
    pub first_seen_ms: u64,
    pub requests: u64,
}

/// The runtime itself.
pub struct Runtime {
    opts: RuntimeOptions,
    agent: Agent,
    started: Instant,
    events: Mutex<VecDeque<Event>>,
    next_seq: Mutex<u64>,
    clients: Mutex<VecDeque<ClientInfo>>,
    spawn_guard: Mutex<Option<Instant>>,
    session: Mutex<Option<Session>>,
    session_dir: Mutex<Option<SessionDir>>,
    /// Serializes command execution so two terminals issuing commands at the
    /// same instant are ordered rather than racing. Shared by every front end.
    gate: Mutex<()>,
    /// Set while a shutdown has been requested but not yet performed.
    stopping: AtomicBool,
}

impl Runtime {
    pub fn new(opts: RuntimeOptions) -> Self {
        Self {
            opts,
            agent: Agent::new(),
            started: Instant::now(),
            events: Mutex::new(VecDeque::new()),
            next_seq: Mutex::new(1),
            clients: Mutex::new(VecDeque::new()),
            spawn_guard: Mutex::new(None),
            session: Mutex::new(None),
            session_dir: Mutex::new(None),
            gate: Mutex::new(()),
            stopping: AtomicBool::new(false),
        }
    }

    /// The command gate. Lock it around [`Runtime::handle`] to serialize
    /// commands arriving from different front ends or connections.
    pub fn gate(&self) -> &Mutex<()> {
        &self.gate
    }

    /// Standalone mode: one process, reading stdin, printing to stdout.
    pub fn standalone(allow_spawn: bool) -> Self {
        Self::new(RuntimeOptions { mode: Mode::Standalone, allow_spawn, ..Default::default() })
    }

    pub fn tasks(&self) -> &Agent {
        &self.agent
    }

    pub fn mode(&self) -> Mode {
        self.opts.mode
    }

    pub fn allow_spawn(&self) -> bool {
        self.opts.allow_spawn
    }

    pub fn shutdown_requested(&self) -> bool {
        self.opts.shutdown.load(Ordering::Relaxed)
    }

    pub fn uptime_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Record the coordinates this daemon is published under.
    pub fn set_session(&self, session: Session, dir: SessionDir) {
        *lock(&self.session) = Some(session);
        *lock(&self.session_dir) = Some(dir);
    }

    pub fn session(&self) -> Option<Session> {
        lock(&self.session).clone()
    }

    pub fn session_dir(&self) -> Option<SessionDir> {
        lock(&self.session_dir).clone()
    }

    // ------------------------------------------------------------- event log

    /// Append an event and hand back its sequence number.
    pub fn record(&self, kind: &str, text: impl Into<String>) -> u64 {
        let mut next = lock(&self.next_seq);
        let seq = *next;
        *next += 1;
        drop(next);

        let mut events = lock(&self.events);
        if events.len() == EVENT_BACKLOG {
            events.pop_front();
        }
        events.push_back(Event::new(seq, kind, text));
        seq
    }

    /// Events after `since`, bounded by `limit`.
    pub fn events_since(&self, since: u64, limit: usize) -> Vec<Event> {
        let events = lock(&self.events);
        events
            .iter()
            .filter(|e| e.seq > since)
            .take(limit)
            .cloned()
            .collect()
    }

    pub fn event_backlog(&self) -> usize {
        lock(&self.events).len()
    }

    // --------------------------------------------------------------- clients

    /// Note a connection. Called once per accepted socket.
    pub fn register_client(&self, peer: &str) -> ClientInfo {
        let mut clients = lock(&self.clients);
        if let Some(existing) = clients.iter_mut().find(|c| c.peer == peer) {
            existing.requests += 1;
            return existing.clone();
        }
        let info = ClientInfo {
            peer: peer.to_string(),
            first_seen_ms: now_unix_ms(),
            requests: 0,
        };
        if clients.len() == CLIENT_MEMORY {
            clients.pop_front();
        }
        clients.push_back(info.clone());
        info
    }

    pub fn note_request(&self, peer: &str) {
        let mut clients = lock(&self.clients);
        if let Some(existing) = clients.iter_mut().find(|c| c.peer == peer) {
            existing.requests += 1;
        }
    }

    pub fn clients(&self) -> Vec<ClientInfo> {
        lock(&self.clients).iter().cloned().collect()
    }

    // ---------------------------------------------------------------- serving

    /// Handle one request from a client. The reply is always a `Response` so the
    /// client gets any events it missed along with the answer.
    pub fn handle(&self, peer: &str, request: &Request) -> Response {
        self.note_request(peer);

        let expected = self.session().map(|s| s.token).unwrap_or_default();
        if !expected.is_empty() && request.token != expected {
            self.record("auth", format!("rejected command from {peer}: bad token"));
            return Response::new(
                Reply::err("invalid or missing session token; re-read the session file"),
                Vec::new(),
            );
        }

        let (reply, is_control) = if request.cmd.trim().is_empty() {
            (Reply::ok(""), false)
        } else {
            self.dispatch(&request.cmd)
        };

        // A client set may want the reply first and the shutdown second; the
        // server acts on `shutdown` after writing the response.
        let mut response = Response::new(reply, self.events_since(request.since, 200));
        if is_control && self.stopping.load(Ordering::Relaxed) {
            response.reply.shutdown = true;
        }
        response
    }

    /// Dispatch one command. Returns the reply and whether it was a control
    /// command that may have set the shutdown flag.
    fn dispatch(&self, input: &str) -> (Reply, bool) {
        let mut parts = input.trim().split_whitespace();
        let cmd = parts.next().unwrap_or("");
        let args: Vec<String> = parts.map(str::to_string).collect();

        match cmd {
            // Handshake only: proves the token is good and names the peer.
            "hello" | "ping" => (
                self.reply(format!(
                    "agent v{VERSION} {} ready (pid {}, api level {}, {} task(s) running)",
                    self.opts.mode.as_str(),
                    std::process::id(),
                    crate::protocol::API_LEVEL,
                    self.agent.len()
                )),
                false,
            ),
            "help" | "?" | "/help" => (self.reply(help_text()), false),
            "version" => (
                self.reply(format!(
                    "agent {} (api level {})",
                    VERSION,
                    crate::protocol::API_LEVEL
                )),
                false,
            ),
            "status" => (self.reply(self.status_text()), false),
            "daemon_status" | "daemon-status" | "server_status" => {
                (self.reply(self.daemon_status_text()), false)
            }
            "list" | "tasks" => (self.reply(self.list_tasks()), false),

            "browser" | "browse" | "open_browser" | "open" => {
                self.dispatch_browser(args.first().map(String::as_str))
            }
            "browsers" | "browser_chain" => (
                self.reply(format!(
                    "browser fallback ladder (tried in this order):\n  {}",
                    launcher::describe_ladder().join("\n  ")
                )),
                false,
            ),

            "repeat_enter" => {
                let ms = args.first().and_then(|s| s.parse().ok()).unwrap_or(1000);
                let label = format!("repeat_enter {ms}ms");
                self.spawn(label, Box::new(RepeatEnter { interval_ms: ms }))
            }
            "repeat_enter_jitter" => {
                let base = args.first().and_then(|s| s.parse().ok()).unwrap_or(1000);
                let jitter = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(100);
                let label = format!("repeat_enter_jitter {base}ms+/-{jitter}ms");
                self.spawn(
                    label,
                    Box::new(RepeatEnterJitter { base_ms: base, jitter_ms: jitter }),
                )
            }

            "stop" => (self.stop_task(args.first().map(String::as_str).unwrap_or("")), false),
            "stopall" | "stop_all" => {
                let n = self.agent.stop_all();
                self.record("task", format!("stop all requested ({n} task(s))"));
                (self.reply(format!("stopping {n} task(s)")), false)
            }

            "events" => {
                let since = args.first().and_then(|s| s.parse().ok()).unwrap_or(0);
                let events = self.events_since(since, 50);
                let text = if events.is_empty() {
                    "(no new events)".to_string()
                } else {
                    events
                        .iter()
                        .map(|e| format!("{:>6}  {:<6}  {}", e.seq, e.kind, e.text))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                (self.reply(text), false)
            }
            "clients" => {
                let clients = self.clients();
                let text = if clients.is_empty() {
                    "(no client connections recorded)".to_string()
                } else {
                    clients
                        .iter()
                        .map(|c| format!("{:<24} {:>5} requests", c.peer, c.requests))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                (self.reply(text), false)
            }

            "daemon_stop" | "shutdown" | "stop_daemon" => {
                if !self.opts.shutdown.swap(true, Ordering::SeqCst) {
                    self.record("daemon", "shutdown requested");
                    (
                        self.reply(format!(
                            "shutting the {} down; running tasks were asked to stop",
                            self.opts.mode.as_str()
                        )),
                        true,
                    )
                } else {
                    (self.reply("shutdown already in progress"), true)
                }
            }

            other => (self.reply_err(format!("unknown command: {other}")), false),
        }
    }

    fn dispatch_browser(&self, url: Option<&str>) -> (Reply, bool) {
        if !self.opts.allow_spawn {
            return (
                self.reply_err("browser launching is disabled (started with --no-open)"),
                false,
            );
        }

        let target = launcher::target_url(url);

        // Hold the guard while deciding, so two near-simultaneous requests
        // cannot both slip past the check.
        {
            let mut guard = lock(&self.spawn_guard);
            if guard.map(|at| at.elapsed() < SPAWN_GUARD).unwrap_or(false) {
                return (
                    self.reply_err(format!(
                        "a browser launch is already in flight (target {target})"
                    )),
                    false,
                );
            }
            *guard = Some(Instant::now());
        }

        // Also dedupe against a tracked open_browser task, which covers a
        // launch that a routine is still working through.
        if self.agent.is_running("open_browser") {
            return (
                self.reply_err(format!(
                    "a browser launch is already in flight (target {target})"
                )),
                false,
            );
        }

        let requested = url.map(str::to_string);
        let label = format!("open_browser {target}");
        let started = self.spawn(label, Box::new(OpenBrowser::new(requested)));
        if started.0.ok {
            self.record("browser", format!("opening {target}"));
        }
        started
    }

    /// Start a routine and describe it.
    fn spawn(&self, label: String, routine: routines::BoxedRoutine) -> (Reply, bool) {
        let base = routines::Routine::name(&*routine).to_string();
        match self.agent.start(label.clone(), routine) {
            Ok(name) => {
                self.record("task", format!("started '{name}' ({label})"));
                (
                    Reply::ok(format!(
                        "started task '{name}'\n  {}\n  {}",
                        self.task_count_text(),
                        self.how_to_stop(&name)
                    )),
                    false,
                )
            }
            Err(e) => (self.reply_err(format!("could not start {base}: {e}")), false),
        }
    }

    fn stop_task(&self, typed: &str) -> Reply {
        if typed.is_empty() {
            return self.reply_err("usage: stop <task>");
        }
        match self.agent.resolve(typed) {
            Ok(name) => {
                if self.agent.stop(&name) {
                    self.record("task", format!("stopping '{name}'"));
                    self.reply(format!("stopping task '{name}'"))
                } else {
                    self.reply_err(format!("task '{name}' vanished before it could be stopped"))
                }
            }
            Err(e) => {
                let running = self.agent.names();
                let hint = if running.is_empty() {
                    String::new()
                } else {
                    format!("\n  running: {}", running.join(", "))
                };
                self.reply_err(format!("{e}{hint}"))
            }
        }
    }

    /// Wrap output as a successful reply, attaching the event cursor.
    fn reply(&self, output: impl Into<String>) -> Reply {
        Reply::ok(output)
    }

    fn reply_err(&self, output: impl Into<String>) -> Reply {
        Reply::err(output)
    }

    // ------------------------------------------------------------- formatting

    pub fn list_tasks(&self) -> String {
        let infos = self.agent.infos();
        if infos.is_empty() {
            return "(no running tasks)".to_string();
        }
        infos.iter().map(|t| t.line()).collect::<Vec<_>>().join("\n")
    }

    fn task_count_text(&self) -> String {
        format!("{} task(s) running", self.agent.len())
    }

    fn how_to_stop(&self, name: &str) -> String {
        if self.opts.mode == Mode::Daemon {
            format!("stop it from any terminal: agentctl stop {name}")
        } else {
            format!("stop it here with: stop {name}")
        }
    }
    pub fn status_text(&self) -> String {
        let session = self.session();
        let mut out = String::new();
        out.push_str(&format!(
            "agent      : v{VERSION} ({}, api level {})\n",
            self.opts.mode.as_str(),
            crate::protocol::API_LEVEL
        ));
        out.push_str(&format!("pid        : {}\n", std::process::id()));
        out.push_str(&format!("uptime     : {}s\n", self.uptime_secs()));
        out.push_str(&format!("tasks      : {}\n", self.task_count_text()));
        out.push_str(&format!(
            "browser    : {}\n",
            if self.opts.allow_spawn { "enabled" } else { "disabled" }
        ));
        out.push_str(&format!("landing    : {GOOGLE_URL}\n"));
        out.push_str(&format!(
            "control    : {}\n",
            match &session {
                Some(s) => format!("127.0.0.1:{} (token {}...)", s.port, &s.token[..8.min(s.token.len())]),
                None => "not published (standalone mode)".to_string(),
            }
        ));
        out.push_str(&self.list_tasks());
        out
    }

    pub fn daemon_status_text(&self) -> String {
        let dir = self.session_dir();
        let session = self.session();
        let mut out = String::new();
        out.push_str(&format!("mode       : {}\n", self.opts.mode.as_str()));
        out.push_str(&format!("pid        : {}\n", std::process::id()));
        out.push_str(&format!("uptime     : {}s\n", self.uptime_secs()));
        out.push_str(&format!("version    : {VERSION}\n"));
        out.push_str(&format!(
            "session dir: {}\n",
            match &dir {
                Some(d) => format!("{} (from {})", d.path().display(), d.source()),
                None => "(none)".to_string(),
            }
        ));
        out.push_str(&format!(
            "session    : {}\n",
            match &dir {
                Some(d) => d.file().display().to_string(),
                None => "(not written)".to_string(),
            }
        ));
        out.push_str(&format!(
            "endpoint   : {}\n",
            match &session {
                Some(s) => s.endpoint(),
                None => "n/a".to_string(),
            }
        ));
        out.push_str(&format!(
            "clients    : {} recorded, {} event(s) buffered\n",
            self.clients().len(),
            self.event_backlog()
        ));
        out.push_str(&format!("tasks      : {}\n", self.task_count_text()));
        out.push_str(&self.list_tasks());
        out
    }
}

/// The ladder, for tests and the `browsers` command.
pub fn ladder_routes() -> Vec<Route> {
    launcher::candidates().into_iter().map(|(route, _)| route).collect()
}

pub fn help_text() -> String {
    "agent commands (identical in standalone and daemon mode):\n\
     \x20 help | ?                       this list\n\
     \x20 version                        version and api level\n\
     \x20 status                         uptime, tasks, control endpoint\n\
     \x20 daemon_status                  daemon detail: session file, clients, events\n\
     \x20 list | tasks                   running tasks\n\
     \x20 browser [url]                  open default -> chrome -> firefox (default: Google)\n\
     \x20 open_browser [url]             subroutine form of the above\n\
     \x20 browsers                       show the fallback ladder\n\
     \x20 repeat_enter [ms]              press Enter on an interval\n\
     \x20 repeat_enter_jitter [b] [j]    press Enter with jitter\n\
     \x20 stop <task>                    stop one task\n\
     \x20 stopall                        stop every task\n\
     \x20 events [since]                 recent daemon events\n\
     \x20 clients                        recent client connections\n\
     \x20 daemon_stop                    stop the agent (daemon or standalone)\n\
     \n\
     console-only: quit | exit leaves a standalone session\n"
        .to_string()
}
