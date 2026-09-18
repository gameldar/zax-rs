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
    assert_eq!(head.head_repo.to_string(), "a-contributor/omega");
    assert!(head.is_cross_repository);
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
