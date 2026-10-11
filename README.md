# dev_rust-agent.ajc

A small Rust "agent": a program that presses keys, opens browsers, and gets
driven from a console, a browser tab, or another terminal on the machine.

It is written **four times** on purpose. `v01` through `v04` are four stages of
the same program growing up, and each stage is a self-contained, runnable
project. If you are learning Rust, read them in order; every version works, so
you can run any of them at any point.

| version | the program became… | the Rust you learn |
|---------|--------------------|--------------------|
| [v01](#v01--one-file-that-types-for-you) | one file that presses Enter on a timer | structs, closures, `Arc`/`Mutex`, threads |
| [v02](#v02--splitting-the-program-into-two-crates) | two crates, with a `Routine` trait | traits, trait objects, workspaces, `.workspace = true` |
| [v03](#v03--a-browser-subroutine-and-a-web-page) | a browser launcher plus a web control panel | `Option`/`Result` chains, `include_str!`, parsing bytes |
| [v04](#v04--one-process-or-a-daemon) | a background daemon any terminal can command | binary targets, IPC, session files, real integration tests |

**Where to start:** if you have never written Rust, read
[Start here: Rust in this project](#start-here-rust-in-this-project) first — it
explains, in the order you will meet them, every Rust idea these versions use.
Then read v01, run it, and move up. Each version section ends with the exact
commands to run it.

---

## Table of contents

- [The project in one page](#the-project-in-one-page)
- [Repository layout](#repository-layout)
- [Start here: Rust in this project](#start-here-rust-in-this-project)
- [The four versions](#the-four-versions)
  - [v01 — one file that types for you](#v01--one-file-that-types-for-you)
  - [v02 — splitting the program into two crates](#v02--splitting-the-program-into-two-crates)
  - [v03 — a browser subroutine and a web page](#v03--a-browser-subroutine-and-a-web-page)
  - [v04 — one process, or a daemon](#v04--one-process-or-a-daemon)
- [Reading the diff: what changed at each step](#reading-the-diff-what-changed-at-each-step)
- [Operating the program](#operating-the-program)
- [Everything that went wrong](#everything-that-went-wrong)
- [Repository and development](#repository-and-development)
- [Glossary](#glossary)

---

## The project in one page

Picture a program with two halves.

The **front end** takes what you type and decides what it means. It is
deliberately boring: split the line on spaces, look at the first word, match it.

The **back end** does the actual work: hold down a key repeatedly, or launch a
browser. The back end runs on its own thread, so a routine that presses Enter
forever does not stop you from typing `list`.

Everything between those two halves is where the versions differ:

- **v01** puts both halves in one `main.rs`, using a closure for the back end.
- **v02** moves the back end into a second crate and gives it a trait, so a
  routine is a *thing you can pass around*.
- **v03** adds a second front end — a tiny web server — and a browser routine
  that tries three browsers in order.
- **v04** adds a third front end, a socket, so the back end can live in a
  background process and other terminals can command it.

The same four commands survive all four versions, which makes the comparison
easy to follow: `repeat_enter`, `repeat_enter_jitter`, `stop`, `list`.

## Repository layout

```
dev_rust-agent.ajc/
├── README.md              ← you are here
├── .gitignore
├── v01/rust-agent/        four independent Cargo projects;
├── v02/                     each has its own target/ directory,
├── v03/                     so they do not interfere with each other
└── v04/
```

Each version directory is its own [Cargo workspace](#what-is-a-workspace). You
`cd` into a version to build it. Nothing is shared between versions on purpose —
v02 does not import from v01 — so you can read one without the others, and so a
change to v04 can never break v03.

## Start here: Rust in this project

This section is a guided tour of the Rust features the four versions use. It is
ordered roughly by when you will meet each one. If you already know Rust, skip
to [The four versions](#the-four-versions).

### A crate, a package, and a workspace

A **crate** is one compilation unit: one library or one binary. A **package** is
the thing described by a `Cargo.toml` file, and it produces one or more crates.
When people say "crate" casually they usually mean a package, and so does this
README.

A **workspace** is a `Cargo.toml` listing several packages that share one
`target/` directory and one `Cargo.lock`. v02, v03 and v04 are workspaces; v01
is a single package.

```toml
# v02/Cargo.toml
[workspace]
resolver = "2"
members = ["agent", "routines"]       # the folders that are workspace members

[workspace.package]                    # settings members can inherit
edition = "2021"
version = "0.1.0"

[workspace.dependencies]               # versions members can borrow
enigo = "0.2"
rand = "0.8"
```

A member then opts in with `.workspace = true`, which is why you see this in
`v02/routines/Cargo.toml` and nowhere else:

```toml
[dependencies]
enigo.workspace = true   # "whatever the workspace said"
rand.workspace = true
```

The point: four packages pin `enigo` in *one* place, so they cannot drift apart.

> **Editions.** A Cargo.toml says `edition = "2021"` or `"2024"`. That is not the
> Rust version you have installed; it selects which edition of the language
> rules your code is compiled under (editions change in small, mostly
> backward-incompatible ways every three years). v01 uses the 2024 edition; v02
> onwards uses 2021. Both are fine — the later versions simply do not need
> anything newer. If your toolchain is older than 1.85, v01 will refuse to build;
> use a recent `rustup` and all four work.

### Values, mutation, and `let`

```rust
let line = String::from("repeat_enter 500");   // owned, growable string
let ms: u64 = 1000;                            // plain 64-bit unsigned integer
let mut count = 0;                             // `mut` = I will change this
count += 1;
```

Rust variables are immutable unless you write `mut`. That is the compiler
helping you: if you did not say a value changes, it cannot accidentally change.
You will see `let mut parts = ...` a lot in this project when a value is built
up line by line.

### Ownership, borrowing, and why `&` is everywhere

Every value has exactly one owner. When the owner goes away, the value is freed.
To use a value without taking ownership, you **borrow** it with `&` (read-only)
or `&mut` (writable).

```rust
fn describe(cmd: &str) -> String {        // borrows the text, does not own it
    format!("you typed: {cmd}")           // builds and returns a new String
}

let owned = String::from("status");
let text = describe(&owned);              // lend it out
println!("{text} {owned}");               // still usable: we only borrowed
```

Most function arguments in this project are borrows (`&str`, `&[PathBuf]`,
`&AtomicBool`) so that the caller keeps its data. When a function *must* keep a
value, it takes it by value and the caller gives ownership up — that is why you
see code like `Box::new(RepeatEnter { interval_ms: ms })` being handed to a
thread and never mentioned again.

### Functions and closures

A **closure** is a function you write inline, capturing nearby variables:

```rust
let ms = 500;
let body = move |stop| repeat_enter_loop(ms, stop);   // `move` = take `ms` in
```

`move` is essential before handing a closure to a thread: the thread may outlive
the current function, so it cannot borrow locals — it has to own them.

A closure that is *accepted as a parameter* is written as a generic type with
bounds. This is v01's `start_task`, which takes the work as a closure:

```rust
fn start_task<F>(registry: &Registry, name: &str, body: F)
where
    F: FnOnce(Arc<AtomicBool>) -> () + Send + 'static,   // see below
{
    // … thread::spawn(move || body(stop));
}
```

The three bounds are the price of `thread::spawn`:

- `FnOnce(Arc<AtomicBool>) -> ()` — "callable once, takes this, returns nothing".
- `Send` — "safe to move to another thread".
- `'static` — "borrows nothing that could die before the thread does".

From v02 onwards this same slot is filled by a `Box<dyn Routine>` instead, which
is the subject of the next section. *(Note that `-> ()` is the explicit way to
write "returns nothing". A generic bound like this works in argument position;
you cannot write `impl FnOnce` on a struct field — if you needed to store a
closure you would box it. This project never needs to, because the `Routine`
trait does that job.)*

### `match`, `if let`, and the humble `Option`

`Option<T>` is Rust's replacement for null: it is either `Some(value)` or
`None`, and the compiler makes you handle both. `Result<T, E>` is the same idea
for fallible work: `Ok(value)` or `Err(problem)`.

```rust
match args.get(0).and_then(|s| s.parse::<u64>().ok()) {
    Some(ms) => println!("interval {ms}"),
    None => println!("using the default"),
}
```

`and_then` and `ok` are **combinators** — they let you chain "and then, if it
worked, and then…" without a pyramid of `if`s. `.unwrap_or(1000)` at the end
supplies a fallback. You will see this exact chain in every version:

```rust
let ms = args.get(0).and_then(|s| s.parse().ok()).unwrap_or(1000);
```

When you only care about the success case, `if let` is the short form:

```rust
if let Some(h) = reg.get(name) {
    h.stop.store(true, Ordering::Relaxed);
}
```

**The three unwraps** — learn to tell these apart, because their tone differs:

| form | meaning | used here when |
|------|---------|----------------|
| `.unwrap()` | "if this fails, panic; I am certain it cannot" | a poisoned lock, which we then chose to tolerate instead (see below) |
| `.unwrap_or(x)` / `.unwrap_or_default()` | "if this fails, use this instead" | a missing CLI argument |
| `?` | "if this fails, return the failure to my caller" | parsing a line, reading a file |

### `Box<dyn Trait>` — one variable, many behaviours

This is the heart of v02 onwards. Suppose you want to store "a unit of work"
without listing every kind of work:

```rust
pub trait Routine: Send + 'static {
    fn name(&self) -> &'static str;
    fn run(self: Box<Self>, stop: Arc<AtomicBool>, log: LogHandle);
}

pub type BoxedRoutine = Box<dyn Routine>;
```

`trait` declares a shared interface; each `impl Routine for Something` provides
it. `dyn Routine` means "some type implementing Routine, decided at runtime",
and `Box` puts it on the heap because the compiler does not know how big the
concrete type is. The result is a single variable — `BoxedRoutine` — that can
hold a `RepeatEnter`, an `RepeatEnterJitter`, or an `OpenBrowser`. The registry
does not need to know which.

Two details that trip people up:

- `fn run(self: Box<Self>, ...)` is unusual-looking but necessary. A trait
  method that consumes the value would normally be `fn run(self)`, but through a
  `Box<dyn Trait>` there is no sized `self` to move, so you say `Box<Self>`.
- `&'static str` for `name()` means "a string literal, living for the whole
  program". It is why routines can return `"repeat_enter"` directly.

### Shared state: `Arc`, `Mutex`, and why the two always travel together

The agent has state that several threads touch: which tasks are running. Rust
refuses to let two threads share plain data. The fix is a pair:

- `Arc<T>` = **atomically reference counted** pointer. Cloning it gives another
  handle to the same value; the value is freed when the last handle is dropped.
- `Mutex<T>` = a lock. Only one thread at a time may touch the `T` inside.

Together, `Arc<Mutex<HashMap<...>>>` means "one shared map that several threads
may take turns using". `Arc` answers *who owns it*; `Mutex` answers *who may
touch it right now*. You need both, and you will see the pair in all four
versions.

```rust
type Registry = Arc<Mutex<HashMap<&'static str, TaskHandle>>>;

let registry: Registry = Arc::new(Mutex::new(HashMap::new()));

let mut reg = registry.lock().unwrap();   // take the lock …
reg.insert(name, TaskHandle { stop });    // … change the data …
drop(reg);                               // … release it (or let it fall out of scope)
```

**Lock poisoning.** If a thread panics while holding a lock, Rust marks the
mutex "poisoned" and every later `.lock()` returns an `Err`. `.unwrap()` would
then crash a second thread because a *different* thread failed earlier. v03 and
v04 refuse that trade and recover the data instead:

```rust
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())   // keep going with the data
}
```

That one-line helper is a nice example of Rust making a design decision visible.

### Interruptible sleeping

Every long-running routine must stop quickly when asked. A single
`thread::sleep(60_000)` cannot be interrupted, so the project sleeps in 20 ms
slices and re-checks the stop flag between them:

```rust
pub fn sleep_interruptible(total_ms: u64, stop: &AtomicBool) {
    let mut slept = 0;
    while slept < total_ms && !stop.load(Ordering::Relaxed) {
        let chunk = 20.min(total_ms - slept);
        thread::sleep(Duration::from_millis(chunk));
        slept += chunk;
    }
}
```

`AtomicBool` is a boolean that threads may read and write without a lock.
`Ordering::Relaxed` is enough because we only need "did someone set this flag",
not a strict ordering with other memory.

### `#[derive(…)]`, `impl Display`, and `impl Error`

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInfo { pub name: String, /* … */ }
```

`derive` asks the compiler to write boring code for you: `Debug` enables
`{:?}` printing, `Clone` gives `.clone()`, `PartialEq`/`Eq` enable `==`.
v03 and v04 also hand-write `impl fmt::Display` and `impl Error` for their
error types, which is what makes an error printable with `{e}` *and* usable
with `?`.

### Errors as values: `?` and `.map_err`

```rust
self.connect().map_err(|e| format!("cannot reach the daemon at {}: {e}", endpoint))?
```

`map_err` converts one error type into another — here an `io::Error` into a
readable `String`. The `?` then returns early if it was an error. Rust has no
exceptions; errors are ordinary values that travel up through return types, and
`?` is the shorthand that keeps the happy path readable.

You meet this in v02 as well, where `stop_task` turns a missing name into a
message the user can act on:

```rust
match reg.get(name) {
    Some(handle) => { /* … */ }
    None => println!("no such running task: '{name}'"),
}
```

### Reading bytes without a parser library

v03 and v04 parse HTTP and JSON by hand. That involves:

- `line.split_whitespace()`, `split_once('?')` — cheap text chopping.
- `u8::from_str_radix(hex, 16)` — decoding `%2F` escapes.
- `String::from_utf8_lossy(&bytes)` — "turn bytes into text, replacing anything
  invalid" rather than failing.
- Walk-the-string state machines that track `in_string` and `escaped` so that a
  quote inside a value does not end the value. You will see this in
  `v04/agent_core/src/protocol.rs`; it is the fiddliest code in the repository
  and the best small exercise in careful indexing.

### `cfg!`, `#[cfg(...)]`, and cross-platform code

Rust can compile a different branch per platform, and it *checks* the branch it
is not compiling only when `cfg` is an attribute:

```rust
if cfg!(windows) {                    // a runtime bool; both sides are compiled
    names.push("chrome.exe");
}

#[cfg(windows)]                       // the other side is not compiled at all
fn ctrlc_flag(flag: Arc<AtomicBool>) -> Result<(), String> { /* Win32 call */ }

#[cfg(unix)]
fn ctrlc_flag(flag: Arc<AtomicBool>) -> Result<(), String> { /* signal() call */ }
```

The distinction matters: `cfg!` is for "same code, different values", while
`#[cfg]` is for "this code only exists on this platform", which is required when
the other platform's API does not exist to compile.

### Modules, files, and `mod`

A file becomes a module when you declare it:

```rust
mod console;                    // pulls in src/console.rs
use std::collections::HashMap;  // brings a name into scope
use agent_core::{Runtime, SessionDir};
```

Inside a module you rename things with `as`:

```rust
use json::{string as json_string};   // in v04's protocol.rs
```

That rename exists because `json_string` was ambiguous with a *reader* of the
same name — a real problem the compiler caught, described in
[Everything that went wrong](#everything-that-went-wrong).

### `include_str!` — embedding a file in the binary

v03's dashboard is a real HTML file that is compiled *into* the executable:

```rust
fn dashboard_html(spawning: bool) -> String {
    let html = include_str!("dashboard.html");     // read at compile time
    html.replace("{{SPAWNING}}", if spawning { "true" } else { "false" })
}
```

`include_str!` is a macro that pastes the file's contents into the binary as a
`&'static str`. No runtime file loading, no path that can break, and a single
self-contained executable.

### Unit tests next to the code

Every version has tests in a `#[cfg(test)] mod tests` at the bottom of a file:

```rust
#[cfg(test)]
mod tests {
    use super::*;                  // the test module can see the parent's items

    #[test]
    fn default_target_is_google() {
        assert_eq!(target_url(None), GOOGLE_URL);
    }
}
```

`cargo test` compiles a second binary containing only the tests, so test code
costs nothing in the shipped program. `assert_eq!` compares and prints both
sides when it fails.

### Integration tests: running the real binaries

Unit tests can only see inside the crate. v04 needs more: it needs to start a
real daemon process and talk to it. That is an **integration test** — a file in
`tests/` compiled as its own crate, which can only use the public API:

```rust
// v04/agent/tests/daemon.rs
fn agent_bin() -> &'static str { env!("CARGO_BIN_EXE_agent") }

let child = Command::new(agent_bin())
    .arg("daemon")
    .arg("--session-dir").arg(&self.session_dir)
    .spawn()
    .expect("spawn daemon");
```

`env!("CARGO_BIN_EXE_agent")` is Cargo handing the test the exact path of the
binary it just built. That is what makes "test the thing you ship" possible
without any test framework beyond `cargo test`.

---

## The four versions

### v01 — one file that types for you

```
v01/rust-agent/
├── Cargo.toml
└── src/main.rs        202 lines, everything in one place
```

**What it does.** `v01` is a friendly little program that presses the Enter key
for you, on a timer, optionally with random variation so the timing does not look
mechanical. It reads commands from the console:

```
repeat_enter [ms]            press Enter every ms (default 1000)
repeat_enter_jitter [b] [j]  press Enter every b ms, ± j ms (default 1000/100)
stop <name>                  stop a running task
list                         show what is running
quit | exit                  leave
```

**What is new.** Everything. This version establishes the shape that survives:

- a `Registry` — a shared map from task name to a stop flag,
- `start_task`, which registers a task and runs it on a new thread,
- the small interruptible-sleep helper,
- the command loop: read a line, split it, `match` on the first word.

Four commands: `repeat_enter`, `repeat_enter_jitter`, `stop`, `list`, plus
`quit`/`exit` to leave. It also uses `enigo`, the crate that actually synthesises
keystrokes, and `rand` for the jitter.

**The interesting part.** `start_task` accepts a closure rather than a type:

```rust
fn start_task<F>(registry: &Registry, name: &str, body: F)
where
    F: FnOnce(Arc<AtomicBool>) -> () + Send + 'static,
{
    // … refuse duplicates, insert the flag, then:
    thread::spawn(move || {
        body(stop);
        registry_clone.lock().unwrap().remove(&name_owned);
        println!("[task '{name_owned}' finished]");
    });
}
```

That generic `F` is the seed of v02's trait. Both express "some unit of work";
the closure version is simpler, and the trait version is more powerful. Seeing
why someone would switch from one to the other is the whole lesson of v02.

**Limitations you can feel.** The behaviour of a task lives inside a closure, so
it cannot be named, listed, or stored. Cleaning up after a task means repeating
the removal line in the spawn block. And because edition 2024 requires a recent
toolchain, this is also the least portable version to build.

**Run it**

```bash
cd v01/rust-agent
cargo build
cargo run
# then type:  repeat_enter 500
#             list
#             stop repeat_enter
#             quit
```

> ⚠️ **This presses real keys on your machine**, into whatever window has focus.
> Start it with a long interval, keep a terminal focused, and `stop` it before
> switching windows.

### v02 — splitting the program into two crates

```
v02/
├── Cargo.toml          workspace: members = ["agent", "routines"]
├── agent/src/main.rs   114 lines — decide and track
└── routines/src/lib.rs  79 lines — do the work
```

**What it does.** The same four commands, but the work now lives in a **library**
crate that the **binary** crate depends on.

**What is new.**

1. **A workspace.** Two packages, one `target/`, shared dependency versions.
2. **A trait, and trait objects.** `Routine` describes a unit of work, and
   `BoxedRoutine = Box<dyn Routine>` lets the agent store any of them:

   ```rust
   pub trait Routine: Send + 'static {
       fn name(&self) -> &'static str;
       fn run(self: Box<Self>, stop: Arc<AtomicBool>);
   }
   ```

3. **The agent stops knowing what a routine is.** Compare the two dispatchers:

   ```rust
   // v01: behaviour is a closure written at the call site
   start_task(&registry, "repeat_enter", move |stop| repeat_enter_loop(ms, stop));

   // v02: behaviour is a value with a name
   start_task(&registry, Box::new(RepeatEnter { interval_ms: ms }));
   ```

   Note the second version does not even pass a name — it asks the routine:
   `let name = routine.name();`.

4. **`enigo` and `rand` are pinned once** in `[workspace.dependencies]` and
   borrowed with `.workspace = true`.

**Why this is the right refactor.** Three things fall out of it for free:

- Adding a routine means writing a struct and an `impl` — no change to the agent.
- `agent` can print a task's name without knowing its type.
- The library can be tested *without starting the program*. v02 does not take
  advantage of that (it has no tests at all — see
  [Repository and development](#repository-and-development)), but the seam is
  exactly what makes v04's library testable.

**Rust worth pausing on.** `impl Routine for RepeatEnter` has to satisfy the
trait's bounds, which is why you see `pub struct RepeatEnter { pub interval_ms: u64 }`
with public fields: constructing it from the `agent` crate requires visibility.

**Run it**

```bash
cd v02
cargo build
cargo run -p agent         # -p selects the package in a workspace
# repeat_enter 500 / list / stop repeat_enter / quit
```

### v03 — a browser subroutine and a web page

```
v03/
├── Cargo.toml
├── agent/
│   ├── src/main.rs         command dispatch, task registry, wiring
│   └── src/console.rs      CLI options and banner
├── agent_web/
│   ├── src/lib.rs          loopback HTTP server + JSON API
│   └── src/dashboard.html  the control panel (embedded in the binary)
└── routines/
    ├── src/lib.rs          routines, including OpenBrowser
    └── src/launcher.rs     the browser fallback ladder
```

**What it does.** Two additions:

1. **A browser subroutine.** `browser [url]` opens a browser by trying three in
   order and stopping at the first that works:

   | order | rung | how |
   |-------|------|-----|
   | 1 | system default | `Start-Process <url>` (Windows), `open` (macOS), `xdg-open` (Linux) |
   | 2 | Google Chrome | `chrome.exe <url>`, found under `%PROGRAMFILES%`, `%PROGRAMFILES(X86)%`, `%LOCALAPPDATA%`, or on `PATH` |
   | 3 | Mozilla Firefox | `firefox.exe <url>`, same search order |

   With no argument it opens `https://www.google.com`. `google.com` becomes
   `https://google.com`; `rust async` becomes a Google search for it.

2. **Web interactivity.** A standard-library HTTP server on `127.0.0.1` serves a
   dashboard (`/`) with buttons and a live task list, plus a JSON API:

   ```
   GET  /                    dashboard
   GET  /api/status          JSON status
   GET  /api/cmd?cmd=…       run any command
   POST /api/cmd             cmd=… in the body
   GET  /api/browser         shorthand for `browser`
   ```

**What is new in Rust terms.**

- **`Result` becomes the main character.** The launcher returns
  `Result<LaunchReport, LaunchError>`, and the ladder is a loop that collects
  failures and returns them only if every rung failed.
- **`Option` chaining for arguments.** `target_url(url: Option<&str>)` treats
  `None` and `""` the same way — "open the default page".
- **A real error type.** `LaunchError` implements `Display` and `Error`, so it
  can be printed with `{e}` and returned with `?`.
- **Embedding an asset** with `include_str!`.
- **Parsing bytes.** Percent-decoding, query strings, and headers, by hand.
- **A second front end.** `main.rs` wires stdin and HTTP into one `dispatch`
  function, which is the first appearance of an idea v04 leans on hard: *one
  place decides what a command means, regardless of where it came from*.

**Design details worth copying.**

- **A launch only counts if the process stays alive.** Direct browser launches
  are polled for ~1.2 s; a browser that dies instantly (locked profile, broken
  install) counts as a failure and the ladder moves on.
- **One browser at a time.** A second request while a launch is in flight is
  refused, `429` on the web side. Without this, a double-click opens a dozen
  windows.

**Run it**

```bash
cd v03
cargo build
cargo run -p agent                       # console + web on http://127.0.0.1:8765/
cargo run -p agent -- --port 9000        # different port
cargo run -p agent -- --no-web           # console only
cargo run -p agent -- --no-open          # refuse to launch browsers
cargo run -p agent -- --open-web         # also open the dashboard in a browser
cargo run -p agent -- --browser          # launch a browser at Google, then exit
cargo run -p agent -- --browser https://example.com
```

Then open <http://127.0.0.1:8765/> for the dashboard.

### v04 — one process, or a daemon

```
v04/
├── Cargo.toml
├── agent/
│   ├── src/main.rs          mode selection + both front ends
│   ├── src/console.rs       `agent` CLI parsing
│   ├── src/bin/agentctl.rs  the remote-control client (a second binary)
│   ├── src/agentctl_cli.rs  `agentctl` CLI parsing
│   └── tests/daemon.rs      integration tests that run both real binaries
├── agent_core/
│   ├── src/lib.rs           module list and public re-exports
│   ├── src/runtime.rs       command semantics — no I/O front end at all
│   ├── src/state.rs         the task registry
│   ├── src/server.rs        the daemon's control socket
│   ├── src/client.rs        the client end of that socket
│   ├── src/session.rs       how a client finds a daemon
│   └── src/protocol.rs      newline-delimited JSON over loopback TCP
└── routines/
    ├── src/lib.rs           routines now report through a Log sink
    └── src/launcher.rs      the same fallback ladder as v03
```

**What it does.** The agent can now run two ways:

| mode | started with | who can drive it |
|------|--------------|------------------|
| **standalone** | `agent` or `agent run` | only its own console — exactly v03's behaviour |
| **daemon** | `agent daemon` | **any terminal on the machine**, via `agentctl` |

**Why this is the biggest version.** v03 already had the key idea — the runtime
does not care where a command came from. v04 turns that into an architecture:

```
   console ─┐
            ├─→  Runtime (one implementation of every command)  ─→  routines
   socket  ─┘
```

`agent_core::Runtime` has no `println!` of its own: every command *returns* a
string, and the front end decides where it goes. The console prints it; the
server sends it back down the socket. The console front end and the socket front
end both call `runtime.handle(...)` under the same lock. That is why the two modes
cannot drift apart: there is only one implementation.

*(One deliberate exception: `server.rs` uses `eprintln!` for connection errors
that happen before a reply can be written — you cannot report a broken socket
over the socket.)*

**What is new in Rust terms.**

- **A second binary in one package.** `agent/Cargo.toml` declares
  `[[bin]] name = "agent"` and `[[bin]] name = "agentctl"`, so one package ships
  two executables. `cargo build` builds both.
- **`env!("CARGO_BIN_EXE_agentctl")`** in the integration test — Cargo passes
  the built binary's path to the test.
- **Trait evolution.** `Routine::run` grew a parameter:

  ```rust
  // v03
  fn run(self: Box<Self>, stop: Arc<AtomicBool>);
  // v04
  fn run(self: Box<Self>, stop: Arc<AtomicBool>, log: LogHandle);
  ```

  The new `Log` trait is where a routine's output goes. In standalone mode it is
  the console; in daemon mode it is the task's own buffer, so any client can read
  back what a routine said. This is why `Log` is a trait and not a `println!`.
- **`VecDeque`** for two bounded queues: the event backlog (last 500) and each
  task's output (last 100 lines). `push_back` + `pop_front` is the classic
  ring-buffer pair.
- **Hand-rolled signal handling**, because a background daemon must stop on
  Ctrl-C without pulling in a crate:

  ```rust
  #[cfg(windows)]  extern "system" fn handler(_ctrl_type: u32) -> i32 { … }
  #[cfg(unix)]     extern "C"      fn handler(_signum: i32)         { … }
  ```

  Both flip an `AtomicBool` that the accept loop polls.

**How the two processes find each other.**

1. The daemon binds a **loopback-only TCP socket** (port 45917, walking upwards
   if taken, ephemeral as a last resort).
2. It writes `session.json` — port, pid, version, and a random 32-hex **token** —
   into its session directory.
3. `agentctl` reads that file, connects, and sends the token with every request.
4. On shutdown the daemon deletes the file, so a later `agentctl` says "no
   daemon" instead of chasing a dead port.

The session directory is **discovered, never assumed**: `AGENT_SESSION_DIR`,
then the per-user state directory (`%LOCALAPPDATA%\rust-agent` on Windows,
`~/.local/state/rust-agent` on Linux), then the temp directory, then
`.rust-agent` in the working directory. The first candidate that *accepts a real
write* wins — a directory that exists but is not writable is worse than none,
which is exactly what happened in the sandbox this was built in (see
[Everything that went wrong](#everything-that-went-wrong)).

The protocol is one JSON object per line, in both directions:

```json
→ {"token":"<hex>","cmd":"repeat_enter 500","since":41}
← {"ok":true,"output":"started task 'repeat_enter'","shutdown":false,"version":"0.4.0","events":[…]}
```

`since` is a cursor into the event log, so a client that was away catches up on
what it missed without replaying everything.

**Security, stated plainly.** The socket is loopback-only and every request
carries the token from the session file, so another user on the same machine
cannot drive your agent. A wrong token is refused and logged. This is *not* a
system service: it runs with your privileges, in your session, with your `PATH`
and browser profile — which is the point, because `agentctl browser` should open
a browser as you. There is no elevation, no service registration, and no TLS.

**Run it**

```bash
cd v04
cargo build
cargo test                                   # 53 tests, including real processes

# standalone (v03 behaviour)
./target/debug/agent
./target/debug/agent run --no-open

# daemon, in its own terminal
./target/debug/agent daemon
./target/debug/agent daemon --port 46000 --session-dir /tmp/agent

# from any other terminal
./target/debug/agentctl status
./target/debug/agentctl browser
./target/debug/agentctl repeat_enter 500
./target/debug/agentctl list
./target/debug/agentctl stop repeat_enter
./target/debug/agentctl daemon-stop
```

## Reading the diff: what changed at each step

| | v01 | v02 | v03 | v04 |
|---|---|---|---|---|
| **packages** | 1 | 2 | 3 | 4 |
| **files** | 1 | 2 | 5 | 14 |
| **lines of Rust** | 202 | 193 | 1 864 | 4 667 |
| **commands** | 4 | 4 | 8 | 13 |
| **front ends** | console | console | console + HTTP | console + HTTP + socket |
| **deps** | enigo 0.6, rand 0.8 | enigo 0.2, rand 0.8 | same | **same** |
| **tests** | 0 | 0 | 19 | 53 |
| **edition** | 2024 | 2021 | 2021 | 2021 |

Step by step:

- **v01 → v02: "behaviour as a value".** The closure in `start_task` becomes
  `Box<dyn Routine>`. Adding a routine stops touching the agent. A workspace
  appears so two packages share dependency versions.
- **v02 → v03: "more than one way in, and more than one browser".** A second
  front end (HTTP) appears, and `dispatch()` becomes the single decision point.
  The browser ladder introduces the "try things in order, report every failure"
  pattern. `include_str!` embeds the dashboard so there is no asset to deploy.
- **v03 → v04: "more than one process".** The runtime moves into `agent_core`
  and grows a socket. Routines stop printing and start logging, because output
  now has many possible readers. Tasks learn to prune themselves. Tests stop
  being only unit tests and start spawning real daemons.

**What did *not* change**, and why that matters: `repeat_enter`, the stop flag,
`sleep_interruptible`, the `Registry` idea, and the browser ladder's order. The
architecture moved three times; those pieces were already right.

---

## Operating the program

This section is the manual. It is organised by version, and then by "what do I
do when…".

### Prerequisites

- A Rust toolchain (`rustup` is the easy path). All four versions work on stable
  Rust; v01's edition 2024 needs roughly 1.85 or newer.
- Windows, macOS or Linux. Anything platform-specific is behind `#[cfg]`.
- For `v01`–`v04` keyboard routines: a desktop session. They synthesise real
  keystrokes into the focused window, so they do nothing useful in a
  headless/SSH session.

### Build once per version

```bash
cd v04          # or v01/rust-agent, v02, v03
cargo build            # debug build in target/debug/
cargo build --release  # optimised build in target/release/
cargo test             # run that version's tests
```

Run a workspace binary with `-p` (v02 onwards):

```bash
cargo run -p agent                 # v02/v03: the agent
cargo run -p agent -- --port 9000  # note the `--`: everything after it is the program's own argument
```

In v04 there are two binaries, so `-p agent` alone is ambiguous:

```bash
cargo run -p agent --bin agent -- daemon
cargo run -p agent --bin agentctl -- status
./target/debug/agentctl status     # or just use the built binary directly
```

### v01 — the single-file agent

```bash
cd v01/rust-agent
cargo run
```

```
repeat_enter 500              press Enter every 500 ms
repeat_enter_jitter 1000 100  press Enter every 1000 ms ± 100 ms
list                          what is running
stop repeat_enter             stop that task
quit                          exit
```

There is no `stopall`, no browser command, and no web page. Tasks self-remove
when they stop.

### v02 — the same agent, two crates

```bash
cd v02
cargo run -p agent
```

Identical console commands to v01. The difference is internal; if you are
following the tutorial, the way to "see" v02 is to open
`v02/routines/src/lib.rs` and `v02/agent/src/main.rs` side by side.

### v03 — agent plus web control panel

**Console commands**

```
help | ?                       list commands
status                         uptime, tasks, browser state
list | tasks                   running tasks
browser [url]                  default → chrome → firefox (default page: Google)
open_browser [url]             subroutine form of the above
browsers                       show the ladder without launching anything
repeat_enter [ms]              press Enter on an interval
repeat_enter_jitter [b] [j]    press Enter with jitter
stop <task>                    stop one task
stopall                        stop every task
quit | exit                    exit
```

**Options**

```
-p, --port <n>   web port (default 8765, 0 = let the OS pick one)
    --no-web     console only, no HTTP server
    --no-open    refuse to launch browsers
    --open-web   open the dashboard after startup
    --browser [url]
                 launch a browser once (default: Google) and exit
-h, --help       usage
```

**Web API**

```bash
curl "http://127.0.0.1:8765/api/status"
curl "http://127.0.0.1:8765/api/cmd?cmd=browser"
curl "http://127.0.0.1:8765/api/cmd?cmd=repeat_enter%20500"
curl -X POST -d "cmd=list" http://127.0.0.1:8765/api/cmd
```

Replies are JSON: `{"ok":true,"cmd":"…","output":"…"}`. A rejected command is
`400`; a browser launch that arrives while another is in flight is `429`.

### v04 — standalone or daemon

**`agent` (the program)**

```
agent [options]              standalone: commands on this console
agent run [options]          same, said explicitly
agent daemon [options]       daemon: any terminal can drive it
agent --help                 usage

-p, --port <n>        control socket port (default: first free near 45917)
    --session-dir <p> where the session file lives
    --no-open         refuse to launch browsers
-q, --quiet           no startup banner
```

**`agentctl` (the remote control)**

```
agentctl                     interactive session against the daemon
agentctl <command> [args…]   one command, then exit
agentctl session             where the session file is, and who answers
agentctl daemon-status       port, session file, clients, event backlog
agentctl daemon-start        start a daemon in the background and wait for it
agentctl daemon-stop         stop the daemon
agentctl --version | -V      client version

-p, --port <n>        port (used by daemon-start)
    --session-dir <p> session directory to read
    --no-open         pass through to a daemon started with daemon-start
-v, --verbose         show which session file was used
```

`agentctl` accepts its flags **before or after** the command, so
`agentctl session --session-dir /tmp/a` and
`agentctl --session-dir /tmp/a session` both work.

**Commands available in both modes**

```
help | ?                       command list
version                        version and api level
status                         uptime, tasks, control endpoint
daemon_status                  daemon detail: session file, clients, events
list | tasks                   running tasks
browser [url]                  default → chrome → firefox (default: Google)
open_browser [url]             subroutine form of the above
browsers                       show the ladder without launching anything
repeat_enter [ms]              press Enter on an interval
repeat_enter_jitter [b] [j]    press Enter with jitter
stop <task>                    stop one task (exact name, or unique prefix)
stopall                        stop every task
events [since]                 recent daemon events
clients                        recent client connections
daemon_stop                    stop the agent
quit | exit                    leave a standalone console only
```

**Exit codes for `agentctl`** — designed for scripts:

| code | meaning |
|------|---------|
| `0` | the daemon accepted the command |
| `1` | the daemon refused it (unknown command, no such task) |
| `2` | a local problem (no daemon running, bad flags) |

### Workflows

**Start a daemon and keep using the terminal**

```bash
./target/debug/agent daemon &          # foreground gives you the log live
./target/debug/agentctl daemon-start   # or let agentctl start it detached,
                                       # logging to <session-dir>/daemon.log
```

**Watch a long routine and stop it**

```bash
./target/debug/agentctl repeat_enter 2000
./target/debug/agentctl list              # - repeat_enter   up 4s   (repeat_enter 2000ms)
./target/debug/agentctl stop repeat_enter
./target/debug/agentctl list              # (no running tasks)
```

**Run two independent agents on one machine**

```bash
agent daemon --session-dir /tmp/agent-a --port 47001
AGENT_SESSION_DIR=/tmp/agent-b agent daemon --port 47002

agentctl --session-dir /tmp/agent-a status
AGENT_SESSION_DIR=/tmp/agent-b agentctl status
```

**Check on the daemon without changing anything**

```bash
./target/debug/agentctl session        # where the file is, is the daemon alive
./target/debug/agentctl daemon-status  # clients, event backlog, endpoint
./target/debug/agentctl events         # what has happened recently
```

**Stop everything**

```bash
./target/debug/agentctl stopall        # stop tasks, keep the daemon
./target/debug/agentctl daemon-stop    # tasks stop, then the daemon exits
```

### Troubleshooting

| symptom | cause | fix |
|---------|-------|-----|
| `no daemon is running (looked in …)` | nothing is listening, or the two processes are looking at different session directories | start one (`agent daemon`), or give both the same `--session-dir` / `AGENT_SESSION_DIR` |
| `found a session file … but the daemon is not usable` | the daemon died without cleaning up (killed, power loss) | the message names the file; delete it, or just start a new daemon, which replaces it |
| `invalid or missing session token` | a client read a stale session file | restart the client; if it persists, restart the daemon |
| `a browser launch is already in flight` | the one-at-a-time guard, working as designed | wait ~1.5 s |
| `unknown command: quit` in a daemon | `quit` is a console concept | use `daemon_stop`, or `agentctl quit` inside an interactive session |
| `could not bind the control socket` | the port is taken by something else | `--port 46000`, or omit `--port` to walk the default range |
| `stop` says a prefix "matches 2 tasks" | two routines share the prefix | use the full name, shown in the message and in `list` |
| keyboard routines do nothing | no focused desktop window, or the process is not allowed to send input | run it in a normal desktop session, focus the target window first |
| v01 fails to build on an old toolchain | edition 2024 | `rustup update`, or start with v02 |
| `cargo build` cannot reach crates.io | offline/sandboxed environment | `cargo build --offline` with a warm cargo cache (see below) |

Two environment notes:

- **`--offline`.** If the machine cannot reach crates.io but has built these
  projects before, `cargo build --offline` works from the local cache. This is
  how the repository was built and tested during development.
- **`AGENT_SESSION_DIR`** (v04 only) overrides session-file discovery for both
  the agent and `agentctl` — the cleanest way to run isolated instances, or to
  work somewhere `%LOCALAPPDATA%` is not writable.

---

## Everything that went wrong

Bugs are the best documentation, because they show what the types could not.
These were all found by *running* the programs, not by reading them — which is
itself the lesson.

**v03**

1. **`/api/browser` dispatched `/open_browser`.** A leading slash turned a valid
   command into `unknown command`. The handler had been written by copying the
   *URL path* instead of the *command name*. Fix: pass `"open_browser"`, and add
   a test asserting the shorthand reaches the dispatcher.
2. **Mixed path separators in the ladder output** — `C:\Program Files\Google/Chrome/…`.
   Built by `PathBuf::join` with `"Google/Chrome/Application"` in one string.
   Fix: join each component separately; normalize only the *path* for display and
   never the URL (a first attempt replaced `/` in `https://` too, and
   `describe_ladder` promptly printed `https:\\www.google.com`).
3. **The browser ladder's failure path was untestable.** It read process
   environment variables directly, so a test could not simulate "no browser
   installed". Fix: `find_chrome_in(&[PathBuf])` takes the directories to search,
   and the public `find_chrome()` passes the real environment in. A testable seam.
4. **A doubled `bye`** — `quit` printed a reply *and* the loop printed its own
   farewell. Fix: treat `quit` as a loop-level command, not a dispatcher command.

**v04**

5. **The handshake command did not exist.** `agentctl` sent `hello`; the runtime
   answered `unknown command: hello`, so every client concluded the daemon was
   unusable while the daemon was perfectly healthy. Fix: implement `hello`.
6. **JSON strings were sliced but never unescaped.** `Response::from_line` cut
   the text between quotes and returned it raw, so any output containing a quote
   or backslash came back mangled. The unit test `escaping_survives_awkward_output`
   caught it. Fix: `json::unescape`, plus a test with quotes, backslashes,
   newlines and a non-ASCII character.
7. **`json_string` meant two different things** — "quote this for output" and
   "read this field" — and the compiler refused to let both exist. Fix: group the
   escaping helpers in a `json` module (`json::string`, `json::escape`,
   `json::unescape`) and name the reader `json_string_field`. The type system
   forcing a naming decision is a feature.
8. **`stop repeat_enter` failed** because tasks registered under a pretty label
   (`repeat_enter 30000ms`) rather than the routine's name. Fix: register under
   the routine name — what v02 and v03 did — and keep the parameter string as a
   separate display label. Then add *prefix* resolution, so `stop repeat` works
   when unambiguous and is refused with the candidate list when not.
9. **Finished tasks were never removed.** v02/v03 removed a task from the
   registry inside the task's own thread; the daemon rewrite dropped that, so
   `open_browser` (which completes on its own) stayed in `list` forever. Fix:
   the registry prunes any task whose thread has finished, before every read.
10. **`agentctl` stopped parsing flags at the first positional argument**, so
    `agentctl session --session-dir X` silently ignored `X` and probed the
    default directory — a silent wrong answer, the worst kind. Fix: collect
    global flags from anywhere in the argument list.
11. **`daemon-start` hung the shell that called it.** The spawned daemon
    inherited the caller's stderr handle, so a shell waiting for that pipe never
    got its prompt back. Fix: give the daemon its own log file for stdout and
    stderr, and start it detached (`DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`
    on Windows, `setsid` on unix).
12. **The session directory was assumed, not discovered.** The first
    implementation wrote to `%LOCALAPPDATA%\rust-agent`, which does not exist and
    is not writable in a sandboxed environment. Fix: probe candidates with a real
    write and take the first that succeeds. Failures here are silent by nature —
    the daemon starts, publishes nothing, and every client says "no daemon" — so
    `agentctl session` now reports the chosen directory *and why*.

## Repository and development

```
dev_rust-agent.ajc/
├── README.md
├── .gitignore
├── v01/rust-agent/     standalone package
├── v02/                workspace: agent, routines
├── v03/                workspace: agent, routines, agent_web
└── v04/                workspace: agent, routines, agent_core
```

Each version carries its own `Cargo.toml`, its own `Cargo.lock`, and its own
`target/`. `Cargo.lock` is committed so builds are reproducible; `target/` is
ignored.

**Where to change what**

| you want to… | go to |
|--------------|-------|
| add a new routine (either version) | `vNN/routines/src/lib.rs` — a struct plus `impl Routine` |
| change how a browser is found or launched | `vNN/routines/src/launcher.rs` |
| add a command | v03: `dispatch()` in `agent/src/main.rs`; v04: `dispatch()` in `agent_core/src/runtime.rs` |
| change the web panel | `v03/agent_web/src/dashboard.html` (and `lib.rs` for routes/API) |
| change the wire protocol | `v04/agent_core/src/protocol.rs` (bump `API_LEVEL` if shapes change) |
| change how a daemon is discovered | `v04/agent_core/src/session.rs` |
| change `agentctl`'s behaviour | `v04/agent/src/bin/agentctl.rs` |

**Tests.** `cargo test` inside a version directory. Counts: v03 has 19, v04 has
53 (including 8 integration tests that start real processes). v01 and v02 have
no tests — they are the versions where you are meant to be reading, not
regression-testing.

**Conventions this codebase follows**

- No dependency is added without a reason; v04 adds none at all, using `std::net`,
  hand-written JSON, and `#[cfg]`-gated OS calls.
- Errors are values: `Result`/`Option` with `?`, and hand-written `Display`/`Error`.
- Anything shared across threads is `Arc<Mutex<…>>` behind a `lock()` helper that
  survives poisoning.
- Every routine takes a stop flag and sleeps in slices.
- Tests live next to the code they test; anything needing a real process lives in
  `tests/`.

## Glossary

| term | meaning here |
|------|--------------|
| **agent** | the program, in any version |
| **daemon** | the v04 agent running as a background process that other terminals can command |
| **routine** | a unit of work the agent can start and stop (`repeat_enter`, `open_browser`, …) |
| **task** | a routine that is currently running, and its entry in the registry |
| **registry** | the shared map of running tasks to their stop flags |
| **subroutine** | "routine", from the command's point of view — `open_browser` is a subroutine |
| **control socket** | the loopback TCP port the v04 daemon listens on |
| **session file** | `session.json` — the port and token a client needs to reach a daemon |
| **session directory** | the folder holding `session.json`, discovered rather than assumed |
| **token** | the random secret in the session file, checked on every request |
| **ladder** | the browser fallback order: default → Chrome → Firefox |
| **front end** | whatever turns input into commands: console, HTTP, socket |
| **stop flag** | the `Arc<AtomicBool>` a routine polls so it can be asked to stop |

## License

MIT. See the `license` field in each version's `Cargo.toml`.
