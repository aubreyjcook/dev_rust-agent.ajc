# v0_02 — splitting the program into two crates

> **Read [v0_01](../v0_01/rust-agent/README.md) first.** This version is a refactor, and a
> refactor only makes sense against the thing being refactored. If you know Rust
> and just want the summary: *the work stopped being a closure and became a value
> with a name.*
>
> For the Rust concepts themselves, see
> [Start here: Rust in this project](../README.md#start-here-rust-in-this-project).

---

## What changed, and why

`v0_02` runs the same four commands as v0_01 and looks the same to a user. The change
is entirely structural, and it comes from noticing one thing in v0_01's file.

In v0_01, starting a task looked like this:

```rust
// v0_01
start_task(&registry, "repeat_enter", move |stop| {
    repeat_enter_loop(ms, stop)
});
```

The **what** (`repeat_enter`), the **name** (`"repeat_enter"`), and the **how**
(`repeat_enter_loop`) are three separate things passed to one call. The `how` is
an anonymous closure: it has no type you can name, so it cannot be stored,
listed, or passed anywhere else.

In v0_02, the same call is:

```rust
// v0_02
start_task(&registry, Box::new(RepeatEnter { interval_ms: ms }));
```

The **what**, the **name**, and the **how** are now one value — a `RepeatEnter`
struct — and it answers questions about itself via a trait. Note that the name is
no longer passed at all: `start_task` asks the routine for it.

That is the entire version. Everything below is the consequences.

### The diff, function by function

| v0_01 | v0_02 | why |
|-----|-----|-----|
| `start_task<F>(reg, name, body)` | `start_task(reg, routine: BoxedRoutine)` | the work is a type, not a closure |
| name passed in as a `&str` | `let name = routine.name();` | the task knows its own name |
| `repeat_enter_loop(ms, stop)` free function | `impl Routine for RepeatEnter` | behaviour attached to its data |
| the `while slept < …` block, twice | `sleep_interruptible(ms, &stop)`, once | the duplication is now visible in one crate |
| one file, 202 lines | two files, 193 lines | fewer lines, more structure |
| `use rand::Rng;` mid-file | imports at the top of `routines/src/lib.rs` | tidiness the split made obvious |
| one package | a workspace with two packages | dependency versions pinned once |

### What v0_01 "wanted" but could not express

Three of v0_01's rough edges disappear on their own once the work is a value:

- **The banner can be honest.** Adding a routine is now two edits in two files
  that each know their own half: an `impl Routine` in the library, a `match` arm
  in the agent.
- **`TaskHandle` gained a reason to exist.** It still only wraps the stop flag —
  but now the registry key comes from the routine, so the map and the logs can
  never disagree about a task's name.
- **The duplicate sleep loop is gone**, replaced by one `pub fn` in the library
  that both routines call.

---

## The new shape

```
v0_02/
├── Cargo.toml            workspace: members = ["agent", "routines"]
├── agent/
│   ├── Cargo.toml        depends on routines
│   └── src/main.rs       decide and track      (114 lines)
└── routines/
    ├── Cargo.toml        depends on enigo, rand
    └── src/lib.rs        do the work           (79 lines)
```

The dependency arrow points one way:

```
   agent  ──depends on──▶  routines
   (commands,               (Routine trait,
    registry, threads)       keystroke loops)
```

`routines` does not know `agent` exists. That is deliberate: the library is the
reusable part, and the binary is the shell around it.

### Why not the other order?

You might wonder why the *behaviour* went into the library rather than, say, the
registry or the CLI. The test is: **which part would you want to reuse?** The
keystroke-sending routines are plausible building blocks for another program. The
command loop, banner and registry are the shape of *this* program. Behaviour is
reusable; presentation is not.

---

## Reading `routines/src/lib.rs`

This file is the heart of the version. Read it top to bottom.

### 1. The interface

```rust
pub trait Routine: Send + 'static {
    fn name(&self) -> &'static str;
    fn run(self: Box<Self>, stop: Arc<AtomicBool>);
}

pub type BoxedRoutine = Box<dyn Routine>;
```

Line by line:

- `pub trait Routine: Send + 'static` — a trait is an interface other types can
  implement. The `: Send + 'static` is a **supertrait bound**: every implementor
  must also be `Send` and `'static`. The agent needs those anyway, because it
  moves routines onto threads; stating them here means `start_task` does not have
  to.
- `fn name(&self) -> &'static str` — `&self` means "borrows the routine"; the
  return type is a string literal that lives for the whole program. That is why
  implementations can just write `"repeat_enter"` with no allocation.
- `fn run(self: Box<Self>, …)` — this is the one that looks strange. Normally a
  method that consumes the value is `fn run(self)`. But through `Box<dyn
  Routine>` the compiler does not have a sized value to move; you are holding a
  box. `Box<Self>` says "take the box, and with it the value inside".
- `pub type BoxedRoutine = Box<dyn Routine>;` — a nickname for the boxed trait
  object, so signatures read `BoxedRoutine` instead of `Box<dyn Routine>`.

`dyn Routine` is a **trait object**: the concrete type is decided at runtime, and
the box provides the indirection that makes that possible. This is the price of
being able to put a `RepeatEnter` and an `OpenBrowser` in the same `Vec` — and it
is a price worth paying, as you will see in [v0_03](../v0_03/README.md).

### 2. A routine is a struct plus an impl

```rust
pub struct RepeatEnter {
    pub interval_ms: u64,
}

impl Routine for RepeatEnter {
    fn name(&self) -> &'static str { "repeat_enter" }

    fn run(self: Box<Self>, stop: Arc<AtomicBool>) {
        let mut enigo = match Enigo::new(&Settings::default()) {
            Ok(e) => e,
            Err(e) => { eprintln!("[repeat_enter] Enigo init failed: {e}"); return; }
        };

        while !stop.load(Ordering::Relaxed) {
            if let Err(e) = enigo.key(Key::Return, Direction::Click) {
                eprintln!("[repeat_enter] key error: {e}");
            }
            sleep_interruptible(self.interval_ms, &stop);
        }
        println!("[repeat_enter] stopped");
    }
}
```

Four things to notice:

- **The struct is the configuration.** `interval_ms` used to be a parameter
  threaded through a free function; now it is a field, owned by the routine for
  as long as the routine runs.
- **`pub` on the field, not just the struct.** The `agent` crate constructs
  `RepeatEnter { interval_ms: ms }` from *outside* this crate, so the field must
  be public. Forgetting this produces `error[E0451]: field 'interval_ms' of
  struct 'RepeatEnter' is private`, which is a good error to see once.
- **The match handles the failure and returns.** `Enigo::new` can fail (no
  display, no permission); the routine reports it and gives up rather than
  panicking on a background thread.
- **The loop is identical to v0_01's**, except the sleep is now a call.

### 3. The extracted helper

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

Note the signature takes `&AtomicBool`, not `Arc<AtomicBool>`. The caller owns the
`Arc`; this function only needs to *look* at the flag, so it borrows. Preferring
the narrowest type a function needs is idiomatic Rust, and it is why callers
write `sleep_interruptible(self.interval_ms, &stop)` with an ampersand.

### 4. The jitter routine, and a casting lesson

```rust
let delta: i64 = if self.jitter_ms == 0 {
    0
} else {
    rng.gen_range(-(self.jitter_ms as i64)..=(self.jitter_ms as i64))
};
let sleep_ms = ((self.base_ms as i64) + delta).max(1) as u64;
```

`u64` cannot represent a negative number, so "± jitter" cannot be computed in
`u64`. The code casts to `i64`, does the signed arithmetic, and `.max(1)` clamps
away zero and negatives before casting back to `u64` — because
`thread::sleep(0)` would busy-loop. `as` casts are unchecked in Rust: they
truncate silently, which is why the clamp matters.

---

## Reading `agent/src/main.rs`

### The dependency, and the new argument type

```rust
use routines::{BoxedRoutine, RepeatEnter, RepeatEnterJitter};

/// The agent's entire job: spawn, track, stop.
fn start_task(registry: &Registry, routine: BoxedRoutine) {
    let name = routine.name();
    …
}
```

Compare with v0_01's five lines of bounds. The agent now needs to know **nothing**
about `Send`, `'static`, or keystrokes — those constraints live on the trait, and
the concrete types only appear where they are constructed.

### The registry key changed type, for a reason

```rust
type Registry = Arc<Mutex<HashMap<&'static str, TaskHandle>>>;
```

v0_01 used `String`. v0_02 uses `&'static str`, matching `Routine::name`'s return
type. Because the key is a string literal known at compile time, no allocation is
needed to look a task up — and the compiler now guarantees the name lives long
enough to be a key.

### The dispatch arms got readable

```rust
"repeat_enter" => {
    let ms = args.get(0).and_then(|s| s.parse().ok()).unwrap_or(1000);
    start_task(&registry, Box::new(RepeatEnter { interval_ms: ms }));
}
```

There is no name to pass and no closure to write: construct the struct, box it,
hand it over. Adding a routine adds a struct field worth of configuration and one
line here — and nothing else anywhere.

### The cleanup line is still duplicated

```rust
thread::spawn(move || {
    routine.run(stop);
    let mut reg = registry_clone.lock().unwrap();
    reg.remove(&name);                       // ← still here
    println!("[task '{name}' finished]");
});
```

v0_02 did **not** fix v0_01's pressure #5. The cleanup still lives inside the spawn
block. It is now easier to see as a wart, because everything around it got
cleaner — and it is still there in v0_03. Look for it to disappear in v0_04.

---

## Why this is the right refactor

Three payoffs, in increasing order of importance:

1. **Adding a routine no longer touches the agent's logic.** You write a struct
   and an `impl` in the library, then one arm in the `match`.
2. **The agent can report on things it does not understand.** `list` prints
   `routine.name()` without knowing whether it is looking at a keystroke presser
   or (in v0_03) a browser launcher.
3. **The library is now testable without running the program.** This version does
   not take advantage of it — v0_02 has no tests at all — but the seam is what
   makes [v0_03](../v0_03/README.md) and [v0_04](../v0_04/README.md) testable.

Point 3 is the deep one. A `#[cfg(test)] mod tests` in a library crate is
compiled and run by `cargo test` without any console, any keyboard, or any
window. That is only possible because the behaviour was extracted into a crate
whose functions can be called directly.

---

## The workspace, and `.workspace = true`

`v0_02/Cargo.toml` is not a package; it is a workspace, a container for packages:

```toml
[workspace]
resolver = "2"
members = ["agent", "routines"]

[workspace.package]          # values members may inherit
edition = "2021"
version = "0.1.0"
license = "MIT"

[workspace.dependencies]     # versions members may borrow
enigo = "0.2"
rand = "0.8"
```

A member opts in with `.workspace = true`:

```toml
# routines/Cargo.toml
[dependencies]
enigo.workspace = true   # "whatever the workspace pinned"
rand.workspace = true
```

Two consequences worth knowing:

- **One `target/` directory and one `Cargo.lock`** at the workspace root. Two
  packages, one build cache.
- **`cargo build` at the root builds every member.** To run just one, use
  `cargo run -p agent` — `-p` selects a package in a workspace. You will need
  that flag in v0_03 and v0_04, which have more packages.

---

## Why `.lock().unwrap()` is still here (and what changes)

v0_02 keeps v0_01's habit:

```rust
let mut reg = registry.lock().unwrap();
```

Because a mutex becomes **poisoned** when a thread panics while holding it, this
`.unwrap()` means "if any task ever panicked mid-update, crash the agent on the
next `list`". For a program with no tests, no long uptime and one user, that is a
defensible trade.

[v0_03](../v0_03/README.md) and [v0_04](../v0_04/README.md) decide differently and
ship a helper instead:

```rust
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}
```

`into_inner()` recovers the data from the poisoned guard. The reasoning: a
partially-updated registry is more useful than a crashed agent, and "the other
thread panicked" is not a reason to refuse to work.

---

## Build and run

```bash
cd v0_02
cargo build
cargo run -p agent          # -p: pick the package in the workspace
```

The commands are identical to v0_01:

```
repeat_enter 500              press Enter every 500 ms
repeat_enter_jitter 1000 100  press Enter every 1000 ms, ± 100 ms
list                          what is running
stop repeat_enter             ask it to stop
quit                          exit
```

> ⚠️ Still real keystrokes into the focused window. Same caution as v0_01: use a
> long interval, keep the terminal focused, `stop` before switching windows.

### Seeing the refactor

The commands behave identically, so there is nothing to *watch*. To see the
change, read the two dispatch arms side by side — v0_01's
[line 46](../v0_01/rust-agent/src/main.rs#L51) and v0_02's
[line 41](agent/src/main.rs#L93) — and notice that the second one is shorter while
saying more.

There are still no tests, and still no command-line options.

---

## Try changing it

1. **Add a routine with no keystrokes at all.** In `routines/src/lib.rs`, write:

   ```rust
   pub struct Sleep { pub ms: u64 }

   impl Routine for Sleep {
       fn name(&self) -> &'static str { "sleep" }
       fn run(self: Box<Self>, stop: Arc<AtomicBool>) {
           sleep_interruptible(self.ms, &stop);
       }
   }
   ```

   Then add one arm in `agent/src/main.rs`. Count how many places you had to
   touch — and compare with how many v0_01 would have needed.

2. **Make the compiler complain about visibility.** Remove `pub` from
   `RepeatEnter::interval_ms` and build. Read error `E0451` carefully; it is one
   of Rust's clearer messages.

3. **Try to store two different routines in one `Vec`.** `vec![Box::new(RepeatEnter
   { interval_ms: 1 }) as BoxedRoutine, Box::new(Sleep { ms: 1 }) as BoxedRoutine]`
   compiles; `vec![RepeatEnter { … }, Sleep { … }]` does not. This is the concrete
   reason trait objects exist.

4. **Add the missing test.** `sleep_interruptible` is trivial to test without a
   keyboard:

   ```rust
   #[test]
   fn stops_early() {
       let stop = AtomicBool::new(true);
       let start = std::time::Instant::now();
       sleep_interruptible(10_000, &stop);
       assert!(start.elapsed() < Duration::from_millis(500));
   }
   ```

   You have just written the first test for this project. (This test ships in
   [v0_03](../v0_03/routines/src/lib.rs).)

5. **Break the supertrait bound.** Change `pub trait Routine: Send + 'static` to
   `pub trait Routine` and see where the errors land. They will point at
   `thread::spawn` in the agent, not at the trait — which is a good lesson in how
   bounds propagate to the point of use.

When the shapes here feel natural, go to [**v0_03**](../v0_03/README.md), where the
program grows a second front end and a routine that talks to the operating
system.

---

## Roadmap for v0_03

This version is finished. Here is what the next one will change.

**The pressure.** Two things now stick out:

- **One front end.** `dispatch` lives in `main` and produces nothing but printed
  output, so anything else that wanted to run a command would have to duplicate
  the `match`. The program is one interface away from being drivable from
  somewhere other than its own console.
- **Nothing but keystrokes.** Every routine so far presses keys. There is no
  routine that talks to the *operating system*, and none that can fail in a way
  worth reporting — which means no experience with `Result`-returning work, or
  with falling back between alternatives.

**The plan.**

1. **Add a browser routine** that opens a URL by trying the system default
   browser, then Chrome, then Firefox, stopping at the first that actually
   starts. It introduces two ideas worth a version of their own: a *ladder* of
   candidates held as data, and "success" defined as more than "spawn returned
   `Ok`".
2. **Return replies instead of printing them.** Change `dispatch` to
   `fn dispatch(agent, input) -> Reply`, with `Reply { ok, output }`.
3. **Add a second front end**: a standard-library HTTP server on loopback,
   serving a dashboard and a JSON API. It cannot depend on the binary, so the
   agent hands it a *boxed closure* — `Dispatch` — and the web crate never learns
   what a command is.
4. **Make the ladder inspectable** with a `browsers` command, possible only
   because deciding (a pure `Vec`) is separate from acting.
5. **Guard the one dangerous command**: at most one browser launch in flight.

**What stays the same.** The `Routine` trait, `sleep_interruptible`, the registry
and the stop flag. Both new front ends must call the same `dispatch` — if they
ever diverge, the version has failed.

**Not planned yet.** Tests beyond the two in `routines`, the `stop`-by-name bug
this version introduces, and any thought of running outside this process.
