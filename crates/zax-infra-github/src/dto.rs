//! GitHub's JSON, and the mapping into domain types. Every field is optional
//! here so a shape change degrades a row rather than failing the pane.

use std::time::SystemTime;

use serde::Deserialize;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use zax_app::PullRequestHead;
use zax_domain::{BranchStatus, CheckState, Mergeable, PullRequest, RepoSlug, ReviewState};

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
    // A node that is not a pull request comes back as `{}` because the inline
    // fragment did not match. Skip it.
    Ok(data
        .search
        .nodes
        .into_iter()
        .filter_map(to_pull_request)
        .collect())
}

pub fn parse_branch_status_response(json: &str) -> Result<Option<BranchStatus>, String> {
    let data: RepositoryData = unwrap_envelope(json)?;
    let Some(repository) = data.repository else {
        return Ok(None);
    };
    let Some(node) = repository.pull_requests.nodes.into_iter().next() else {
        return Ok(None);
    };

    let unresolved_threads = node
        .review_threads
        .as_ref()
        .map(|t| t.nodes.iter().filter(|n| !n.is_resolved).count())
        .unwrap_or(0);
    let review = review_state(node.review_decision.as_deref());
    let mergeable = mergeable_state(node.mergeable.as_deref());

    let Some(pr) = to_pull_request(node) else {
        return Err("pull request node was missing required fields".to_owned());
    };

    Ok(Some(BranchStatus {
        pr,
        review,
        unresolved_threads: u32::try_from(unresolved_threads).unwrap_or(u32::MAX),
        mergeable,
    }))
}

pub fn parse_pr_head_response(json: &str) -> Result<PullRequestHead, String> {
    let parsed: PrHeadJson = serde_json::from_str(json).map_err(|e| format!("{e}"))?;
    let owner = parsed
        .head_repository_owner
        .ok_or_else(|| "pull request has no head repository owner".to_owned())?
        .login;
    let name = parsed
        .head_repository
        .ok_or_else(|| "pull request has no head repository".to_owned())?
        .name;
    let head_repo = RepoSlug::parse(&format!("{owner}/{name}")).map_err(|e| e.to_string())?;

    Ok(PullRequestHead {
        number: parsed.number,
        head_ref: parsed.head_ref_name,
        head_repo,
        is_cross_repository: parsed.is_cross_repository,
    })
}

fn to_pull_request(node: PrNode) -> Option<PullRequest> {
    let repo = RepoSlug::parse(&node.repository?.name_with_owner).ok()?;
    Some(PullRequest {
        repo,
        number: node.number?,
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
