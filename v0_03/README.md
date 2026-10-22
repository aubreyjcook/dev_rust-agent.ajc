# v0_03 — a browser subroutine and a web page

> **Read [v0_01](../v0_01/rust-agent/README.md) and [v0_02](../v0_02/README.md) first.** This version
> builds directly on v0_02's `Routine` trait; if that trait is not yet clear, go
> back, because the rest of this file assumes it.
>
> For the Rust concepts themselves, see
> [Start here: Rust in this project](../README.md#start-here-rust-in-this-project).
> For the original reference-style notes on this version (API table, limits),
> see [the reference appendix](#reference-appendix) at the bottom.

---

## What changed, and why

v0_02 had one way in (the console) and two routines that both pressed keys. v0_03
adds two things that each pull the design in a new direction:

1. **A browser subroutine** — `browser` opens a browser by trying three in order.
   It is the first routine that talks to the *operating system* rather than the
   keyboard, and the first that can **fail in a way worth reporting**.
2. **A web control panel** — a standard-library HTTP server on loopback, with a
   dashboard and a JSON API, so the same commands can be driven from a browser
   tab.

The second one is the interesting change, because it forces a question that had
not come up before: **where does "what a command means" live now that two things
need to know it?**

v0_02's answer would have been "in `main`'s `match`". But the web crate cannot see
inside `main`, and it must not depend on the `agent` crate. The answer v0_03 lands
on is a small, portable type:

```rust
pub type Dispatch = Arc<dyn Fn(&str) -> Reply + Send + Sync>;
```

A boxed closure. The agent hands the web layer *a function that runs commands*,
and the web layer never learns what a command is.

### The diff, module by module

| v0_02 | v0_03 | why |
|-----|-----|-----|
| `routines/src/lib.rs` | `+ routines/src/launcher.rs` | the ladder is ~350 lines; it deserved its own file |
| no browser capability | `OpenBrowser` + `launcher::open` | the new feature |
| `dispatch` inlined in `main`'s loop | `fn dispatch(agent, input) -> Reply` | one decision point for two front ends |
| replies printed directly | replies returned as `Reply { ok, output }` | the caller decides where output goes |
| two packages | three: `agent`, `routines`, `agent_web` | the web layer needs its own crate |
| — | `Dispatch`, `StatusFn`, `Reply`, `AgentStatus` | dependency inversion (below) |
| — | `include_str!("dashboard.html")` | the UI ships inside the binary |
| 0 tests | 19 tests | the seam v0_02 created is finally used |
| `stop` matches the task name | `stop` matches a display label (a bug) | fixed in v0_04, see [the label bug](#change-3-dispatch-got-a-signature-and-tasks-got-labels) |

---

## Change 1: the browser ladder

The requirement was "open a browser, falling back to Chrome then Firefox". The
implementation lives in `routines/src/launcher.rs`, and its shape is the most
reusable idea in the version.

### The three rungs, as data

```rust
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
```

- `Route` is an enum naming the rungs, so the code can talk about *which* browser
  ran without using a string.
- `Candidate` is an enum with two shapes: `DefaultShell` (ask the OS to resolve
  the URL) and `Direct(PathBuf)` (run a known executable).
- The function **launches nothing**. It decides what *would* be tried, which is
  what makes the `browsers` command possible: you can inspect the plan.

Note `if let Some(chrome) = find_chrome()` with no `else`. A missing browser
simply means a shorter ladder — which is why this returns a `Vec` and not a
fixed-size array.

### Walking the ladder

```rust
pub fn open(url: Option<&str>) -> Result<LaunchReport, LaunchError> {
    let url = target_url(url);
    let mut attempts: Vec<Attempt> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    for (route, candidate) in candidates() {
        match &candidate {
            Candidate::DefaultShell => {
                let cmdline = shell_command_line(&url);
                match run_default_shell(&url) {
                    Ok(()) => return Ok(LaunchReport { url, route, cmdline, skipped }),
                    Err(e) => attempts.push(Attempt { route, cmdline, error: e }),
                }
            }
            Candidate::Direct(path) => {
                if !path.is_file() {
                    skipped.push(format!("{}: {} -> not installed", route.label(), path.display()));
                    continue;
                }
                let cmdline = direct_command_line(path, &url);
                match run_direct(path, &url) {
                    Ok(()) => return Ok(LaunchReport { url, route, cmdline, skipped }),
                    Err(e) => attempts.push(Attempt { route, cmdline, error: e }),
                }
            }
        }
    }

    let mut lines: Vec<String> = skipped;
    lines.extend(attempts.iter().map(Attempt::render));
    Err(LaunchError { url, attempts: lines })
}
```

The control flow *is* the design. Reading it slowly:

- **`return Ok(...)` the moment a rung succeeds.** No flags, no `break` — the
  function exits with a report of what worked.
- **Failures are collected, not discarded.** `attempts` accumulates *why* each
  rung failed; `skipped` records rungs never tried because the browser is not
  installed. Both end up in the error if everything fails.
- **Two different kinds of "no".** "Not installed" and "tried and failed" are
  kept apart because they mean different things to whoever reads the message.
- **`match &candidate` borrows.** The `for` loop owns the `Vec`; matching on
  `&candidate` lets `path` be a `&PathBuf` without consuming the item. Change it
  to `match candidate` and the compiler will complain that the value was moved on
  the first arm.

### "Success" needed a definition

A rung only counts if the browser actually starts:

```rust
    // A browser that is really starting stays alive. One that dies instantly
    // (bad install, locked profile, missing libraries) does not.
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(120));
        match child.try_wait() {
            Ok(None) => return Ok(()),            // still running: good sign
            Ok(Some(status)) => { … return Err(…) } // it exited
            Err(e) => return Err(format!("could not poll process: {e}")),
        }
    }
```

`try_wait()` is the idiom to learn here: `Ok(None)` means "still running",
`Ok(Some(status))` means "it exited". So "spawn succeeded" and "the browser
works" are different questions, and this loop answers the second one. That is
what makes the fallback more than decoration.

All three streams are `Stdio::null()`: a browser writes to its own windows, and
its output should not mix into the agent's console.

### Turning loose input into a URL

```rust
pub fn target_url(url: Option<&str>) -> String {
    match url {
        Some(u) if !u.trim().is_empty() => normalize(u.trim()),
        _ => GOOGLE_URL.to_string(),
    }
}
```

`Option<&str>` with a wildcard arm is the pattern for "optional argument with a
default". `Some("")` and `None` behave identically — an empty argument is not an
error, it is "use the default". A guard clause (`Some(u) if …`) is how you add a
condition to a pattern.

`normalize` then chooses between an explicit URL, a bare hostname
(`example.com` → `https://example.com`), and a search (`rust async` →
`google.com/search?q=rust+async`).

### A hand-written error type

```rust
pub struct LaunchError {
    pub url: String,
    pub attempts: Vec<String>,
}

impl fmt::Display for LaunchError { … }
impl Error for LaunchError {}
```

Implementing `Display` is what makes `{e}` work in `println!`. Implementing
`Error` is what lets `?` convert it in a function returning
`Result<_, Box<dyn Error>>`. Both are short impls, and writing them by hand once
teaches you what derives would have hidden.

---

## Change 2: a second front end

This is the architectural change. The new crate, `agent_web`, knows nothing about
routines, browsers, or the registry. It knows how to serve HTTP and how to call a
function.

### The seam: a boxed closure

In `agent/src/main.rs`:

```rust
let dispatch: Dispatch = Arc::new(move |cmd: &str| dispatch(&dispatch_agent, cmd));
let status: StatusFn = Arc::new(move || AgentStatus { … });

match AgentWeb::start(WebConfig { port: opts.port, … }, dispatch, status) {
```

In `agent_web/src/lib.rs`:

```rust
pub type Dispatch = Arc<dyn Fn(&str) -> Reply + Send + Sync>;
pub type StatusFn = Arc<dyn Fn() -> AgentStatus + Send + Sync>;
```

Four things to unpack:

- **`dyn Fn(&str) -> Reply`** is a trait object for a *callable* — the same idea
  as `Box<dyn Routine>`, but for a closure instead of a struct. `Dispatch` is a
  nickname for the whole boxed thing.
- **`move` takes `dispatch_agent` in.** The closure runs on a server thread that
  outlives `run_standalone`, so it must own its data — hence the `Arc` clone on
  the line above.
- **`Send + Sync`** are required because the server hands the closure to many
  connection threads. `Send` is "safe to move to another thread"; `Sync` is
  "safe to share by reference".
- **The dependency is inverted.** `agent_web` declares the *type* of function it
  needs; `agent` supplies an implementation. The library does not reach into the
  binary; the binary plugs itself into the library.

That is why `agent_web` does **not** depend on `agent`:

```
       agent ─────────────▶ agent_web
         │                     │
         └────▶ routines ◀─────┘
```

### Replies became values

v0_02 printed inside its `match`. v0_03 returns:

```rust
pub struct Reply {
    pub ok: bool,
    pub output: String,
}
```

with `Reply::ok(…)` and `Reply::err(…)`. That one change is what lets a single
implementation serve two front ends: the console prints `reply.output`; the web
layer turns it into JSON and picks a status code (200 or 400). The same call
answers both, so a command cannot behave differently depending on where it came
from.

### Loopback only, on purpose

```rust
let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port))?;
```

`Ipv4Addr::LOCALHOST` is `127.0.0.1`: reachable only from this machine. This is
not a detail. The API can start routines and launch browsers, so a routable bind
would let anyone on the network drive the agent. v0_03 has no authentication, which
is acceptable *because of this line*. (v0_04 adds a token as well, because a socket
other local users might reach deserves one.)

### The HTTP server in about 200 lines

`agent_web` speaks just enough HTTP, on `std::net` alone:

- **One thread per connection**: `for stream in self.listener.incoming()` then
  `thread::spawn`. Adequate for a local control panel, and easy to reason about.
- **Reading a request** uses `BufReader::read_line` for the request line, then
  headers until a blank line, then `Content-Length` bytes of body.
- **Writing a response** is assembling bytes: status line, headers, body. Notice
  `Content-Length` must be a *byte* length, which is why `write_response` takes
  `&[u8]` and callers write `.as_bytes()` or `b"…"`.
- **Routing is a `match` on a tuple:**

  ```rust
  match (request.method.as_str(), request.path.as_str()) {
      ("GET", "/") | ("GET", "/index.html") => …dashboard…
      ("GET", "/api/status") | ("GET", "/status") => …status JSON…
      ("GET", "/api/cmd") | ("GET", "/cmd") => …run query param…
      ("POST", "/api/cmd") | ("POST", "/cmd") => …run body param…
      _ => write_response(&mut stream, 404, "text/plain; charset=utf-8", b"not found\n"),
  }
  ```

  Matching `(method, path)` as a tuple makes the arms read like a routing table,
  with exactly one catch-all.

- **Percent-decoding is hand-written.** `%2F` becomes `/`, `+` becomes a space.
  Fiddly enough to get subtly wrong, so it has tests.

### The one place the two front ends differ

```rust
if self.spawning
    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
    .is_err()
{
    return write_response(stream, 429, "application/json", …already in flight…);
}
```

`compare_exchange` is the atomic "set this to `true` **only if** it is currently
`false`, and tell me whether I won". It is the primitive behind most lock-free
guards, and here it stops a double-clicked button opening two browser windows.
The web layer answers HTTP 429; the console path relies on the registry check
instead. One rule, enforced in the place each front end can express it best.

### The dashboard is compiled in

```rust
fn dashboard_html(spawning: bool) -> String {
    let html = include_str!("dashboard.html");
    html.replace("{{SPAWNING}}", if spawning { "true" } else { "false" })
}
```

`include_str!` pastes the file into the binary at compile time: no runtime file
lookup, no path that can be wrong after deployment, one self-contained
executable. The `{{SPAWNING}}` substitution is deliberately primitive — the
server needs exactly one dynamic value at render time — and everything else the
page needs it fetches from `/api/status` with `fetch` + `setInterval`. That keeps
the Rust side honest and the JavaScript side boring.

---

## Change 3: `dispatch` got a signature, and tasks got labels

### One decision point

```rust
/// The single decision point for every command, console or web.
fn dispatch(agent: &Agent, input: &str) -> Reply {
    let mut parts = input.trim().split_whitespace();
    let cmd = parts.next().unwrap_or("");
    let args: Vec<String> = parts.map(str::to_string).collect();

    match cmd {
        "help" | "?" | "/help" => Reply::ok(help_text()),
        "status" => Reply::ok(status_text(agent)),
        "browser" | "browse" | "open_browser" | "open" => {
            open_browser(agent, args.first().map(String::as_str))
        }
        …
        other => Reply::err(format!("unknown command: {other}")),
    }
}
```

This is v0_02's `match`, promoted out of the loop into a named function with a
contract: *text in, `Reply` out, no printing, no I/O*. v0_04 moves this function
into its own crate and calls it from three front ends without changing its shape.

Note the alias spelling: `"browser" | "browse" | "open_browser" | "open"` in one
arm. Pattern alternatives need no extra syntax.

### Task labels, and the bug that ships

v0_03 starts tasks through a helper that builds a *display label*:

```rust
fn start_task(agent: &Agent, label: String, routine: BoxedRoutine) -> Reply {
    let name = routine.name().to_string();
    start_named(agent, name, label, routine)
}
```

and `start_named` then decides on a unique instance name, allowing `name`,
`name#2`, `name#3`, … so you can run two jitter routines with different timings.

This is also where v0_03 **introduces a bug that v0_04 has to fix**. Look at what
ends up as the registry key: in v0_03's `start_named` it is the `label`, not the
routine's `name`. So the task is registered as `repeat_enter 500`, and:

```
▶ started task 'repeat_enter'  (repeat_enter 500)
list
- repeat_enter 500
stop repeat_enter
no such running task: 'repeat_enter'
```

The lesson is about identifiers, and it outlives this project: **use one
identifier for the key and the message, or the two will drift.** v0_04's fix
(register under the routine name, keep the label for display, then allow
unambiguous prefixes) is described in
[v0_04 — the registry learned to forget](../v0_04/README.md#change-3-the-registry-learned-to-forget).

### Lock poisoning stopped being fatal

v0_02 used `registry.lock().unwrap()`. v0_03 switches the hot paths to:

```rust
registry.lock().unwrap_or_else(|e| e.into_inner())
```

A mutex is *poisoned* when a thread panics while holding it, and `.unwrap()`
would then crash the next thread to touch it. `into_inner()` recovers the data
instead. The reasoning: a partially-updated registry is more useful than a dead
agent, and "some other thread panicked" is not the user's problem. v0_04 turns
this into a one-line helper used everywhere.

---

## The workspace grew a member

```
v0_03/
├── Cargo.toml            members = ["agent", "routines", "agent_web"]
├── agent/                binary: wiring, dispatch, registry, CLI
├── agent_web/            library: HTTP server, JSON, dashboard
└── routines/             library: routines + browser ladder
```

`agent_web` depends on `routines` (for `GOOGLE_URL` and the ladder description in
status output) but **not** on `agent`. That absence is what keeps the graph
acyclic and the front ends independent.

```bash
cargo build
cargo run -p agent          # -p now matters: three packages
cargo test                  # 19 tests
```

---

## Why "one dispatch, two doors" matters

It is tempting to read the web panel as a feature. It is more useful to read it
as a **constraint** that improved the code:

- Because the web layer had to run commands, commands had to become a function
  with a data-shaped result (`Reply`) instead of `println!` calls.
- Because replies became data, the console loop got simpler: it prints and
  decides nothing.
- Because two front ends call the same function, behaviour cannot diverge between
  them — and you can verify that by hand: `curl` and the console give the same
  answer for `status`.

v0_04 takes this constraint seriously enough to move `dispatch` into a library and
add a third door. The question to carry forward is: *what is the smallest
interface a front end needs in order to drive this program?* v0_03's answer is one
function and one struct.

---

## Build and run

```bash
cd v0_03
cargo build
cargo run -p agent                        # console + web on http://127.0.0.1:8765/
cargo run -p agent -- --port 9000         # `--` separates cargo's args from the program's
cargo run -p agent -- --no-web            # console only
cargo run -p agent -- --no-open           # refuse to launch browsers
cargo run -p agent -- --open-web          # also open the dashboard
cargo run -p agent -- --browser           # launch a browser at Google, then exit
cargo build --release
cargo test
```

Then open <http://127.0.0.1:8765/>.

```
help | ?                       list commands
status                         uptime, tasks, browser state
list | tasks                   running tasks
browser [url]                  default → chrome → firefox (default page: Google)
open_browser [url]             subroutine form
browsers                       show the ladder without launching anything
repeat_enter [ms]              press Enter on an interval
repeat_enter_jitter [b] [j]    press Enter with jitter
stop <task>                    stop one task
stopall                        stop every task
quit | exit                    exit
```

> ⚠️ `browser` opens a real browser window, and `repeat_enter` presses real keys
> into the focused window.

---

## Try changing it

1. **Add a fourth rung.** Add Chromium to the candidate list (or write
   `find_edge`), then see how little of `open` changes: a `Route` variant, a
   finder, and one `if let` in `candidates()`.

2. **Inspect the plan without acting on it.** Temporarily make `candidates()`
   return only `DefaultShell` and run `browsers`. The ladder is data, so you can
   change the plan without changing the launch code.

3. **Exercise the failure path.** Point a rung at a path that does not exist by
   changing `find_chrome_in` to return `Some(PathBuf::from("C:\\nope.exe"))`, then
   run `browser`. You will see `not installed` in the skip list and the ladder
   move on — `run_direct` is never reached without `path.is_file()`.

4. **Break the reply contract.** Make one `dispatch` arm `println!` its answer and
   *also* return `Reply::ok("")`. Now the console and the web panel disagree.
   That is the concrete cost of printing instead of returning.

5. **Translate an error into a status code.** Run
   `curl -i "http://127.0.0.1:8765/api/cmd?cmd=nope"` and read the status line.
   A `Reply::err` became HTTP 400 in exactly one place. Find it.

6. **Fix the v0_03 bug early.** Change `start_named` to register under `name` while
   keeping `label` for display, then confirm `stop repeat_enter` works. Compare
   your diff with what v0_04 ends up with.

---

## Reference appendix

Kept from the original notes because it is genuinely useful when operating the
program.

**Browser ladder**

| order | rung | how |
|-------|------|-----|
| 1 | system default | `Start-Process <url>` (Windows), `open` (macOS), `xdg-open` (Linux) |
| 2 | Google Chrome | `chrome.exe <url>`, found under `%PROGRAMFILES%`, `%PROGRAMFILES(X86)%`, `%LOCALAPPDATA%`, or on `PATH` |
| 3 | Mozilla Firefox | `firefox.exe <url>`, same search order |

- **Landing page is Google.** No argument means `https://www.google.com`. A bare
  word like `rust async` becomes a Google search; `example.com` gets `https://`.
- **Failures move down the ladder.** The default rung fails if the shell call
  reports an error; direct rungs fail if the executable is missing or the process
  dies within ~1.2 s.
- **Total failure is loud.** One line per attempt, not a silent no-op.
- **One launch at a time.** A second request while one is in flight is refused
  (`429` on the web side).

**Web API**

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/` | dashboard: buttons, live task list, ladder, output pane |
| GET | `/api/status` | JSON: uptime, running tasks, ladder, spawn guard |
| GET | `/api/cmd?cmd=…` | run any command |
| POST | `/api/cmd` | same, with `cmd=…` in the body |
| GET | `/api/browser` | shorthand for `browser` |
| GET | `/api/help` | command list as text |
| OPTIONS | any | CORS preflight |

Replies are JSON: `{"ok":true,"cmd":"browsers","output":"…"}`. A rejected command
answers `400`; an overlapping browser launch answers `429`. The dashboard polls
`/api/status` every 2 s.

```powershell
curl.exe "http://127.0.0.1:8765/api/cmd?cmd=browser"
curl.exe "http://127.0.0.1:8765/api/status"
curl.exe -X POST -d "cmd=repeat_enter 500" http://127.0.0.1:8765/api/cmd
```

**Limits**

- Plaintext HTTP on loopback with no authentication — deliberate for a local
  control panel. Do not bind it to a routable address.
- The dashboard polls rather than using websockets; `GET /ws` answers `501` and
  says so.
- Request bodies are capped at 64 KiB; each connection has a 5 s read/write
  budget; one thread per connection.
- `open_browser` runs once per invocation, so it is tracked and stoppable like any
  other task, but it is not a repeating loop.
- No new dependencies: the HTTP server is `std::net`. In an offline environment,
  build with `cargo build --offline` against a warm cargo cache.

---

## Roadmap for v0_04

This version is finished. Here is what the next one will change.

**The pressure.** Three problems, in increasing order of size:

- **A bug this version introduced.** Tasks are registered under their display
  label (`repeat_enter 500`), not the routine's name, so `stop repeat_enter`
  fails. The registry key and the user-facing name have to be the same thing.
- **Silent stale state.** A task that finishes on its own (as `open_browser`
  does) leaves its entry behind, because cleanup only happens on the path the
  task thread controls.
- **One process.** Everything runs inside one console. If that console closes,
  the agent is gone — even though the work it can do (press keys, open browsers)
  would be useful from any terminal on the machine.

**The plan.**

1. **Fix the name/label split.** Register under the routine's name; keep the
   parameter string in a separate display label. Then allow *unambiguous
   prefixes*, so `stop repeat` works when only one `repeat_*` task is running,
   and is refused with the candidate list when it is not.
2. **Let the registry prune itself.** Ask each thread whether it has finished
   (`JoinHandle::is_finished`) before every read, instead of trusting the task to
   clean up. State that describes a process should be derived from the process.
3. **Extract the runtime into `agent_core`**, so the command semantics are a
   library rather than part of `main`.
4. **Add a third front end**: a loopback TCP socket, plus a session file so a
   separate `agentctl` process can find it, and a token so another local user
   cannot drive it.
5. **Give routines a log sink** instead of letting them print, because in a
   daemon there are many possible readers of a routine's output.
6. **Test the real thing**: integration tests that spawn a daemon and drive it
   with the real client binary.

**What stays the same.** The `Routine` trait's shape, the browser ladder's
order, `sleep_interruptible`, and the narrow `dispatch` contract from v0_03.
v0_04 changes *where* the runtime lives and *how* many front ends feed it, not
what a command means.

**Not planned yet.** Mouse control, persistence across logout, streaming output,
and authentication beyond a shared token — see the roadmap for v0_05 in the
[v0_04 guide](../v0_04/README.md#roadmap-for-v0_05).
