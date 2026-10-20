//! The three queries. All exist because `gh`'s own `--json` flag cannot
//! produce the fields the panes require: `gh search prs --json` has no check
//! state, and `gh pr view --json` has no unresolved-review-thread count.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;
use zax_domain::{PrFilter, RepoPattern};

/// Variables: `q` (the GitHub search query string).
pub const SEARCH_REVIEW_REQUESTS: &str = r#"
query($q: String!) {
  search(query: $q, type: ISSUE, first: 50) {
    nodes {
      ... on PullRequest {
        number
        title
        url
        isDraft
        createdAt
        repository { nameWithOwner }
        author { login }
        commits(last: 1) {
          nodes { commit { statusCheckRollup { state } } }
        }
      }
    }
  }
}
"#;

/// Variables: `owner`, `name`, `branch`.
///
/// `reviewThreads` is capped at 100. A pull request with more than a hundred
/// review threads will under-report; that is an acceptable limit for a
/// glanceable pane.
pub const BRANCH_STATUS: &str = r#"
query($owner: String!, $name: String!, $branch: String!) {
  repository(owner: $owner, name: $name) {
    pullRequests(headRefName: $branch, states: OPEN, first: 1) {
      nodes {
        number
        title
        url
        headRefName
        isDraft
        createdAt
        mergeable
        reviewDecision
        repository { nameWithOwner }
        author { login }
        commits(last: 1) {
          nodes { commit { statusCheckRollup { state } } }
        }
        reviewThreads(first: 100) { nodes { isResolved } }
      }
    }
  }
}
"#;

/// Variables: `q` (the GitHub search query string).
///
/// `BRANCH_STATUS`'s field set inside a search. The review search cannot be
/// reused - it carries no `reviewDecision`, `mergeable` or `reviewThreads` -
/// and `BRANCH_STATUS` cannot, because it is scoped to one branch of one
/// repository.
///
/// The same two caps as the queries above: 50 results, and `reviewThreads`
/// capped at 100 per pull request.
pub const SEARCH_AUTHORED: &str = r#"
query($q: String!) {
  search(query: $q, type: ISSUE, first: 50) {
    nodes {
      ... on PullRequest {
        number
        title
        url
        headRefName
        isDraft
        createdAt
        mergeable
        reviewDecision
        repository { nameWithOwner }
        author { login }
        commits(last: 1) {
          nodes { commit { statusCheckRollup { state } } }
        }
        reviewThreads(first: 100) { nodes { isResolved } }
      }
    }
  }
}
"#;

/// The `repo:` and `-repo:` qualifiers a filter translates into exactly.
///
/// An `owner/*` entry has no qualifier: GitHub spells an organisation `org:`
/// and a person `user:`, and which an owner is cannot be known here. One such
/// entry therefore drops the whole include list, because sending the rest
/// would ask for less than the filter admits. An exclusion only ever widens
/// what is left out, so dropping a wildcard one loses nothing the local
/// filter will not catch.
fn repo_qualifiers(filter: &PrFilter) -> String {
    let mut qualifiers = String::new();
    let exact: Option<Vec<String>> = filter.include.iter().map(RepoPattern::exact_slug).collect();
    for slug in exact.unwrap_or_default() {
        qualifiers.push_str(&format!(" repo:{slug}"));
    }
    for slug in filter.exclude.iter().filter_map(RepoPattern::exact_slug) {
        qualifiers.push_str(&format!(" -repo:{slug}"));
    }
    qualifiers
}

/// The `my-prs` pane's search. No `created:` qualifier: `max_age` is the
/// review queue's knob, and ageing out your own oldest pull request would
/// hide the one most worth seeing.
pub fn authored_query(login: &str, filter: &PrFilter) -> String {
    format!(
        "is:pr is:open archived:false author:{login}{}",
        repo_qualifiers(filter)
    )
}

/// The search string the spec's `gh search prs --review-requested=@me
/// --state=open` stands for. GitHub's search API does not understand `@me`, so
/// the login is resolved first.
///
/// The filter is folded in as search qualifiers so that the 50-result cap
/// applies to a queue that has already been narrowed. Only what translates
/// exactly is sent, and only where sending it cannot hide a pull request the
/// filter itself admits - `PrFilter` remains the authority over what the pane
/// shows, and this never has to agree with it, only to be no stricter.
pub fn review_requested_query(login: &str, filter: &PrFilter, now: SystemTime) -> String {
    let mut query = format!(
        "is:pr is:open archived:false review-requested:{login}{}",
        repo_qualifiers(filter)
    );
    if let Some(date) = created_since(now, filter.max_age) {
        query.push_str(&format!(" created:>={date}"));
    }
    query
}

/// The `created:>=` date for an age threshold, rounded down to the whole day.
///
/// `created:>=2023-10-31` admits everything from that midnight on, which is
/// earlier than the threshold itself. Rounding the other way would exclude
/// pull requests the filter admits, and the search is only ever allowed to be
/// looser than the filter.
fn created_since(now: SystemTime, max_age: Option<Duration>) -> Option<String> {
    let at = now.checked_sub(max_age?)?;
    let secs = i64::try_from(at.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()?;
    let date = OffsetDateTime::from_unix_timestamp(secs).ok()?.date();
    Some(format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    ))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use zax_domain::{PrFilter, RepoPattern};

    use super::*;

    /// 2023-11-14T22:13:20Z.
    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }

    fn patterns(raw: &[&str]) -> Vec<RepoPattern> {
        raw.iter().map(|p| RepoPattern::parse(p).unwrap()).collect()
    }

    #[test]
    fn an_unfiltered_search_asks_for_the_whole_review_queue() {
        let q = review_requested_query("octocat", &PrFilter::default(), now());
        assert_eq!(
            q,
            "is:pr is:open archived:false review-requested:octocat"
        );
    }

    #[test]
    fn exact_includes_become_repo_qualifiers() {
        let filter = PrFilter {
            include: patterns(&["acme/omega", "gameldar/zax"]),
            ..PrFilter::default()
        };
        let q = review_requested_query("octocat", &filter, now());
        assert!(q.contains("repo:acme/omega"), "got: {q}");
        assert!(q.contains("repo:gameldar/zax"), "got: {q}");
    }

    #[test]
    fn one_wildcard_include_suppresses_every_include_qualifier() {
        // GitHub splits `org:` from `user:` and we cannot tell which an owner
        // is, so `acme/*` has no qualifier. Sending the other entries alone
        // would narrow the search to less than the filter admits, which would
        // silently hide pull requests the pane is supposed to show.
        let filter = PrFilter {
            include: patterns(&["acme/*", "gameldar/zax"]),
            ..PrFilter::default()
        };
        let q = review_requested_query("octocat", &filter, now());
        assert!(!q.contains("repo:"), "got: {q}");
    }

    #[test]
    fn exact_excludes_become_negated_repo_qualifiers() {
        let filter = PrFilter {
            exclude: patterns(&["acme/legacy-api"]),
            ..PrFilter::default()
        };
        let q = review_requested_query("octocat", &filter, now());
        assert!(q.contains("-repo:acme/legacy-api"), "got: {q}");
    }

    #[test]
    fn a_wildcard_exclude_is_left_to_the_local_filter() {
        // Dropping one exclusion only widens the search, so the others can
        // still be sent; the pane's own filter removes what is left.
        let filter = PrFilter {
            exclude: patterns(&["acme/*", "gameldar/other"]),
            ..PrFilter::default()
        };
        let q = review_requested_query("octocat", &filter, now());
        assert!(!q.contains("acme"), "got: {q}");
        assert!(q.contains("-repo:gameldar/other"), "got: {q}");
    }

    #[test]
    fn max_age_becomes_a_created_qualifier_on_the_day_the_threshold_falls_in() {
        let filter = PrFilter {
            max_age: Some(Duration::from_secs(14 * 86_400)),
            ..PrFilter::default()
        };
        let q = review_requested_query("octocat", &filter, now());
        // 1_700_000_000 - 14 days is 2023-10-31T22:13:20Z, and the qualifier
        // rounds down to that whole day so it can never exclude a pull request
        // the filter itself would admit.
        assert!(q.contains("created:>=2023-10-31"), "got: {q}");
    }

    #[test]
    fn a_threshold_older_than_the_epoch_asks_for_no_date_at_all() {
        let filter = PrFilter {
            max_age: Some(Duration::from_secs(365 * 86_400)),
            ..PrFilter::default()
        };
        let q = review_requested_query("octocat", &filter, UNIX_EPOCH);
        assert!(!q.contains("created:"), "got: {q}");
    }

    #[test]
    fn the_authored_search_asks_for_your_own_open_pull_requests() {
        let q = authored_query("octocat", &PrFilter::default());
        assert_eq!(q, "is:pr is:open archived:false author:octocat");
    }

    #[test]
    fn the_authored_search_carries_the_repository_qualifiers() {
        let filter = PrFilter {
            include: patterns(&["acme/omega"]),
            exclude: patterns(&["acme/legacy-api"]),
            ..PrFilter::default()
        };
        let q = authored_query("octocat", &filter);
        assert!(q.contains("repo:acme/omega"), "got: {q}");
        assert!(q.contains("-repo:acme/legacy-api"), "got: {q}");
    }

    #[test]
    fn the_authored_search_never_carries_a_date_qualifier() {
        // max_age is the review queue's knob. Sending it here would hide the
        // oldest pull request of your own, which is the one most worth seeing.
        let filter = PrFilter {
            max_age: Some(Duration::from_secs(14 * 86_400)),
            ..PrFilter::default()
        };
        let q = authored_query("octocat", &filter);
        assert!(!q.contains("created:"), "got: {q}");
    }
}
