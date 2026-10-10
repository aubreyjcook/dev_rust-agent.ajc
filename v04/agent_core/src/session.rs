//! How a client finds a running daemon on this machine.
//!
//! The daemon binds a loopback control socket and writes a small JSON session
//! file describing it. Clients (`agentctl`, and the agent's own `status`
//! passthrough) read that file to discover the port and the shared secret.
//!
//! The directory is *discovered*, never assumed: an explicit
//! `AGENT_SESSION_DIR` wins, then the platform's per-user state directory,
//! then the system temp directory. The first one that actually accepts a write
//! is used, because a candidate that exists but is not writable is worse than
//! no candidate at all.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::protocol::{json_number, json_string_field, random_token, VERSION};

/// Name of the session file inside the session directory.
pub const SESSION_FILE: &str = "session.json";

/// Port the daemon prefers; nearby ports are tried if it is busy.
pub const DEFAULT_PORT: u16 = 45917;
/// How many consecutive ports to try before falling back to an ephemeral one.
pub const PORT_ATTEMPTS: u16 = 12;

/// A daemon's published coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub started_unix: u64,
    pub version: String,
}

impl Session {
    pub fn new(port: u16) -> Self {
        Self {
            port,
            token: random_token(),
            pid: process::id(),
            started_unix: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            version: VERSION.to_string(),
        }
    }

    pub fn endpoint(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"port\":{},\"token\":\"{}\",\"pid\":{},\"started_unix\":{},\"version\":\"{}\"}}",
            self.port,
            self.token,
            self.pid,
            self.started_unix,
            self.version
        )
    }

    /// Parse a session file. Anything unexpected yields `None`, so a corrupt
    /// or foreign file is treated as "no daemon" rather than a hard error.
    pub fn from_json(text: &str) -> Option<Self> {
        let port = json_number(text, "port")?;
        if port == 0 || port > u16::MAX as u64 {
            return None;
        }
        let token = json_string_field(text, "token")?;
        if token.len() < 8 {
            return None;
        }
        Some(Self {
            port: port as u16,
            token,
            pid: json_number(text, "pid").unwrap_or(0) as u32,
            started_unix: json_number(text, "started_unix").unwrap_or(0),
            version: json_string_field(text, "version").unwrap_or_else(|| VERSION.to_string()),
        })
    }
}

/// A resolved session directory.
#[derive(Debug, Clone)]
pub struct SessionDir {
    dir: PathBuf,
    source: String,
    writable: bool,
}

impl SessionDir {
    /// Walk the candidate list and take the first writable directory.
    pub fn discover() -> Self {
        for (dir, source) in candidates() {
            let writable = probe_writable(&dir);
            if writable {
                return Self { dir, source, writable };
            }
        }
        // Nothing was writable. Report the preferred location so the error
        // message names a real path the user can fix.
        let (dir, source) = candidates()
            .into_iter()
            .next()
            .unwrap_or_else(|| (env::temp_dir().join("rust-agent"), "temp".into()));
        Self { dir, source, writable: false }
    }

    /// Force a specific directory, mostly for tests and `--session-dir`.
    pub fn at(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        let writable = probe_writable(&dir);
        Self { dir, source: "explicit".into(), writable }
    }

    pub fn path(&self) -> &Path {
        &self.dir
    }

    pub fn file(&self) -> PathBuf {
        self.dir.join(SESSION_FILE)
    }

    /// Where this directory came from, for user-facing messages.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Write the session file. Fails loudly: without it, no client can find
    /// this daemon, so the daemon should not pretend to be reachable.
    pub fn save(&self, session: &Session) -> std::io::Result<PathBuf> {
        fs::create_dir_all(&self.dir)?;
        let file = self.file();
        fs::write(&file, session.to_json())?;
        restrict_permissions(&file);
        Ok(file)
    }

    /// Read the session file, if it parses.
    pub fn load(&self) -> Option<Session> {
        let text = fs::read_to_string(self.file()).ok()?;
        Session::from_json(&text)
    }

    /// Remove the session file, ignoring "already gone".
    pub fn clear(&self) -> std::io::Result<()> {
        match fs::remove_file(self.file()) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Candidate directories, best first.
fn candidates() -> Vec<(PathBuf, String)> {
    let mut list: Vec<(PathBuf, String)> = Vec::new();

    if let Some(dir) = env::var_os("AGENT_SESSION_DIR").filter(|d| !d.is_empty()) {
        list.push((PathBuf::from(dir), "AGENT_SESSION_DIR".into()));
    }

    if cfg!(windows) {
        if let Some(base) = env::var_os("LOCALAPPDATA").filter(|d| !d.is_empty()) {
            list.push((PathBuf::from(base).join("rust-agent"), "LOCALAPPDATA".into()));
        }
        if let Some(base) = env::var_os("APPDATA").filter(|d| !d.is_empty()) {
            list.push((PathBuf::from(base).join("rust-agent"), "APPDATA".into()));
        }
    } else if cfg!(target_os = "macos") {
        if let Some(home) = env::var_os("HOME").filter(|d| !d.is_empty()) {
            list.push((
                PathBuf::from(home).join("Library/Application Support/rust-agent"),
                "HOME".into(),
            ));
        }
    } else {
        if let Some(base) = env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) {
            list.push((PathBuf::from(base).join("rust-agent"), "XDG_RUNTIME_DIR".into()));
        }
        if let Some(home) = env::var_os("HOME").filter(|d| !d.is_empty()) {
            list.push((PathBuf::from(home).join(".local/state/rust-agent"), "HOME".into()));
        }
    }

    let temp = env::temp_dir().join("rust-agent");
    list.push((temp.clone(), "temp".into()));

    // Last resort: a dot-directory in the working directory. Keeps a daemon
    // usable in restricted environments where no standard location is writable.
    if let Ok(cwd) = env::current_dir() {
        list.push((cwd.join(".rust-agent"), "working directory".into()));
    }

    list
}

/// Try an actual write, because existence checks lie about permissions.
fn probe_writable(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(format!(".probe-{}", process::id()));
    match fs::write(&probe, b"probe") {
        Ok(()) => {
            let _ = fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

#[cfg(unix)]
fn restrict_permissions(file: &Path) {
    use std::os::unix::fs::PermissionsExt;
    // The token is a bearer credential; keep it to the owner where possible.
    let _ = fs::set_permissions(file, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_permissions(_file: &Path) {
    // On Windows the file inherits the user profile ACL.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_round_trips_through_json() {
        let session = Session::new(45917);
        let parsed = Session::from_json(&session.to_json()).expect("parse");
        assert_eq!(parsed, session);
        assert_eq!(parsed.endpoint(), "127.0.0.1:45917");
    }

    #[test]
    fn corrupt_sessions_are_rejected_not_fatal() {
        assert!(Session::from_json("").is_none());
        assert!(Session::from_json("{}").is_none());
        assert!(Session::from_json("{\"port\":0,\"token\":\"0123456789\"}").is_none());
        assert!(Session::from_json("{\"port\":99999,\"token\":\"0123456789\"}").is_none());
        assert!(Session::from_json("{\"port\":45917,\"token\":\"short\"}").is_none());
        assert!(Session::from_json("not json at all").is_none());
    }

    #[test]
    fn a_directory_this_test_can_write_is_reported_writable() {
        let dir = env::temp_dir().join(format!("v04-session-test-{}", process::id()));
        let session_dir = SessionDir::at(&dir);
        assert!(session_dir.is_writable(), "temp dir should be writable");

        let session = Session::new(1234);
        session_dir.save(&session).expect("save");
        assert_eq!(session_dir.load(), Some(session.clone()));
        session_dir.clear().expect("clear");
        assert!(session_dir.load().is_none());
        // Clearing twice is fine.
        session_dir.clear().expect("clear again");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovery_always_yields_a_writable_directory() {
        // In any environment the agent supports, one candidate must be usable.
        let dir = SessionDir::discover();
        assert!(
            dir.is_writable(),
            "no writable session directory found (source: {})",
            dir.source()
        );
        assert!(dir.file().ends_with(SESSION_FILE));
    }
}
