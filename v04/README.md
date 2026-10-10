# Agent v04 — standalone or daemon

v04 takes v03's agent and gives it two ways to run:

| mode | how it starts | who can drive it |
|------|---------------|------------------|
| **standalone** | `agent` / `agent run` | only the console it was started in — v03's behaviour, unchanged |
| **daemon** | `agent daemon` | **any terminal on the machine**, via `agentctl` |

Both modes run the same code. The commands, the task registry, the browser
fallback ladder and the output formatting all live in one place
(`agent_core::Runtime`), so the two modes cannot drift apart.

```
v04/
  Cargo.toml            workspace (agent, routines, agent_core)
  routines/             the subroutines themselves
    src/lib.rs          Routine trait (name + run with a Log sink), repeat_enter, open_browser
    src/launcher.rs     the browser fallback ladder
  agent_core/           everything shared by both modes
    src/runtime.rs      command semantics: what the agent can do
    src/state.rs        the task registry
    src/server.rs       the daemon's control socket
    src/client.rs       the client end of that socket
    src/session.rs      how a client finds a daemon
    src/protocol.rs     newline-delimited JSON over loopback TCP
  agent/                the binary
    src/main.rs         mode selection and both front ends
    src/console.rs      agent CLI parsing
    src/bin/agentctl.rs the remote-control client
    src/agentctl_cli.rs agentctl CLI parsing
    tests/daemon.rs     end-to-end tests that run both real binaries
```

## Starting it

```powershell
cd v04
cargo build

# Standalone (v03 behaviour): a process you type into and quit.
.\target\debug\agent.exe
.\target\debug\agent.exe run --no-open

# Daemon: stays up, serves every terminal on this machine.
.\target\debug\agent.exe daemon
.\target\debug\agent.exe daemon --port 46000 --session-dir C:\tmp\agent
```

The daemon prints where its session file is, then waits. Its commands are the
same ones the standalone console accepts.

## Driving a daemon from any terminal

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

Exit status: `0` accepted, `1` refused by the daemon, `2` no daemon or bad
usage — so `agentctl` composes in scripts.

### Commands

Identical in both modes, over both front ends:

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
one `repeat_*` routine is running — and refuses with the candidate list when it
is ambiguous.

## How the two processes find each other

1. The daemon binds a **loopback-only TCP socket** (port 45917, walking upwards
   if taken; ephemeral as a last resort).
2. It writes `session.json` — port, PID, version, and a random 32-hex **token** —
   into its session directory.
3. `agentctl` reads that file, connects, and sends the token with every request.
4. On shutdown the daemon removes the file, so a later `agentctl` says "no
   daemon" instead of chasing a dead port.

The session directory is **discovered, never assumed**: `AGENT_SESSION_DIR`,
then the per-user state directory (`%LOCALAPPDATA%\rust-agent` on Windows,
`~/.local/state/rust-agent` on Linux), then the temp directory, then
`.rust-agent` in the working directory. The first candidate that *accepts a
write* wins — a directory that exists but is not writable is worse than none.
`agentctl session` reports which one was chosen and why.

Override both sides for an isolated instance:

```powershell
$env:AGENT_SESSION_DIR = "C:\tmp\agent-a"   # or --session-dir on either binary
```

### Protocol

Newline-delimited JSON over TCP, one request line and one response line:

```json
{"token":"<hex>","cmd":"repeat_enter 500","since":41}
{"ok":true,"output":"started task 'repeat_enter'","shutdown":false,"version":"0.4.0","events":[...]}
```

A `since` cursor makes the event backlog incremental, so a client that was away
catches up on what it missed without replaying everything. Requests are
serialized through one gate, so two terminals issuing commands at the same
instant are ordered rather than racing.

### Security boundary

The socket is loopback-only by construction, and every request carries the token
from the session file — so **another user on the same machine cannot drive your
agent**. A wrong token is refused and recorded as an event.

This is *not* a system service: it runs with your privileges, in your session,
with your `PATH` and browser profile. That is the point — `agentctl browser`
opens a browser as you, in your desktop session. There is no elevation, no
service registration, and no Tls; do not expose the port.

## What changed from v03

- **Modes.** One binary, two run modes; `agentctl` added for remote control.
- **Routines log instead of printing.** `Routine::run` now takes a `Log` sink,
  so a routine's output lands in its task's buffer where any client can read it,
  rather than only in the daemon's stdout.
- **Tasks prune themselves.** A routine that finishes on its own (like
  `open_browser`) is removed from the registry, so `list` never shows dead tasks.
- **`stop` resolves prefixes**, and refuses ambiguity instead of guessing.
- **No new dependencies.** `std::net` for the socket, hand-rolled JSON, and a
  hand-rolled `SetConsoleCtrlHandler`/`signal` binding for Ctrl-C.

## Testing

```powershell
cargo test
```

53 tests: unit tests for the protocol, session file, registry and both CLIs,
plus `agent/tests/daemon.rs`, which runs the **real binaries** — a daemon
process driven by `agentctl` — covering session publication, cross-client task
visibility, refusal exit codes, stale session files, `daemon-stop` cleanup, and
that standalone mode publishes nothing.

> This sandbox has no crates.io access, so builds here need `--offline` against
> the existing registry cache (`cargo build --offline`). v04 introduces no new
> dependencies, so that cache is enough.

## Notes and limits

- `agentctl daemon-start` starts the daemon **detached**, with its output in
  `<session-dir>/daemon.log`. `agent daemon` in a terminal of its own is still
  the clearest way to watch it.
- Ctrl-C stops the daemon and asks running tasks to stop, waiting up to 5s; a
  task that ignores its stop flag is left behind rather than blocking exit.
- One thread per client connection, with a 30s socket budget and a 16 KiB
  request cap.
- The daemon is meant to run as you, for you. It keeps running until it is
  stopped, so `agentctl daemon-stop` (or Ctrl-C in its console) is the way out.
