//! Basic web interactivity for the agent.
//!
//! Standard library only: a tiny single-threaded-per-connection HTTP/1.1
//! server bound to **loopback**. It exposes an HTML dashboard plus a small
//! JSON API so the same commands you can type at the agent console can also be
//! driven from a browser:
//!
//! | Method | Path        | Purpose                                        |
//! |--------|-------------|------------------------------------------------|
//! | GET    | `/`         | dashboard (buttons + live task list)           |
//! | GET    | `/api/status` | JSON: uptime, running tasks, browser ladder  |
//! | GET    | `/api/cmd`  | run `?cmd=<command>`                           |
//! | POST   | `/api/cmd`  | same, body or query may carry `cmd`            |
//! | GET    | `/favicon.ico` | 204, keeps the console quiet                |
//!
//! Binding to `127.0.0.1` is deliberate: the API can start subroutines on this
//! machine and launch browsers, so it must not be reachable from the network.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use routines::launcher;

/// Body size ceiling, so a hostile local client cannot exhaust memory.
const MAX_BODY: usize = 64 * 1024;
/// Socket read/write budget for one request.
const IO_TIMEOUT: Duration = Duration::from_secs(5);

/// Outcome of a dispatched command, rendered to the client as JSON.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    pub ok: bool,
    pub output: String,
}

impl Reply {
    pub fn ok(output: impl Into<String>) -> Self {
        Self { ok: true, output: output.into() }
    }

    pub fn err(output: impl Into<String>) -> Self {
        Self { ok: false, output: output.into() }
    }
}

/// Snapshot of the agent, used by the dashboard's live panel.
#[derive(Debug, Clone, Default)]
pub struct AgentStatus {
    pub uptime_secs: u64,
    pub tasks: Vec<String>,
    pub spawning: bool,
    /// False when the browser routines were switched off at startup.
    pub allow_spawn: bool,
}

pub type Dispatch = Arc<dyn Fn(&str) -> Reply + Send + Sync>;
pub type StatusFn = Arc<dyn Fn() -> AgentStatus + Send + Sync>;

pub struct Config {
    pub port: u16,
    pub allow_spawn: bool,
    pub open_command: String,
}

pub struct AgentWeb {
    listener: TcpListener,
    dispatch: Dispatch,
    status: StatusFn,
    /// Guards against two web clicks launching two browsers at once.
    spawning: Arc<AtomicBool>,
    started: Instant,
}

impl AgentWeb {
    pub fn start(config: Config, dispatch: Dispatch, status: StatusFn) -> std::io::Result<Self> {
        // Loopback only, by construction.
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))?;
        let web = Self {
            listener,
            dispatch,
            status,
            spawning: Arc::new(AtomicBool::new(false)),
            started: Instant::now(),
        };
        Ok(web)
    }

    /// The address actually bound (resolves port 0 to the chosen port).
    pub fn addr(&self) -> SocketAddr {
        self.listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
    }

    /// Serve until the process exits. Each connection runs on its own thread.
    pub fn serve(self) -> std::io::Result<()> {
        for stream in self.listener.incoming() {
            match stream {
                Ok(stream) => {
                    let ctx = self.clone_for_connection();
                    thread::spawn(move || {
                        if let Err(e) = ctx.handle(stream) {
                            eprintln!("[web] connection error: {e}");
                        }
                    });
                }
                Err(e) => eprintln!("[web] accept error: {e}"),
            }
        }
        Ok(())
    }

    fn clone_for_connection(&self) -> ConnectionContext {
        ConnectionContext {
            dispatch: Arc::clone(&self.dispatch),
            status: Arc::clone(&self.status),
            spawning: Arc::clone(&self.spawning),
            started: self.started,
        }
    }
}

struct ConnectionContext {
    dispatch: Dispatch,
    status: StatusFn,
    spawning: Arc<AtomicBool>,
    started: Instant,
}

/// Parsed request line + headers + body.
struct Request {
    method: String,
    path: String,
    query: String,
    body: String,
}

impl ConnectionContext {
    fn handle(&self, mut stream: TcpStream) -> std::io::Result<()> {
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;

        let request = match read_request(&mut stream)? {
            Some(r) => r,
            None => {
                // Not an HTTP request (raw socket probe, port scanner, ...).
                return write_response(
                    &mut stream,
                    400,
                    "text/plain; charset=utf-8",
                    b"bad request\n",
                );
            }
        };

        match (request.method.as_str(), request.path.as_str()) {
            ("GET", "/") | ("GET", "/index.html") => write_response(
                &mut stream,
                200,
                "text/html; charset=utf-8",
                dashboard_html(self.spawning.load(Ordering::Relaxed)).as_bytes(),
            ),
            ("GET", "/api/status") | ("GET", "/status") => write_response(
                &mut stream,
                200,
                "application/json",
                self.status_json().as_bytes(),
            ),
            ("GET", "/api/cmd") | ("GET", "/cmd") => {
                let cmd = query_param(&request.query, "cmd").unwrap_or_default();
                self.run_command(&mut stream, &cmd)
            }
            ("POST", "/api/cmd") | ("POST", "/cmd") => {
                // Query first, then the form body; either may carry `cmd`.
                let cmd = query_param(&request.query, "cmd")
                    .or_else(|| query_param(&request.body, "cmd"))
                    .unwrap_or_default();
                self.run_command(&mut stream, &cmd)
            }
            ("GET", "/api/browser") | ("GET", "/browser") => {
                self.run_command(&mut stream, "open_browser")
            }
            ("GET", "/api/help") | ("GET", "/help") | ("GET", "/api/commands") => {
                write_response(&mut stream, 200, "text/plain; charset=utf-8", HELP.as_bytes())
            }
            ("OPTIONS", _) => {
                let mut response = Vec::new();
                response.extend_from_slice(b"HTTP/1.1 204 No Content\r\n");
                response.extend_from_slice(b"Allow: GET, POST, OPTIONS\r\n");
                response.extend_from_slice(b"Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n");
                response.extend_from_slice(b"Access-Control-Allow-Headers: content-type\r\n");
                response.extend_from_slice(b"Access-Control-Allow-Origin: *\r\n");
                response.extend_from_slice(b"Content-Length: 0\r\n");
                response.extend_from_slice(b"Connection: close\r\n\r\n");
                stream.write_all(&response)?;
                Ok(())
            }
            ("GET", "/favicon.ico") => {
                write_response(&mut stream, 204, "image/x-icon", b"")
            }
            ("GET", "/ws") | ("GET", "/websocket") => write_response(
                &mut stream,
                501,
                "text/plain; charset=utf-8",
                b"websockets are not implemented; the dashboard polls /api/status\n",
            ),
            _ => write_response(
                &mut stream,
                404,
                "text/plain; charset=utf-8",
                b"not found\n",
            ),
        }
    }

    fn run_command(&self, stream: &mut TcpStream, cmd: &str) -> std::io::Result<()> {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            return write_response(
                stream,
                400,
                "application/json",
                json_reply(false, "empty command", None).as_bytes(),
            );
        }

        // The one command the web layer serializes: letting a browser fire
        // repeated launches is the difference between a demo and a nuisance.
        if is_browser_command(cmd) {
            if self
                .spawning
                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                return write_response(
                    stream,
                    429,
                    "application/json",
                    json_reply(false, "a browser launch is already in flight", Some(cmd))
                        .as_bytes(),
                );
            }
        }

        let reply = (self.dispatch)(cmd);

        if is_browser_command(cmd) {
            let guard = Arc::clone(&self.spawning);
            thread::spawn(move || {
                // Give the OS a moment to register the new process before
                // another request is allowed through.
                thread::sleep(Duration::from_millis(1500));
                guard.store(false, Ordering::SeqCst);
            });
        }

        // 4xx for anything the dispatcher rejected, so callers see the failure.
        let code = if reply.ok { 200 } else { 400 };
        write_response(
            stream,
            code,
            "application/json",
            json_reply(reply.ok, &reply.output, Some(cmd)).as_bytes(),
        )
    }

    fn status_json(&self) -> String {
        let status = (self.status)();
        let spawning = self.spawning.load(Ordering::Relaxed);

        let uptime = self.started.elapsed().as_secs();
        let tasks: Vec<String> = status.tasks.iter().map(|t| json_string(t)).collect();
        let ladder: Vec<String> = launcher::describe_ladder()
            .iter()
            .map(|l| json_string(l))
            .collect();

        format!(
            "{{\"ok\":true,\"uptime_secs\":{uptime},\"tasks\":[{tasks}],\
             \"browser_spawn_in_flight\":{spawning},\"allow_spawn\":{allow},\
             \"browser_ladder\":[{ladder}]}}",
            tasks = tasks.join(","),
            allow = status.allow_spawn,
            ladder = ladder.join(","),
        )
    }
}

fn is_browser_command(cmd: &str) -> bool {
    let first = cmd.split_whitespace().next().unwrap_or("");
    matches!(first, "open_browser" | "browser" | "browse" | "open")
}

// --------------------------------------------------------------- HTTP parsing

fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut reader = BufReader::new(stream.try_clone()?);

    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Ok(None);
    }
    let mut parts = request_line.split_whitespace();
    let method = match parts.next() {
        Some(m) => m.to_ascii_uppercase(),
        None => return Ok(None),
    };
    let target = parts.next().unwrap_or("/").to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };

    let mut content_length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            }
        }
    }

    let mut body = String::new();
    if content_length > 0 {
        let take = content_length.min(MAX_BODY);
        let mut buf = vec![0u8; take];
        reader.read_exact(&mut buf)?;
        body = String::from_utf8_lossy(&buf).to_string();
    }

    Ok(Some(Request { method, path: percent_decode(&path), query, body }))
}

fn write_response(
    stream: &mut TcpStream,
    code: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match code {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        429 => "Too Many Requests",
        501 => "Not Implemented",
        _ => "Unknown",
    };
    let mut response = Vec::with_capacity(body.len() + 160);
    response.extend_from_slice(format!("HTTP/1.1 {code} {reason}\r\n").as_bytes());
    response.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
    response.extend_from_slice(format!("Content-Length: {}\r\n", body.len()).as_bytes());
    response.extend_from_slice(b"Cache-Control: no-store\r\n");
    response.extend_from_slice(b"X-Content-Type-Options: nosniff\r\n");
    response.extend_from_slice(b"Connection: close\r\n\r\n");
    response.extend_from_slice(body);
    stream.write_all(&response)?;
    stream.flush()
}

/// First value for `key` in an `a=1&b=2` style string.
fn query_param(query: &str, key: &str) -> Option<String> {
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = match pair.split_once('=') {
            Some((n, v)) => (n, v),
            None => (pair, ""),
        };
        if name == key {
            return Some(percent_decode(&value.replace('+', " ")));
        }
    }
    None
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
            if let Some(value) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(value);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn json_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len() + 8);
    for ch in input.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn json_string(input: &str) -> String {
    format!("\"{}\"", json_escape(input))
}

fn json_reply(ok: bool, output: &str, cmd: Option<&str>) -> String {
    let cmd = match cmd {
        Some(c) => json_string(c),
        None => "null".to_string(),
    };
    format!(
        "{{\"ok\":{ok},\"cmd\":{cmd},\"output\":{}}}",
        json_string(output)
    )
}

/// Cheap lock helper: a poisoned lock still yields usable state.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

// ------------------------------------------------------------------ dashboard

pub const HELP: &str = "\
commands (console or /api/cmd?cmd=...):
  help                     show this list
  status                   uptime, running tasks, browser ladder
  list / tasks             list running tasks
  browser [url]            open a browser: default -> chrome -> firefox
  open_browser [url]       subroutine form of the above
  open [url]               alias for open_browser
  browsers                 show the fallback ladder without launching
  repeat_enter [ms]        start the Enter routine
  repeat_enter_jitter [b] [j]
  stop <task>              stop a running task
  stopall                  stop everything
  quit                     exit the agent (console only)

web:
  GET  /                   dashboard
  GET  /api/status         JSON status
  GET  /api/cmd?cmd=...    run a command
  POST /api/cmd            cmd=<command>
  GET  /api/browser        open_browser shorthand
";

fn dashboard_html(spawning: bool) -> String {
    let html = include_str!("dashboard.html");
    html.replace("{{SPAWNING}}", if spawning { "true" } else { "false" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_query_parameters() {
        let query = "cmd=open_browser%20https%3A%2F%2Fexample.com&x=1";
        assert_eq!(
            query_param(query, "cmd").as_deref(),
            Some("open_browser https://example.com")
        );
        assert_eq!(query_param(query, "missing"), None);
    }

    #[test]
    fn form_bodies_use_plus_for_spaces() {
        assert_eq!(
            query_param("cmd=repeat_enter+500", "cmd").as_deref(),
            Some("repeat_enter 500")
        );
    }

    #[test]
    fn json_is_escaped() {
        assert_eq!(json_string("a\"b\n"), "\"a\\\"b\\n\"");
        assert_eq!(json_reply(true, "hi", Some("list")), "{\"ok\":true,\"cmd\":\"list\",\"output\":\"hi\"}");
    }

    #[test]
    fn browser_commands_are_recognized() {
        assert!(is_browser_command("open_browser"));
        assert!(is_browser_command("open_browser https://x.test"));
        assert!(is_browser_command("browser"));
        assert!(!is_browser_command("list"));
    }

    #[test]
    fn dashboard_has_no_placeholder_left() {
        let html = dashboard_html(false);
        assert!(!html.contains("{{SPAWNING}}"));
        assert!(html.contains("/api/status"));
    }

    /// End-to-end check of the HTTP layer on an ephemeral port.
    #[test]
    fn serves_status_and_commands() {
        let dispatch: Dispatch = Arc::new(|cmd: &str| match cmd {
            "status" => Reply::ok("all good"),
            "boom" => Reply::err("nope"),
            other => Reply::ok(format!("ran {other}")),
        });
        let status: StatusFn = Arc::new(|| AgentStatus {
            uptime_secs: 1,
            tasks: vec!["repeat_enter".into()],
            spawning: false,
            allow_spawn: true,
        });

        let web = AgentWeb::start(
            Config { port: 0, allow_spawn: true, open_command: "/".into() },
            dispatch,
            status,
        )
        .expect("bind loopback");
        let addr = web.addr();
        thread::spawn(move || {
            let _ = web.serve();
        });

        let get = |path: &str| -> String {
            let mut stream = TcpStream::connect(addr).expect("connect");
            stream
                .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
                .expect("write");
            let mut response = String::new();
            stream.read_to_string(&mut response).expect("read");
            response
        };

        let status_response = get("/api/status");
        assert!(status_response.starts_with("HTTP/1.1 200"));
        assert!(status_response.contains("\"tasks\":[\"repeat_enter\"]"));

        // The browser shorthand must dispatch a real command, without a
        // leading slash. Done before the other browser tests, because this
        // takes the launch guard for ~1.5s.
        let browser = get("/api/browser");
        assert!(browser.starts_with("HTTP/1.1 200"), "got: {browser}");
        assert!(browser.contains("ran open_browser"), "got: {browser}");
        assert!(!browser.contains("unknown command"));

        let cmd_response = get("/api/cmd?cmd=open_browser%20https%3A%2F%2Fx.test");
        assert!(cmd_response.contains("already in flight") || cmd_response.contains("ran open_browser"));

        let non_browser = get("/api/cmd?cmd=status");
        assert!(non_browser.starts_with("HTTP/1.1 200"), "got: {non_browser}");
        assert!(non_browser.contains("all good"), "got: {non_browser}");

        let failure = get("/api/cmd?cmd=boom");
        assert!(failure.starts_with("HTTP/1.1 400"));

        let empty = get("/api/cmd?cmd=");
        assert!(empty.starts_with("HTTP/1.1 400"));

        let missing = get("/nope");
        assert!(missing.starts_with("HTTP/1.1 404"));
    }

    /// A second browser request while one is in flight is refused with 429.
    #[test]
    fn concurrent_browser_launches_are_refused() {
        let dispatch: Dispatch = Arc::new(|cmd: &str| Reply::ok(format!("ran {cmd}")));
        let status: StatusFn = Arc::new(|| AgentStatus::default());

        let web = AgentWeb::start(
            Config { port: 0, allow_spawn: true, open_command: String::new() },
            dispatch,
            status,
        )
        .expect("bind loopback");
        let addr = web.addr();
        thread::spawn(move || {
            let _ = web.serve();
        });

        let get = |path: &str| -> String {
            let mut stream = TcpStream::connect(addr).expect("connect");
            stream
                .write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
                .expect("write");
            let mut response = String::new();
            stream.read_to_string(&mut response).expect("read");
            response
        };

        // First request takes the guard (held for ~1.5s), so a request that
        // arrives right after it must be refused.
        let first = get("/api/browser");
        assert!(first.starts_with("HTTP/1.1 200"), "got: {first}");

        let second = get("/api/browser");
        assert!(
            second.starts_with("HTTP/1.1 429"),
            "second launch should be refused, got: {second}"
        );
        assert!(second.contains("already in flight"), "got: {second}");
    }
}
