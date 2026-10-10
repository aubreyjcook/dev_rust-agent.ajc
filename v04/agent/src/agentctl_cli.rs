//! `agentctl` command-line handling.

use std::path::PathBuf;

use agent_core::{SessionDir, DEFAULT_PORT, PORT_ATTEMPTS};

/// What the user asked `agentctl` to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invocation {
    /// Run one command against the daemon and exit.
    Command(String),
    /// Interactive session against the daemon.
    Repl,
    /// Report the session file and whether a daemon answers.
    Session,
    /// Start a daemon in the background and wait for it.
    DaemonStart,
    /// Print the client version.
    Version,
}

#[derive(Debug, Clone)]
pub struct Options {
    pub invocation: Invocation,
    pub session_dir: Option<PathBuf>,
    pub port: Option<u16>,
    pub no_open: bool,
    pub verbose: bool,
    pub error: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            invocation: Invocation::Repl,
            session_dir: None,
            port: None,
            no_open: false,
            verbose: false,
            error: None,
        }
    }
}

impl Options {
    pub fn parse(args: Vec<String>) -> Self {
        let mut opts = Options::default();
        let mut remaining: Vec<String> = Vec::new();
        let mut only_positional = false;
        let mut iter = args.into_iter();

        // Global flags may appear before *or after* the command, because
        // `agentctl session --session-dir X` reads more naturally than the
        // other order and users will type both.
        while let Some(arg) = iter.next() {
            if only_positional {
                remaining.push(arg);
                continue;
            }
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
                "--verbose" | "-v" => opts.verbose = true,
                "--help" | "-h" => opts.error = Some("help requested".into()),
                "--version" | "-V" => opts.invocation = Invocation::Version,
                "--" => only_positional = true,
                other if other.starts_with('-') => {
                    opts.error = Some(format!("unknown option: {other}"))
                }
                _ => remaining.push(arg),
            }
        }

        if opts.error.is_some() {
            return opts;
        }

        match remaining.first().map(String::as_str) {
            None => {
                // No command word: interactive, unless --version already won.
            }
            Some("session") => opts.invocation = Invocation::Session,
            Some("daemon-start") | Some("start") => opts.invocation = Invocation::DaemonStart,
            Some("daemon-stop") => opts.invocation = Invocation::Command("daemon_stop".into()),
            Some("daemon-status") => {
                opts.invocation = Invocation::Command("daemon_status".into())
            }
            Some("daemon-restart") => {
                opts.error = Some(
                    "daemon-restart is not a command; use: agentctl daemon-stop && agentctl daemon-start"
                        .into(),
                );
            }
            Some(other) => {
                // Everything after the command word is its argument list: it
                // must reach the daemon verbatim, so it is joined, not parsed.
                let mut parts = vec![other.to_string()];
                parts.extend(remaining.into_iter().skip(1));
                opts.invocation = Invocation::Command(parts.join(" "));
            }
        }

        opts
    }

    /// The session directory to use: explicit, else discovered.
    pub fn session_dir(&self) -> SessionDir {
        match &self.session_dir {
            Some(dir) => SessionDir::at(dir.clone()),
            None => SessionDir::discover(),
        }
    }
}

pub fn usage() -> String {
    format!(
        "agentctl v{} - control a running agent daemon from any terminal\n\n\
         usage:\n\
         \x20 agentctl [options]                 interactive session\n\
         \x20 agentctl <command> [args...]       run one command and exit\n\
         \x20 agentctl session                   where the session file is, and who answers\n\
         \x20 agentctl daemon-status             daemon detail: port, session file, clients\n\
         \x20 agentctl daemon-start              start a daemon in the background, wait for it\n\
         \x20 agentctl daemon-stop               stop the daemon\n\
         \x20 agentctl --version | -V            client version\n\n\
         options:\n\
         \x20 -p, --port <n>        port (used by daemon-start; default {DEFAULT_PORT}..+{})\n\
         \x20     --session-dir <p> session directory to read (default: discovered)\n\
         \x20     --no-open         pass through to a daemon started with daemon-start\n\
         \x20 -v, --verbose         show which session file was used\n\
         \x20 -h, --help            this message\n\n\
         examples:\n\
         \x20 agentctl status\n\
         \x20 agentctl list\n\
         \x20 agentctl browser\n\
         \x20 agentctl repeat_enter 500\n\
         \x20 agentctl stop repeat_enter\n\
         \x20 agentctl events\n\n\
         Every command the agent understands works here. Exit status: 0 accepted,\n\
         1 refused by the daemon, 2 no daemon or bad usage.\n",
        agent_core::VERSION,
        PORT_ATTEMPTS - 1
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts(list: &[&str]) -> Options {
        Options::parse(list.iter().map(|s| s.to_string()).collect())
    }

    #[test]
    fn no_arguments_means_interactive() {
        let parsed = opts(&[]);
        assert_eq!(parsed.invocation, Invocation::Repl);
        assert!(parsed.error.is_none());
    }

    #[test]
    fn a_command_keeps_its_arguments_verbatim() {
        let parsed = opts(&["repeat_enter_jitter", "1000", "250"]);
        assert_eq!(
            parsed.invocation,
            Invocation::Command("repeat_enter_jitter 1000 250".into())
        );

        let parsed = opts(&["browser", "https://example.com/a?b=c"]);
        assert_eq!(
            parsed.invocation,
            Invocation::Command("browser https://example.com/a?b=c".into())
        );
    }

    #[test]
    fn daemon_shortcuts_map_to_commands() {
        assert_eq!(opts(&["daemon-stop"]).invocation, Invocation::Command("daemon_stop".into()));
        assert_eq!(
            opts(&["daemon-status"]).invocation,
            Invocation::Command("daemon_status".into())
        );
        assert_eq!(opts(&["daemon-start"]).invocation, Invocation::DaemonStart);
    }

    #[test]
    fn flags_may_precede_the_command() {
        let parsed = opts(&["--session-dir", "/tmp/s", "status"]);
        assert_eq!(parsed.invocation, Invocation::Command("status".into()));
        assert_eq!(parsed.session_dir.as_deref(), Some(std::path::Path::new("/tmp/s")));
    }

    #[test]
    fn flags_may_follow_the_command() {
        let parsed = opts(&["session", "--session-dir", "/tmp/s", "--verbose"]);
        assert_eq!(parsed.invocation, Invocation::Session);
        assert_eq!(parsed.session_dir.as_deref(), Some(std::path::Path::new("/tmp/s")));
        assert!(parsed.verbose);
        assert!(parsed.error.is_none());

        // A command's own arguments are still forwarded untouched.
        let parsed = opts(&["repeat_enter", "500", "--session-dir", "/tmp/s"]);
        assert_eq!(parsed.invocation, Invocation::Command("repeat_enter 500".into()));
        assert_eq!(parsed.session_dir.as_deref(), Some(std::path::Path::new("/tmp/s")));
    }

    #[test]
    fn stop_double_dash_keeps_flags_as_arguments() {
        let parsed = opts(&["--", "status"]);
        assert_eq!(parsed.invocation, Invocation::Command("status".into()));
        assert!(parsed.error.is_none());
    }

    #[test]
    fn bad_usage_is_reported() {
        assert!(opts(&["--port", "nope"]).error.is_some());
        assert!(opts(&["--session-dir"]).error.is_some());
        assert!(opts(&["--wat"]).error.is_some());
        assert!(opts(&["daemon-restart"]).error.is_some());
        assert_eq!(opts(&["--help"]).error.as_deref(), Some("help requested"));
    }

    #[test]
    fn version_flag_is_recognized() {
        assert_eq!(opts(&["--version"]).invocation, Invocation::Version);
        assert_eq!(opts(&["-V"]).invocation, Invocation::Version);
    }

    #[test]
    fn usage_documents_every_subcommand() {
        let text = usage();
        for needle in ["session", "daemon-start", "daemon-stop", "daemon-status", "browser"] {
            assert!(text.contains(needle), "usage should mention {needle}");
        }
    }
}
