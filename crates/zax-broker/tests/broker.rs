#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};
use zax_broker::client::{Client, Event};
use zax_broker::server::{self, ServerConfig};
use zax_broker::{BrokerPaths, Source, SourceFactory};
use zax_protocol::SourceKey;

struct Counting {
    fetches: Arc<AtomicUsize>,
    delay: Duration,
}

impl Source for Counting {
    fn fetch(&mut self) -> Result<Value, String> {
        let n = self.fetches.fetch_add(1, Ordering::SeqCst) + 1;
        thread::sleep(self.delay);
        Ok(json!(n))
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        Ok(json!({"method": method, "params": params}))
    }
    fn interval(&self) -> Duration {
        Duration::from_secs(3600)
    }
}

/// Panics on its first fetch and answers normally after that.
struct PanicsOnce(bool);

impl Source for PanicsOnce {
    fn fetch(&mut self) -> Result<Value, String> {
        if !self.0 {
            self.0 = true;
            panic!("the source fell over");
        }
        Ok(json!("recovered"))
    }
    fn call(&mut self, _method: &str, _params: Value) -> Result<Value, String> {
        panic!("the call fell over");
    }
    fn interval(&self) -> Duration {
        Duration::from_secs(3600)
    }
}

struct Factory(Arc<AtomicUsize>);

impl SourceFactory for Factory {
    fn create(&self, key: &SourceKey) -> Result<Box<dyn Source>, String> {
        let delay = match key.topic.as_str() {
            "broken" => return Err("no such source".into()),
            "panics" => return Ok(Box::new(PanicsOnce(false))),
            _ => Duration::from_millis(100),
        };
        Ok(Box::new(Counting {
            fetches: self.0.clone(),
            delay,
        }))
    }
}

fn start(
    dir: &std::path::Path,
    identity: u64,
) -> (BrokerPaths, Arc<AtomicUsize>, thread::JoinHandle<()>) {
    let paths = BrokerPaths::in_dir(dir, dir);
    let fetches = Arc::new(AtomicUsize::new(0));
    let config = ServerConfig {
        paths: paths.clone(),
        identity,
        idle_exit: Duration::from_millis(500),
    };
    let factory = Arc::new(Factory(fetches.clone()));
    let handle = thread::spawn(move || server::run(config, factory).unwrap());
    for _ in 0..50 {
        if paths.socket.exists() {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    (paths, fetches, handle)
}

fn next_snapshot(client: &Client) -> zax_protocol::Snapshot {
    match client
        .events()
        .recv_timeout(Duration::from_secs(5))
        .unwrap()
    {
        Event::Snapshot(s) => s,
        Event::Disconnected(why) => panic!("disconnected: {why}"),
    }
}

fn key() -> SourceKey {
    SourceKey {
        topic: "t".into(),
        source: "{}".into(),
    }
}

fn topic(name: &str) -> SourceKey {
    SourceKey {
        topic: name.into(),
        source: "{}".into(),
    }
}

#[test]
fn two_clients_share_one_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, fetches, _h) = start(dir.path(), 10);
    let a = Client::connect_or_spawn(&paths, 10, &[]).unwrap();
    a.subscribe(key()).unwrap();
    assert_eq!(next_snapshot(&a).value, Some(json!(1)));
    let b = Client::connect_or_spawn(&paths, 10, &[]).unwrap();
    b.subscribe(key()).unwrap();
    assert_eq!(
        next_snapshot(&b).value,
        Some(json!(1)),
        "served from the cache"
    );
    assert_eq!(fetches.load(Ordering::SeqCst), 1);
}

#[test]
fn a_refresh_reaches_every_subscriber() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _f, _h) = start(dir.path(), 10);
    let a = Client::connect_or_spawn(&paths, 10, &[]).unwrap();
    let b = Client::connect_or_spawn(&paths, 10, &[]).unwrap();
    a.subscribe(key()).unwrap();
    b.subscribe(key()).unwrap();
    next_snapshot(&a);
    next_snapshot(&b);
    a.refresh(key()).unwrap();
    assert_eq!(next_snapshot(&b).value, Some(json!(2)));
}

#[test]
fn a_call_is_answered_and_then_the_topic_repolls() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _f, _h) = start(dir.path(), 10);
    let a = Client::connect_or_spawn(&paths, 10, &[]).unwrap();
    a.subscribe(key()).unwrap();
    next_snapshot(&a);
    let reply = a
        .call(
            key(),
            "tickets/transition",
            json!({"key":"A-1","id":"Done"}),
            Duration::from_secs(5),
        )
        .unwrap();
    assert_eq!(reply["method"], "tickets/transition");
    assert_eq!(next_snapshot(&a).value, Some(json!(2)));
}

#[test]
fn a_source_that_cannot_be_created_is_an_error_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _f, _h) = start(dir.path(), 10);
    let a = Client::connect_or_spawn(&paths, 10, &[]).unwrap();
    a.subscribe(topic("broken")).unwrap();
    assert_eq!(next_snapshot(&a).error.as_deref(), Some("no such source"));
}

#[test]
fn a_refresh_retries_a_source_that_could_not_be_created() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _f, _h) = start(dir.path(), 10);
    let a = Client::connect_or_spawn(&paths, 10, &[]).unwrap();
    a.subscribe(topic("broken")).unwrap();
    next_snapshot(&a);
    // The retry fails the same way, but it is answered rather than leaving
    // the source stuck in flight with no worker to finish it.
    a.refresh(topic("broken")).unwrap();
    assert_eq!(next_snapshot(&a).error.as_deref(), Some("no such source"));
}

#[test]
fn a_panicking_source_is_an_error_and_recovers_on_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _f, _h) = start(dir.path(), 10);
    let a = Client::connect_or_spawn(&paths, 10, &[]).unwrap();
    a.subscribe(topic("panics")).unwrap();
    assert_eq!(
        next_snapshot(&a).error.as_deref(),
        Some("panics source panicked")
    );
    a.refresh(topic("panics")).unwrap();
    assert_eq!(next_snapshot(&a).value, Some(json!("recovered")));
    let err = a
        .call(topic("panics"), "x", json!({}), Duration::from_secs(5))
        .unwrap_err();
    assert_eq!(err, "panics source panicked");
}

#[test]
fn the_broker_exits_once_idle() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _f, handle) = start(dir.path(), 10);
    drop(Client::connect_or_spawn(&paths, 10, &[]).unwrap());
    thread::sleep(Duration::from_millis(1500));
    assert!(handle.is_finished());
    assert!(
        !paths.socket.exists(),
        "the socket is removed on the way out"
    );
}

#[test]
fn an_older_broker_steps_aside_for_a_newer_client() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _f, handle) = start(dir.path(), 10);
    let other = Client::connect_or_spawn(&paths, 10, &[]).unwrap();
    // No spawn command: after the restart nothing new starts, so connect fails.
    let err = Client::connect_or_spawn(&paths, 11, &[]).err();
    assert!(err.is_some());
    thread::sleep(Duration::from_millis(300));
    assert!(handle.is_finished());
    assert!(
        !paths.socket.exists(),
        "the socket is removed on the way out"
    );
    // Every other pane is told, rather than left waiting on a dead broker.
    assert!(matches!(
        other.events().recv_timeout(Duration::from_secs(5)).unwrap(),
        Event::Disconnected(_)
    ));
}

#[test]
fn a_newer_broker_serves_an_older_client() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _f, _h) = start(dir.path(), 10);
    assert!(Client::connect_or_spawn(&paths, 9, &[]).is_ok());
}

#[test]
fn an_exiting_broker_leaves_a_successors_socket_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (paths, _f, handle) = start(dir.path(), 10);
    // A successor binds the same path, as a freshly spawned broker would.
    std::fs::remove_file(&paths.socket).unwrap();
    let _successor = std::os::unix::net::UnixListener::bind(&paths.socket).unwrap();
    thread::sleep(Duration::from_millis(1000));
    assert!(handle.is_finished());
    assert!(paths.socket.exists(), "the successor's socket survives");
}

#[test]
fn two_clients_racing_share_one_broker() {
    // The spawn command only appends to a marker file, so no broker ever
    // comes up and both clients eventually fail. What is under test is the
    // lock: between them they must run the spawn command exactly once.
    let dir = tempfile::tempdir().unwrap();
    let paths = BrokerPaths::in_dir(dir.path(), dir.path());
    let marker = dir.path().join("spawned");
    let spawn = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        format!("echo x >> {}", marker.display()),
    ];
    let a = {
        let (p, s) = (paths.clone(), spawn.clone());
        thread::spawn(move || Client::connect_or_spawn(&p, 1, &s))
    };
    let b = {
        let (p, s) = (paths.clone(), spawn.clone());
        thread::spawn(move || Client::connect_or_spawn(&p, 1, &s))
    };
    assert!(a.join().unwrap().is_err());
    assert!(b.join().unwrap().is_err());
    let spawns = std::fs::read_to_string(&marker)
        .unwrap_or_default()
        .lines()
        .count();
    assert_eq!(spawns, 1, "the lock let exactly one client spawn a broker");
}
