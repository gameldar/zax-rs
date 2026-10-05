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

/// Which incarnation of a key's worker sent a result. A key reaped after
/// lingering orphaned and then subscribed again gets a new worker, and the
/// old one's late result must not land on the new entry.
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

#[derive(Debug, PartialEq)]
enum Exit {
    Idle,
    /// A newer client said hello; it is told so once the socket is gone.
    Restart(ClientId),
}

/// Everything the event loop owns. It is driven by [`Broker::handle`] and
/// [`Broker::tick`] with the time passed in, so it can be tested with
/// explicit events and no sleeps.
struct Broker {
    hub: Hub,
    writers: BTreeMap<ClientId, UnixStream>,
    workers: BTreeMap<SourceKey, Worker>,
    next_generation: Generation,
    factory: Arc<dyn SourceFactory>,
    events: Sender<Ev>,
    identity: u64,
    /// How long the broker outlives its last connection, and how long a
    /// source outlives its last subscriber: the same window, so whatever
    /// keeps the broker warm for a restarted pane keeps its cache warm too.
    idle_exit: Duration,
    idle_since: Option<Instant>,
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

    let mut broker = Broker::new(
        config.identity,
        config.idle_exit,
        factory,
        tx,
        Instant::now(),
    );

    let exit = loop {
        let now = Instant::now();
        if let Some(exit) = broker.tick(now) {
            break exit;
        }
        match rx.recv_timeout(broker.wait(now)) {
            Ok(ev) => {
                if let Some(exit) = broker.handle(ev, Instant::now()) {
                    break exit;
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
    fn new(
        identity: u64,
        idle_exit: Duration,
        factory: Arc<dyn SourceFactory>,
        events: Sender<Ev>,
        now: Instant,
    ) -> Self {
        Self {
            hub: Hub::new(),
            writers: BTreeMap::new(),
            workers: BTreeMap::new(),
            next_generation: 0,
            factory,
            events,
            identity,
            idle_exit,
            idle_since: Some(now),
        }
    }

    /// The loop's timer: start what is due, reap sources that have lingered
    /// unwatched, and say whether the broker has been idle long enough to go.
    fn tick(&mut self, now: Instant) -> Option<Exit> {
        for key in self.hub.due(now) {
            self.dispatch(&key, now);
        }
        for key in self.hub.orphans(now, self.idle_exit) {
            // Dropping the sender ends the worker, and the source (and any
            // provider child) with it.
            self.workers.remove(&key);
        }
        self.idle_since
            .is_some_and(|since| now.duration_since(since) >= self.idle_exit)
            .then_some(Exit::Idle)
    }

    /// How long the loop may sleep before the next tick. It is capped so
    /// lingering orphans and the idle timer are checked often enough.
    fn wait(&self, now: Instant) -> Duration {
        self.hub
            .next_due()
            .map_or(Duration::from_secs(1), |at| {
                at.saturating_duration_since(now)
            })
            .min(Duration::from_millis(250))
    }

    /// One event from a reader or a worker.
    fn handle(&mut self, ev: Ev, now: Instant) -> Option<Exit> {
        match ev {
            Ev::Connected(client, stream) => {
                let _ = stream.set_write_timeout(Some(WRITE_WAIT));
                self.writers.insert(client, stream);
                // A connection mid-handshake keeps the broker up, so a pane
                // that connects just as the idle timer runs out is not
                // dropped before its hello is read.
                self.idle_since = None;
            }
            Ev::Message(client, ClientMessage::Hello { identity }) => {
                if identity > self.identity {
                    return Some(Exit::Restart(client));
                }
                self.send(client, &BrokerMessage::Welcome);
            }
            Ev::Message(client, ClientMessage::Subscribe { key }) => {
                let subscribed = self.hub.subscribe(client, key.clone(), now);
                if let Some(snapshot) = subscribed.snapshot {
                    self.send(client, &BrokerMessage::Snapshot(snapshot));
                }
                // Fetch a new source now rather than on the next turn; the
                // refresh marks it in flight, as dispatching requires. A
                // source still lingering keeps its worker and its cache.
                if subscribed.created && self.hub.refresh(&key) {
                    self.dispatch(&key, now);
                }
            }
            Ev::Message(_, ClientMessage::Refresh { key }) => {
                if self.hub.refresh(&key) {
                    self.dispatch(&key, now);
                }
            }
            Ev::Message(
                client,
                ClientMessage::Call {
                    id,
                    key,
                    method,
                    params,
                },
            ) => self.call(client, id, &key, method, params),
            Ev::Gone(client) => {
                self.writers.remove(&client);
                // Its sources linger orphaned; the timer reaps them.
                self.hub.disconnect(client, now);
                // Connections, not subscriptions: a pane that has said hello
                // but not yet subscribed still keeps the broker alive.
                if self.writers.is_empty() {
                    self.idle_since = Some(now);
                }
            }
            Ev::Fetched {
                key,
                generation,
                result,
                interval,
            } => {
                if self.is_current(&key, generation) {
                    let sent =
                        self.hub
                            .finish(&key, result, SystemTime::now(), now, interval, roll());
                    self.fan_out(sent);
                }
            }
            Ev::Called {
                client,
                id,
                key,
                generation,
                result,
            } => {
                let ok = result.is_ok();
                let (result, error) = match result {
                    Ok(v) => (Some(v), None),
                    Err(e) => (None, Some(e)),
                };
                // The caller is answered whichever worker served it.
                self.send(client, &BrokerMessage::Reply { id, result, error });
                // A write changes what everyone should see.
                if ok && self.is_current(&key, generation) && self.hub.refresh(&key) {
                    self.dispatch(&key, now);
                }
            }
        }
        None
    }

    /// Start a fetch of a key the hub has just marked in flight. Every path
    /// out of here either hands the fetch to a worker or finishes it, since
    /// only `finish` clears the in-flight flag.
    fn dispatch(&mut self, key: &SourceKey, now: Instant) {
        if !self.workers.contains_key(key) {
            match self.factory.create(key) {
                Ok(source) => {
                    self.next_generation += 1;
                    let generation = self.next_generation;
                    let work = spawn_worker(key.clone(), generation, source, self.events.clone());
                    self.workers
                        .insert(key.clone(), Worker { generation, work });
                }
                Err(error) => return self.fail(key, error, CREATE_RETRY, now),
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
                now,
            );
        }
    }

    fn fail(&mut self, key: &SourceKey, error: String, retry: Duration, now: Instant) {
        let sent = self
            .hub
            .finish(key, Err(error), SystemTime::now(), now, retry, 0.5);
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::sync::mpsc::Receiver;

    use serde_json::json;
    use zax_protocol::read_message;

    use super::*;

    const LINGER: Duration = Duration::from_secs(30);

    /// Answers at once. Its worker's own results land on a channel these
    /// tests never drain; every result the broker sees is fed in by hand.
    struct Quiet;

    impl Source for Quiet {
        fn fetch(&mut self) -> Result<Value, String> {
            Ok(json!("from the worker"))
        }
        fn call(&mut self, _method: &str, _params: Value) -> Result<Value, String> {
            Ok(json!({}))
        }
        fn interval(&self) -> Duration {
            Duration::from_secs(3600)
        }
    }

    struct QuietFactory;

    impl SourceFactory for QuietFactory {
        fn create(&self, _key: &SourceKey) -> Result<Box<dyn Source>, String> {
            Ok(Box::new(Quiet))
        }
    }

    fn broker(now: Instant) -> (Broker, Receiver<Ev>) {
        let (tx, rx) = mpsc::channel();
        (Broker::new(10, LINGER, Arc::new(QuietFactory), tx, now), rx)
    }

    fn key() -> SourceKey {
        SourceKey {
            topic: "tickets".into(),
            source: "{}".into(),
        }
    }

    /// Connects a client and returns the pane's end of its socket. Writes
    /// happen inside `handle`, so whatever the broker sent is already there.
    fn connect(broker: &mut Broker, client: ClientId, now: Instant) -> BufReader<UnixStream> {
        let (ours, theirs) = UnixStream::pair().unwrap();
        // Nothing to read is a failure now, not a wait: it can only mean
        // the broker sent nothing.
        theirs.set_nonblocking(true).unwrap();
        assert_eq!(broker.handle(Ev::Connected(client, ours), now), None);
        BufReader::new(theirs)
    }

    fn subscribe(broker: &mut Broker, client: ClientId, now: Instant) {
        let ev = Ev::Message(client, ClientMessage::Subscribe { key: key() });
        assert_eq!(broker.handle(ev, now), None);
    }

    fn fetched(generation: Generation, value: &str) -> Ev {
        Ev::Fetched {
            key: key(),
            generation,
            result: Ok(json!(value)),
            interval: Duration::from_secs(3600),
        }
    }

    fn next_value(pane: &mut BufReader<UnixStream>) -> Option<Value> {
        match read_message::<_, BrokerMessage>(pane).unwrap() {
            Some(BrokerMessage::Snapshot(snapshot)) => snapshot.value,
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }

    fn generation(broker: &Broker) -> Generation {
        broker.workers[&key()].generation
    }

    #[test]
    fn a_late_result_from_a_reaped_worker_is_ignored() {
        let t0 = Instant::now();
        let (mut broker, _results) = broker(t0);
        let _a = connect(&mut broker, 1, t0);
        subscribe(&mut broker, 1, t0);
        let old = generation(&broker);
        broker.handle(Ev::Gone(1), t0);
        let t1 = t0 + LINGER;
        assert_eq!(broker.tick(t1), Some(Exit::Idle));
        assert!(broker.workers.is_empty(), "the orphan was reaped");

        let mut b = connect(&mut broker, 2, t1);
        subscribe(&mut broker, 2, t1);
        let new = generation(&broker);
        assert_ne!(old, new, "a reaped key gets a new worker");
        broker.handle(fetched(old, "stale"), t1);
        broker.handle(fetched(new, "fresh"), t1);
        assert_eq!(
            next_value(&mut b),
            Some(json!("fresh")),
            "the old worker's result never reached the pane"
        );
    }

    #[test]
    fn a_resubscribe_within_the_linger_is_served_the_cache_by_the_same_worker() {
        let t0 = Instant::now();
        let (mut broker, _results) = broker(t0);
        let mut a = connect(&mut broker, 1, t0);
        subscribe(&mut broker, 1, t0);
        let first = generation(&broker);
        broker.handle(fetched(first, "cached"), t0);
        assert_eq!(next_value(&mut a), Some(json!("cached")));
        broker.handle(Ev::Gone(1), t0);

        let t1 = t0 + Duration::from_secs(5);
        assert_eq!(broker.tick(t1), None);
        let mut b = connect(&mut broker, 2, t1);
        subscribe(&mut broker, 2, t1);
        assert_eq!(next_value(&mut b), Some(json!("cached")), "sent at once");
        assert_eq!(generation(&broker), first, "the worker was kept");
    }
}
