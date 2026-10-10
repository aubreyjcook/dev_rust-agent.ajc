//! Command-line options and terminal output for the v04 agent.

use std::path::PathBuf;

use agent_core::{SessionDir, DEFAULT_PORT};

/// How the process is being started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// v03 behaviour: one process, commands typed at its own console.
    Standalone,
    /// v04: a control socket other terminals can reach.
    Daemon,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Standalone => "standalone",
            Role::Daemon => "daemon",
        }
    }
}

pub struct Options {
    pub role: Role,
    /// Bind port; `None` means "try the default range".
    pub port: Option<u16>,
    pub no_open: bool,
    /// Override session-directory discovery.
    pub session_dir: Option<PathBuf>,
    /// `--quiet` suppresses the startup banner.
    pub quiet: bool,
    pub error: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            role: Role::Standalone,
            port: None,
            no_open: false,
            session_dir: None,
            quiet: false,
            error: None,
        }
    }
}

impl Options {
    /// Parse `args`, which must already have the `run`/`daemon` word removed
    /// and carried in `role`.
    pub fn parse(role: Role, args: Vec<String>) -> Self {
        let mut opts = Options { role, ..Default::default() };
        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--port" | "-p" => match iter.next().and_then(|v| v.parse::<u16>().ok()) {
                    Some(port) => opts.port = Some(port),
                    None => opts.error = Some("--port needs a number between 0 and 65535".into()),
                },
                "--session-dir" => match iter.next() {
                    Some(dir) => opts.session_dir = Some(PathBuf::from(dir)),
                    None => opts.error = Some("--session-dir needs a path".into()),
                },
                "--no-open" => opts.no_open = true,
                "--quiet" | "-q" => opts.quiet = true,
                "--help" | "-h" => opts.error = Some("help requested".into()),
                other if other.starts_with('-') => {
                    opts.error = Some(format!("unknown option: {other}"))
                }
                _ => {}
            }
        }
        opts
    }

    /// Resolve the session directory, honouring an explicit override.
    pub fn resolve_session_dir(&self) -> SessionDir {
        match &self.session_dir {
            Some(dir) => SessionDir::at(dir.clone()),
            None => SessionDir::discover(),
        }
    }
}

const COMMON_FLAGS: &str = "\
options:
  -p, --port <n>        control socket port (default: first free near 45917)
      --session-dir <p> where the session file is written/read
      --no-open         refuse to launch browsers
  -q, --quiet           suppress the startup banner
  -h, --help            show this message
";

pub fn usage(role: Option<Role>) -> String {
    let mut out = String::from("agent v04 - standalone or daemon, same commands either way\n\n");
    out.push_str("usage:\n  agent [options]              standalone: commands on this console\n");
    out.push_str("  agent run [options]          same thing, explicitly\n");
    out.push_str("  agent daemon [options]       system-wide daemon: any terminal can drive it\n");
    out.push_str("  agent daemon --foreground    stay attached (default for `daemon`)\n");
    out.push_str("  agent --help                 this message\n\n");
    out.push_str(COMMON_FLAGS);
    out.push_str(&format!(
        "\ncontrol socket:\n  loopback only, port {} by default; the port and a session token are\n  \
         written to a session file so `agentctl` can find the daemon.\n",
        DEFAULT_PORT
    ));
    if let Some(role) = role {
        out.push_str(&format!("\n(starting in {} mode)\n", role.as_str()));
    }
    out
}

pub fn banner(opts: &Options, session_dir: &SessionDir) {
    if opts.quiet {
        return;
    }
    println!("=== Agent v04 ({}) ===", opts.role.as_str());
    println!("  open_browser -> default browser, then Chrome, then Firefox");
    println!("  landing page -> {}", routines::GOOGLE_URL);
    println!("  session dir  -> {} (from {})", session_dir.path().display(), session_dir.source());
    if opts.no_open {
        println!("  browser open -> disabled (--no-open)");
    }
    println!();
}

/// Print to the stream that suits the role: a daemon must not clutter stdout,
/// which a supervising process may be capturing.
pub fn note(role: Role, text: &str) {
    match role {
        Role::Standalone => println!("{text}"),
        Role::Daemon => eprintln!("{text}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn role_defaults_to_standalone() {
        let opts = Options::parse(Role::Standalone, args(&[]));
        assert_eq!(opts.role, Role::Standalone);
        assert!(opts.port.is_none());
        assert!(!opts.no_open);
        assert!(opts.error.is_none());
    }

    #[test]
    fn daemon_flags_parse() {
        let opts = Options::parse(
            Role::Daemon,
            args(&["--port", "46000", "--no-open", "--session-dir", "C:\\tmp\\sess"]),
        );
        assert_eq!(opts.role, Role::Daemon);
        assert_eq!(opts.port, Some(46000));
        assert!(opts.no_open);
        assert_eq!(
            opts.resolve_session_dir().path(),
            std::path::Path::new("C:\\tmp\\sess")
        );
    }

    #[test]
    fn bad_input_is_reported() {
        assert!(Options::parse(Role::Daemon, args(&["--port", "x"])).error.is_some());
        assert!(Options::parse(Role::Daemon, args(&["--session-dir"])).error.is_some());
        assert!(Options::parse(Role::Daemon, args(&["--nope"])).error.is_some());
        assert_eq!(
            Options::parse(Role::Daemon, args(&["--help"])).error.as_deref(),
            Some("help requested")
        );
    }

    #[test]
    fn usage_mentions_both_modes() {
        let text = usage(None);
        assert!(text.contains("standalone"));
        assert!(text.contains("daemon"));
        assert!(text.contains("agentctl"));
    }
}
