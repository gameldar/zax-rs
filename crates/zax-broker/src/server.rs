//! The broker daemon: socket, workers and the event loop around the hub.
//!
//! One thread accepts connections and one thread per connection reads it;
//! one worker thread per source fetches it. All of them report to a single
//! loop over a channel, and only that loop touches the [`Hub`] or writes to
//! a client, so the bookkeeping needs no locks.

use std::collections::BTreeMap;
use std::io::BufReader;
use std::net::Shutdown;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use zax_protocol::{
    read_message, write_message, BrokerMessage, ClientMessage, Snapshot, SourceKey,
};

use crate::hub::{ClientId, Hub};
use crate::paths::BrokerPaths;
use crate::source::{Source, SourceFactory};

/// When a source could not be created, how long before it is tried again
/// unprompted. A refresh tries again at once.
const CREATE_RETRY: Duration = Duration::from_secs(3600);
/// When a fetch panicked the source's own interval is not to be trusted
/// either, so the next attempt waits this long.
const PANIC_RETRY: Duration = Duration::from_secs(60);
/// A pane that stops reading (suspended, wedged) must not stall every other
/// pane behind a full socket buffer; past this it is dropped.
const WRITE_WAIT: Duration = Duration::from_secs(2);

pub struct ServerConfig {
    pub paths: BrokerPaths,
    pub identity: u64,
    pub idle_exit: Duration,
}

/// Which incarnation of a key's worker sent a result. A key orphaned and
/// then subscribed again gets a new worker, and the old one's late result
/// must not land on the new entry.
type Generation = u64;

enum Ev {
    Connected(ClientId, UnixStream),
    Message(ClientId, ClientMessage),
    Gone(ClientId),
    Fetched {
        key: SourceKey,
        generation: Generation,
        result: Result<Value, String>,
        interval: Duration,
    },
    Called {
        client: ClientId,
        id: u64,
        key: SourceKey,
        generation: Generation,
        result: Result<Value, String>,
    },
}

enum Work {
    Fetch,
    Call {
        client: ClientId,
        id: u64,
        method: String,
        params: Value,
    },
}

struct Worker {
    generation: Generation,
    work: Sender<Work>,
}

enum Exit {
    Idle,
    /// A newer client said hello; it is told so once the socket is gone.
    Restart(ClientId),
}

/// Everything the event loop owns.
struct Broker {
    hub: Hub,
    writers: BTreeMap<ClientId, UnixStream>,
    workers: BTreeMap<SourceKey, Worker>,
    next_generation: Generation,
    factory: Arc<dyn SourceFactory>,
    events: Sender<Ev>,
}

/// Serve until idle for `idle_exit` or until a newer client asks for a
/// restart.
pub fn run(config: ServerConfig, factory: Arc<dyn SourceFactory>) -> std::io::Result<()> {
    let socket = &config.paths.socket;
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(socket);
    let listener = UnixListener::bind(socket)?;
    let bound = file_id(socket);
    let (tx, rx) = mpsc::channel::<Ev>();
    {
        let tx = tx.clone();
        thread::spawn(move || accept_loop(listener, tx));
    }

    let mut broker = Broker {
        hub: Hub::new(),
        writers: BTreeMap::new(),
        workers: BTreeMap::new(),
        next_generation: 0,
        factory,
        events: tx,
    };
    let mut idle_since = Some(Instant::now());

    let exit = loop {
        let now = Instant::now();
        for key in broker.hub.due(now) {
            broker.dispatch(&key);
        }
        if idle_since.is_some_and(|since| now.duration_since(since) >= config.idle_exit) {
            break Exit::Idle;
        }
        let wait = broker
            .hub
            .next_due()
            .map_or(Duration::from_secs(1), |at| {
                at.saturating_duration_since(now)
            })
            .min(Duration::from_millis(250));

        match rx.recv_timeout(wait) {
            Ok(Ev::Connected(client, stream)) => {
                let _ = stream.set_write_timeout(Some(WRITE_WAIT));
                broker.writers.insert(client, stream);
                // A connection mid-handshake keeps the broker up, so a pane
                // that connects just as the idle timer runs out is not
                // dropped before its hello is read.
                idle_since = None;
            }
            Ok(Ev::Message(client, ClientMessage::Hello { identity })) => {
                if identity > config.identity {
                    break Exit::Restart(client);
                }
                broker.send(client, &BrokerMessage::Welcome);
            }
            Ok(Ev::Message(client, ClientMessage::Subscribe { key })) => {
                let subscribed = broker.hub.subscribe(client, key.clone(), Instant::now());
                if let Some(snapshot) = subscribed.snapshot {
                    broker.send(client, &BrokerMessage::Snapshot(snapshot));
                }
                // Fetch a new source now rather than on the next turn; the
                // refresh marks it in flight, as dispatching requires.
                if subscribed.created && broker.hub.refresh(&key) {
                    broker.dispatch(&key);
                }
            }
            Ok(Ev::Message(_, ClientMessage::Refresh { key })) => {
                if broker.hub.refresh(&key) {
                    broker.dispatch(&key);
                }
            }
            Ok(Ev::Message(
                client,
                ClientMessage::Call {
                    id,
                    key,
                    method,
                    params,
                },
            )) => broker.call(client, id, &key, method, params),
            Ok(Ev::Gone(client)) => {
                broker.writers.remove(&client);
                broker.hub.disconnect(client);
                for key in broker.hub.orphans() {
                    // Dropping the sender ends the worker, and the source
                    // (and any provider child) with it.
                    broker.workers.remove(&key);
                }
                // Connections, not subscriptions: a pane that has said hello
                // but not yet subscribed still keeps the broker alive.
                if broker.writers.is_empty() {
                    idle_since = Some(Instant::now());
                }
            }
            Ok(Ev::Fetched {
                key,
                generation,
                result,
                interval,
            }) => {
                if broker.is_current(&key, generation) {
                    let sent = broker.hub.finish(
                        &key,
                        result,
                        SystemTime::now(),
                        Instant::now(),
                        interval,
                        roll(),
                    );
                    broker.fan_out(sent);
                }
            }
            Ok(Ev::Called {
                client,
                id,
                key,
                generation,
                result,
            }) => {
                let ok = result.is_ok();
                let (result, error) = match result {
                    Ok(v) => (Some(v), None),
                    Err(e) => (None, Some(e)),
                };
                // The caller is answered whichever worker served it.
                broker.send(client, &BrokerMessage::Reply { id, result, error });
                // A write changes what everyone should see.
                if ok && broker.is_current(&key, generation) && broker.hub.refresh(&key) {
                    broker.dispatch(&key);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Exit::Idle,
        }
    };

    // Stop accepting first: with the receiver gone the accept loop's next
    // send fails, and one throwaway connection wakes it to find that out.
    drop(rx);
    if bound.is_some() && file_id(socket) == bound {
        let _ = UnixStream::connect(socket);
        let _ = std::fs::remove_file(socket);
    }
    // Only now, with the socket gone, is the newer client told to spawn its
    // own broker; telling it sooner would let it find this one still there.
    if let Exit::Restart(client) = exit {
        broker.send(client, &BrokerMessage::RestartRequired);
    }
    // The reader threads hold their own handles on each connection, so
    // closing ours alone would leave every pane waiting on a dead broker.
    for stream in broker.writers.values() {
        let _ = stream.shutdown(Shutdown::Both);
    }
    Ok(())
}

impl Broker {
    /// Start a fetch of a key the hub has just marked in flight. Every path
    /// out of here either hands the fetch to a worker or finishes it, since
    /// only `finish` clears the in-flight flag.
    fn dispatch(&mut self, key: &SourceKey) {
        if !self.workers.contains_key(key) {
            match self.factory.create(key) {
                Ok(source) => {
                    self.next_generation += 1;
                    let generation = self.next_generation;
                    let work = spawn_worker(key.clone(), generation, source, self.events.clone());
                    self.workers
                        .insert(key.clone(), Worker { generation, work });
                }
                Err(error) => return self.fail(key, error, CREATE_RETRY),
            }
        }
        let sent = self
            .workers
            .get(key)
            .is_some_and(|worker| worker.work.send(Work::Fetch).is_ok());
        if !sent {
            // The worker thread is gone; the next attempt starts a new one.
            self.workers.remove(key);
            self.fail(
                key,
                format!("the {} source stopped", key.topic),
                PANIC_RETRY,
            );
        }
    }

    fn fail(&mut self, key: &SourceKey, error: String, retry: Duration) {
        let sent = self.hub.finish(
            key,
            Err(error),
            SystemTime::now(),
            Instant::now(),
            retry,
            0.5,
        );
        self.fan_out(sent);
    }

    fn call(&mut self, client: ClientId, id: u64, key: &SourceKey, method: String, params: Value) {
        let work = Work::Call {
            client,
            id,
            method,
            params,
        };
        let sent = self
            .workers
            .get(key)
            .is_some_and(|worker| worker.work.send(work).is_ok());
        if !sent {
            let error = Some("subscribe before calling".to_owned());
            self.send(
                client,
                &BrokerMessage::Reply {
                    id,
                    result: None,
                    error,
                },
            );
        }
    }

    fn is_current(&self, key: &SourceKey, generation: Generation) -> bool {
        self.workers
            .get(key)
            .is_some_and(|worker| worker.generation == generation)
    }

    fn fan_out(&mut self, sent: Vec<(ClientId, Snapshot)>) {
        for (to, snapshot) in sent {
            self.send(to, &BrokerMessage::Snapshot(snapshot));
        }
    }

    fn send(&mut self, client: ClientId, message: &BrokerMessage) {
        if let Some(stream) = self.writers.get_mut(&client) {
            if write_message(stream, message).is_err() {
                // A half-written message leaves the stream unusable. Shutting
                // it down makes the pane notice and ends its reader thread,
                // which reports the client gone.
                let _ = stream.shutdown(Shutdown::Both);
                self.writers.remove(&client);
            }
        }
    }
}

/// The device and inode of a path, which tell this broker's socket from a
/// successor's bound at the same path.
fn file_id(path: &Path) -> Option<(u64, u64)> {
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

fn accept_loop(listener: UnixListener, tx: Sender<Ev>) {
    let clients = (1 as ClientId)..;
    for (client, stream) in clients.zip(listener.incoming().map_while(Result::ok)) {
        let Ok(reader) = stream.try_clone() else {
            continue;
        };
        if tx.send(Ev::Connected(client, stream)).is_err() {
            return;
        }
        let tx = tx.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(reader);
            while let Ok(Some(message)) = read_message::<_, ClientMessage>(&mut reader) {
                if tx.send(Ev::Message(client, message)).is_err() {
                    return;
                }
            }
            let _ = tx.send(Ev::Gone(client));
        });
    }
}

/// One thread per source. It exits when the loop drops its sender, which
/// drops the source with it (and a provider child with that). A panic in
/// the source is caught and reported as that fetch's or call's error, so
/// the entry is always finished and the worker lives on.
fn spawn_worker(
    key: SourceKey,
    generation: Generation,
    mut source: Box<dyn Source>,
    tx: Sender<Ev>,
) -> Sender<Work> {
    let (work_tx, work_rx) = mpsc::channel::<Work>();
    thread::spawn(move || {
        let panicked = format!("{} source panicked", key.topic);
        while let Ok(work) = work_rx.recv() {
            let ev = match work {
                Work::Fetch => {
                    let (result, interval) =
                        catch_unwind(AssertUnwindSafe(|| (source.fetch(), source.interval())))
                            .unwrap_or_else(|_| (Err(panicked.clone()), PANIC_RETRY));
                    Ev::Fetched {
                        key: key.clone(),
                        generation,
                        result,
                        interval,
                    }
                }
                Work::Call {
                    client,
                    id,
                    method,
                    params,
                } => Ev::Called {
                    client,
                    id,
                    key: key.clone(),
                    generation,
                    result: catch_unwind(AssertUnwindSafe(|| source.call(&method, params)))
                        .unwrap_or_else(|_| Err(panicked.clone())),
                },
            };
            if tx.send(ev).is_err() {
                return;
            }
        }
    });
    work_tx
}

fn roll() -> f64 {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    f64::from(nanos) / 1_000_000_000.0
}
