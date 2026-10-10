//! The daemon's control socket.
//!
//! A `TcpListener` on **loopback only**. One thread per connection, each
//! speaking newline-delimited JSON (see [`crate::protocol`]). Requests are
//! serialized through the runtime's command gate, so two terminals issuing
//! commands at the same instant are ordered rather than racing.
//!
//! Loopback TCP rather than a named pipe or unix socket because it is the one
//! transport that behaves the same on Windows, macOS and Linux, and because a
//! port plus a token in the session file is easy to reason about — the token is
//! what stops another local user from driving the agent.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::protocol::{Reply, Request, Response, MAX_LINE};
use crate::runtime::Runtime;
use crate::session::{DEFAULT_PORT, PORT_ATTEMPTS};
use crate::state::lock;

/// Socket read/write budget for one request.
const IO_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for the accept loop to notice a shutdown broadcast.
const SHUTDOWN_POLL: Duration = Duration::from_millis(200);

/// The listening daemon.
pub struct Server {
    listener: TcpListener,
    runtime: Arc<Runtime>,
    start: std::time::Instant,
    serving: AtomicBool,
}

impl Server {
    /// Bind a control socket, preferring `preferred` and walking upwards.
    ///
    /// An explicit port is taken as-is (one attempt); the default walks a short
    /// range so a leftover socket does not block startup. If every candidate is
    /// taken, the OS picks an ephemeral port rather than refusing to start.
    pub fn bind(runtime: Arc<Runtime>, preferred: Option<u16>) -> std::io::Result<Self> {
        let base = preferred.unwrap_or(DEFAULT_PORT);
        let attempts: Vec<u16> = if preferred.is_some() {
            vec![base]
        } else {
            (0..PORT_ATTEMPTS).map(|offset| base.saturating_add(offset)).collect()
        };

        let mut last_error = None;
        for port in attempts {
            match TcpListener::bind((Ipv4Addr::LOCALHOST, port)) {
                Ok(listener) => return Ok(Self::with_listener(listener, runtime)),
                Err(e) => last_error = Some(e),
            }
        }

        match TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) {
            Ok(listener) => Ok(Self::with_listener(listener, runtime)),
            Err(e) => Err(last_error.unwrap_or(e)),
        }
    }

    fn with_listener(listener: TcpListener, runtime: Arc<Runtime>) -> Self {
        Self {
            listener,
            runtime,
            start: std::time::Instant::now(),
            serving: AtomicBool::new(true),
        }
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.listener
            .local_addr()
            .unwrap_or_else(|_| SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
    }

    pub fn port(&self) -> u16 {
        self.local_addr().port()
    }

    pub fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    pub fn uptime_secs(&self) -> u64 {
        self.start.elapsed().as_secs()
    }

    /// True once the accept loop has stopped.
    pub fn is_serving(&self) -> bool {
        self.serving.load(Ordering::Relaxed)
    }

    /// Accept and serve connections until the runtime asks to shut down.
    ///
    /// The listener is briefly non-blocking so the shutdown flag is noticed
    /// promptly instead of waiting for the next client to connect.
    pub fn serve(&self) -> std::io::Result<()> {
        self.listener.set_nonblocking(true)?;
        let mut workers: Vec<thread::JoinHandle<()>> = Vec::new();

        while !self.runtime.shutdown_requested() {
            match self.listener.accept() {
                Ok((stream, peer)) => {
                    // Connections themselves block.
                    let _ = stream.set_nonblocking(false);
                    let runtime = Arc::clone(&self.runtime);
                    workers.push(thread::spawn(move || {
                        if let Err(e) = handle_connection(stream, peer, runtime) {
                            eprintln!("[server] connection from {peer} ended: {e}");
                        }
                    }));
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(SHUTDOWN_POLL);
                }
                Err(e) => eprintln!("[server] accept error: {e}"),
            }
        }

        self.serving.store(false, Ordering::Relaxed);
        // Let in-flight requests finish before the process goes away.
        for worker in workers {
            let _ = worker.join();
        }
        Ok(())
    }
}

/// Serve one client connection: authenticate, then answer requests in order.
fn handle_connection(
    stream: TcpStream,
    peer: SocketAddr,
    runtime: Arc<Runtime>,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;

    let peer_label = peer.to_string();
    runtime.register_client(&peer_label);
    runtime.record("client", format!("{peer_label} connected"));

    let mut reader = BufReader::new(stream.try_clone()?);
    let mut writer = stream;
    let mut line = String::new();

    loop {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break; // client hung up
        }

        if line.len() > MAX_LINE {
            write_response(
                &mut writer,
                &Response::new(
                    Reply::err(format!("request too long (limit {MAX_LINE} bytes)")),
                    Vec::new(),
                ),
            )?;
            break;
        }

        let request = match Request::from_line(&line) {
            Some(request) => request,
            None => {
                write_response(
                    &mut writer,
                    &Response::new(
                        Reply::err("malformed request: expected one JSON object per line"),
                        Vec::new(),
                    ),
                )?;
                continue;
            }
        };

        // Hold the gate so concurrent terminals are ordered.
        let response = {
            let _guard = lock(runtime.gate());
            runtime.handle(&peer_label, &request)
        };

        write_response(&mut writer, &response)?;

        if response.reply.shutdown {
            break;
        }
    }

    runtime.record("client", format!("{peer_label} disconnected"));
    let _ = writer.shutdown(Shutdown::Both);
    Ok(())
}

fn write_response(writer: &mut TcpStream, response: &Response) -> std::io::Result<()> {
    writer.write_all(response.to_json().as_bytes())?;
    writer.write_all(b"\n")?;
    writer.flush()
}
