//! The two queries. Both exist because `gh`'s own `--json` flag cannot produce
//! the fields the spec's panes require: `gh search prs --json` has no check
//! state, and `gh pr view --json` has no unresolved-review-thread count.

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
        isDraft
        createdAt
        mergeable
        reviewDecision
        repository { nameWithOwner }
        commits(last: 1) {
          nodes { commit { statusCheckRollup { state } } }
        }
        reviewThreads(first: 100) { nodes { isResolved } }
      }
    }
  }
}
"#;

/// The search string the spec's `gh search prs --review-requested=@me
/// --state=open` stands for. GitHub's search API does not understand `@me`, so
/// the login is resolved first.
pub fn review_requested_query(login: &str) -> String {
    format!("is:pr is:open archived:false review-requested:{login}")
}
