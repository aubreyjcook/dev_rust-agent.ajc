//! The task registry: what is running, and how to stop it.
//!
//! A *task* is a running routine. In v03 the routine ran on a thread the agent
//! started and the agent's stdout was the only place its messages appeared.
//! In v04 the daemon has many clients, so each task captures its own recent
//! output instead of printing into the void.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use routines::{BoxedRoutine, Log, LogHandle};

/// How much output to keep per task.
const OUTPUT_LINES: usize = 100;

/// Why a task could not be started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StartError {
    /// A task with this instance name already exists.
    Duplicate(String),
}

impl std::fmt::Display for StartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StartError::Duplicate(name) => write!(f, "task '{name}' is already running"),
        }
    }
}

impl std::error::Error for StartError {}

/// Why a typed task name did not identify exactly one task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// Nothing matches.
    NotFound(String),
    /// A prefix matched more than one task.
    Ambiguous(String, Vec<String>),
}

impl std::fmt::Display for ResolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ResolveError::NotFound(name) => write!(f, "no such running task: '{name}'"),
            ResolveError::Ambiguous(name, matches) => write!(
                f,
                "'{name}' matches {} tasks: {}",
                matches.len(),
                matches.join(", ")
            ),
        }
    }
}

impl std::error::Error for ResolveError {}

/// Output sink handed to a running routine.
///
/// Implements [`routines::Log`], so a routine reports into the task's own
/// buffer and any client can read it back later.
#[derive(Clone, Default)]
pub struct TaskLog {
    lines: Arc<Mutex<VecDeque<String>>>,
}

impl TaskLog {
    pub fn push(&self, line: impl Into<String>) {
        let mut lines = lock(&self.lines);
        if lines.len() == OUTPUT_LINES {
            lines.pop_front();
        }
        lines.push_back(line.into());
    }

    pub fn recent(&self, limit: usize) -> Vec<String> {
        let lines = lock(&self.lines);
        let skip = lines.len().saturating_sub(limit);
        lines.iter().skip(skip).cloned().collect()
    }

    pub fn len(&self) -> usize {
        lock(&self.lines).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// A handle suitable for handing to a routine.
    pub fn handle(&self) -> LogHandle {
        Arc::new(self.clone())
    }
}

impl Log for TaskLog {
    fn line(&self, text: &str) {
        self.push(text);
    }
}

/// A running task.
struct Task {
    stop: Arc<AtomicBool>,
    /// Registered name: the routine's name, suffixed if that name is taken.
    /// This is what `stop <name>` matches.
    name: String,
    /// Human-facing description carrying the parameters.
    label: String,
    started: Instant,
    handle: Option<std::thread::JoinHandle<()>>,
    log: TaskLog,
}

impl Task {
    /// Has the routine's thread finished?
    ///
    /// A routine that completes on its own — `open_browser` does — leaves a
    /// finished thread behind, and the registry has to notice or `list` would
    /// keep showing dead tasks forever.
    fn finished(&self) -> bool {
        self.handle.as_ref().map(|h| h.is_finished()).unwrap_or(true)
    }

    fn info(&self) -> TaskInfo {
        TaskInfo {
            name: self.name.clone(),
            label: self.label.clone(),
            uptime_secs: self.started.elapsed().as_secs(),
            output: self.log.recent(10),
        }
    }
}

/// A snapshot of one running task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskInfo {
    /// Registered name, for `stop`.
    pub name: String,
    /// Parameters, e.g. `repeat_enter 500ms`.
    pub label: String,
    pub uptime_secs: u64,
    pub output: Vec<String>,
}

impl TaskInfo {
    /// One line for `status`/`list`.
    pub fn line(&self) -> String {
        if self.name == self.label {
            format!("- {:<28} up {}s", self.name, self.uptime_secs)
        } else {
            format!("- {:<28} up {}s   ({})", self.name, self.uptime_secs, self.label)
        }
    }
}

/// Shared, clonable handle to the registry.
#[derive(Clone)]
pub struct Agent {
    tasks: Arc<Mutex<Vec<Task>>>,
}

impl Default for Agent {
    fn default() -> Self {
        Self::new()
    }
}

impl Agent {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Start a routine in its own thread.
    ///
    /// The task is registered under the routine's own name — so `stop
    /// repeat_enter` works — with `label` used for display. A second instance
    /// of the same routine gets `#2`, `#3`, ... appended.
    pub fn start(&self, label: impl Into<String>, routine: BoxedRoutine) -> Result<String, StartError> {
        let label = label.into();
        let name = self.unique_name(routine.name());
        let stop = Arc::new(AtomicBool::new(false));
        let log = TaskLog::default();

        // Every task's messages land in its own buffer so remote clients can
        // read them, not just whoever is watching the daemon's stdout.
        let sink = log.handle();
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            routine.run(thread_stop, sink);
        });

        lock(&self.tasks).push(Task {
            stop,
            name: name.clone(),
            label,
            started: Instant::now(),
            handle: Some(handle),
            log,
        });

        Ok(name)
    }

    /// Pick a free instance name: `repeat_enter`, then `repeat_enter#2`, ...
    pub fn unique_name(&self, base: &str) -> String {
        let tasks = lock(&self.tasks);
        let taken = |candidate: &str| tasks.iter().any(|t| t.name == candidate);
        if !taken(base) {
            return base.to_string();
        }
        let mut n = 2;
        loop {
            let candidate = format!("{base}#{n}");
            if !taken(&candidate) {
                return candidate;
            }
            n += 1;
        }
    }

    /// Drop tasks whose routine has finished.
    ///
    /// Called before every read of the registry, so `list` only ever shows
    /// tasks that are actually alive.
    fn prune(&self, tasks: &mut Vec<Task>) {
        tasks.retain(|task| !task.finished());
    }

    /// Every live task, oldest first.
    pub fn infos(&self) -> Vec<TaskInfo> {
        let mut tasks = lock(&self.tasks);
        self.prune(&mut tasks);
        tasks.iter().map(Task::info).collect()
    }

    pub fn names(&self) -> Vec<String> {
        let mut tasks = lock(&self.tasks);
        self.prune(&mut tasks);
        tasks.iter().map(|t| t.name.clone()).collect()
    }

    pub fn is_running(&self, name: &str) -> bool {
        let mut tasks = lock(&self.tasks);
        self.prune(&mut tasks);
        tasks.iter().any(|t| t.name == name)
    }

    /// Resolve what the user typed to a registered task name.
    ///
    /// Accepts an exact name, or an unambiguous prefix of one, so `stop
    /// repeat` works when only `repeat_enter` is running. Ambiguity is an
    /// error rather than a guess.
    pub fn resolve(&self, typed: &str) -> Result<String, ResolveError> {
        let mut tasks = lock(&self.tasks);
        self.prune(&mut tasks);
        let names: Vec<&str> = tasks.iter().map(|t| t.name.as_str()).collect();

        if names.iter().any(|n| *n == typed) {
            return Ok(typed.to_string());
        }

        let matches: Vec<&str> = names
            .iter()
            .copied()
            .filter(|n| n.starts_with(typed))
            .collect();

        match matches.len() {
            0 => Err(ResolveError::NotFound(typed.to_string())),
            1 => Ok(matches[0].to_string()),
            _ => Err(ResolveError::Ambiguous(
                typed.to_string(),
                matches.iter().map(|s| s.to_string()).collect(),
            )),
        }
    }

    pub fn len(&self) -> usize {
        let mut tasks = lock(&self.tasks);
        self.prune(&mut tasks);
        tasks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// How many tasks are still registered, finished ones included. Used by
    /// the log/status paths where pruning would be a side effect.
    pub fn slack(&self) -> usize {
        lock(&self.tasks).len()
    }

    /// Ask one task to stop by its registered name. Returns false when there is
    /// no such task.
    pub fn stop(&self, name: &str) -> bool {
        let mut tasks = lock(&self.tasks);
        self.prune(&mut tasks);
        match tasks.iter().find(|t| t.name == name) {
            Some(task) => {
                task.stop.store(true, Ordering::Relaxed);
                true
            }
            None => false,
        }
    }

    /// Ask every live task to stop; returns how many were asked.
    pub fn stop_all(&self) -> usize {
        let mut tasks = lock(&self.tasks);
        self.prune(&mut tasks);
        for task in tasks.iter() {
            task.stop.store(true, Ordering::Relaxed);
        }
        tasks.len()
    }

    /// Block until every task has actually exited, bounded by `timeout`.
    ///
    /// Used on shutdown so the daemon does not vanish mid-keypress. Tasks that
    /// ignore their stop flag are simply left behind rather than hanging exit.
    pub fn join_all(&self, timeout: Duration) -> usize {
        let mut pending: Vec<std::thread::JoinHandle<()>> = {
            let mut tasks = lock(&self.tasks);
            tasks.drain(..).filter_map(|mut t| t.handle.take()).collect()
        };

        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if pending.iter().all(|h| h.is_finished()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }

        // Collect whatever became ready. Anything still running is detached:
        // the process is exiting anyway, and blocking here would be worse.
        let mut joined = 0;
        for handle in pending.drain(..) {
            if handle.is_finished() {
                let _ = handle.join();
                joined += 1;
            }
        }
        joined
    }
}

/// A poisoned lock still exposes usable state; recovering beats panicking.
pub fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use routines::Routine;

    /// A routine that loops until stopped, for exercising the registry.
    struct Looper {
        name: &'static str,
    }

    impl Routine for Looper {
        fn name(&self) -> &'static str {
            self.name
        }

        fn run(self: Box<Self>, stop: Arc<AtomicBool>, log: LogHandle) {
            log.line(&format!("[{}] looping", self.name));
            while !stop.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            log.line(&format!("[{}] done", self.name));
        }
    }

    #[test]
    fn tasks_register_under_the_routine_name_and_stop() {
        let agent = Agent::new();
        let name = agent
            .start("looper 5ms", Box::new(Looper { name: "looper" }))
            .expect("start");
        assert_eq!(name, "looper", "the registered name is the routine name");
        assert_eq!(agent.len(), 1);
        assert!(agent.is_running("looper"));
        assert_eq!(agent.names(), vec!["looper".to_string()]);

        // The descriptive label is kept alongside, for display.
        let infos = agent.infos();
        assert_eq!(infos[0].name, "looper");
        assert_eq!(infos[0].label, "looper 5ms");
        assert!(infos[0].line().contains("looper 5ms"), "got: {}", infos[0].line());

        assert!(agent.stop("looper"));
        assert!(!agent.stop("nope"));
        assert_eq!(agent.join_all(Duration::from_secs(2)), 1);
        assert!(agent.is_empty());
    }

    #[test]
    fn duplicate_routines_are_registered_side_by_side() {
        let agent = Agent::new();
        let first = agent.start("dup 1", Box::new(Looper { name: "dup" })).expect("first");
        let second = agent.start("dup 2", Box::new(Looper { name: "dup" })).expect("second");
        assert_eq!(first, "dup");
        assert_eq!(second, "dup#2");
        assert_eq!(agent.len(), 2);

        // Both are stoppable independently.
        assert!(agent.stop("dup"));
        assert!(agent.stop("dup#2"));

        agent.stop_all();
        agent.join_all(Duration::from_secs(2));
        assert!(agent.is_empty());
    }

    #[test]
    fn task_names_resolve_by_exact_name_or_unique_prefix() {
        let agent = Agent::new();
        agent.start("a", Box::new(Looper { name: "repeat_enter" })).expect("start");

        // Exact name, and an unambiguous prefix.
        assert_eq!(agent.resolve("repeat_enter").as_deref(), Ok("repeat_enter"));
        assert_eq!(agent.resolve("repeat").as_deref(), Ok("repeat_enter"));
        assert_eq!(agent.resolve("rep").as_deref(), Ok("repeat_enter"));

        assert_eq!(
            agent.resolve("nothing"),
            Err(ResolveError::NotFound("nothing".to_string()))
        );

        agent.stop_all();
        agent.join_all(Duration::from_secs(2));
    }

    #[test]
    fn an_ambiguous_prefix_is_refused_rather_than_guessed() {
        let agent = Agent::new();
        agent.start("x", Box::new(Looper { name: "repeat_enter" })).expect("one");
        agent.start("y", Box::new(Looper { name: "repeat_enter_jitter" })).expect("two");

        match agent.resolve("repeat") {
            Err(ResolveError::Ambiguous(typed, matches)) => {
                assert_eq!(typed, "repeat");
                assert_eq!(matches.len(), 2);
            }
            other => panic!("expected Ambiguous, got {other:?}"),
        }
        // A longer prefix disambiguates.
        assert_eq!(
            agent.resolve("repeat_enter_j").as_deref(),
            Ok("repeat_enter_jitter")
        );

        agent.stop_all();
        agent.join_all(Duration::from_secs(2));
    }

    #[test]
    fn task_logs_are_bounded_and_ordered() {
        let log = TaskLog::default();
        for i in 0..(OUTPUT_LINES + 25) {
            log.push(format!("line {i}"));
        }
        let recent = log.recent(5);
        assert_eq!(recent.len(), 5);
        assert_eq!(recent.last().unwrap(), &format!("line {}", OUTPUT_LINES + 24));
        assert_eq!(log.recent(1000).len(), OUTPUT_LINES);
    }

    #[test]
    fn running_tasks_report_uptime_and_output() {
        let agent = Agent::new();
        agent.start("looper", Box::new(Looper { name: "looper" })).expect("start");
        // Give the thread a moment to write its first log line.
        std::thread::sleep(Duration::from_millis(60));
        let infos = agent.infos();
        assert_eq!(infos.len(), 1);
        assert_eq!(infos[0].name, "looper");
        assert!(infos[0].uptime_secs < 60);
        assert!(
            infos[0].output.iter().any(|l| l.contains("looping")),
            "task output should be captured, got {:?}",
            infos[0].output
        );
        agent.stop_all();
        agent.join_all(Duration::from_secs(2));
    }

    #[test]
    fn a_task_that_finishes_on_its_own_is_pruned() {
        /// Returns immediately, like `open_browser` does.
        struct Once;
        impl Routine for Once {
            fn name(&self) -> &'static str { "once" }
            fn run(self: Box<Self>, _stop: Arc<AtomicBool>, log: LogHandle) {
                log.line("ran");
            }
        }

        let agent = Agent::new();
        agent.start("once", Box::new(Once)).expect("start");

        // The thread is already finished; the registry must not keep reporting it.
        let deadline = Instant::now() + Duration::from_secs(2);
        while agent.len() > 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(agent.len(), 0, "a finished task must be pruned from `len`");
        assert!(agent.names().is_empty(), "and from `names`");
        assert!(agent.infos().is_empty(), "and from `infos`");
        assert!(!agent.is_running("once"));
        assert_eq!(
            agent.resolve("once"),
            Err(ResolveError::NotFound("once".to_string()))
        );
    }

    #[test]
    fn a_task_that_ignores_stop_does_not_hang_shutdown() {
        struct Stubborn;
        impl Routine for Stubborn {
            fn name(&self) -> &'static str { "stubborn" }
            fn run(self: Box<Self>, _stop: Arc<AtomicBool>, _log: LogHandle) {
                std::thread::sleep(Duration::from_secs(30));
            }
        }

        let agent = Agent::new();
        agent.start("stubborn", Box::new(Stubborn)).expect("start");
        let start = Instant::now();
        let joined = agent.join_all(Duration::from_millis(200));
        assert_eq!(joined, 0, "the stubborn task must not be reported as joined");
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "shutdown must stay bounded, took {:?}",
            start.elapsed()
        );
    }
}
