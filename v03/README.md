# Agent v03 — web-enabled

v03 keeps v02's console agent (a routine registry you start and stop from
stdin) and adds two things: a **browser subroutine** with a fallback chain, and
**basic web interactivity** over loopback HTTP.

```
v03/
  Cargo.toml            workspace (agent, routines, agent_web)
  routines/             the subroutines themselves
    src/lib.rs          Routine trait, repeat_enter, repeat_enter_jitter, open_browser
    src/launcher.rs     the browser fallback ladder
  agent_web/            loopback HTTP server + dashboard
    src/lib.rs          server, JSON API, command dispatch bridge
    src/dashboard.html  the dashboard (buttons, live status, polling)
  agent/                the binary
    src/main.rs         command dispatch, task registry, wiring
    src/console.rs      CLI options and banner
```

## `open_browser`

The subroutine opens a browser by walking a ladder and stopping at the first
rung that actually starts:

| order | rung | how |
|-------|------|-----|
| 1 | system default | `Start-Process <url>` on Windows, `open` on macOS, `xdg-open` on Linux |
| 2 | Google Chrome | `chrome.exe <url>` found under `%PROGRAMFILES%`, `%PROGRAMFILES(X86)%`, `%LOCALAPPDATA%`, or on `PATH` |
| 3 | Mozilla Firefox | `firefox.exe <url>` found under `%PROGRAMFILES%`, `%PROGRAMFILES(X86)%`, or on `PATH` |

Details that matter:

- **Landing page is Google.** With no argument the URL is
  `https://www.google.com`. A bare word like `rust async` becomes a Google
  search; `example.com` gets `https://` prepended.
- **Failures move down the ladder.** The default rung fails if the shell call
  reports an error; the direct rungs fail if the executable is missing or the
  process dies within ~1.2 s instead of staying alive. A rung that is skipped
  (browser not installed) or that failed is reported, so you can see exactly
  why the next browser was used.
- **Total failure is loud.** If every rung fails you get one line per attempt
  rather than a silent no-op.
- **One launch at a time.** A second launch request while one is in flight is
  refused, with `429` on the web side.

Inspect the ladder without launching anything:

```
browsers
```

## Commands

Console and web accept the same command strings.

```
help | ?                        this list
status                          uptime, tasks, browser state
list | tasks                    running tasks
browser [url]                   open default -> chrome -> firefox
open_browser [url]              subroutine form of the above
browsers                        show the fallback ladder
repeat_enter [ms]               press Enter on an interval
repeat_enter_jitter [base] [j]  press Enter with jitter
stop <task>                     stop one task
stopall                         stop every task
quit                            exit (console)
```

## Web interactivity

The agent starts a standard-library HTTP server on **127.0.0.1 only** — it can
start subroutines and open browsers on this machine, so it must not be
reachable from the network.

| Method | Path | Purpose |
|--------|------|---------|
| GET | `/` | dashboard: buttons, live task list, ladder, output pane |
| GET | `/api/status` | JSON: uptime, running tasks, ladder, spawn guard |
| GET | `/api/cmd?cmd=…` | run any command |
| POST | `/api/cmd` | same, with `cmd=…` in the body |
| GET | `/api/browser` | shorthand for `browser` |
| GET | `/api/help` | command list as text |
| OPTIONS | any | CORS preflight |

Replies are JSON: `{"ok":true,"cmd":"browsers","output":"…"}`. A rejected
command answers `400`; an overlapping browser launch answers `429`.

```powershell
curl.exe "http://127.0.0.1:8765/api/cmd?cmd=browser"
curl.exe "http://127.0.0.1:8765/api/status"
curl.exe -X POST -d "cmd=repeat_enter 500" http://127.0.0.1:8765/api/cmd
```

The dashboard polls `/api/status` every 2 s for the live task list and stays
honest about when a browser launch is in flight.

## Running it

```powershell
cd v03
cargo run                      # console + web on http://127.0.0.1:8765/
cargo run -- --port 9000       # pick the port
cargo run -- --open-web        # also open the dashboard in a browser
cargo run -- --no-web          # console only
cargo run -- --no-open         # refuse browser launches
cargo run -- --browser         # launch a browser at Google, then exit
cargo run -- --browser https://example.com
cargo build --release
cargo test
```

> This sandbox has no crates.io access, so builds here need `--offline` against
> the existing registry cache (`cargo build --offline`). v03 introduces no new
> dependencies — the HTTP server is `std::net` — so the cache is enough.

## Notes and limits

- The web server is plaintext HTTP on loopback with no authentication. That is
  deliberate for a local control panel; do not bind it to a routable address.
- The dashboard polls rather than using websockets; `GET /ws` returns `501` and
  says so.
- Request bodies are capped at 64 KiB, and each connection has a 5 s read/write
  budget. One thread per connection.
- `open_browser` runs once per invocation, so it is tracked and stoppable like
  any other task, but it is not a repeating loop.
