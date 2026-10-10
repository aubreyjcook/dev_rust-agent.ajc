//! The client end of the control socket — what `agentctl` uses.
//!
//! Every command opens a short-lived connection, sends one request, reads one
//! response and hangs up. That is cheap on loopback, keeps no shared state
//! between terminals, and means a client that dies cannot leave the daemon
//! holding a half-open session.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpStream};
use std::time::Duration;

use crate::protocol::{Request, Response, VERSION};
use crate::session::{Session, SessionDir};

/// Connect timeout: loopback, so anything slower means nobody is listening.
const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
/// Read timeout for a reply. Commands are fast; a browser launch replies before
/// the launch finishes, so this stays short.
const READ_TIMEOUT: Duration = Duration::from_secs(20);

/// What a probe found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// A daemon answered and its API level matches.
    Running(Session),
    /// No session file, or nothing listening.
    NotRunning,
    /// Something is there but wrong: a foreign file, a dead port, or a
    /// version/API mismatch.
    Unusable(String),
}

impl Probe {
    pub fn is_running(&self) -> bool {
        matches!(self, Probe::Running(_))
    }

    /// One line describing the outcome, for CLI output.
    pub fn summary(&self) -> String {
        match self {
            Probe::Running(session) => format!(
                "daemon running: endpoint 127.0.0.1:{} (pid {}, v{})",
                session.port, session.pid, session.version
            ),
            Probe::NotRunning => "no daemon running".to_string(),
            Probe::Unusable(why) => format!("daemon not usable: {why}"),
        }
    }
}

/// The outcome of sending one command.
#[derive(Debug, Clone)]
pub struct SendOutcome {
    pub ok: bool,
    pub output: String,
    pub shutdown: bool,
    pub version: String,
    pub events: Vec<(u64, String, String)>,
}

/// A client for one daemon.
#[derive(Debug, Clone)]
pub struct Client {
    session: Session,
}

impl Client {
    pub fn new(session: Session) -> Self {
        Self { session }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    /// Look for a daemon using the discovered session directory.
    pub fn discover() -> Probe {
        Self::probe_dir(&SessionDir::discover())
    }

    /// Look for a daemon in a specific session directory.
    pub fn probe_dir(dir: &SessionDir) -> Probe {
        let session = match dir.load() {
            Some(session) => session,
            None => return Probe::NotRunning,
        };

        match Self::new(session.clone()).handshake() {
            Ok(_) => Probe::Running(session),
            Err(e) => Probe::Unusable(format!("{}: {e}", session.endpoint())),
        }
    }

    fn connect(&self) -> std::io::Result<TcpStream> {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.session.port));
        let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
        stream.set_read_timeout(Some(READ_TIMEOUT))?;
        stream.set_write_timeout(Some(READ_TIMEOUT))?;
        Ok(stream)
    }

    /// Authenticate and confirm the daemon speaks our protocol.
    pub fn handshake(&self) -> Result<String, String> {
        let response = self.send_request(&Request::hello(self.session.token.clone()))?;
        if !response.reply.ok {
            return Err(response.reply.output);
        }
        if response.version != VERSION {
            return Err(format!(
                "version mismatch: client {VERSION}, daemon {}",
                response.version
            ));
        }
        Ok(response.reply.output)
    }

    /// Send one command and read one reply.
    pub fn send(&self, cmd: &str, since: u64) -> Result<SendOutcome, String> {
        let response = self.send_request(&Request {
            token: self.session.token.clone(),
            cmd: cmd.to_string(),
            since,
        })?;

        Ok(SendOutcome {
            ok: response.reply.ok,
            output: response.reply.output,
            shutdown: response.reply.shutdown,
            version: response.version,
            events: response
                .events
                .into_iter()
                .map(|e| (e.seq, e.kind, e.text))
                .collect(),
        })
    }

    fn send_request(&self, request: &Request) -> Result<Response, String> {
        let mut stream = self.connect().map_err(|e| {
            format!("cannot reach the daemon at {}: {e}", self.session.endpoint())
        })?;

        stream
            .write_all(request.to_line().as_bytes())
            .and_then(|_| stream.write_all(b"\n"))
            .and_then(|_| stream.flush())
            .map_err(|e| format!("write failed: {e}"))?;

        let mut reader = BufReader::new(stream.try_clone().map_err(|e| e.to_string())?);
        let mut line = String::new();
        let read = reader.read_line(&mut line).map_err(|e| format!("read failed: {e}"))?;

        let _ = stream.shutdown(Shutdown::Both);
        if read == 0 {
            return Err("daemon closed the connection without replying".to_string());
        }

        Response::from_line(&line)
            .ok_or_else(|| format!("could not parse the daemon's reply: {}", line.trim()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{random_token, VERSION as PROTOCOL_VERSION};

    #[test]
    fn probing_an_empty_directory_reports_not_running() {
        let dir = std::env::temp_dir().join(format!("v04-client-empty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let dir = SessionDir::at(&dir);
        assert_eq!(Client::probe_dir(&dir), Probe::NotRunning);
        assert!(!Probe::NotRunning.is_running());
        let _ = std::fs::remove_dir_all(dir.path());
    }

    #[test]
    fn probing_a_dead_port_reports_unusable() {
        // Bind a port, learn it, then close it: nothing is listening there.
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let dir = std::env::temp_dir().join(format!("v04-client-dead-{}", std::process::id()));
        let dir = SessionDir::at(&dir);
        let session = Session {
            port,
            token: random_token(),
            pid: 999_999,
            started_unix: 0,
            version: PROTOCOL_VERSION.to_string(),
        };
        dir.save(&session).expect("save");

        match Client::probe_dir(&dir) {
            Probe::Unusable(why) => assert!(why.contains(&port.to_string()), "got: {why}"),
            other => panic!("expected Unusable, got {other:?}"),
        }
        assert!(Client::probe_dir(&dir).summary().contains("not usable"));
        let _ = std::fs::remove_dir_all(dir.path());
    }

    #[test]
    fn probe_summaries_are_readable() {
        let session = Session::new(45917);
        let summary = Probe::Running(session).summary();
        assert!(summary.contains("45917"), "got: {summary}");
        assert_eq!(Probe::NotRunning.summary(), "no daemon running");
    }
}
