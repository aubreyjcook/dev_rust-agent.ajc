# v0_04 — one process, or a daemon

> **Read [v0_01](../v0_01/rust-agent/README.md), [v0_02](../v0_02/README.md) and
> [v0_03](../v0_03/README.md) first.** This version reorganises everything those
> three built; it is much easier to follow as a continuation than as a fresh
> start.
>
> For the Rust concepts themselves, see
> [Start here: Rust in this project](../README.md#start-here-rust-in-this-project).
> For the reference-style notes on commands, protocol and limits, see
> [the reference appendix](#reference-appendix) at the bottom.

---

## What changed, and why

v0_03 ended with a good property: **one `dispatch` function, two front ends**. The
console and the web panel both called it, so a command could not behave
differently depending on which door it came through.

v0_04 takes that property seriously and asks the obvious next question: if the
front end does not matter, why should it matter *which process* it lives in?

So the program learns to run two ways:

| mode | started with | who can drive it |
|------|--------------|------------------|
| **standalone** | `agent` or `agent run` | only its own console — exactly v0_03's behaviour |
| **daemon** | `agent daemon` | **any terminal on the machine**, via a new `agentctl` client |

The daemon publishes a session file, listens on a loopback socket, and answers
commands from any number of clients. The commands themselves are unchanged.

### The diff, in one table

| v0_03 | v0_04 | why |
|-----|-----|-----|
| `dispatch` in `agent/src/main.rs` | `Runtime::dispatch` in `agent_core/src/runtime.rs` | a third front end needed the semantics, and `main` is the wrong place for shared logic |
| `Routine::run(self, stop)` | `Routine::run(self, stop, log)` | output has many possible readers now, not just stdout |
| `println!` inside routines | `log.line(…)` | a daemon's stdout is not where a remote client can see it |
| no tests in the library | 22 unit tests in `agent_core` | the extracted crate is now worth testing |
| tasks removed by their own thread | the registry **prunes** finished tasks | a routine that ends on its own left a dead entry behind |
| `stop <base name>` | `stop` resolves exact names *and* unique prefixes | the v0_03 label bug, fixed properly |
| 2 binaries? no — 1 | `agent` **and** `agentctl` | two roles, two front ends, one package |
| one way to find the program | session file + token + discovery | two processes must agree on where to talk |
| 19 tests | 53 tests, including 8 that run real processes | the pieces are now testable and worth testing |

---

## Change 1: four front ends, one dispatch

This is the shape v0_04 moves to:

```
   console (stdin)  ─┐
                     ├─→  Runtime  ─→  routines
   socket (agentctl)─┘
```

`agent_core::Runtime` owns the semantics. It has no `println!` of its own: every
command *returns* a string, and the front end decides where it goes. The console
prints it; the server writes it back down the socket. Both call the same method
under the same lock:

```rust
let response = {
    let _guard = lock(runtime.gate());   // one command at a time, process-wide
    runtime.handle(&peer_label, &request)
};
```

Two things to notice:

- **The gate lives on the Runtime, not on the server.** Serialising commands is a
  property of "there is one agent", not of "there is a socket". If v05 added a
  third front end, it would take the same lock without touching the server.
- **`handle` takes a `&Request` and returns a `Response`.** Both are plain data
  types from `protocol.rs`. The runtime knows about *requests*, not about TCP,
  stdin, or JSON strings on the wire — that conversion happens in the front end.

Compare the two loops that drive it. The console, in `agent/src/main.rs`:

```rust
let cmd = line.trim().to_string();
if matches!(cmd.as_str(), "quit" | "exit") {
    break;                                  // a console-only idea
}
let response = {
    let _guard = agent_core::state::lock(runtime.gate());
    runtime.handle("console", &agent_core::Request::new("", &cmd))
};
print!("{}", response.reply.output);
```

and the socket, in `agent_core/src/server.rs`:

```rust
let request = match Request::from_line(&line) { … };
let response = {
    let _guard = lock(runtime.gate());
    runtime.handle(&peer_label, &request)
};
write_response(&mut writer, &response)?;
```

Same two lines in the middle. Everything else is transport.

Note also what the console loop does *not* send: `quit` never reaches the
runtime, because "leaving this console" is a property of *this front end*. In
daemon mode there is no console to leave, and the equivalent is `daemon_stop`.

---

## Change 2: routines stopped printing

This is the most visible change in the routine API.

```rust
// v0_03
fn run(self: Box<Self>, stop: Arc<AtomicBool>);

// v0_04
fn run(self: Box<Self>, stop: Arc<AtomicBool>, log: LogHandle);
```

Why does output need a trait? Because in v0_03 there was exactly one reader —
whoever was looking at the agent's console — and in v0_04 there are many: the
daemon's log file, each connected client, and (in standalone mode) still the
console. A routine cannot know which, so it is handed a sink:

```rust
pub trait Log: Send + Sync + 'static {
    fn line(&self, text: &str);
}

pub type LogHandle = Arc<dyn Log>;
```

- **`LogHandle` is `Arc<dyn Log>`**, so the sink can be shared and is chosen at
  runtime — the same pattern as `BoxedRoutine`, one level down.
- **`Send + Sync`** because the sink is used from task threads and read from
  connection threads.
- **`Arc`, not `Box`**, because the runtime keeps its own handle to the same
  buffer while the routine writes into it.

There are two implementations, and they are three lines each:

```rust
impl Log for std::io::Stdout {
    fn line(&self, text: &str) {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{text}");
        let _ = out.flush();
    }
}

impl Log for TaskLog {          // in agent_core/src/state.rs
    fn line(&self, text: &str) {
        self.push(text);
    }
}
```

`TaskLog` is a bounded ring buffer of the last 100 lines, wrapped in
`Arc<Mutex<VecDeque<String>>>`. Because it is shared, the runtime can read back
"what did this task say?" long after the routine moved on — which is how
`agentctl list` and the dashboard can show output for tasks running in a
different process.

`routines` also ships `NullLog`, which discards everything. It exists so tests can
run a routine without any output plumbing, and it is a good example of how cheap
implementing a small trait is.

### Where a routine reports now

```rust
log.line(&format!("[repeat_enter] pressing Enter every {}ms", self.interval_ms));
let mut presses: u64 = 0;
while !stop.load(Ordering::Relaxed) {
    if let Err(e) = enigo.key(Key::Return, Direction::Click) {
        log.line(&format!("[repeat_enter] key error: {e}"));
    }
    presses += 1;
    sleep_interruptible(self.interval_ms, &stop);
}
log.line(&format!("[repeat_enter] stopped after {presses} presses"));
```

Returning to the same call shape as `println!` while writing into a buffer is
the point: the routine did not get harder to write, it got *relocatable*.

---

## Change 3: the registry learned to forget

v0_01 through v0_03 all removed a finished task from the registry **inside the task's
own thread**:

```rust
thread::spawn(move || {
    routine.run(stop);
    registry.lock().unwrap().remove(&name);     // v0_02/v0_03
});
```

That works right up until a routine finishes *on its own*. `open_browser` does
exactly that: it launches a browser and returns. In the v0_04 rewrite, the cleanup
line was dropped, and the consequence was visible:

```
$ agentctl browser
$ agentctl list
- open_browser          up 42s      ← dead, but still listed
```

The fix is to stop making cleanup the task's responsibility and let the registry
notice for itself:

```rust
impl Task {
    /// Has the routine's thread finished?
    fn finished(&self) -> bool {
        self.handle.as_ref().map(|h| h.is_finished()).unwrap_or(true)
    }
}

fn prune(&self, tasks: &mut Vec<Task>) {
    tasks.retain(|task| !task.finished());
}
```

`JoinHandle::is_finished()` is the non-blocking way to ask "is that thread
done?". `prune` is then called before **every read** of the registry — `infos`,
`names`, `is_running`, `len`, `resolve`, `stop`, `stop_all` — so a finished task
can never be observed by any caller:

```rust
pub fn names(&self) -> Vec<String> {
    let mut tasks = lock(&self.tasks);
    self.prune(&mut tasks);
    tasks.iter().map(|t| t.name.clone()).collect()
}
```

Why is this better than the per-thread cleanup?

- **It cannot be forgotten.** A new routine cannot break it, because the routine
  is not involved.
- **`retain` is one line** where the old approach needed a lock dance inside
  every spawn.
- **Locks can be taken on a `&mut Vec`**, so pruning and reading happen under one
  lock — no window where a caller sees a task that is about to vanish.

The lesson generalises: **state that describes a process should be derived from
the process, not maintained by it.** `is_finished()` is the truth; the `Vec` is a
cache of it.

`Agent::join_all(timeout)` uses the same idea for shutdown: it polls
`is_finished()` up to a deadline instead of blocking on `join()`, so a routine
that ignores its stop flag cannot hang the daemon's exit.

### And `stop` learned prefixes

With names fixed (tasks register under the routine's name, and the parameter
string lives in a separate display `label`), v0_03's bug is gone. v0_04 goes one
further: `stop` accepts an unambiguous prefix.

```rust
pub fn resolve(&self, typed: &str) -> Result<String, ResolveError> {
    let mut tasks = lock(&self.tasks);
    self.prune(&mut tasks);
    let names: Vec<&str> = tasks.iter().map(|t| t.name.as_str()).collect();

    if names.iter().any(|n| *n == typed) {
        return Ok(typed.to_string());          // exact match wins
    }

    let matches: Vec<&str> = names.iter().copied().filter(|n| n.starts_with(typed)).collect();
    match matches.len() {
        0 => Err(ResolveError::NotFound(typed.to_string())),
        1 => Ok(matches[0].to_string()),
        _ => Err(ResolveError::Ambiguous(typed.to_string(), matches.iter().map(|s| s.to_string()).collect())),
    }
}
```

The design point: **ambiguity is an error, never a guess.** `stop repeat` with two
matching tasks does not pick one; it lists them. An error type with two variants
(`NotFound`, `Ambiguous`) makes that explicit, and the caller can add its own
"here is what is running" hint to whichever variant came back.

---

## Change 4: two processes, and how they find each other

### The session file

A daemon that nobody can find is useless, so the daemon writes down where to find
it:

```rust
pub struct Session {
    pub port: u16,
    pub token: String,
    pub pid: u32,
    pub started_unix: u64,
    pub version: String,
}
```

serialised by hand into `session.json`. `agentctl` reads it, and that file is the
entire contract between the two processes.

The ordering matters, and the code comments say so:

```rust
// Bind first: if the port is unusable we want to fail before publishing a
// session file that points at nothing.
let server = match Server::bind(Arc::clone(&runtime), opts.port) { … };
…
let session = Session::new(server.port());
if let Err(e) = session_dir.save(&session) { … }
```

If you wrote the file first and then failed to bind, every client would chase a
dead port. **Publish last** is a rule that generalises to every kind of service
registration.

On shutdown the file is removed, so a later client gets an honest "no daemon"
rather than a connection timeout. When a daemon is killed rudely the file
survives, which is why clients distinguish "not running" from "unusable" and the
troubleshooting table has an entry for a stale file.

### Discovery, not assumption

The session directory is chosen by trying candidates and taking the first that
**accepts a real write**:

```rust
pub fn discover() -> Self {
    for (dir, source) in candidates() {
        let writable = probe_writable(&dir);
        if writable {
            return Self { dir, source, writable };
        }
    }
    …
}

fn probe_writable(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() { return false; }
    let probe = dir.join(format!(".probe-{}", process::id()));
    match fs::write(&probe, b"probe") {
        Ok(()) => { let _ = fs::remove_file(&probe); true }
        Err(_) => false,
    }
}
```

The candidate order is `AGENT_SESSION_DIR`, then the per-user state directory,
then temp, then `.rust-agent` in the working directory.

Two lessons are packed into those thirty lines:

- **Existence is not writability.** A directory can exist, be readable, and still
  refuse writes — the first implementation of this wrote to
  `%LOCALAPPDATA%\rust-agent` and failed in a sandbox. The only reliable test is
  to write.
- **A silent failure here is expensive.** When discovery fails, the daemon writes
  an error and exits non-zero — correct, but the *effect* is that nothing gets
  published, and every client merely reports "no daemon". Neither message tells
  you which directory was wrong. That is why `SessionDir` carries a `source`
  string and `agentctl session` prints it: when something is off, you need to know
  *which* candidate was chosen and why.

`Session::from_json` is the other half of the defensive design: anything
unexpected returns `None`, so a corrupt or foreign file is treated as "no daemon"
rather than crashing the client.

### The wire protocol

Newline-delimited JSON, one object per line, in both directions:

```json
→ {"token":"<hex>","cmd":"repeat_enter 500","since":41}
← {"ok":true,"output":"started task 'repeat_enter'","shutdown":false,"version":"0.4.0","events":[…]}
```

Three design decisions worth understanding:

- **One object per line.** Reads are `read_line`, so no framing machinery is
  needed and a stream can be debugged with `netcat`.
- **A `since` cursor instead of a push.** Clients report the last event they saw;
  the daemon answers with anything newer (bounded). A client that was away
  catches up; a client that is current gets nothing. There are no push channels,
  no websockets, and no threads per subscriber.
- **`token` on every request, not just the first.** Connections are short-lived,
  so there is no session state to keep — and a token that must be presented every
  time cannot be forgotten on a reconnection.

The server reads until the client hangs up, so one connection can carry many
requests; `agentctl`'s interactive mode uses that, while a one-shot
`agentctl status` sends one request and closes.

`Request` and `Response` also have `to_line`/`from_line` methods rather than a
serialisation framework. That is a deliberate choice for this project: the
message shapes are flat, and hand-writing them keeps the dependency list empty.
It is also where the fiddliest code in the repository lives — the field extractor
in `protocol.rs` walks the string tracking `in_string` and `escaped` so a quote
inside a value cannot end the value. There is a test for exactly that case.

### Handshake and version

```rust
pub fn handshake(&self) -> Result<String, String> {
    let response = self.send_request(&Request::hello(self.session.token.clone()))?;
    if !response.reply.ok {
        return Err(response.reply.output);
    }
    if response.version != VERSION {
        return Err(format!("version mismatch: client {VERSION}, daemon {}", response.version));
    }
    Ok(response.reply.output)
}
```

`hello` exists purely so a client can answer two questions before it does
anything: *is the token good?* and *does this daemon speak my protocol?* The
`api_level` in `status` output serves the second question in a longer-lived way.

The daemon refuses a bad token:

```rust
let expected = self.session().map(|s| s.token).unwrap_or_default();
if !expected.is_empty() && request.token != expected {
    self.record("auth", format!("rejected command from {peer}: bad token"));
    return Response::new(Reply::err("invalid or missing session token; …"), Vec::new());
}
```

Note that the rejection is *recorded as an event*, so the daemon's owner can see
that something tried.

### Shutting down without a crate

A background process needs to stop on <kbd>Ctrl</kbd>+<kbd>C</kbd>, and v0_04 has no
dependencies to help:

```rust
#[cfg(windows)]
fn ctrlc_flag(shutdown: Arc<AtomicBool>) -> Result<(), String> {
    use std::sync::OnceLock;
    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();

    extern "system" fn handler(_ctrl_type: u32) -> i32 {
        if let Some(flag) = FLAG.get() { flag.store(true, Ordering::SeqCst); }
        1 // handled: do not run the default terminate handler
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn SetConsoleCtrlHandler(handler: Option<extern "system" fn(u32) -> i32>, add: i32) -> i32;
    }
    …
}

#[cfg(unix)]
fn ctrlc_flag(shutdown: Arc<AtomicBool>) -> Result<(), String> {
    extern "C" fn handler(_signum: i32) { … }
    extern "C" { fn signal(signum: i32, handler: usize) -> usize; }
    …
}
```

Rust ideas on display:

- **`extern "system"` / `extern "C"`** declare a function whose calling
  convention matches the platform, which is what a C library callback requires.
- **The handler must be a plain `fn` with no captures**, because the OS has no
  idea what a closure environment is. Hence the `OnceLock<Arc<AtomicBool>>`
  static — a global the handler can reach.
- **`OnceLock`** is the standard-library "initialise once, safely" cell.
- **`unsafe` appears only where it must**, wrapped around the FFI calls, with the
  rest of the function remaining safe code.

The flag the handler sets is the same one the accept loop polls, so the socket is
noticed as closed within ~200 ms and the shutdown path runs: stop tasks, join them
with a 5 s grace, remove the session file, exit.

---

## Change 5: a second binary, and tests that run it

### Two binaries in one package

```toml
# agent/Cargo.toml
[[bin]]
name = "agent"
path = "src/main.rs"

[[bin]]
name = "agentctl"
path = "src/bin/agentctl.rs"
```

One package, two executables. Cargo builds both with `cargo build`, which means
`agentctl` is guaranteed to be installed alongside `agent` — and `agentctl
daemon-start` relies on that, looking for a sibling binary rather than searching
`PATH`.

A consequence worth knowing: in this package `cargo run` is now ambiguous. You
must say `cargo run --bin agent -- daemon` or `cargo run --bin agentctl --
status`. Cargo's error message spells that out.

### `agentctl`'s shape

`agentctl` is deliberately stateless: every command opens a short-lived
connection, sends one request, reads one response, closes.

```rust
fn send(client: &Client, cmd: &str, since: &mut u64) -> Result<u8, String> {
    let outcome = client.send(cmd, *since)?;

    for (seq, kind, text) in &outcome.events {
        *since = (*since).max(*seq);
        if kind == "client" { continue; }        // connection noise is filtered
        eprintln!("[{kind}] {text}");
    }
    …
    Ok(if outcome.ok { EXIT_OK } else { EXIT_REFUSED })
}
```

Why this shape:

- **No shared state between terminals.** Two `agentctl` processes cannot interfere
  because neither holds anything.
- **A client that dies leaves nothing behind** — no half-open session for the
  daemon to reap.
- **The cursor advances even for filtered events**, so a one-shot command never
  replays the daemon's history. (That bug was real: before the cursor was tracked
  per client, every `agentctl` invocation printed the whole backlog.)
- **Events go to stderr, the answer to stdout**, so `agentctl list > file` gives
  you the list and nothing else.

Exit codes are a small contract that makes the tool scriptable: `0` accepted,
`1` refused by the daemon, `2` a local problem like "no daemon". Note that the
*daemon's* refusal and the *client's* inability to connect are different codes,
because a script usually wants to react differently.

### Tests that start real processes

`agent/tests/daemon.rs` is an integration test: a separate crate that can only use
public APIs, so it drives the real binaries.

```rust
fn agent_bin() -> &'static str { env!("CARGO_BIN_EXE_agent") }

fn start_daemon(&mut self) {
    let child = Command::new(agent_bin())
        .arg("daemon")
        .arg("--session-dir").arg(&self.session_dir)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().expect("spawn daemon");
    self.daemon = Some(child);
    self.wait_ready();
}
```

What makes these tests work, and worth copying:

- **`env!("CARGO_BIN_EXE_agent")`** is Cargo handing the test the path of the
  binary it just built. No path guessing, no "did you build first?".
- **`wait_ready()` polls instead of sleeping a fixed time.** It runs
  `agentctl session` until it sees "daemon reachable", so the suite is not
  flaky-slow and not flaky-broken.
- **The harness reports *why* the daemon died.** When the daemon exits early, the
  test reads its stderr and panics with it. A test that says "the daemon never
  became reachable" is nearly useless; one that says "exited with status 1:
  could not bind the control socket" is a diagnosis.
- **`Drop` cleans up**, killing the child if a graceful stop did not work and
  removing the temp directory.
- **Each test uses its own session directory in the temp dir**, so tests can run
  in parallel without fighting over the default port or session file.

Together these eight tests cover what unit tests cannot: that the two binaries
agree about the session file, that one client's tasks are visible to another, that
refusals produce the right exit codes, that a stale session file is reported
rather than trusted, and that standalone mode publishes nothing.

---

## The new module layout, and why each file exists

```
v0_04/
├── agent/                          the front ends
│   ├── src/main.rs                 mode selection, console loop, daemon startup
│   ├── src/console.rs              `agent` CLI parsing
│   ├── src/bin/agentctl.rs         the client binary
│   ├── src/agentctl_cli.rs         `agentctl` CLI parsing
│   └── tests/daemon.rs             integration tests
├── agent_core/                     everything shared
│   ├── src/lib.rs                  module list + public re-exports
│   ├── src/runtime.rs              command semantics — no transport at all
│   ├── src/state.rs                the task registry, and TaskLog
│   ├── src/server.rs               the daemon's control socket
│   ├── src/client.rs               the client end of that socket
│   ├── src/session.rs              discovery and the session file
│   └── src/protocol.rs             wire types, JSON, framing
└── routines/                       the subroutines
    ├── src/lib.rs                  Routine + Log traits, the routines
    └── src/launcher.rs             the browser ladder (unchanged from v0_03)
```

The split between `runtime.rs` and `server.rs` is the one to internalise. Ask of
any function: *would this still make sense in a program with no sockets?* If yes
it belongs in `runtime`; if it mentions `TcpStream`, `Request::from_line`, or
`IO_TIMEOUT`, it belongs in `server`. That question is what keeps the three front
ends sharing one behaviour instead of slowly diverging.

---

## Build and run

```bash
cd v0_04
cargo build
cargo test                                   # 53 tests, 8 of which start real processes

# standalone — v0_03's behaviour, unchanged
./target/debug/agent
./target/debug/agent run --no-open

# daemon
./target/debug/agent daemon
./target/debug/agent daemon --port 46000 --session-dir /tmp/agent

# remote control, from any terminal
./target/debug/agentctl status
./target/debug/agentctl browser
./target/debug/agentctl repeat_enter 500
./target/debug/agentctl list
./target/debug/agentctl stop repeat_enter
./target/debug/agentctl daemon-stop
```

Because the package has two binaries, `cargo run` needs a `--bin`:

```bash
cargo run --bin agent -- daemon
cargo run --bin agentctl -- status
```

> ⚠️ `browser` opens a real browser window on the machine running the daemon;
> `repeat_enter` presses real keys into the focused window.

---

## Try changing it

1. **Add a command and reach all three front ends.** Add a `"ping" => …` arm to
   `Runtime::dispatch`, then check that the console, `agentctl ping`, and
   `curl`-style socket calls all answer. You changed one function, not three —
   that is the payoff of the extraction.

2. **Write a Log sink that timestamps.** Implement `Log` for a struct wrapping a
   file and prefix each line with the time. In standalone mode there is no wiring
   for it (routines get `TaskLog`), so notice where you would have to plug it in —
   that is the seam v05 would widen.

3. **Watch pruning happen.** Start `agentctl browser` (which finishes on its own),
   then run `agentctl list` twice with a second between. The task disappears
   without anything having "removed" it, because `prune` notices the finished
   thread.

4. **Break the discovery order.** Set `AGENT_SESSION_DIR` to a path inside a
   read-only location and run `agentctl session`. It falls through to another
   candidate and tells you which — the `source` field exists for exactly this
   moment.

5. **Race two clients — and discover which routines dedupe and which queue.**
   Open two terminals and run `agentctl repeat_enter 1000` in both at once, then
   `agentctl list`: you get `repeat_enter` and `repeat_enter#2`, because
   `unique_name` finds a free suffix under the same lock as the insert. Now try
   the same with browser launches: the second one is *refused*, not renamed,
   because `open_browser` is guarded against concurrent launches. Two routines,
   two policies, both deliberate — and reading `dispatch_browser` next to
   `spawn` is a good exercise in why "start a task" and "start this particular
   task" are different operations.

6. **Remove the gate and see what breaks.** Delete the `lock(runtime.gate())`
   around one of the two front ends and hammer the daemon with two terminals of
   `browser`. What you are looking for is the class of bug the gate prevents:
   two commands interleaving their reads and writes of the same state.

When the daemon feels routine, go back to the
[**root README**](../README.md#reading-the-diff-what-changed-at-each-step) and
read the four-version diff as a whole. The interesting story is not any single
feature; it is that the same three pressures — *name the work*, *separate the
front end*, *separate the process* — each forced one refactor and made the next
one possible.

---

## Roadmap for v0_05

v0_04 is the first version that is a *service*, and the honest list of what it
still lacks is longer than the list of what it does. These are the gaps, in the
order they are worth closing.

### 1. Mouse control — the biggest gap against the stated purpose

The project's agenda is a program that controls **the keyboard and mouse**. Every
version so far drives only the keyboard (`repeat_enter`, `repeat_enter_jitter`),
even though `enigo` — already a dependency — can move the pointer and click.

Planned work:

- a `mouse_move <x> <y>` routine that eases the pointer to a position rather than
  teleporting it, because a jump does not look like a person;
- a `repeat_click [button] [ms]` routine built on the same interruptible-sleep
  discipline as the keyboard ones;
- a shared input handle so routines stop each constructing their own `Enigo` on
  startup, which is currently duplicated in every `run`;
- documentation of the mouse's safety implications, in the same spirit as the
  existing "this presses real keys" warnings.

The trait changes needed are small: `Routine` already receives a stop flag and a
log sink, and a mouse routine needs nothing more.

### 2. Daemon lifecycle

The daemon is a *user-session* process, not a service: it does not survive logout
and cannot be made to start automatically.

- **v0_05**: a `--detach` flag that leaves it running after the terminal closes —
  half-present already in `agentctl daemon-start`, and worth lifting into the
  binary itself;
- **later**: platform integration (a systemd user unit, a launchd agent, a
  scheduled task), *documented rather than bundled*, because installing a service
  is a decision the user should make explicitly;
- an idle timeout, so a forgotten daemon does not hold a port forever.

### 3. Output that streams

`agentctl` currently learns about a task by *asking* (`events`, `list`), and each
one-shot invocation starts its event cursor at zero. That is fine for polling and
wrong for watching.

- a `--follow` mode that holds one connection open and prints events as they
  arrive, instead of re-connecting per command;
- a `since` cursor persisted in a small state file, so consecutive invocations do
  not re-print the same backlog;
- possibly a real push: the protocol is already line-oriented, so a long-lived
  subscriber needs no framing change — only a rule for which events a connection
  wants.

### 4. Configuration

Everything is a command-line flag today. A `~/.config/rust-agent/config.toml`
would let the session directory, preferred port and browser order persist — which
in turn means *parsing TOML*, a good argument for the project's first real
serialisation dependency. That decision is deliberately deferred: v0_04 has no
dependencies, and adding one should be a version's headline, not an accident.

### 5. Protocol and robustness

Small and cheap, but worth recording:

- `protocol.rs`'s `is_at_top_level` re-scans from the start on every candidate
  match, so field extraction is quadratic. Irrelevant at 16 KiB, but a
  hand-written tokeniser would be both faster and simpler to read;
- one thread per connection with a fixed 30 s budget does not bound the number of
  threads — a connection cap or a small thread pool is the next step;
- the token never rotates during a daemon's lifetime; a `rotate_token` command
  would let a long-running daemon invalidate a leaked session file.

### 6. Tests

The eight integration tests cover the happy path and the obvious failures. The
gaps worth filling first:

- **concurrency**: two clients issuing commands at once, asserting that the gate
  really serialises them;
- **a client dying mid-request**, and the daemon continuing to serve;
- **shutdown with a running task**, asserting the task stops and the session file
  is removed;
- **a panicking routine**, asserting that the poisoned-lock recovery in
  `agent_core`'s `lock` helper does what it claims.

### What is deliberately *not* planned

- **Exposing the socket beyond loopback.** TLS and a real authentication scheme
  are a different project; pretending otherwise would put a remote input service
  on a network.
- **A GUI.** The dashboard exists; a native window would add a fourth front end
  without proving anything new about the architecture.
- **Renaming the program.** It is called what it is called.

---

## Reference appendix

Kept from the original notes because it is genuinely useful when operating the
program.

**Starting it**

```powershell
cd v0_04
cargo build

.\target\debug\agent.exe                       # standalone (v0_03 behaviour)
.\target\debug\agent.exe run --no-open
.\target\debug\agent.exe daemon                # daemon
.\target\debug\agent.exe daemon --port 46000 --session-dir C:\tmp\agent
```

**`agentctl`**

```powershell
agentctl                     # interactive session
agentctl status              # one command, then exit
agentctl browser             # open a browser on the daemon's machine (Google)
agentctl repeat_enter 500    # start a subroutine
agentctl list                # what is running
agentctl stop repeat_enter   # stop it
agentctl events              # recent daemon events
agentctl daemon-status       # port, session file, clients, event backlog
agentctl daemon-start        # start a daemon in the background, wait for it
agentctl daemon-stop         # stop the daemon
agentctl session             # where the session file is, and who answers
```

Exit status: `0` accepted, `1` refused by the daemon, `2` no daemon or bad usage.

**Commands — identical in both modes**

```
help | ?                        command list
version                         version and api level
status                          uptime, tasks, control endpoint
daemon_status                   daemon detail: session file, clients, events
list | tasks                    running tasks
browser [url]                   open default -> chrome -> firefox (default: Google)
open_browser [url]              subroutine form of the above
browsers                        show the fallback ladder without launching
repeat_enter [ms]               press Enter on an interval
repeat_enter_jitter [base] [j]  press Enter with jitter
stop <task>                     stop one task (exact name or unique prefix)
stopall                         stop every task
events [since]                  recent daemon events
clients                         recent client connections
daemon_stop                     stop the agent
quit | exit                     leave a standalone console only
```

`stop` accepts an unambiguous prefix, so `agentctl stop repeat` works when only
one `repeat_*` routine is running — and refuses with the candidate list when it is
ambiguous.

**How the two processes find each other**

1. The daemon binds a **loopback-only TCP socket** (port 45917, walking upwards if
   taken; ephemeral as a last resort).
2. It writes `session.json` — port, PID, version, and a random 32-hex **token** —
   into its session directory.
3. `agentctl` reads that file, connects, and sends the token with every request.
4. On shutdown the daemon removes the file, so a later `agentctl` says "no daemon"
   instead of chasing a dead port.

The session directory is discovered rather than assumed: `AGENT_SESSION_DIR`, then
the per-user state directory (`%LOCALAPPDATA%\rust-agent` on Windows,
`~/.local/state/rust-agent` on Linux), then the temp directory, then `.rust-agent`
in the working directory. Override both sides for an isolated instance:

```powershell
$env:AGENT_SESSION_DIR = "C:\tmp\agent-a"   # or --session-dir on either binary
```

**Security boundary**

The socket is loopback-only by construction, and every request carries the token
from the session file — so another user on the same machine cannot drive your
agent. A wrong token is refused and recorded as an event.

This is *not* a system service: it runs with your privileges, in your session,
with your `PATH` and browser profile. That is the point — `agentctl browser` opens
a browser as you, in your desktop session. There is no elevation, no service
registration, and no TLS; do not expose the port.

**Notes and limits**

- `agentctl daemon-start` starts the daemon **detached**, with its output in
  `<session-dir>/daemon.log`. Running `agent daemon` in a terminal of its own is
  still the clearest way to watch it.
- Ctrl-C stops the daemon and asks running tasks to stop, waiting up to 5 s; a
  task that ignores its stop flag is left behind rather than blocking exit.
- One thread per client connection, with a 30 s socket budget and a 16 KiB request
  cap.
- The daemon runs until it is stopped, so `agentctl daemon-stop` (or Ctrl-C in its
  console) is the way out.
- No new dependencies: `std::net` for the socket, hand-written JSON, and a
  hand-rolled `SetConsoleCtrlHandler`/`signal` binding. In an offline environment,
  build with `cargo build --offline` against a warm cargo cache.
