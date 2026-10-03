//! What zax knows about pull requests, independent of any forge.

use std::fmt;
use std::time::{Duration, SystemTime};

use thiserror::Error;

use crate::branch::BranchName;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoSlug {
    owner: String,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RepoSlugError {
    #[error("`{0}` is not a repository; write it as owner/name")]
    Malformed(String),
    #[error("repository `{0}` contains a control character")]
    ControlCharacter(String),
}

impl RepoSlug {
    /// Beyond `owner/name` with both sides non-empty, the only characters
    /// refused are control characters. The charset is deliberately not
    /// restricted further: a slug normally arrives from `gh`, which has already
    /// validated it, and hard-coding what a forge may call a repository would
    /// reject legitimate names on GitHub Enterprise or after any future change
    /// to GitHub's own rules. Being too strict here breaks real repositories;
    /// being permissive only passes a bad name through to `gh`, which rejects
    /// it with a better message than this crate could write.
    ///
    /// Control characters are the exception because a NUL cannot survive
    /// `exec` at all: the spawn fails with a bare "invalid input parameter"
    /// that names neither the repository nor the argument it came from.
    pub fn parse(raw: &str) -> Result<Self, RepoSlugError> {
        let malformed = || RepoSlugError::Malformed(raw.to_owned());
        let (owner, name) = raw.split_once('/').ok_or_else(malformed)?;
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return Err(malformed());
        }
        if raw.chars().any(char::is_control) {
            return Err(RepoSlugError::ControlCharacter(raw.to_owned()));
        }
        Ok(Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
        })
    }

    pub fn owner(&self) -> &str {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl fmt::Display for RepoSlug {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// The rolled-up state of a head commit's checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckState {
    Passing,
    Failing,
    Pending,
    /// No checks are configured, or none have reported.
    None,
}

/// GitHub's `reviewDecision`. There is deliberately no `Commented` variant:
/// GitHub does not report one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    ReviewRequired,
    /// No review is required and none has been given.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mergeable {
    Mergeable,
    Conflicting,
    /// GitHub is still computing the merge commit.
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequest {
    pub repo: RepoSlug,
    pub number: u32,
    pub title: String,
    pub url: String,
    pub is_draft: bool,
    pub created_at: SystemTime,
    pub checks: CheckState,
}

impl PullRequest {
    /// How old the pull request is against an injected clock. A clock that has
    /// gone backwards yields zero rather than an error: a pane must not fail
    /// over a skewed timestamp.
    pub fn age(&self, now: SystemTime) -> Duration {
        now.duration_since(self.created_at)
            .unwrap_or(Duration::ZERO)
    }

    /// `name#number`, for a pane row that has no width for the owner.
    pub fn short_ref(&self) -> String {
        format!("{}#{}", self.repo.name(), self.number)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchStatus {
    pub pr: PullRequest,
    pub review: ReviewState,
    pub unresolved_threads: u32,
    pub mergeable: Mergeable,
}

/// What the `branch-status` pane reports on: where HEAD is, and the pull
/// request status if the branch has one.
///
/// An enum rather than a pair of options because a status without a branch is
/// not a state this can be in: the status is the forge's answer about a branch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchView {
    /// HEAD is detached, so there is no branch to report on.
    Detached,
    OnBranch {
        branch: BranchName,
        status: Option<BranchStatus>,
    },
}

impl BranchView {
    pub fn on_branch(branch: BranchName, status: Option<BranchStatus>) -> Self {
        Self::OnBranch { branch, status }
    }

    pub fn branch(&self) -> Option<&BranchName> {
        match self {
            Self::Detached => None,
            Self::OnBranch { branch, .. } => Some(branch),
        }
    }

    pub fn status(&self) -> Option<&BranchStatus> {
        match self {
            Self::Detached => None,
            Self::OnBranch { status, .. } => status.as_ref(),
        }
    }
}

/// How a user names a pull request on the command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrRef {
    Number(u32),
    Url(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PrRefError {
    #[error("`{0}` is not a pull request; give a number like 1963 or an https pull URL")]
    Malformed(String),
}

impl PrRef {
    /// The host is deliberately not checked. `gh` talks to GitHub Enterprise as
    /// well as github.com, selected by `GH_HOST` or by how the repository is
    /// configured, so deciding here which hosts are acceptable would reject
    /// URLs that `gh` handles perfectly well. For the same reason a URL with no
    /// number after `/pull/` is accepted: `gh` rejects it with a message about
    /// the actual request, and reimplementing its URL parsing would only create
    /// a second place to be wrong.
    pub fn parse(raw: &str) -> Result<Self, PrRefError> {
        let malformed = || PrRefError::Malformed(raw.to_owned());
        let trimmed = raw.strip_prefix('#').unwrap_or(raw);

        if !trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_digit()) {
            return trimmed.parse().map(Self::Number).map_err(|_| malformed());
        }
        if raw.starts_with("https://") && raw.contains("/pull/") {
            return Ok(Self::Url(raw.to_owned()));
        }
        Err(malformed())
    }

    /// The repository the pull request lives in, when the reference names one.
    ///
    /// A URL states it: `https://<host>/<owner>/<name>/pull/<n>`. A bare number
    /// does not - the forge resolves it against the current directory - so that
    /// case is `None` rather than a guess. `None` is also the answer for a URL
    /// whose shape is not recognised, because a reference this cannot read must
    /// not be reported as belonging to some other repository.
    pub fn repository(&self) -> Option<RepoSlug> {
        let Self::Url(url) = self else { return None };
        let rest = url.strip_prefix("https://")?;
        let mut segments = rest.split('/');
        let _host = segments.next()?;
        let owner = segments.next()?;
        let name = segments.next()?;
        if segments.next()? != "pull" {
            return None;
        }
        RepoSlug::parse(&format!("{owner}/{name}")).ok()
    }

    /// How to hand this to `gh pr view`.
    pub fn as_gh_arg(&self) -> String {
        match self {
            Self::Number(n) => n.to_string(),
            Self::Url(u) => u.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use super::*;

    fn pr() -> PullRequest {
        PullRequest {
            repo: RepoSlug::parse("acme/omega").unwrap(),
            number: 1963,
            title: "Export reports".to_owned(),
            url: "https://github.com/acme/omega/pull/1963".to_owned(),
            is_draft: false,
            created_at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            checks: CheckState::Passing,
        }
    }

    #[test]
    fn a_repo_slug_splits_owner_from_name() {
        let slug = RepoSlug::parse("acme/omega").unwrap();
        assert_eq!(slug.owner(), "acme");
        assert_eq!(slug.name(), "omega");
        assert_eq!(slug.to_string(), "acme/omega");
    }

    #[test]
    fn a_repo_slug_rejects_anything_that_is_not_owner_slash_name() {
        for bad in ["", "omega", "acme/", "/omega", "a/b/c"] {
            assert!(
                RepoSlug::parse(bad).is_err(),
                "expected `{bad}` to be rejected"
            );
        }
    }

    #[test]
    fn a_pull_request_reports_its_age_against_an_injected_clock() {
        let now = UNIX_EPOCH + Duration::from_secs(1_700_259_200);
        assert_eq!(pr().age(now), Duration::from_secs(259_200));
    }

    #[test]
    fn age_is_zero_rather_than_negative_when_the_clock_is_behind() {
        let now = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        assert_eq!(pr().age(now), Duration::ZERO);
    }

    #[test]
    fn a_pull_request_has_a_compact_reference() {
        assert_eq!(pr().short_ref(), "omega#1963");
    }

    #[test]
    fn a_pr_ref_parses_a_bare_number() {
        assert_eq!(PrRef::parse("1963").unwrap(), PrRef::Number(1963));
        assert_eq!(PrRef::parse("#1963").unwrap(), PrRef::Number(1963));
        assert_eq!(PrRef::Number(1963).as_gh_arg(), "1963");
    }

    #[test]
    fn a_pr_ref_parses_a_url() {
        let url = "https://github.com/acme/omega/pull/1963";
        assert_eq!(PrRef::parse(url).unwrap(), PrRef::Url(url.to_owned()));
        assert_eq!(PrRef::Url(url.to_owned()).as_gh_arg(), url);
    }

    #[test]
    fn a_pull_url_says_which_repository_the_pull_request_is_in() {
        // The expected slug is written out here, not derived from the URL by
        // the same splitting the method does.
        let pr = PrRef::parse("https://github.com/acme/omega/pull/1963").unwrap();
        let repo = pr.repository().unwrap();
        assert_eq!(repo.owner(), "acme");
        assert_eq!(repo.name(), "omega");

        // Enterprise hosts and deeper paths read the same way.
        assert_eq!(
            PrRef::parse("https://git.example.com/acme/widgets/pull/7/files")
                .unwrap()
                .repository()
                .unwrap()
                .to_string(),
            "acme/widgets"
        );
    }

    #[test]
    fn a_reference_that_names_no_repository_says_so_rather_than_guessing() {
        // A bare number is resolved against the current directory, and a URL
        // this cannot read must not be reported as some other repository.
        assert_eq!(PrRef::Number(1963).repository(), None);
        for odd in [
            "https://github.com/acme/pull/1963",
            "https://github.com/pull/1963",
        ] {
            assert_eq!(
                PrRef::Url(odd.to_owned()).repository(),
                None,
                "`{odd}` names no repository"
            );
        }
    }

    #[test]
    fn a_pr_ref_rejects_anything_else() {
        for bad in ["", "not-a-pr", "ftp://example.com/1", "-3"] {
            assert!(
                PrRef::parse(bad).is_err(),
                "expected `{bad}` to be rejected"
            );
        }
    }

    #[test]
    fn a_pull_url_on_another_host_is_accepted_for_github_enterprise() {
        // `gh` serves GitHub Enterprise as well as github.com, so deciding here
        // which hosts are legitimate would reject URLs that work.
        assert!(PrRef::parse("https://git.example.com/o/r/pull/7").is_ok());
    }

    #[test]
    fn a_repo_slug_rejects_control_characters() {
        // A NUL cannot survive exec, and the spawn failure names neither the
        // repository nor the argument, so catch it here instead.
        assert!(matches!(
            RepoSlug::parse("acme/no\0rth"),
            Err(RepoSlugError::ControlCharacter(_))
        ));
        assert!(matches!(
            RepoSlug::parse("acme/no\nrth"),
            Err(RepoSlugError::ControlCharacter(_))
        ));
    }

    #[test]
    fn a_repo_slug_keeps_an_open_charset_otherwise() {
        // Too strict here rejects real repositories; too loose only passes a
        // bad name to gh, which reports it better than this crate could.
        assert!(RepoSlug::parse("café/résumé").is_ok());
    }

    #[test]
    fn the_state_enums_are_copy_so_a_pane_can_read_them_through_a_reference() {
        // `prs_pane` does `check_glyph(pr.checks)` with only a `&PullRequest` in
        // hand. That compiles only while these enums are `Copy`.
        fn by_value(
            c: CheckState,
            r: ReviewState,
            m: Mergeable,
        ) -> (CheckState, ReviewState, Mergeable) {
            (c, r, m)
        }
        let pr = pr();
        let taken = by_value(pr.checks, ReviewState::Approved, Mergeable::Unknown);
        assert_eq!(
            taken,
            (
                CheckState::Passing,
                ReviewState::Approved,
                Mergeable::Unknown
            )
        );
        assert_eq!(
            pr.checks,
            CheckState::Passing,
            "copied, not moved out of pr"
        );
    }

    #[test]
    fn a_branch_status_carries_its_unresolved_thread_count() {
        let status = BranchStatus {
            pr: pr(),
            review: ReviewState::ChangesRequested,
            unresolved_threads: 3,
            mergeable: Mergeable::Conflicting,
        };
        assert_eq!(status.unresolved_threads, 3);
        assert_eq!(status.pr.number, 1963);
    }

    #[test]
    fn a_branch_view_names_the_branch_it_is_reporting_on() {
        let branch = BranchName::parse_local("OMEGA-1963-stamp").unwrap();
        let view = BranchView::on_branch(branch.clone(), None);
        assert_eq!(view.branch(), Some(&branch));
        assert_eq!(view.status(), None);
    }

    #[test]
    fn a_detached_head_has_no_branch_and_no_status() {
        let view = BranchView::Detached;
        assert_eq!(view.branch(), None);
        assert_eq!(view.status(), None);
    }

    #[test]
    fn a_branch_view_carries_the_pull_request_status_when_there_is_one() {
        let status = BranchStatus {
            pr: pr(),
            review: ReviewState::Approved,
            unresolved_threads: 0,
            mergeable: Mergeable::Mergeable,
        };
        let view = BranchView::on_branch(
            BranchName::parse_local("OMEGA-1963-stamp").unwrap(),
            Some(status.clone()),
        );
        assert_eq!(view.status(), Some(&status));
    }

    #[test]
    fn systemtime_keeps_the_domain_dependency_free() {
        // Guards the "thiserror and nothing else" budget: if this compiles with
        // SystemTime, no date-time crate has crept into zax-domain.
        let _: SystemTime = pr().created_at;
    }
}
