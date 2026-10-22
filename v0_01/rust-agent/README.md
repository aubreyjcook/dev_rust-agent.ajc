# v01 — one file that types for you

> **New here?** Read the [root README](../../README.md) first, especially
> [Start here: Rust in this project](../../README.md#start-here-rust-in-this-project).
> It covers the Rust ideas this version uses.
>
> **This is the first version**, so there is no "what changed" section. If you
> are reading the versions in order, start here. If you already know Rust and
> want to see the interesting refactors, jump to [v02](../../v02/README.md).

---

## The program

`v01` is a console program that presses the <kbd>Enter</kbd> key for you, on a
timer, and lets you start and stop that behaviour while it runs:

```
=== Agent ===
Commands:
  repeat_enter [ms]  - press Enter on interval
  stop <name>        - stop a running task
  list               - show running tasks
  quit               - exit

repeat_enter 500
▶ started task 'repeat_enter'
list
- repeat_enter
stop repeat_enter
⏸ stopping task 'repeat_enter'
[repeat_enter] stopped
[task 'repeat_enter' finished]
```

That is the whole program: **202 lines in one file**, `src/main.rs`. Everything —
the command loop, the task registry, the keystroke sending — lives there.

It is small enough to read in one sitting, and it already contains most of the
ideas the later versions build on. That is why it is worth reading slowly.

## The one idea to take away

Two things happen at once, and keeping them apart is the whole design:

1. **You keep typing.** The main thread sits in a loop reading lines from stdin.
2. **A task keeps pressing Enter.** A *second* thread runs the `repeat_enter`
   loop.

They talk through one shared value: an `AtomicBool` called the **stop flag**. The
command thread sets it to `true`; the task thread checks it in its loop and exits
when it sees `true`.

```
   main thread                          task thread
   ───────────                          ───────────
   read "repeat_enter 500"
   create stop = AtomicBool(false)
   spawn ──────────────────────────────▶ loop {
                                            press Enter
   read "stop repeat_enter"                 sleep 20ms
   stop.store(true)                         if stop.load() { break }
                                          }
```

Everything else in this file is bookkeeping around that picture.

---

## Reading `src/main.rs`

The file is laid out in this order, and it reads best top to bottom:

| lines | what |
|-------|------|
| [1–7](../../v01/rust-agent/src/main.rs#L1-L7) | imports |
| [9–15](../../v01/rust-agent/src/main.rs#L9-L15) | the two type definitions: one task, and the map of tasks |
| [17–77](../../v01/rust-agent/src/main.rs#L17-L77) | `main`: the banner and the command loop |
| [79–106](../../v01/rust-agent/src/main.rs#L79-L106) | `start_task`: register a task, run it on a thread |
| [108–117](../../v01/rust-agent/src/main.rs#L108-L117) | `stop_task`: find a task, set its flag |
| [119–128](../../v01/rust-agent/src/main.rs#L119-L128) | `list_tasks`: print the names |
| [130–135](../../v01/rust-agent/src/main.rs#L130-L135) | `stop_all`: set every flag, for shutdown |
| [139–202](../../v01/rust-agent/src/main.rs#L139-L202) | the two keystroke loops |

### The pieces

**A task is just a stop flag.** There is nothing else to remember about a running
task, because the *behaviour* lives in its thread, not in the map:

```rust
/// A handle to a running task, used to stop it later.
struct TaskHandle {
    stop: Arc<AtomicBool>,
}
```

Note that `TaskHandle` never actually appears in any function signature — see
[Things a learner should notice](#things-a-learner-should-notice).

**The map is shared between threads**, so it needs the `Arc<Mutex<…>>` pair:

```rust
type Registry = Arc<Mutex<HashMap<String, TaskHandle>>>;
```

A `type` alias like this is not a new type; it is a nickname. `Registry` and
`Arc<Mutex<HashMap<String, TaskHandle>>>` are the same thing to the compiler, but
the nickname keeps the function signatures readable.

**Reading a line and splitting it** is the whole front end:

```rust
let mut parts = line.trim().split_whitespace();
let cmd = parts.next().unwrap_or("");
let args: Vec<String> = parts.map(|s| s.to_string()).collect();
```

`split_whitespace` hands back an iterator *by reference into `line`*, so the
arguments have to be converted into owned `String`s before `line` goes out of
scope at the end of the loop body. This is the borrow checker making you think
about lifetimes for the first time, and it is why `.map(|s| s.to_string())`
is there.

**Parsing an optional number** is one chained expression rather than nested
`if`s:

```rust
let ms = args
    .get(0)                                  // Option<&String>
    .and_then(|s| s.parse::<u64>().ok())     // Option<u64>: parse, discarding the error
    .unwrap_or(1000);                        // fall back to the default
```

`.ok()` converts `Result<u64, ParseIntError>` into `Option<u64>` — "I do not care
*why* it failed, only that it did". This exact chain appears in every later
version.

**Starting a task** is the interesting function, because it takes the work as a
parameter:

```rust
fn start_task<F>(registry: &Registry, name: &str, body: F)
where
    F: FnOnce(Arc<AtomicBool>) -> () + Send + 'static,
```

- `F` is a generic type parameter: the caller picks it.
- The `where` clause constrains it. `FnOnce(...)` means "a closure I can call
  once with this argument". `Send` means "safe to move to another thread".
  `'static` means "owns everything it uses; it cannot borrow from the caller".

The body of `start_task` then does three things in a careful order:

```rust
let mut reg = registry.lock().unwrap();
if reg.contains_key(name) { … return; }              // 1. refuse duplicates
let stop = Arc::new(AtomicBool::new(false));
reg.insert(name.to_string(), TaskHandle { stop: Arc::clone(&stop) });
drop(reg);                                           // 2. release the lock

let registry_clone = Arc::clone(registry);
thread::spawn(move || {
    body(stop);                                      // 3. run the work
    let mut reg = registry_clone.lock().unwrap();
    reg.remove(&name_owned);                         //    then clean up
    println!("[task '{name_owned}' finished]");
});
```

Two details worth understanding rather than skimming:

- **`drop(reg)` is deliberate.** The lock is released *before* spawning. If the
  lock were still held, and the new thread tried to take it, the two could
  deadlock.
- **`Arc::clone(&stop)` is not a deep copy.** It creates a second handle to the
  *same* flag. One handle goes into the map (so `stop` can reach it); the other
  is moved into the thread (so the loop can read it). This is exactly why `Arc`
  exists.
- **The thread cleans up after itself.** When `body(stop)` returns, the thread
  removes its own entry from the map. That is why `list` is accurate without any
  polling or supervision thread.

**The keystroke loop** is short but does one subtle thing:

```rust
while !stop.load(Ordering::Relaxed) {
    if let Err(e) = enigo.key(Key::Return, Direction::Click) {
        eprintln!("[repeat_enter] key press error: {e}");
    }
    // Sleep in small chunks so we react to `stop` quickly.
    let mut slept = 0;
    while slept < interval_ms && !stop.load(Ordering::Relaxed) {
        let chunk = 20.min(interval_ms - slept);
        thread::sleep(Duration::from_millis(chunk));
        slept += chunk;
    }
}
```

If it slept `interval_ms` in one call, `stop repeat_enter` would appear to do
nothing for up to a second. Sleeping in 20 ms slices and re-checking the flag
means a stop is noticed within ~20 ms. Note also that the error from `enigo` is
*reported and ignored* — a failed keypress is not a reason to abandon the task.

**Stopping** is a lookup and a store:

```rust
match reg.get(name) {
    Some(handle) => {
        handle.stop.store(true, Ordering::Relaxed);
        println!("⏸ stopping task '{name}'");
    }
    None => println!("no such running task: '{name}'"),
}
```

Note the wording: it has been *asked* to stop. The actual stopping happens on the
task's own thread, a few milliseconds later.

---

## Rust concepts, in the order you meet them

| concept | where in this file | what to notice |
|---------|-------------------|----------------|
| `struct` | `TaskHandle` | a struct with one field is still worth naming |
| `type` alias | `Registry` | a nickname, not a new type |
| `HashMap` | the registry | `insert`, `get`, `keys`, `values`, `contains_key` |
| `Arc<T>` | the stop flag and the registry | shared ownership between threads |
| `Mutex<T>` + `.lock()` | every registry access | one thread at a time |
| `AtomicBool` | the stop flag | shared mutable value without a lock |
| iterator → collection | `parts.map(…).collect()` | `collect()` needs a type annotation |
| `Option` chain | parsing `ms` | `get` → `and_then` → `unwrap_or` |
| `match` on `Result` | reading a line | handle the error, do not panic |
| closures | the `body` argument | `move` takes ownership |
| generic + bounds | `start_task<F>` | the caller chooses the behaviour |
| `thread::spawn` | `start_task` | the closure must be `Send + 'static` |
| `if let Err(e)` | the keystroke loop | act on failure, continue on success |

### Why `.lock().unwrap()` and not something gentler?

`.unwrap()` panics. Here that means: *if the registry mutex is poisoned, crash.*
A mutex becomes poisoned when a thread panics while holding it, so a panic in one
task could take down the whole agent on the next `list`. It is a reasonable
choice for a small program, and later versions decide differently — see
[v02's note on lock poisoning](../../v02/README.md#why-lockunwrap-is-still-here-and-what-changes).

---

## Things a learner should notice

These are not mistakes to fix before reading on. They are the *pressure* that
causes v02, and noticing them is the point.

1. **The banner lies.** The printed command list at
   [lines 20–26](../../v01/rust-agent/src/main.rs#L20-L26) mentions four
   commands, but the dispatcher also accepts `repeat_enter_jitter` and `exit`.
   Adding a command means updating two places that do not know about each other.

2. **`TaskHandle` is never named.** It is defined at line 10 and constructed at
   line 91, but no function takes or returns one, and the struct adds nothing to
   the `Arc<AtomicBool>` it wraps. It is a placeholder for a name that will
   matter later.

3. **The behaviour cannot be stored.** Look at what the registry holds: a flag.
   The *work* — press Enter every N ms — exists only inside a closure passed at
   the call site. So the program cannot answer questions like "what kind of task
   is this?" or "show me its parameters". It can only print the name it was
   given.

4. **The name is passed twice.** `start_task(&registry, "repeat_enter", …)` uses
   the string `"repeat_enter"` both as the map key and as the log prefix. If the
   two ever disagreed, the bug would be silent.

5. **Cleanup is duplicated.** The `reg.remove(&name_owned)` line lives inside the
   spawned closure. Every future kind of task would need to remember to repeat
   it.

6. **The sleep loop is copy-pasted twice.** `repeat_enter_loop` and
   `repeat_enter_jitter_loop` each contain their own identical `while slept <
   …` block. The `sleep_interruptible` function that fixes this appears in v02.

7. **`use rand::Rng;` sits in the middle of the file**, at
   [line 164](../../v01/rust-agent/src/main.rs#L164), just above the function
   that needs it. Legal Rust, unusual style; later versions put imports at the
   top.

Each of these is a small thing. Together they are the argument for v02:
**give the work a name, and give it a place to live.**

---

## Build and run

```bash
cd v01/rust-agent
cargo build
cargo run
```

Then type commands:

```
repeat_enter 500              press Enter every 500 ms
repeat_enter_jitter 1000 100  press Enter every 1000 ms, ± 100 ms
list                          what is running
stop repeat_enter             ask it to stop
quit                          exit
```

There are no tests in this version, and no command-line options — `main` takes no
arguments.

> ⚠️ **This presses real keys on your machine.** They go to whatever window has
> focus. Use a long interval (1000 ms or more) while experimenting, keep the
> terminal focused, and `stop` the task before switching to another window. If a
> task gets away from you, `quit` stops everything on the way out.

### Requirements

This version uses the **2024 edition** of Rust, which needs a recent toolchain
(roughly 1.85+). If `cargo build` complains about the edition, either

```bash
rustup update
```

or start with [v02](../../v02/README.md), which uses the 2021 edition and builds on
older compilers.

---

## Try changing it

The fastest way to learn from this file is to break it deliberately. In rough
order of difficulty:

1. **Add a command that does nothing.** Add `"ping" => println!("pong"),` to the
   `match` at [line 39](../../v01/rust-agent/src/main.rs#L39). Notice you also had
   to think about the banner at line 20 — that is pressure #1 above.

2. **Make the banner honest.** Replace the three `println!` calls with a single
   string, and see how you feel about keeping it in sync by hand.

3. **Change the stop latency.** Replace the inner chunked sleep with one
   `thread::sleep(Duration::from_millis(interval_ms))`, then run
   `repeat_enter 5000` and try to stop it. Feel the delay. Put it back.

4. **Break the deduplication.** Delete the `if reg.contains_key(name)` check and
   start `repeat_enter` twice. You now have two threads both registered under
   `"repeat_enter"`, and `stop` can only reach one of them. This is why v04 gives
   each task an instance name like `repeat_enter#2`.

5. **Extract the sleep loop.** Pull the `while slept < …` block out of
   `repeat_enter_loop` into a function `sleep_interruptible(ms, &stop)`, and call
   it from both loops. You have just written the function that v02 ships.

6. **Give the task a name it knows.** Add a `kind: &'static str` field to
   `TaskHandle` and store `"repeat_enter"`. Now `list` can print the kind without
   you passing it. You are one step away from the `Routine` trait.

When you have done a few of these — or when you would rather read the refactor
than invent it — go to [**v02**](../../v02/README.md), where the work becomes a
value with a name.
