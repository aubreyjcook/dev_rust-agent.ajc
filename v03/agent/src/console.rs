//! Command-line options and startup banner for the v03 agent.

use routines::GOOGLE_URL;

pub struct Options {
    /// Port for the loopback web server; 0 asks the OS to pick a free one.
    pub port: u16,
    /// `--no-web` disables the HTTP server entirely (console-only mode).
    pub no_web: bool,
    /// `--no-open` refuses to launch browsers from this agent.
    pub no_open: bool,
    /// Launch the dashboard in a browser once the server is up.
    pub open_web: bool,
    /// `--open [url]`: run one browser launch and exit instead of serving.
    pub one_shot_browser: Option<String>,
    /// Set when parsing failed; main refuses to start on a bad flag.
    pub error: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            port: 8765,
            no_web: false,
            no_open: false,
            open_web: false,
            one_shot_browser: None,
            error: None,
        }
    }
}

impl Options {
    pub fn parse(args: Vec<String>) -> Self {
        let mut opts = Options::default();
        let mut iter = args.into_iter();

        while let Some(arg) = iter.next() {
            match arg.as_str() {
                "--port" | "-p" => match iter.next().and_then(|v| v.parse::<u16>().ok()) {
                    Some(port) => opts.port = port,
                    None => opts.error = Some("--port needs a number between 0 and 65535".into()),
                },
                "--no-web" => opts.no_web = true,
                "--no-open" => opts.no_open = true,
                "--open-web" => opts.open_web = true,
                "--help" | "-h" => opts.error = Some("help requested".into()),
                "--browser" => {
                    // Optional URL argument; bare `--browser` means Google.
                    let next = iter.next();
                    opts.one_shot_browser = Some(match next {
                        Some(url) if !url.starts_with('-') => url,
                        _ => GOOGLE_URL.to_string(),
                    });
                }
                other if other.starts_with('-') => {
                    opts.error = Some(format!("unknown option: {other}"))
                }
                _ => {}
            }
        }
        opts
    }
}

pub fn banner(opts: &Options) {
    println!("=== Agent v03 (web-enabled) ===");
    println!("  open_browser -> default browser, then Chrome, then Firefox");
    println!("  landing page -> {GOOGLE_URL}");
    if opts.no_web {
        println!("  web control  -> disabled (--no-web)");
    } else {
        println!("  web control  -> http://127.0.0.1:{}/ (loopback only)", opts.port);
    }
    if opts.no_open {
        println!("  browser open -> disabled (--no-open)");
    }
    println!();
    println!("type 'help' for commands, 'quit' to exit");
    println!();
}

pub const USAGE: &str = "\
agent v03 - console agent with loopback web control

usage:
  agent [options]

options:
  -p, --port <n>   port for the web control server (default 8765, 0 = auto)
      --no-web     console only, no HTTP server
      --no-open    refuse to launch browsers
      --open-web   open the dashboard in a browser after startup
      --browser [url]
                   launch a browser once (default page: Google) and exit
  -h, --help       show this message
";

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn defaults_are_sane() {
        let opts = Options::parse(args(&[]));
        assert_eq!(opts.port, 8765);
        assert!(!opts.no_web && !opts.no_open && !opts.open_web);
        assert!(opts.one_shot_browser.is_none());
        assert!(opts.error.is_none());
    }

    #[test]
    fn parses_flags_and_port() {
        let opts = Options::parse(args(&["--port", "9001", "--no-open", "--no-web"]));
        assert_eq!(opts.port, 9001);
        assert!(opts.no_open && opts.no_web);
    }

    #[test]
    fn one_shot_browser_defaults_to_google() {
        let opts = Options::parse(args(&["--browser"]));
        assert_eq!(opts.one_shot_browser.as_deref(), Some(GOOGLE_URL));

        let opts = Options::parse(args(&["--browser", "https://example.com"]));
        assert_eq!(
            opts.one_shot_browser.as_deref(),
            Some("https://example.com")
        );
    }

    #[test]
    fn bad_flags_are_reported() {
        assert!(Options::parse(args(&["--port", "nope"])).error.is_some());
        assert!(Options::parse(args(&["--wat"])).error.is_some());
    }
}
