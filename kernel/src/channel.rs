// SPDX-License-Identifier: GPL-2.0-only
//! Channels: connections between programs that are not related.
//!
//! A pipe connects programs that a common parent set up. A channel lets
//! one program find another by name: a server *listens* on a name, a
//! client *connects* to it, and each side gets a descriptor it can read
//! and write — two pipes, one per direction. It is the smallest thing
//! that lets a desktop accept windows from programs it did not start.
//!
//! Names are a flat namespace, first come first served; there are no
//! permissions. A name is free again when its listener is closed.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;

use crate::pipe::{self, Reader, Writer};
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

/// Connections waiting to be accepted before new ones are refused.
const BACKLOG: usize = 16;

/// A name being listened on. Dropping it frees the name; connections
/// nobody accepted are closed.
pub struct Listener {
    name: String,
    pending: SpinLock<VecDeque<(Reader, Writer)>>,
    arrived: WaitQueue,
}

static LISTENERS: SpinLock<BTreeMap<String, Arc<Listener>>> = SpinLock::new(BTreeMap::new());

/// A handle on a listener, held by descriptors. The name is released
/// when the last handle goes away.
pub struct Handle(Arc<Listener>);

/// Why a connection could not be made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectError {
    /// Nobody listens on the name.
    NoListener,
    /// The listener is not accepting fast enough.
    Busy,
}

/// Starts listening on `name`. `None` if the name is taken.
pub fn listen(name: &str) -> Option<Arc<Handle>> {
    let mut listeners = LISTENERS.lock();
    if listeners.contains_key(name) {
        return None;
    }
    let listener = Arc::new(Listener {
        name: name.into(),
        pending: SpinLock::new(VecDeque::new()),
        arrived: WaitQueue::new(),
    });
    listeners.insert(name.into(), listener.clone());
    Some(Arc::new(Handle(listener)))
}

/// Connects to the listener on `name` and returns the client's end.
pub fn connect(name: &str) -> Result<(Reader, Writer), ConnectError> {
    let listener = LISTENERS
        .lock()
        .get(name)
        .cloned()
        .ok_or(ConnectError::NoListener)?;
    let (to_server_read, to_server_write) = pipe::new();
    let (to_client_read, to_client_write) = pipe::new();
    {
        let mut pending = listener.pending.lock();
        if pending.len() >= BACKLOG {
            return Err(ConnectError::Busy);
        }
        pending.push_back((to_server_read, to_client_write));
    }
    listener.arrived.wake_all();
    crate::poll::notify();
    Ok((to_client_read, to_server_write))
}

impl Handle {
    /// Whether a connection is waiting.
    pub fn ready(&self) -> bool {
        !self.0.pending.lock().is_empty()
    }

    /// Takes the next waiting connection, if any.
    pub fn try_accept(&self) -> Option<(Reader, Writer)> {
        self.0.pending.lock().pop_front()
    }

    /// Waits for a connection. `None` if the calling thread was killed.
    pub fn accept(&self) -> Option<(Reader, Writer)> {
        loop {
            if let Some(connection) = self.try_accept() {
                return Some(connection);
            }
            if crate::sched::current_killed() {
                return None;
            }
            self.0
                .arrived
                .wait_until(|| self.ready() || crate::sched::current_killed());
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        LISTENERS.lock().remove(&self.0.name);
        // Close what nobody accepted, outside the lock: closing a pipe
        // end wakes threads.
        let abandoned: VecDeque<_> = core::mem::take(&mut *self.0.pending.lock());
        drop(abandoned);
    }
}

/// Exercises listen, connect and accept within one thread.
pub fn self_test() -> Result<(), &'static str> {
    const NAME: &str = "kernel-self-test";
    if connect(NAME).err() != Some(ConnectError::NoListener) {
        return Err("connected to a name nobody listens on");
    }
    let listener = listen(NAME).ok_or("cannot listen on a free name")?;
    if listen(NAME).is_some() {
        return Err("two listeners on one name");
    }
    if listener.ready() || listener.try_accept().is_some() {
        return Err("a connection appeared from nowhere");
    }
    let (client_read, client_write) = connect(NAME).map_err(|_| "cannot connect")?;
    if !listener.ready() {
        return Err("the listener does not see the connection");
    }
    let (server_read, server_write) = listener.accept().ok_or("accept failed")?;
    let mut buffer = [0u8; 8];
    client_write
        .write(b"ping")
        .map_err(|_| "client cannot write")?;
    if server_read.read(&mut buffer) != Ok(4) || &buffer[..4] != b"ping" {
        return Err("the server did not receive the client's data");
    }
    if server_write.try_write(b"pong") != Ok(true) {
        return Err("server cannot write");
    }
    if client_read.read(&mut buffer) != Ok(4) || &buffer[..4] != b"pong" {
        return Err("the client did not receive the server's data");
    }
    // Closing the server's end is end of input for the client.
    drop((server_read, server_write));
    if !client_read.ready() || client_read.read(&mut buffer) != Ok(0) {
        return Err("the client did not see the server hang up");
    }
    // An unaccepted connection is closed with the listener, and the name
    // becomes free.
    let (orphan_read, _orphan_write) = connect(NAME).map_err(|_| "cannot connect again")?;
    drop(listener);
    if orphan_read.read(&mut buffer) != Ok(0) {
        return Err("an unaccepted connection outlived its listener");
    }
    if connect(NAME).err() != Some(ConnectError::NoListener) {
        return Err("the name outlived its listener");
    }
    listen(NAME).map(drop).ok_or("the name was not released")
}
