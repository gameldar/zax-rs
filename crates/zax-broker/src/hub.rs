//! The broker's bookkeeping, free of threads and sockets so it can be tested
//! with nothing but a clock value.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;
use zax_protocol::{Snapshot, SourceKey};

pub type ClientId = u64;

#[derive(Debug, Clone, PartialEq)]
pub struct Subscribed {
    /// The cache, when there is one, to send the new subscriber at once.
    pub snapshot: Option<Snapshot>,
    /// The hub had no such source: the caller must start its worker.
    pub created: bool,
}

#[derive(Debug)]
struct Entry {
    subscribers: BTreeSet<ClientId>,
    value: Option<Value>,
    fetched_at: Option<u64>,
    error: Option<String>,
    due: Instant,
    in_flight: bool,
    /// A refresh arrived during a fetch: run once more when it lands.
    again: bool,
}

impl Entry {
    fn snapshot(&self, key: &SourceKey) -> Snapshot {
        Snapshot {
            key: key.clone(),
            value: self.value.clone(),
            fetched_at: self.fetched_at,
            error: self.error.clone(),
        }
    }

    fn has_result(&self) -> bool {
        self.value.is_some() || self.error.is_some()
    }
}

#[derive(Debug, Default)]
pub struct Hub {
    entries: BTreeMap<SourceKey, Entry>,
    clients: BTreeSet<ClientId>,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&mut self, client: ClientId, key: SourceKey, now: Instant) -> Subscribed {
        self.clients.insert(client);
        let mut created = false;
        let entry = self.entries.entry(key.clone()).or_insert_with(|| {
            created = true;
            Entry {
                subscribers: BTreeSet::new(),
                value: None,
                fetched_at: None,
                error: None,
                due: now,
                in_flight: false,
                again: false,
            }
        });
        entry.subscribers.insert(client);
        Subscribed {
            snapshot: entry.has_result().then(|| entry.snapshot(&key)),
            created,
        }
    }

    pub fn disconnect(&mut self, client: ClientId) {
        self.clients.remove(&client);
        for entry in self.entries.values_mut() {
            entry.subscribers.remove(&client);
        }
    }

    pub fn client_count(&self) -> usize {
        self.clients.len()
    }

    /// Mark due now. Returns true if a fetch should start (none in flight).
    pub fn refresh(&mut self, key: &SourceKey) -> bool {
        let Some(entry) = self.entries.get_mut(key) else {
            return false;
        };
        if entry.in_flight {
            entry.again = true;
            return false;
        }
        entry.in_flight = true;
        true
    }

    /// Keys whose fetch should start now; marks them in flight.
    pub fn due(&mut self, now: Instant) -> Vec<SourceKey> {
        let mut due = Vec::new();
        for (key, entry) in &mut self.entries {
            if !entry.in_flight && !entry.subscribers.is_empty() && entry.due <= now {
                entry.in_flight = true;
                due.push(key.clone());
            }
        }
        due
    }

    /// Record a finished fetch; returns who to send which snapshot to.
    #[allow(clippy::too_many_arguments)]
    pub fn finish(
        &mut self,
        key: &SourceKey,
        result: Result<Value, String>,
        at: SystemTime,
        now: Instant,
        interval: Duration,
        roll: f64,
    ) -> Vec<(ClientId, Snapshot)> {
        let Some(entry) = self.entries.get_mut(key) else {
            return Vec::new();
        };
        match result {
            Ok(value) => {
                entry.value = Some(value);
                entry.fetched_at = at
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs());
                entry.error = None;
            }
            Err(error) => entry.error = Some(error),
        }
        entry.in_flight = false;
        entry.due = if entry.again {
            entry.again = false;
            now
        } else {
            now + next_delay(interval, 0.15, roll)
        };
        let snapshot = entry.snapshot(key);
        entry
            .subscribers
            .iter()
            .map(|client| (*client, snapshot.clone()))
            .collect()
    }

    /// Earliest due time across subscribed, idle sources.
    pub fn next_due(&self) -> Option<Instant> {
        self.entries
            .values()
            .filter(|e| !e.in_flight && !e.subscribers.is_empty())
            .map(|e| e.due)
            .min()
    }

    /// Keys with no subscribers left; the caller tears their workers down.
    pub fn orphans(&mut self) -> Vec<SourceKey> {
        let orphans: Vec<SourceKey> = self
            .entries
            .iter()
            .filter(|(_, e)| e.subscribers.is_empty())
            .map(|(k, _)| k.clone())
            .collect();
        for key in &orphans {
            self.entries.remove(key);
        }
        orphans
    }
}

/// `base` spread by up to `jitter_fraction` either way: `roll` 0 is the
/// shortest delay, 1 the longest, 0.5 exactly `base`. The same formula the
/// panes use, so a source polls no more eagerly than a pane would.
pub fn next_delay(base: Duration, jitter_fraction: f64, roll: f64) -> Duration {
    let roll = if roll.is_nan() {
        0.5
    } else {
        roll.clamp(0.0, 1.0)
    };
    let jitter = jitter_fraction.clamp(0.0, 1.0);
    let scale = 1.0 + jitter * (2.0 * roll - 1.0);
    let secs = (base.as_secs_f64() * scale).max(1.0);
    Duration::from_secs_f64(secs)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::*;

    fn key(topic: &str) -> SourceKey {
        SourceKey {
            topic: topic.into(),
            source: "{}".into(),
        }
    }

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn the_first_subscriber_creates_the_source_and_it_is_due_at_once() {
        let mut hub = Hub::new();
        let t0 = Instant::now();
        let s = hub.subscribe(1, key("tickets"), t0);
        assert!(s.created);
        assert_eq!(s.snapshot, None);
        assert_eq!(hub.due(t0), vec![key("tickets")]);
        assert!(
            hub.due(t0).is_empty(),
            "a source in flight is not due again"
        );
    }

    #[test]
    fn a_result_fans_out_to_every_subscriber() {
        let mut hub = Hub::new();
        let t0 = Instant::now();
        hub.subscribe(1, key("tickets"), t0);
        hub.subscribe(2, key("tickets"), t0);
        hub.due(t0);
        let sent = hub.finish(
            &key("tickets"),
            Ok(json!([1])),
            UNIX_EPOCH + Duration::from_secs(100),
            t0,
            MIN,
            0.5,
        );
        let mut clients: Vec<ClientId> = sent.iter().map(|(c, _)| *c).collect();
        clients.sort_unstable();
        assert_eq!(clients, [1, 2]);
        assert_eq!(sent[0].1.value, Some(json!([1])));
        assert_eq!(sent[0].1.fetched_at, Some(100));
    }

    #[test]
    fn a_late_subscriber_gets_the_cache_without_a_new_fetch() {
        let mut hub = Hub::new();
        let t0 = Instant::now();
        hub.subscribe(1, key("tickets"), t0);
        hub.due(t0);
        hub.finish(
            &key("tickets"),
            Ok(json!("v")),
            SystemTime::now(),
            t0,
            MIN,
            0.5,
        );
        let s = hub.subscribe(2, key("tickets"), t0);
        assert!(!s.created);
        assert_eq!(s.snapshot.unwrap().value, Some(json!("v")));
        assert!(hub.due(t0).is_empty());
    }

    #[test]
    fn a_failure_keeps_the_last_good_value_and_adds_the_error() {
        let mut hub = Hub::new();
        let t0 = Instant::now();
        hub.subscribe(1, key("t"), t0);
        hub.due(t0);
        hub.finish(
            &key("t"),
            Ok(json!(1)),
            UNIX_EPOCH + Duration::from_secs(5),
            t0,
            MIN,
            0.5,
        );
        hub.refresh(&key("t"));
        hub.due(t0);
        let sent = hub.finish(
            &key("t"),
            Err("acli: 502".into()),
            SystemTime::now(),
            t0,
            MIN,
            0.5,
        );
        assert_eq!(sent[0].1.value, Some(json!(1)));
        assert_eq!(sent[0].1.fetched_at, Some(5));
        assert_eq!(sent[0].1.error.as_deref(), Some("acli: 502"));
    }

    #[test]
    fn a_refresh_during_a_fetch_rides_on_it_then_runs_once_more() {
        let mut hub = Hub::new();
        let t0 = Instant::now();
        hub.subscribe(1, key("t"), t0);
        hub.due(t0);
        assert!(
            !hub.refresh(&key("t")),
            "in flight, so no second fetch starts now"
        );
        assert!(!hub.refresh(&key("t")));
        hub.finish(&key("t"), Ok(json!(1)), SystemTime::now(), t0, MIN, 0.5);
        assert_eq!(hub.due(t0), vec![key("t")], "exactly one follow-up fetch");
    }

    #[test]
    fn the_next_poll_is_the_interval_with_jitter() {
        let mut hub = Hub::new();
        let t0 = Instant::now();
        hub.subscribe(1, key("t"), t0);
        hub.due(t0);
        hub.finish(&key("t"), Ok(json!(1)), SystemTime::now(), t0, MIN, 0.5);
        assert_eq!(hub.next_due(), Some(t0 + MIN), "a roll of 0.5 is no jitter");
        assert!(hub.due(t0 + MIN - Duration::from_secs(1)).is_empty());
        assert_eq!(hub.due(t0 + MIN), vec![key("t")]);
    }

    #[test]
    fn a_source_nobody_watches_is_an_orphan() {
        let mut hub = Hub::new();
        let t0 = Instant::now();
        hub.subscribe(1, key("a"), t0);
        hub.subscribe(2, key("a"), t0);
        hub.subscribe(2, key("b"), t0);
        hub.disconnect(2);
        assert_eq!(hub.orphans(), vec![key("b")]);
        assert_eq!(hub.client_count(), 1);
        hub.disconnect(1);
        assert_eq!(hub.orphans(), vec![key("a")]);
        assert!(hub.due(t0).is_empty());
    }

    #[test]
    fn a_result_for_a_removed_source_is_dropped() {
        let mut hub = Hub::new();
        let t0 = Instant::now();
        hub.subscribe(1, key("a"), t0);
        hub.due(t0);
        hub.disconnect(1);
        hub.orphans();
        assert!(hub
            .finish(&key("a"), Ok(json!(1)), SystemTime::now(), t0, MIN, 0.5)
            .is_empty());
    }

    #[test]
    fn jitter_spreads_the_delay_either_side_of_the_base() {
        assert_eq!(next_delay(MIN, 0.15, 0.0), Duration::from_secs(51));
        assert_eq!(next_delay(MIN, 0.15, 1.0), Duration::from_secs(69));
    }
}
