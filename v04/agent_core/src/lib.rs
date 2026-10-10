//! Agent core: everything shared between the standalone process, the daemon
//! and the remote-control client.
//!
//! The split that matters in v04:
//!
//! * [`runtime`] holds the state and the command semantics — no I/O front end.
//! * [`server`] adapts that runtime to a loopback control socket (daemon mode).
//! * [`client`] is the other end of that socket (`agentctl`).
//! * [`session`] is how a client finds a running daemon on this machine.
//! * [`protocol`] is the newline-delimited JSON both ends speak.
//! * [`state`] is the task registry: what is running, and how to stop it.

pub mod client;
pub mod protocol;
pub mod runtime;
pub mod server;
pub mod session;
pub mod state;

pub use client::{Client, Probe, SendOutcome};
pub use protocol::{Event, Reply, Request, Response, API_LEVEL, VERSION};
pub use runtime::{ClientInfo, Mode, Runtime, RuntimeOptions};
pub use session::{Session, SessionDir, DEFAULT_PORT, PORT_ATTEMPTS};
pub use state::{Agent, ResolveError, StartError, TaskInfo, TaskLog};
