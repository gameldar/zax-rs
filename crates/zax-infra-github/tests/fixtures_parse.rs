#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use std::time::{Duration, UNIX_EPOCH};

use zax_domain::{CheckState, Mergeable, ReviewState};
use zax_infra_github::dto;

#[test]
fn a_search_response_becomes_pull_requests() {
    let json = include_str!("fixtures/search_prs.json");
    let prs = dto::parse_search_response(json).unwrap();

    assert_eq!(prs.len(), 3, "the non-pull-request node must be skipped");

    assert_eq!(prs[0].repo.to_string(), "acme/omega");
    assert_eq!(prs[0].number, 1963);
    assert_eq!(prs[0].short_ref(), "omega#1963");
    assert_eq!(prs[0].checks, CheckState::Passing);
    assert!(!prs[0].is_draft);
    // 2026-09-15T09:12:33Z
    assert_eq!(
        prs[0].created_at,
        UNIX_EPOCH + Duration::from_secs(1_789_463_553)
    );

    assert_eq!(prs[1].checks, CheckState::Failing);
    assert!(prs[1].is_draft);

    assert_eq!(prs[2].checks, CheckState::None, "no rollup means no checks");
}

#[test]
fn a_branch_status_response_counts_only_unresolved_threads() {
    let json = include_str!("fixtures/branch_status.json");
    let status = dto::parse_branch_status_response(json).unwrap().unwrap();

    assert_eq!(status.unresolved_threads, 2);
    assert_eq!(status.review, ReviewState::ChangesRequested);
    assert_eq!(status.mergeable, Mergeable::Conflicting);
    assert_eq!(status.pr.checks, CheckState::Pending);
    assert_eq!(status.pr.number, 1963);
}

#[test]
fn a_branch_with_no_open_pull_request_is_none_not_an_error() {
    let json = include_str!("fixtures/branch_status_empty.json");
    assert!(dto::parse_branch_status_response(json).unwrap().is_none());
}

#[test]
fn a_pr_head_response_names_the_fork_it_came_from() {
    let json = include_str!("fixtures/pr_head.json");
    let head = dto::parse_pr_head_response(json).unwrap();

    assert_eq!(head.number, 1963);
    assert_eq!(head.head_ref, "patch-1");
    assert_eq!(
        head.head_repo.map(|r| r.to_string()).as_deref(),
        Some("a-contributor/omega")
    );
    assert!(head.is_cross_repository);
}

#[test]
fn a_pull_request_whose_fork_was_deleted_still_parses() {
    // GitHub nulls both head repository fields once the fork is gone, which is
    // routine for an old pull request. refs/pull/<n>/head survives the
    // deletion, so the review this feeds would work; refusing to read the
    // answer made the operation impossible over a field it did not need.
    let json = include_str!("fixtures/pr_head_deleted_fork.json");
    let head = dto::parse_pr_head_response(json).unwrap();

    assert_eq!(head.number, 1963);
    assert_eq!(head.head_ref, "patch-1");
    assert_eq!(head.head_repo, None);
}

#[test]
fn a_pull_request_missing_a_field_is_an_error_not_a_disappearance() {
    // A node that is not a pull request is `{}` and is skipped. A node that IS
    // one but lacks an identifying field must not take the same path, or a real
    // pull request leaves the review queue with nothing to say it has gone.
    let json = include_str!("fixtures/search_prs.json");

    let without_repo = json.replacen(
        r#""repository": { "nameWithOwner": "acme/omega" },"#,
        "",
        1,
    );
    assert_ne!(
        without_repo, json,
        "the fixture shape changed; fix this test"
    );
    let err = dto::parse_search_response(&without_repo).unwrap_err();
    assert!(err.contains("repository"), "got: {err}");

    let without_number = json.replacen(r#""number": 1963,"#, "", 1);
    assert_ne!(
        without_number, json,
        "the fixture shape changed; fix this test"
    );
    let err = dto::parse_search_response(&without_number).unwrap_err();
    assert!(err.contains("number"), "got: {err}");

    // And the genuine non-pull-request node is still skipped, not reported.
    assert_eq!(dto::parse_search_response(json).unwrap().len(), 3);
}

#[test]
fn a_graphql_errors_payload_is_reported_rather_than_parsed_as_empty() {
    let json = r#"{"errors":[{"message":"Could not resolve to a Repository"}]}"#;
    let err = dto::parse_search_response(json).unwrap_err();
    assert!(err.contains("Could not resolve"), "got: {err}");
}

#[test]
fn malformed_json_is_reported_rather_than_panicking() {
    assert!(dto::parse_search_response("not json").is_err());
}

#[test]
fn the_authored_search_parses_every_state_the_pane_renders() {
    let json = include_str!("fixtures/authored_prs.json");
    let parsed = dto::parse_authored_search_response(json).unwrap();

    // Four pull requests and one `{}`, which is a node that is not a pull
    // request and is skipped.
    assert_eq!(parsed.len(), 4);

    assert_eq!(parsed[0].pr.number, 12);
    assert_eq!(parsed[0].review, ReviewState::ChangesRequested);
    assert_eq!(parsed[0].mergeable, Mergeable::Conflicting);
    assert_eq!(parsed[0].unresolved_threads, 1);
    assert_eq!(parsed[0].pr.checks, CheckState::Failing);

    assert_eq!(parsed[1].review, ReviewState::Approved);
    assert_eq!(parsed[1].unresolved_threads, 0);
    assert_eq!(parsed[1].pr.checks, CheckState::Passing);

    assert!(parsed[2].pr.is_draft);
    assert_eq!(parsed[2].mergeable, Mergeable::Unknown);
    assert_eq!(parsed[2].pr.checks, CheckState::None);

    assert_eq!(parsed[3].unresolved_threads, 3);
}

#[test]
fn an_authored_pull_request_missing_its_repository_is_an_error() {
    // A dropped row would take a pull request out of the queue with nothing
    // to say it had gone. `{}` is skipped; a half-built pull request is not.
    let json = r#"{"data":{"search":{"nodes":[{"number":1}]}}}"#;
    assert!(dto::parse_authored_search_response(json).is_err());
}

#[test]
fn graphql_errors_on_the_authored_search_are_not_an_empty_queue() {
    let json = r#"{"errors":[{"message":"rate limited"}]}"#;
    let err = dto::parse_authored_search_response(json).unwrap_err();
    assert!(err.contains("rate limited"), "got: {err}");
}

#[test]
fn an_authored_pull_request_carries_its_head_branch() {
    let json = include_str!("fixtures/authored_prs.json");
    let prs = dto::parse_authored_search_response(json).unwrap();
    let first = prs.first().unwrap();
    assert_eq!(
        first.head.as_ref().map(|b| b.as_str()),
        Some("OMEGA-1963-stamp")
    );
}
