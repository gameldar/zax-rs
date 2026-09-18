//! Last-good-result caching with stale marking.
//!
//! A pane caches its last good result and marks itself stale on failure rather
//! than blanking. A gh hiccup must never look like an empty review queue.

use std::time::SystemTime;

use crate::fmt::format_age;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cached<T> {
    value: Option<T>,
    fetched_at: Option<SystemTime>,
    error: Option<String>,
}

impl<T> Default for Cached<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Cached<T> {
    pub fn new() -> Self {
        Self {
            value: None,
            fetched_at: None,
            error: None,
        }
    }

    pub fn record_ok(&mut self, value: T, at: SystemTime) {
        self.value = Some(value);
        self.fetched_at = Some(at);
        self.error = None;
    }

    /// Keeps whatever was there before.
    pub fn record_err(&mut self, message: String, _at: SystemTime) {
        self.error = Some(message);
    }

    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn is_stale(&self) -> bool {
        self.error.is_some()
    }

    /// The right-hand side of a pane header.
    pub fn status(&self, now: SystemTime) -> String {
        let age = self
            .fetched_at
            .map(|at| format_age(now.duration_since(at).unwrap_or_default()));

        match (&self.error, age) {
            (None, None) => "loading".to_owned(),
            (None, Some(age)) => age,
            (Some(error), None) => format!("stale - {error}"),
            (Some(error), Some(age)) => format!("stale {age} - {error}"),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;

    fn t(secs: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn a_fresh_cache_has_nothing_and_says_so() {
        let cache: Cached<u32> = Cached::new();
        assert_eq!(cache.value(), None);
        assert!(!cache.is_stale());
        assert_eq!(cache.status(t(100)), "loading");
    }

    #[test]
    fn a_good_result_is_remembered_with_its_age() {
        let mut cache = Cached::new();
        cache.record_ok(7, t(100));
        assert_eq!(cache.value(), Some(&7));
        assert!(!cache.is_stale());
        assert_eq!(cache.status(t(100)), "now");
        assert_eq!(cache.status(t(280)), "3m");
    }

    #[test]
    fn a_failure_keeps_the_last_good_value_and_marks_the_pane_stale() {
        // A gh hiccup must never look like an empty review queue.
        let mut cache = Cached::new();
        cache.record_ok(vec![1, 2, 3], t(100));
        cache.record_err("gh: HTTP 502".to_owned(), t(160));

        assert_eq!(cache.value(), Some(&vec![1, 2, 3]));
        assert!(cache.is_stale());
        assert_eq!(cache.error(), Some("gh: HTTP 502"));
        assert!(
            cache.status(t(160)).starts_with("stale"),
            "{}",
            cache.status(t(160))
        );
        assert!(
            cache.status(t(160)).contains("1m"),
            "the age is of the last good result"
        );
    }

    #[test]
    fn a_later_success_clears_the_stale_mark() {
        let mut cache = Cached::new();
        cache.record_ok(1, t(100));
        cache.record_err("boom".to_owned(), t(160));
        cache.record_ok(2, t(220));
        assert!(!cache.is_stale());
        assert_eq!(cache.error(), None);
        assert_eq!(cache.value(), Some(&2));
    }

    #[test]
    fn a_failure_with_nothing_cached_yet_reports_the_error_itself() {
        let mut cache: Cached<u32> = Cached::new();
        cache.record_err("gh is not installed".to_owned(), t(100));
        assert_eq!(cache.value(), None);
        assert!(cache.is_stale());
        assert!(cache.status(t(100)).contains("gh is not installed"));
    }
}
