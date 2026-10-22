//! The wire protocol: newline-delimited JSON over a loopback TCP socket.
//!
//! One request per line, one response per line. Clients may pipeline several
//! requests on one connection, which is what makes an interactive client cheap.
//!
//! Everything here is hand-rolled on purpose: `agent` has no third-party
//! dependencies, and the message shapes are flat enough that a parser is
//! smaller than a dependency would be.

use std::process;
use std::time::{SystemTime, UNIX_EPOCH};

/// Wire/API version. Clients refuse to talk to a daemon with a different one.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Protocol revision, bumped when message shapes change.
pub const API_LEVEL: u32 = 1;
/// Requests longer than this are refused.
pub const MAX_LINE: usize = 16 * 1024;

/// A request from a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub token: String,
    pub cmd: String,
    /// Event sequence the client has already seen, for incremental logs.
    pub since: u64,
}

impl Request {
    pub fn new(token: impl Into<String>, cmd: impl Into<String>) -> Self {
        Self { token: token.into(), cmd: cmd.into(), since: 0 }
    }

    pub fn hello(token: impl Into<String>) -> Self {
        Self::new(token, "hello")
    }

    pub fn to_line(&self) -> String {
        format!(
            "{{\"token\":{},\"cmd\":{},\"since\":{}}}",
            json_string(&self.token),
            json_string(&self.cmd),
            self.since
        )
    }

    /// Parse one wire line. `None` means unparseable, which the server reports
    /// as a protocol error rather than guessing.
    pub fn from_line(line: &str) -> Option<Self> {
        let cmd = json_string_field(line, "cmd").unwrap_or_default();
        let token = json_string_field(line, "token").unwrap_or_default();
        let since = json_number(line, "since").unwrap_or(0);
        if cmd.is_empty() && token.is_empty() {
            return None;
        }
        Some(Self { token, cmd, since })
    }
}

impl Default for Request {
    fn default() -> Self {
        Self { token: String::new(), cmd: String::new(), since: 0 }
    }
}

/// A logging/progress event, kept in a bounded ring inside the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub seq: u64,
    pub unix_ms: u64,
    pub kind: String,
    pub text: String,
}

impl Event {
    pub fn new(seq: u64, kind: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            seq,
            unix_ms: now_unix_ms(),
            kind: kind.into(),
            text: text.into(),
        }
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"seq\":{},\"unix_ms\":{},\"kind\":{},\"text\":{}}}",
            self.seq,
            self.unix_ms,
            json_string(&self.kind),
            json_string(&self.text)
        )
    }
}

/// The daemon's answer to one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub ok: bool,
    pub output: String,
    pub shutdown: bool,
}

impl Reply {
    pub fn ok(output: impl Into<String>) -> Self {
        Self { ok: true, output: output.into(), shutdown: false }
    }

    pub fn err(output: impl Into<String>) -> Self {
        Self { ok: false, output: output.into(), shutdown: false }
    }

    pub fn with_shutdown(mut self) -> Self {
        self.shutdown = true;
        self
    }

    pub fn to_json(&self) -> String {
        format!(
            "{{\"ok\":{},\"output\":{},\"shutdown\":{}}}",
            self.ok,
            json_string(&self.output),
            self.shutdown
        )
    }
}

/// A reply plus the event backlog the client had not seen yet.
#[derive(Debug, Clone)]
pub struct Response {
    pub reply: Reply,
    pub events: Vec<Event>,
    pub version: String,
}

impl Response {
    pub fn new(reply: Reply, events: Vec<Event>) -> Self {
        Self { reply, events, version: VERSION.to_string() }
    }

    pub fn to_json(&self) -> String {
        let events: Vec<String> = self.events.iter().map(Event::to_json).collect();
        format!(
            "{{\"ok\":{},\"output\":{},\"shutdown\":{},\"version\":{},\"events\":[{}]}}",
            self.reply.ok,
            json_string(&self.reply.output),
            self.reply.shutdown,
            json_string(&self.version),
            events.join(",")
        )
    }

    pub fn from_line(line: &str) -> Option<Self> {
        let ok = json_bool(line, "ok")?;
        Some(Self {
            reply: Reply {
                ok,
                output: json_string_field(line, "output").unwrap_or_default(),
                shutdown: json_bool(line, "shutdown").unwrap_or(false),
            },
            events: parse_events(line),
            version: json_string_field(line, "version").unwrap_or_else(|| VERSION.to_string()),
        })
    }
}

/// Pull the `events` array out of a response line.
fn parse_events(line: &str) -> Vec<Event> {
    let start = match line.find("\"events\":[") {
        Some(i) => i + "\"events\":[".len(),
        None => return Vec::new(),
    };
    let bytes = line.as_bytes();
    let mut depth = 1i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut end = bytes.len();
    for (i, ch) in line[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }

    let inner = &line[start..end];
    let mut events = Vec::new();
    for object in split_objects(inner) {
        if let (Some(seq), Some(kind), Some(text)) = (
            json_number(&object, "seq"),
            json_string_field(&object, "kind"),
            json_string_field(&object, "text"),
        ) {
            events.push(Event {
                seq,
                unix_ms: json_number(&object, "unix_ms").unwrap_or(0),
                kind,
                text,
            });
        }
    }
    events
}

/// Split `{...},{...}` into its top-level objects.
fn split_objects(inner: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut start = None;

    for (i, ch) in inner.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        out.push(inner[s..=i].to_string());
                    }
                    start = None;
                }
            }
            _ => {}
        }
    }
    out
}

/// Escaping helpers, grouped so that "serialize a string" and "read a string
/// field" cannot be confused at a call site the way two `json_string`s would be.
pub mod json {
    /// Escape a string for embedding in JSON.
    pub fn escape(input: &str) -> String {
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

    /// Quote a string for embedding in JSON.
    pub fn string(input: &str) -> String {
        format!("\"{}\"", escape(input))
    }

    /// Undo [`escape`]. Unknown escapes are passed through unchanged, which
    /// keeps a foreign or hand-written payload readable instead of mangling it.
    pub fn unescape(input: &str) -> String {
        let mut out = String::with_capacity(input.len());
        let mut chars = input.chars();

        while let Some(ch) = chars.next() {
            if ch != '\\' {
                out.push(ch);
                continue;
            }
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some('/') => out.push('/'),
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some('b') => out.push('\u{08}'),
                Some('f') => out.push('\u{0c}'),
                Some('u') => {
                    // Four hex digits, possibly a surrogate pair.
                    let mut hex = String::new();
                    for _ in 0..4 {
                        match chars.next() {
                            Some(c) => hex.push(c),
                            None => break,
                        }
                    }
                    match u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        Some(c) => out.push(c),
                        None => {
                            out.push_str("\\u");
                            out.push_str(&hex);
                        }
                    }
                }
                Some(other) => {
                    // Not an escape we know about: keep both characters.
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        }
        out
    }
}

pub use json::{string as json_string};

/// Read `"key": <number>` as an unsigned integer.
pub fn json_number(text: &str, key: &str) -> Option<u64> {
    json_raw(text, key)?.trim().parse().ok()
}

/// Read `"key": "value"` as a string, undoing JSON escapes.
pub fn json_string_field(text: &str, key: &str) -> Option<String> {
    let value = json_raw(text, key)?;
    let value = value.trim();
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    Some(json::unescape(inner))
}

/// Undo the escapes that [`json::escape`] produces.
pub fn json_unescape(input: &str) -> String {
    json::unescape(input)
}

/// Read `"key": true|false`.
pub fn json_bool(text: &str, key: &str) -> Option<bool> {
    let value = json_raw(text, key)?;
    match value.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// Extract a top-level field's raw text, respecting string boundaries so a key
/// name appearing inside a value cannot confuse the scan.
pub fn json_raw(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut search_from = 0usize;

    while let Some(offset) = text[search_from..].find(&needle) {
        let at = search_from + offset;

        // Ignore matches that sit inside a string literal.
        if !is_at_top_level(text, at) {
            search_from = at + needle.len();
            continue;
        }

        let after = text[at + needle.len()..].trim_start();
        let after = match after.strip_prefix(':') {
            Some(rest) => rest.trim_start(),
            None => {
                search_from = at + needle.len();
                continue;
            }
        };
        return Some(slice_value(after));
    }
    None
}

/// True when position `at` is outside any string literal before it.
fn is_at_top_level(text: &str, at: usize) -> bool {
    let mut in_string = false;
    let mut escaped = false;
    for ch in text[..at].chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
        }
    }
    !in_string
}

/// Slice one value: a string literal, a nested container, or a bare token.
fn slice_value(text: &str) -> String {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return String::new();
    }

    if bytes[0] == b'"' {
        let mut escaped = false;
        for (i, ch) in text[1..].char_indices() {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                return text[..i + 2].to_string();
            }
        }
        return text.to_string();
    }

    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '[' | '{' => depth += 1,
            ']' | '}' if depth > 0 => depth -= 1,
            ',' | '}' | ']' if depth == 0 => return text[..i].trim().to_string(),
            _ => {}
        }
    }
    text.trim().to_string()
}

// ------------------------------------------------------------------ utilities

/// A random bearer token with no crypto dependency.
///
/// Unpredictability matters (it stops other local users from driving the
/// daemon), absolute cryptographic strength does not: the socket is loopback
/// only and the session file is user-scoped.
pub fn random_token() -> String {
    let mut state = seed();
    let mut out = String::with_capacity(32);
    for _ in 0..4 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let chunk = (state >> 33) as u32;
        out.push_str(&format!("{chunk:08x}"));
    }
    out
}

fn seed() -> u64 {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e3779b97f4a7c15);
    let pid = process::id() as u64;
    let stack = &nanos as *const u64 as u64;
    nanos ^ (pid << 32) ^ stack.rotate_left(17)
}

pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_round_trip() {
        let req = Request::new("tok-12345678", "open_browser https://example.com");
        let parsed = Request::from_line(&req.to_line()).expect("parse");
        assert_eq!(parsed, req);

        let with_since = Request { token: "t".into(), cmd: "status".into(), since: 41 };
        assert_eq!(Request::from_line(&with_since.to_line()), Some(with_since));
    }

    #[test]
    fn unparseable_requests_are_rejected() {
        assert_eq!(Request::from_line(""), None);
        assert_eq!(Request::from_line("{}"), None);
        assert_eq!(Request::from_line("garbage"), None);
    }

    #[test]
    fn replies_round_trip_with_events() {
        let response = Response::new(
            Reply::ok("started"),
            vec![
                Event { seq: 1, unix_ms: 10, kind: "task".into(), text: "started x".into() },
                Event { seq: 2, unix_ms: 11, kind: "task".into(), text: "stopped x".into() },
            ],
        );
        let parsed = Response::from_line(&response.to_json()).expect("parse");
        assert!(parsed.reply.ok);
        assert_eq!(parsed.reply.output, "started");
        assert_eq!(parsed.events.len(), 2);
        assert_eq!(parsed.events[0].seq, 1);
        assert_eq!(parsed.events[1].text, "stopped x");
        assert_eq!(parsed.version, VERSION);
    }

    #[test]
    fn replies_with_no_events_parse() {
        let parsed = Response::from_line(&Response::new(Reply::err("nope"), vec![]).to_json())
            .expect("parse");
        assert!(!parsed.reply.ok);
        assert_eq!(parsed.reply.output, "nope");
        assert!(parsed.events.is_empty());
    }

    #[test]
    fn escaping_survives_awkward_output() {
        let nasty = "quote\" back\\slash\nnewline\ttab ☃";
        let response = Response::new(Reply::ok(nasty), vec![]);
        let parsed = Response::from_line(&response.to_json()).expect("parse");
        assert_eq!(parsed.reply.output, nasty);
    }

    #[test]
    fn a_key_inside_a_value_is_not_mistaken_for_the_field() {
        // "cmd" appears inside token's value; the real cmd must still be found.
        let line = "{\"token\":\"has \\\"cmd\\\": inside\",\"cmd\":\"status\",\"since\":3}";
        assert_eq!(Request::from_line(line).map(|r| r.cmd), Some("status".into()));
    }

    #[test]
    fn tokens_are_long_and_change() {
        let a = random_token();
        let b = random_token();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
