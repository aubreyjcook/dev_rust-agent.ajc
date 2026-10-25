# Architecture overview

A system-level view of what this program is, how its parts fit together, and why
they are arranged that way. Read this after skimming the
[root README](README.md); read the per-version guides for the code itself.

- [What the program is](#what-the-program-is)
- [Shape of each version](#shape-of-each-version)
- [The data flow in v0_04](#the-data-flow-in-v0_04)
- [Package and dependency graph](#package-and-dependency-graph)
- [The three invariants](#the-three-invariants)
- [How a command travels](#how-a-command-travels)
- [Process and thread model](#process-and-thread-model)
- [Files and state on disk](#files-and-state-on-disk)
- [Why there are no dependencies](#why-there-are-no-dependencies)
- [Decision log](#decision-log)

---

## What the program is

A Rust agent with two responsibilities, which the
[project instructions](agent-instructions.md) name directly:

| responsibility | where it lives | what it means |
|----------------|----------------|---------------|
| **control the input devices** | `routines` | synthesise keyboard (and, planned, mouse) events into the operating system |
| **accept commands and manage that work** | `agent`, `agent_core` | parse commands, start/stop routines, report state |

The split between those two is the spine of the whole design. Every version is
an answer to the question *how much should the agent know about the routines?* —
and the answer shrinks at each step.

## Shape of each version

| | v0_01 | v0_02 | v0_03 | v0_04 |
|---|---|---|---|---|
| packages | 1 | 2 | 3 | 4 |
| front ends | console | console | console + HTTP | console + HTTP + socket |
| routines are… | closures inline | typed values | typed values | typed values, with a log sink |
| commands defined in | `main`'s match | `main`'s match | `dispatch()` | `Runtime::dispatch` |
| output | printed | printed | returned as `Reply` | returned as `Reply` |
| process model | one | one | one | one, or a daemon + clients |

The progression is monotone in one specific way: **the agent learns less and less
about the routines, and the routines learn less and less about where their output
goes.** That is what makes the final shape possible.

## The data flow in v0_04

```
             ┌──────────────────────────────────────────────┐
  stdin ────▶│  agent/src/main.rs    console front end       │
             │    · reads a line                             │
  TCP   ────▶│  agent_core/src/server.rs   socket front end  │
             │    · reads a JSON line, parses a Request      │
             └───────────────────────┬──────────────────────┘
                                     │  Request { token, cmd, since }
                                     ▼
                        ┌────────────────────────────┐
                        │  agent_core/src/runtime.rs │
                        │  Runtime::handle           │
                        │   · check the token        │
                        │   · acquire the gate       │
                        │   · match the command      │
                        └───────┬─────────────┬──────┘
                                │             │
                    spawn       │             │  query
                                ▼             ▼
                 ┌────────────────────┐  ┌──────────────────────┐
                 │ agent_core/state.rs│  │ formatting / events  │
                 │ Agent: task registry│ │ Reply { ok, output } │
                 └─────────┬──────────┘  └──────────┬───────────┘
                           │ owns                   │
                           ▼                        │
              ┌──────────────────────────┐          │
              │ routines (task threads)  │          │
              │  · poll the stop flag    │          │
              │  · write to the log sink │          │
              └──────────────────────────┘          │
                                                    ▼
                                     Response → console print
                                              → JSON line on the socket
```

The important property is that **the two arrows out of `Runtime` are the only
places anything happens**. `Runtime` never prints and never touches a socket; the
front ends never decide what a command means.

## Package and dependency graph

```
                        ┌──────────────────────┐
                        │   v0_04 workspace    │
                        └──────────────────────┘
   ┌──────────┐    ┌──────────────┐    ┌────────────────┐
   │  agent   │───▶│  agent_core  │───▶│   routines     │
   │ (binary) │    │  (library)   │    │  (library)     │
   └──────────┘    └──────────────┘    └───────┬────────┘
                                               │
                                    enigo, rand (the only external crates)
```

Reading the arrows as "depends on":

- `agent` produces **two** binaries, `agent` and `agentctl`, from one package.
- `agent_core` holds everything shared; it may not depend on `agent`, which is
  what keeps the front ends interchangeable.
- `routines` is the leaf: it knows about `enigo` and nothing else in this repo.
  A different program could depend on it unchanged.

v0_03's graph is the same shape without `agent_core`; the runtime sat in
`agent/src/main.rs`, and `agent_web` defined the closure type it needed.

## The three invariants

These hold in every version and are worth checking any change against.

**1. One implementation of each command.** The same `match`/`dispatch` answers the
console and every network front end. A command may not behave differently by
front door. If two front ends ever needed to differ, it would be a property of the
*transport* (a status code, a prompt) and never of the command.

**2. Routines never block the agent.** Each routine runs on its own thread, polls
an `AtomicBool` stop flag, and sleeps in short slices so a stop is noticed within
~20 ms. Nothing in the command path waits for a routine to finish.

**3. Everything shared across threads is `Arc<Mutex<…>>`,** acquired through one
`lock()` helper that recovers from poisoning rather than panicking.

## How a command travels

`agentctl repeat_enter 500` from a second terminal, end to end:

1. **CLI parsing** (`agentctl_cli.rs`) — the first positional word is the command,
   everything after it is joined verbatim into one command string. Global flags
   are collected from anywhere in the argument list.
2. **Discovery** (`session.rs` / `client.rs`) — find the session directory, read
   `session.json`, decide whether a daemon is `Running`, `NotRunning` or
   `Unusable`.
3. **Transport** (`client.rs`) — connect to loopback, send one JSON line
   containing the token, the command and the event cursor.
4. **Server** (`server.rs`) — accept, read one line, parse it into a `Request`.
5. **Serialisation point** (`Runtime::gate`) — acquire the process-wide command
   lock, so two terminals cannot interleave.
6. **Authorisation** (`runtime.rs`) — compare the token with the published one;
   reject and record if it does not match.
7. **Dispatch** (`runtime.rs`) — match the first word; `spawn` the routine.
8. **Registry** (`state.rs`) — choose an instance name under the same lock as the
   insert, start the thread, record the entry.
9. **Routine thread** (`routines/lib.rs`) — press keys, poll the flag, write to
   its log.
10. **Reply** — `Response` with `ok`/`output`, plus any events newer than the
    client's cursor, written back as one JSON line.
11. **Client output** — the answer on stdout, filtered events on stderr, an exit
    code that distinguishes "accepted", "refused" and "no daemon".

Steps 5 and 8 are the only places where two clients can affect each other, and
they are the two places that take the registry lock.

## Process and thread model

**Standalone mode** is one process and N+1 threads: the main thread reads stdin,
each running routine gets a thread.

**Daemon mode** is one daemon process plus M short-lived client processes:

```
  agent daemon
    ├── main thread: accepts connections, polls the shutdown flag
    ├── one thread per client connection (short-lived, one command each)
    ├── one thread per running routine
    └── one watcher thread that notices shutdown and stops tasks
```

Why this shape:

- **Thread per connection** is simple and bounded in practice by a local user's
  patience. The cost is that there is no hard cap; see the roadmap in
  [v0_04's guide](v0_04/README.md#5-protocol-and-robustness).
- **A single command gate** means the runtime's internals do not need to be
  reentrant. This is a deliberate trade of concurrency for reasoning.
- **Clients are stateless** (`agentctl` opens, asks, closes), so a client dying
  cannot leave the daemon holding a half-open session, and two terminals cannot
  interfere through shared client state.

## Files and state on disk

| path | written by | contains | lifetime |
|------|-----------|----------|----------|
| `<session-dir>/session.json` | daemon | port, pid, version, token | from startup to clean shutdown |
| `<session-dir>/daemon.log` | `agentctl daemon-start` | the detached daemon's output | appends across restarts |
| `target/` | cargo | build artefacts | ignored by git |

The session directory is **discovered** — `AGENT_SESSION_DIR`, then the per-user
state directory, then temp, then `.rust-agent` in the working directory — taking
the first candidate that accepts a real write. This matters because existence is
not writability, and a failure here is otherwise silent.

All other state (tasks, events, clients) is **in memory only**. Restarting the
daemon loses the task list, which is why nothing pretends to recover it.

## Why there are no dependencies

`v0_04` adds not one crate. Everything it needs is in `std`:

| need | standard-library answer | why not a crate |
|------|-------------------------|-----------------|
| HTTP server (v0_03) | `std::net::TcpListener` + hand-written parsing | the surface is four routes; a framework would be larger than the feature |
| JSON (v0_04) | hand-written serialiser and field extractor | the messages are flat, and the parser doubles as a teaching example |
| transport (v0_04) | loopback TCP | one transport that behaves the same on all three platforms |
| Ctrl-C handling | `SetConsoleCtrlHandler` / `signal` via `extern` | avoids a crate for ~40 lines, and demonstrates FFI honestly |
| random token | a seeded LCG over time/pid | the threat model is another local user, not a cryptanalyst |

The exception is `enigo` and `rand`, which the *routines* need and which cannot be
reimplemented sensibly in a few lines.

This is a deliberate stance, not a rule for its own sake: each of these was
weighed against readability, and the roadmap records where the stance is likely to
break first (TOML configuration, §4 of the v0_05 roadmap).

## Decision log

Decisions that shaped the code, with the reasoning, so a future change can
disagree with them on purpose.

| # | decision | reasoning | revisit when |
|---|----------|-----------|--------------|
| 1 | Behaviour becomes a trait object, not a closure | closures cannot be named, stored or dispatched on | never; this one stuck |
| 2 | Commands return `Reply` instead of printing | two front ends need the same answer, differently rendered | never |
| 3 | The web layer receives a boxed closure, not a reference to the agent | keeps `agent_web` independent of `agent`; no dependency cycle | if the web layer needs richer state access |
| 4 | Loopback-only binding, token in the session file | the API starts input routines; it must not be reachable from the network | if remote access is ever wanted — needs TLS first |
| 5 | A dialect of JSON written by hand | flat messages, zero dependencies, readable parser | if the protocol grows nested structures |
| 6 | The registry prunes finished tasks instead of tasks cleaning up after themselves | cannot be forgotten; derived state beats maintained state | never |
| 7 | `stop` accepts prefixes but refuses ambiguity | typing is easier; guessing is worse than failing | never |
| 8 | Session directory probed by writing, not by checking existence | existence does not imply writability (this caused a real bug) | never |
| 9 | Daemon is a user-session process, not an OS service | installing a service deserves explicit consent | when platform integration is documented in v0_05 |
| 10 | No dependencies in v0_04 | keeps the teaching surface visible and the build trivial | when TOML config lands |
