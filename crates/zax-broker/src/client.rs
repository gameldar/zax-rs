//! The pane-side connection to the broker.

use std::fs::OpenOptions;
use std::io::{BufReader, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use zax_protocol::{
    read_message, write_message, BrokerMessage, ClientMessage, Snapshot, SourceKey,
};

use crate::paths::BrokerPaths;

/// How long a freshly spawned broker gets to start listening.
const SPAWN_WAIT: Duration = Duration::from_secs(2);
/// A spawn recorded in the lock file this recently means another client has
/// just started a broker that is still coming up.
const RECENT_SPAWN: Duration = Duration::from_secs(5);
/// How long a broker that accepted the connection gets to answer the hello.
/// Without it a wedged broker would hang the pane before it ever drew.
const GREETING_WAIT: Duration = Duration::from_secs(2);

pub enum Event {
    Snapshot(Snapshot),
    Disconnected(String),
}

type Pending = Arc<Mutex<Vec<(u64, Sender<Result<Value, String>>)>>>;

pub struct Client {
    writer: Mutex<UnixStream>,
    events: Receiver<Event>,
    pending: Pending,
    next_id: AtomicU64,
}

/// The modification time of a binary in seconds, which is what tells a newer
/// build from an older one. Zero when it cannot be read, which never forces
/// a restart.
pub fn binary_identity(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

impl Client {
    /// Connect, spawning `spawn` (e.g. `[zax, "broker"]`) under the lock when
    /// nothing answers.
    pub fn connect_or_spawn(
        paths: &BrokerPaths,
        identity: u64,
        spawn: &[String],
    ) -> Result<Client, String> {
        for _attempt in 0..2 {
            match Self::connect(paths, identity) {
                Ok(Some(client)) => return Ok(client),
                // The broker was older and is exiting; start ours.
                Ok(None) => thread::sleep(Duration::from_millis(100)),
                Err(_) => {}
            }
            spawn_under_lock(paths, spawn)?;
            let deadline = Instant::now() + SPAWN_WAIT;
            while Instant::now() < deadline {
                if let Ok(Some(client)) = Self::connect(paths, identity) {
                    return Ok(client);
                }
                thread::sleep(Duration::from_millis(50));
            }
        }
        Err(format!(
            "could not reach the zax broker at {}; see {}",
            paths.socket.display(),
            paths.log.display()
        ))
    }

    /// `Ok(None)` when the broker answered `restart_required`, or hung up
    /// before answering, which is what a broker on its way out does.
    fn connect(paths: &BrokerPaths, identity: u64) -> Result<Option<Client>, String> {
        let stream = UnixStream::connect(&paths.socket).map_err(|e| e.to_string())?;
        let mut writer = stream.try_clone().map_err(|e| e.to_string())?;
        stream
            .set_read_timeout(Some(GREETING_WAIT))
            .map_err(|e| e.to_string())?;
        let mut reader = BufReader::new(stream);
        write_message(&mut writer, &ClientMessage::Hello { identity })
            .map_err(|e| e.to_string())?;
        match read_message::<_, BrokerMessage>(&mut reader).map_err(|e| e.to_string())? {
            Some(BrokerMessage::Welcome) => {}
            Some(BrokerMessage::RestartRequired) | None => return Ok(None),
            Some(other) => return Err(format!("unexpected greeting from the broker: {other:?}")),
        }
        // From here on silence is normal: a source may not change for hours.
        reader
            .get_ref()
            .set_read_timeout(None)
            .map_err(|e| e.to_string())?;
        let (tx, events) = mpsc::channel();
        let pending: Pending = Arc::new(Mutex::new(Vec::new()));
        let reader_pending = pending.clone();
        thread::spawn(move || read_loop(reader, tx, reader_pending));
        Ok(Some(Client {
            writer: Mutex::new(writer),
            events,
            pending,
            next_id: AtomicU64::new(1),
        }))
    }

    fn send(&self, message: &ClientMessage) -> Result<(), String> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| "broker connection poisoned".to_owned())?;
        write_message(&mut *writer, message).map_err(|e| format!("lost the broker: {e}"))
    }

    pub fn subscribe(&self, key: SourceKey) -> Result<(), String> {
        self.send(&ClientMessage::Subscribe { key })
    }

    pub fn refresh(&self, key: SourceKey) -> Result<(), String> {
        self.send(&ClientMessage::Refresh { key })
    }

    pub fn call(
        &self,
        key: SourceKey,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.pending
            .lock()
            .map_err(|_| "broker connection poisoned".to_owned())?
            .push((id, tx));
        let sent = self.send(&ClientMessage::Call {
            id,
            key,
            method: method.to_owned(),
            params,
        });
        let answer = sent.and_then(|()| {
            rx.recv_timeout(timeout)
                .map_err(|_| format!("the broker did not answer {method}"))
        });
        if answer.is_err() {
            // A reply that never comes must not leave its slot behind.
            if let Ok(mut waiting) = self.pending.lock() {
                waiting.retain(|(w, _)| *w != id);
            }
        }
        answer?
    }

    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }
}

impl Drop for Client {
    /// The reader thread holds its own handle on the connection, so letting
    /// ours go would not close it: the broker would go on counting a pane
    /// that has left. Shutting it down ends the reader thread too.
    fn drop(&mut self) {
        if let Ok(writer) = self.writer.lock() {
            let _ = writer.shutdown(Shutdown::Both);
        }
    }
}

fn read_loop(mut reader: BufReader<UnixStream>, events: Sender<Event>, pending: Pending) {
    let why = loop {
        match read_message::<_, BrokerMessage>(&mut reader) {
            Ok(Some(BrokerMessage::Snapshot(s))) => {
                if events.send(Event::Snapshot(s)).is_err() {
                    return;
                }
            }
            Ok(Some(BrokerMessage::Reply { id, result, error })) => {
                if let Ok(mut waiting) = pending.lock() {
                    if let Some(index) = waiting.iter().position(|(w, _)| *w == id) {
                        let (_, tx) = waiting.swap_remove(index);
                        let _ = tx.send(match error {
                            Some(e) => Err(e),
                            None => Ok(result.unwrap_or(Value::Null)),
                        });
                    }
                }
            }
            Ok(Some(_)) => {}
            Ok(None) => break "the broker closed the connection".to_owned(),
            Err(e) => break e.to_string(),
        }
    };
    // Dropping the waiting senders fails every call in progress at once,
    // rather than each one sitting out its timeout.
    if let Ok(mut waiting) = pending.lock() {
        waiting.clear();
    }
    let _ = events.send(Event::Disconnected(why));
}

/// Take the lock, and spawn the broker unless one is answering or was
/// spawned moments ago by whoever held the lock before us.
fn spawn_under_lock(paths: &BrokerPaths, spawn: &[String]) -> Result<(), String> {
    let Some((program, args)) = spawn.split_first() else {
        return Err("no broker is running and none can be started here".into());
    };
    if let Some(dir) = paths.lock.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let mut lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&paths.lock)
        .map_err(|e| format!("could not open {}: {e}", paths.lock.display()))?;
    lock.lock()
        .map_err(|e| format!("could not lock {}: {e}", paths.lock.display()))?;

    if UnixStream::connect(&paths.socket).is_ok() {
        return Ok(());
    }
    let recent = std::fs::read_to_string(&paths.lock)
        .ok()
        .and_then(|t| t.trim().parse::<u64>().ok())
        .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
        .and_then(|at| SystemTime::now().duration_since(at).ok())
        .is_some_and(|age| age < RECENT_SPAWN);
    if recent {
        return Ok(());
    }

    // A dead broker can leave its socket file behind; binding would fail.
    let _ = std::fs::remove_file(&paths.socket);
    if let Some(dir) = paths.log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&paths.log)
        .map_err(|e| format!("could not open {}: {e}", paths.log.display()))?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(log)
        .stderr(log_err)
        // Its own process group, so the hangup tmux sends a closing pane's
        // group does not take the shared broker with it.
        .process_group(0)
        .spawn()
        .map_err(|e| format!("could not start the zax broker: {e}"))?;
    // The broker outlives most panes, but when it exits first (idle, or
    // stepping aside for a newer build) this pane must reap it.
    thread::spawn(move || {
        let _ = child.wait();
    });

    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    lock.set_len(0).map_err(|e| e.to_string())?;
    write!(lock, "{now}").map_err(|e| e.to_string())?;
    Ok(())
}
