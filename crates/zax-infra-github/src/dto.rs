//! GitHub's JSON, and the mapping into domain types.
//!
//! Optional fields are used where an absent value has a sensible reading: a
//! missing title shows as blank, a missing rollup means no checks. They are NOT
//! a general shape-change defence, and it is worth being precise about that,
//! because the reassuring version of this sentence hid a real bug.
//!
//! A pull request that arrives without the fields identifying it is reported as
//! an error, not skipped. Skipping is reserved for a node that is not a pull
//! request at all, which `search(type: ISSUE)` returns as `{}`. The difference
//! matters: a dropped row would take a pull request out of the review queue
//! with nothing to say it had gone, and a review queue that is quietly short is
//! the one failure this pane must never have.
//!
//! Nested fields that GitHub's schema declares non-nullable are typed
//! non-optional, so a rename there fails the whole response. That is deliberate.
//! Showing a confidently wrong queue is worse than showing an error.

use std::time::SystemTime;

use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use zax_app::PullRequestHead;
use zax_domain::{
    BranchName, CheckState, Mergeable, PullRequest, PullRequestStatus, RepoSlug, ReviewState,
};

#[derive(Debug, Deserialize)]
struct Envelope<T> {
    data: Option<T>,
    errors: Option<Vec<GraphqlError>>,
}

#[derive(Debug, Deserialize)]
struct GraphqlError {
    message: String,
}

#[derive(Debug, Deserialize)]
struct SearchData {
    search: Nodes<PrNode>,
}

#[derive(Debug, Deserialize)]
struct RepositoryData {
    repository: Option<RepositoryNode>,
}

#[derive(Debug, Deserialize)]
struct RepositoryNode {
    #[serde(rename = "pullRequests")]
    pull_requests: Nodes<PrNode>,
}

#[derive(Debug, Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PrNode {
    number: Option<u32>,
    title: Option<String>,
    url: Option<String>,
    #[serde(rename = "headRefName")]
    head_ref_name: Option<String>,
    #[serde(rename = "isDraft")]
    is_draft: Option<bool>,
    #[serde(rename = "createdAt")]
    created_at: Option<String>,
    mergeable: Option<String>,
    #[serde(rename = "reviewDecision")]
    review_decision: Option<String>,
    repository: Option<RepoRef>,
    commits: Option<Nodes<CommitNode>>,
    #[serde(rename = "reviewThreads")]
    review_threads: Option<Nodes<ThreadNode>>,
}

#[derive(Debug, Deserialize)]
struct RepoRef {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

#[derive(Debug, Deserialize)]
struct CommitNode {
    commit: CommitInner,
}

#[derive(Debug, Deserialize)]
struct CommitInner {
    #[serde(rename = "statusCheckRollup")]
    status_check_rollup: Option<Rollup>,
}

#[derive(Debug, Deserialize)]
struct Rollup {
    state: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ThreadNode {
    #[serde(rename = "isResolved")]
    is_resolved: bool,
}

impl PrNode {
    /// Is this the `{}` a search returns for a node that is not a pull request?
    ///
    /// Every field absent means the inline fragment did not match. One field
    /// absent means a pull request came back in a shape we did not expect, and
    /// the two must not be confused: the first is routine, the second is a
    /// pull request about to disappear from somebody's review queue.
    fn is_empty_node(&self) -> bool {
        self.number.is_none()
            && self.title.is_none()
            && self.url.is_none()
            && self.head_ref_name.is_none()
            && self.is_draft.is_none()
            && self.created_at.is_none()
            && self.mergeable.is_none()
            && self.review_decision.is_none()
            && self.repository.is_none()
            && self.commits.is_none()
            && self.review_threads.is_none()
    }
}

#[derive(Debug, Deserialize)]
struct PrHeadJson {
    number: u32,
    #[serde(rename = "headRefName")]
    head_ref_name: String,
    #[serde(rename = "headRepository")]
    head_repository: Option<NamedRepo>,
    #[serde(rename = "headRepositoryOwner")]
    head_repository_owner: Option<Login>,
    #[serde(rename = "isCrossRepository")]
    is_cross_repository: bool,
}

#[derive(Debug, Deserialize)]
struct NamedRepo {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Login {
    login: String,
}

fn unwrap_envelope<T>(json: &str) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    let envelope: Envelope<T> = serde_json::from_str(json).map_err(|e| format!("{e}"))?;
    if let Some(errors) = envelope.errors {
        if !errors.is_empty() {
            let joined = errors
                .into_iter()
                .map(|e| e.message)
                .collect::<Vec<_>>()
                .join("; ");
            return Err(joined);
        }
    }
    envelope
        .data
        .ok_or_else(|| "GraphQL response had neither data nor errors".to_owned())
}

pub fn parse_search_response(json: &str) -> Result<Vec<PullRequest>, String> {
    let data: SearchData = unwrap_envelope(json)?;
    data.search
        .nodes
        .into_iter()
        // `{}` is a node that is not a pull request: the inline fragment did
        // not match and nothing at all came back. That one is skipped.
        .filter(|node| !node.is_empty_node())
        // Everything else presented itself as a pull request, so a field it
        // should have carried is a shape change worth reporting rather than a
        // row to drop quietly.
        .map(to_pull_request)
        .collect()
}

pub fn parse_authored_search_response(json: &str) -> Result<Vec<PullRequestStatus>, String> {
    let data: SearchData = unwrap_envelope(json)?;
    data.search
        .nodes
        .into_iter()
        .filter(|node| !node.is_empty_node())
        .map(to_pull_request_status)
        .collect()
}

pub fn parse_branch_status_response(json: &str) -> Result<Option<PullRequestStatus>, String> {
    let data: RepositoryData = unwrap_envelope(json)?;
    let Some(repository) = data.repository else {
        return Ok(None);
    };
    let Some(node) = repository.pull_requests.nodes.into_iter().next() else {
        return Ok(None);
    };
    to_pull_request_status(node).map(Some)
}

fn to_pull_request_status(node: PrNode) -> Result<PullRequestStatus, String> {
    let unresolved_threads = node
        .review_threads
        .as_ref()
        .map(|t| t.nodes.iter().filter(|n| !n.is_resolved).count())
        .unwrap_or(0);
    let review = review_state(node.review_decision.as_deref());
    let mergeable = mergeable_state(node.mergeable.as_deref());
    let head = node
        .head_ref_name
        .as_deref()
        .and_then(|raw| BranchName::parse_foreign(raw).ok());
    let pr = to_pull_request(node)?;

    Ok(PullRequestStatus {
        pr,
        review,
        unresolved_threads: u32::try_from(unresolved_threads).unwrap_or(u32::MAX),
        mergeable,
        head,
    })
}

pub fn parse_pr_head_response(json: &str) -> Result<PullRequestHead, String> {
    let parsed: PrHeadJson = serde_json::from_str(json).map_err(|e| format!("{e}"))?;
    // GitHub nulls both fields once the head fork has been deleted, which is
    // routine for an old pull request, and `refs/pull/<n>/head` survives the
    // deletion. Making them required turned a reviewable pull request into a
    // parse failure. Absent is a value here; whoever needs the head repository
    // says so where it is needed.
    let head_repo = match (parsed.head_repository_owner, parsed.head_repository) {
        (Some(owner), Some(repo)) => Some(
            RepoSlug::parse(&format!("{}/{}", owner.login, repo.name))
                .map_err(|e| e.to_string())?,
        ),
        _ => None,
    };

    Ok(PullRequestHead {
        number: parsed.number,
        head_ref: parsed.head_ref_name,
        head_repo,
        is_cross_repository: parsed.is_cross_repository,
    })
}

fn to_pull_request(node: PrNode) -> Result<PullRequest, String> {
    let repository = node
        .repository
        .ok_or("a pull request arrived with no repository")?;
    let repo = RepoSlug::parse(&repository.name_with_owner).map_err(|e| e.to_string())?;

    Ok(PullRequest {
        repo,
        number: node.number.ok_or("a pull request arrived with no number")?,
        // These have a sensible absent reading, so they degrade rather than
        // fail: a blank title is legible, a missing rollup means no checks.
        title: node.title.unwrap_or_default(),
        url: node.url.unwrap_or_default(),
        is_draft: node.is_draft.unwrap_or(false),
        created_at: parse_timestamp(node.created_at.as_deref()),
        checks: check_state(node.commits.as_ref()),
    })
}

/// An unparseable timestamp becomes the epoch, which renders as a very old
/// pull request rather than failing the whole pane.
fn parse_timestamp(raw: Option<&str>) -> SystemTime {
    raw.and_then(|text| OffsetDateTime::parse(text, &Rfc3339).ok())
        .map(SystemTime::from)
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

fn check_state(commits: Option<&Nodes<CommitNode>>) -> CheckState {
    let state = commits
        .and_then(|c| c.nodes.first())
        .and_then(|n| n.commit.status_check_rollup.as_ref())
        .and_then(|r| r.state.as_deref());

    match state {
        Some("SUCCESS") => CheckState::Passing,
        Some("FAILURE" | "ERROR") => CheckState::Failing,
        Some("PENDING" | "EXPECTED") => CheckState::Pending,
        _ => CheckState::None,
    }
}

fn review_state(decision: Option<&str>) -> ReviewState {
    match decision {
        Some("APPROVED") => ReviewState::Approved,
        Some("CHANGES_REQUESTED") => ReviewState::ChangesRequested,
        Some("REVIEW_REQUIRED") => ReviewState::ReviewRequired,
        _ => ReviewState::None,
    }
}

fn mergeable_state(value: Option<&str>) -> Mergeable {
    match value {
        Some("MERGEABLE") => Mergeable::Mergeable,
        Some("CONFLICTING") => Mergeable::Conflicting,
        _ => Mergeable::Unknown,
    }
}
