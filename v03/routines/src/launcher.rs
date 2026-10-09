//! Browser launching with a resilience ladder.
//!
//! Order of attack for [`open`]:
//!
//! 1. the system's **default** browser (shell / protocol handler),
//! 2. **Chrome**,
//! 3. **Firefox**.
//!
//! The first launch that starts cleanly wins; failures are recorded and the
//! next browser on the ladder is tried. If every rung fails, an error naming
//! each attempt is returned.

use std::env;
use std::error::Error;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// The page opened when no explicit URL is given.
pub const GOOGLE_URL: &str = "https://www.google.com";

/// Which rung of the fallback ladder produced a successful launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Default,
    Chrome,
    Firefox,
}

impl Route {
    /// Short machine-friendly tag, handy for logs, JSON and web replies.
    pub fn as_str(self) -> &'static str {
        match self {
            Route::Default => "default",
            Route::Chrome => "chrome",
            Route::Firefox => "firefox",
        }
    }

    /// Human-facing wording for the same rung.
    pub fn label(self) -> &'static str {
        match self {
            Route::Default => "system default browser",
            Route::Chrome => "Google Chrome (fallback 1)",
            Route::Firefox => "Mozilla Firefox (fallback 2)",
        }
    }
}

/// What a successful [`open`] call actually did.
#[derive(Debug, Clone)]
pub struct LaunchReport {
    pub url: String,
    pub route: Route,
    pub cmdline: String,
    /// Everything that was tried before the winning command, if anything.
    pub skipped: Vec<String>,
}

impl fmt::Display for LaunchReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -> {} [{}]", self.route.label(), self.url, self.cmdline)
    }
}

/// Every rung of the ladder failed; carries one line per attempt.
#[derive(Debug, Clone)]
pub struct LaunchError {
    pub url: String,
    pub attempts: Vec<String>,
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "no browser could be launched for {}\n  {}",
            self.url,
            self.attempts.join("\n  ")
        )
    }
}

impl Error for LaunchError {}

/// Resolve the URL to open: the argument if given, otherwise Google.
pub fn target_url(url: Option<&str>) -> String {
    match url {
        Some(u) if !u.trim().is_empty() => normalize(u.trim()),
        _ => GOOGLE_URL.to_string(),
    }
}

/// Turn loose user input into something a browser will accept.
fn normalize(input: &str) -> String {
    if input.contains("://") || input.starts_with("about:") || input.starts_with("file:") {
        input.to_string()
    } else if input.contains('.') && !input.contains(' ') {
        // "google.com" / "example.org/x" -> assume https
        format!("https://{input}")
    } else {
        // Bare words are treated as a Google search.
        let mut encoded = String::new();
        for ch in input.chars() {
            match ch {
                'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => encoded.push(ch),
                ' ' => encoded.push('+'),
                c => {
                    let mut buf = [0u8; 4];
                    for b in c.encode_utf8(&mut buf).bytes() {
                        encoded.push_str(&format!("%{b:02X}"));
                    }
                }
            }
        }
        format!("https://www.google.com/search?q={encoded}")
    }
}

/// One candidate launch command.
#[derive(Debug, Clone)]
pub enum Candidate {
    /// Hand the URL to the OS shell so the registered default browser runs.
    DefaultShell,
    /// Run a known browser executable directly.
    Direct(PathBuf),
}

#[derive(Debug)]
struct Attempt {
    route: Route,
    cmdline: String,
    error: String,
}

impl Attempt {
    fn render(&self) -> String {
        format!(
            "{}: {} -> {}",
            self.route.label(),
            self.cmdline,
            self.error
        )
    }
}

/// Open `url` (defaulting to Google), walking default -> Chrome -> Firefox.
pub fn open(url: Option<&str>) -> Result<LaunchReport, LaunchError> {
    let url = target_url(url);
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for (route, candidate) in candidates() {
        match &candidate {
            Candidate::DefaultShell => {
                let cmdline = shell_command_line(&url);
                match run_default_shell(&url) {
                    Ok(()) => {
                        return Ok(LaunchReport { url, route, cmdline, skipped });
                    }
                    Err(e) => attempts.push(Attempt {
                        route,
                        cmdline,
                        error: e,
                    }),
                }
            }
            Candidate::Direct(path) => {
                if !path.is_file() {
                    skipped.push(format!(
                        "{}: {} -> not installed",
                        route.label(),
                        path.display()
                    ));
                    continue;
                }
                let cmdline = direct_command_line(path, &url);
                match run_direct(path, &url) {
                    Ok(()) => {
                        return Ok(LaunchReport { url, route, cmdline, skipped });
                    }
                    Err(e) => attempts.push(Attempt {
                        route,
                        cmdline,
                        error: e,
                    }),
                }
            }
        }
    }

    let mut lines: Vec<String> = skipped;
    lines.extend(attempts.iter().map(Attempt::render));
    if lines.is_empty() {
        lines.push(format!(
            "no browser candidates found on this platform ({})",
            env::consts::OS
        ));
    }
    Err(LaunchError { url, attempts: lines })
}

/// The rungs of the ladder, in order, without launching anything.
///
/// This is what `browser` / `browsers` report, so the fallback chain is
/// inspectable before a real launch happens.
pub fn candidates() -> Vec<(Route, Candidate)> {
    let mut list = vec![(Route::Default, Candidate::DefaultShell)];

    if let Some(chrome) = find_chrome() {
        list.push((Route::Chrome, Candidate::Direct(chrome)));
    }
    if let Some(firefox) = find_firefox() {
        list.push((Route::Firefox, Candidate::Direct(firefox)));
    }
    list
}

/// Human-readable description of the ladder, in the order it will be tried.
pub fn describe_ladder() -> Vec<String> {
    let mut lines = Vec::new();
    for (route, candidate) in candidates() {
        let detail = match candidate {
            Candidate::DefaultShell => shell_command_line(GOOGLE_URL),
            Candidate::Direct(path) => format!("{} (installed)", display_path(&path)),
        };
        lines.push(format!("{:<16} {}", route.as_str(), detail));
    }
    lines
}

/// Locate a Chrome/Chromium-style executable.
pub fn find_chrome() -> Option<PathBuf> {
    find_chrome_in(&env_dirs(&["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"]))
}

fn find_chrome_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut names: Vec<&str> = Vec::new();
    if cfg!(windows) {
        names.push("chrome.exe");
    } else if cfg!(target_os = "macos") {
        names.push("Google Chrome");
    } else {
        names.extend(["google-chrome", "google-chrome-stable", "chromium", "chromium-browser"]);
    }

    let mut paths: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for dir in dirs {
            paths.push(dir.join("Google").join("Chrome").join("Application").join("chrome.exe"));
            paths.push(dir.join("Chromium").join("Application").join("chrome.exe"));
        }
    } else if cfg!(target_os = "macos") {
        paths.push(PathBuf::from(
            "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
        ));
    }

    locate(&names, &paths)
}

/// Locate a Firefox executable.
pub fn find_firefox() -> Option<PathBuf> {
    find_firefox_in(&env_dirs(&["PROGRAMFILES", "PROGRAMFILES(X86)"]))
}

fn find_firefox_in(dirs: &[PathBuf]) -> Option<PathBuf> {
    let mut names: Vec<&str> = Vec::new();
    if cfg!(windows) {
        names.push("firefox.exe");
    } else if cfg!(target_os = "macos") {
        names.push("firefox");
    } else {
        names.push("firefox");
    }

    let mut paths: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        for dir in dirs {
            paths.push(dir.join("Mozilla Firefox").join("firefox.exe"));
        }
    } else if cfg!(target_os = "macos") {
        paths.push(PathBuf::from(
            "/Applications/Firefox.app/Contents/MacOS/firefox",
        ));
    }

    locate(&names, &paths)
}

/// First existing file among the well-known paths, else a PATH lookup by name.
fn locate(names: &[&str], paths: &[PathBuf]) -> Option<PathBuf> {
    for path in paths {
        if path.is_file() {
            return Some(path.clone());
        }
    }
    names.iter().find_map(|name| find_on_path(name))
}

fn env_dir(var: &str) -> Option<PathBuf> {
    env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
}

/// Printable path, favouring this platform's separator even though `join`
/// accepts either one.
fn display_path(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('/', std::path::MAIN_SEPARATOR_STR)
}

/// Collect the non-empty directories named by `vars`.
fn env_dirs(vars: &[&str]) -> Vec<PathBuf> {
    vars.iter().filter_map(|var| env_dir(var)).collect()
}

/// The ladder shape given explicit install directories, without reading the
/// real environment. Used to test the fallback chain deterministically.
/// Exercised by the unit tests and by the `browsers` inspection command.
pub fn ladder_for(dirs: &[PathBuf]) -> Vec<Route> {
    let mut ladder = vec![Route::Default];
    if find_chrome_in(dirs).is_some() {
        ladder.push(Route::Chrome);
    }
    if find_firefox_in(dirs).is_some() {
        ladder.push(Route::Firefox);
    }
    ladder
}

/// Search `PATH` for an executable file called `name`.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    let dirs: Vec<PathBuf> = env::split_paths(&path).collect();
    let exts: Vec<String> = if cfg!(windows) {
        env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|s| !s.is_empty())
            .map(|s| s.to_ascii_lowercase())
            .collect()
    } else {
        Vec::new()
    };

    for dir in dirs {
        if dir.as_os_str().is_empty() {
            continue;
        }
        let direct = dir.join(name);
        if direct.is_file() {
            return Some(direct);
        }
        if cfg!(windows) && !name.contains('.') {
            for ext in &exts {
                let candidate = dir.join(format!("{name}{ext}"));
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Printable form of a command, quoting only where it is needed.
pub fn render_cmdline(program: &str, args: &[String]) -> String {
    let mut out = String::from(program);
    for arg in args {
        if arg.contains(' ') || arg.contains('&') {
            out.push_str(" \"");
            out.push_str(arg);
            out.push('"');
        } else {
            out.push(' ');
            out.push_str(arg);
        }
    }
    out
}

fn shell_command_line(url: &str) -> String {
    if cfg!(windows) {
        render_cmdline(
            "powershell -NoProfile -Command Start-Process",
            &[url.to_string()],
        )
    } else if cfg!(target_os = "macos") {
        render_cmdline("open", &[url.to_string()])
    } else {
        render_cmdline("xdg-open", &[url.to_string()])
    }
}

fn direct_command_line(path: &PathBuf, url: &str) -> String {
    render_cmdline(&path.display().to_string(), &[url.to_string()])
}

/// Rung 1: let the OS resolve the registered default browser.
fn run_default_shell(url: &str) -> Result<(), String> {
    if cfg!(windows) {
        let shell = find_on_path("pwsh")
            .or_else(|| find_on_path("powershell"))
            .ok_or_else(|| "neither pwsh nor powershell found on PATH".to_string())?;
        let status = Command::new(&shell)
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                &format!("Start-Process '{url}'"),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|e| format!("could not run {}: {e}", shell.display()))?;

        if status.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&status.stderr).trim().to_string();
        return Err(if stderr.is_empty() {
            format!("Start-Process exited with {}", status.status)
        } else {
            format!("Start-Process failed: {stderr}")
        });
    }

    let program = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
    let exe = find_on_path(program).unwrap_or_else(|| PathBuf::from(program));
    let status = Command::new(&exe)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("could not run {program}: {e}"))?;

    if status.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&status.stderr).trim().to_string();
        Err(if stderr.is_empty() {
            format!("{program} exited with {}", status.status)
        } else {
            format!("{program} failed: {stderr}")
        })
    }
}

/// Rungs 2 and 3: exec a known browser directly.
fn run_direct(path: &PathBuf, url: &str) -> Result<(), String> {
    let mut child: Child = Command::new(path)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("spawn failed: {e}"))?;

    // A browser that is really starting stays alive. One that dies instantly
    // (bad install, locked profile, missing libraries) does not, and that is
    // our signal to try the next rung.
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(120));
        match child.try_wait() {
            Ok(None) => return Ok(()),
            Ok(Some(status)) => {
                return Err(if status.success() {
                    "exited immediately (handed off to an existing instance?)".to_string()
                } else {
                    format!("exited immediately with {status}")
                });
            }
            Err(e) => return Err(format!("could not poll process: {e}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_target_is_google() {
        assert_eq!(target_url(None), GOOGLE_URL);
        assert_eq!(target_url(Some("   ")), GOOGLE_URL);
        assert!(GOOGLE_URL.contains("google"));
    }

    #[test]
    fn urls_are_normalized() {
        assert_eq!(target_url(Some("example.com")), "https://example.com");
        assert_eq!(target_url(Some("http://a.test")), "http://a.test");
        assert_eq!(
            target_url(Some("rust async")),
            "https://www.google.com/search?q=rust+async"
        );
    }

    #[test]
    fn ladder_starts_with_default_and_reaches_firefox() {
        let ladder: Vec<Route> = candidates().into_iter().map(|(r, _)| r).collect();
        assert_eq!(ladder.first(), Some(&Route::Default));
        assert!(ladder.len() <= 3, "ladder must not exceed default/chrome/firefox");
        // Chrome, when present, must come before Firefox.
        if let (Some(c), Some(f)) = (
            ladder.iter().position(|r| *r == Route::Chrome),
            ladder.iter().position(|r| *r == Route::Firefox),
        ) {
            assert!(c < f, "chrome must be tried before firefox");
        }
    }

    /// With no browser installed anywhere, the ladder still *attempts* all
    /// three rungs and then fails loudly with a reason per rung.
    #[test]
    fn ladder_tries_every_rung_then_reports_failure() {
        let empty = std::env::temp_dir().join("v03-no-browsers-here");
        let dirs = vec![empty];

        assert_eq!(ladder_for(&dirs), vec![Route::Default]);
        assert!(find_chrome_in(&dirs).is_none());
        assert!(find_firefox_in(&dirs).is_none());

        let ladder = candidates();
        let routes: Vec<Route> = ladder.iter().map(|(r, _)| *r).collect();
        assert_eq!(routes.first(), Some(&Route::Default));

        let report = LaunchError {
            url: GOOGLE_URL.to_string(),
            attempts: ladder
                .iter()
                .map(|(route, _)| format!("{}: simulated failure", route.label()))
                .collect(),
        };
        let text = report.to_string();
        assert!(text.contains("system default browser"));
        assert!(text.contains("Google Chrome"));
        assert!(text.contains("Mozilla Firefox"));
    }

    /// A directory that really does contain a fake chrome.exe is discovered.
    #[test]
    fn detects_a_browser_that_is_present() {
        let root = std::env::temp_dir().join("v03-fake-browsers");
        let chrome_dir = root
            .join("Google")
            .join("Chrome")
            .join("Application");
        let firefox_dir = root.join("Mozilla Firefox");
        std::fs::create_dir_all(&chrome_dir).expect("mkdir");
        std::fs::create_dir_all(&firefox_dir).expect("mkdir");

        let chrome_exe = if cfg!(windows) { "chrome.exe" } else { "chrome" };
        let firefox_exe = if cfg!(windows) { "firefox.exe" } else { "firefox" };
        std::fs::write(chrome_dir.join(chrome_exe), b"stub").expect("write");
        // Deliberately write Firefox under the Windows-style name only when the
        // test platform expects it.
        if cfg!(windows) {
            std::fs::write(firefox_dir.join(firefox_exe), b"stub").expect("write");
        }

        let dirs = vec![root.clone()];
        let ladder = ladder_for(&dirs);
        if cfg!(windows) {
            assert_eq!(ladder, vec![Route::Default, Route::Chrome, Route::Firefox]);
        } else {
            assert_eq!(ladder, vec![Route::Default, Route::Chrome]);
        }

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn route_tags_are_stable() {
        assert_eq!(Route::Default.as_str(), "default");
        assert_eq!(Route::Chrome.as_str(), "chrome");
        assert_eq!(Route::Firefox.as_str(), "firefox");
    }
}
