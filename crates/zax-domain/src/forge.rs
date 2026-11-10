//! What zax knows about pull requests, independent of any forge.

use std::fmt;
use std::num::NonZeroU32;
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
    #[error("`{0}` names a directory, not a repository")]
    PathComponent(String),
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
    /// that names neither the repository nor the argument it came from. So
    /// are `.` and `..`, because a slug's name is joined onto `projects.root`
    /// to make a candidate path: no forge has a repository called `..`, and
    /// accepting one would put back the traversal `ProjectsRoot` rejects.
    pub fn parse(raw: &str) -> Result<Self, RepoSlugError> {
        let malformed = || RepoSlugError::Malformed(raw.to_owned());
        let (owner, name) = raw.split_once('/').ok_or_else(malformed)?;
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return Err(malformed());
        }
        if raw.chars().any(char::is_control) {
            return Err(RepoSlugError::ControlCharacter(raw.to_owned()));
        }
        if matches!(owner, "." | "..") || matches!(name, "." | "..") {
            return Err(RepoSlugError::PathComponent(raw.to_owned()));
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

    /// Read a slug out of a remote URL, in any of the spellings git accepts
    /// for a push or fetch URL: scp syntax (`user@host:owner/name[.git]`),
    /// `ssh://[user@]host[:port]/owner/name[.git]`, or `http(s)://host/owner/
    /// name[.git][/]`. Anything else - a bare filesystem path, a host with no
    /// owner, three or more path segments - is `None` rather than a guess,
    /// because a remote this cannot read must not be reported as some other
    /// repository.
    pub fn from_remote_url(url: &str) -> Option<Self> {
        let trimmed = url.trim();

        let path = if let Some(rest) = trimmed.strip_prefix("ssh://") {
            rest.split_once('/')?.1
        } else if let Some(rest) = trimmed.strip_prefix("https://") {
            rest.split_once('/')?.1
        } else if let Some(rest) = trimmed.strip_prefix("http://") {
            rest.split_once('/')?.1
        } else {
            // scp syntax: `user@host:owner/name`. The colon has to come before
            // any slash, or this is a path that merely contains one.
            let (host, path) = trimmed.split_once(':')?;
            if host.contains('/') {
                return None;
            }
            path
        };

        let path = path.strip_suffix('/').unwrap_or(path);
        let path = path.strip_suffix(".git").unwrap_or(path);

        let mut segments = path.split('/');
        let owner = segments.next()?;
        let name = segments.next()?;
        if owner.is_empty() || name.is_empty() || segments.next().is_some() {
            return None;
        }

        Self::parse(&format!("{owner}/{name}")).ok()
    }

    /// Whether two slugs name the same repository, ignoring case: GitHub
    /// treats `Owner/Name` and `owner/name` as one repository, so a project
    /// override or an origin check must too.
    pub fn same_repository(&self, other: &Self) -> bool {
        self.owner.to_lowercase() == other.owner.to_lowercase()
            && self.name.to_lowercase() == other.name.to_lowercase()
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
    /// The login that opened it. Absent when the account has since been
    /// deleted: GitHub reports no author rather than a placeholder.
    pub author: Option<String>,
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
pub struct PullRequestStatus {
    pub pr: PullRequest,
    pub review: ReviewState,
    pub unresolved_threads: u32,
    pub mergeable: Mergeable,
    /// The branch the pull request merges from, when the forge reported one
    /// that parses. Tickets are joined to pull requests through it.
    pub head: Option<BranchName>,
}

/// Why a pull request of yours is waiting on you.
///
/// `rank` is the ranking, and it is what both the `my-prs` pane's sort order
/// and its row glyph are built from: one ordering, so a row can never sort
/// above one whose glyph looks more urgent.
///
/// The thread count is carried but does not rank. Two pull requests with open
/// comments are equally stuck, and `Ord` derived over the payload would order
/// them by comment count, which is not a thing anybody wants sorted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attention {
    Conflicting,
    ChangesRequested,
    ChecksFailing,
    Unresolved(NonZeroU32),
    ReadyToMerge,
    Waiting,
    Draft,
}

impl Attention {
    pub fn rank(self) -> u8 {
        match self {
            Self::Conflicting => 0,
            Self::ChangesRequested => 1,
            Self::ChecksFailing => 2,
            Self::Unresolved(_) => 3,
            Self::ReadyToMerge => 4,
            Self::Waiting => 5,
            Self::Draft => 6,
        }
    }

    /// A draft is tested first and returns immediately: it is work you parked,
    /// so it ranks last whatever else is true of it. Everything after that is
    /// worst-first.
    pub fn of(status: &PullRequestStatus) -> Self {
        if status.pr.is_draft {
            return Self::Draft;
        }
        if status.mergeable == Mergeable::Conflicting {
            return Self::Conflicting;
        }
        if status.review == ReviewState::ChangesRequested {
            return Self::ChangesRequested;
        }
        if status.pr.checks == CheckState::Failing {
            return Self::ChecksFailing;
        }
        if let Some(count) = NonZeroU32::new(status.unresolved_threads) {
            return Self::Unresolved(count);
        }
        if status.review == ReviewState::Approved
            && status.pr.checks == CheckState::Passing
            && status.mergeable == Mergeable::Mergeable
        {
            return Self::ReadyToMerge;
        }
        Self::Waiting
    }
}

/// A pull request of yours, with the verdict on it.
///
/// It lives in the domain rather than beside the use case that builds it
/// because `zax-tui` depends on `zax-domain` alone. Pairing the verdict with
/// the pull request also keeps the ranking in one place: the pane draws the
/// `Attention` the use case sorted on instead of recomputing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MyPullRequest {
    pub attention: Attention,
    pub status: PullRequestStatus,
}

impl MyPullRequest {
    pub fn new(status: PullRequestStatus) -> Self {
        Self {
            attention: Attention::of(&status),
            status,
        }
    }
}

/// Why a pull request you already reviewed is still yours to look at.
///
/// No variant holds "no open threads and no push": that pull request is not a
/// revisit, and `of` returns `None` for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Revisit {
    /// Your threads are open and nothing has been pushed since you looked.
    OpenThreads(NonZeroU32),
    /// The author pushed since your last review; none of your threads are open.
    Updated,
    /// Both.
    UpdatedWithOpenThreads(NonZeroU32),
}

impl Revisit {
    /// `None` is the pull request that has nothing left for you.
    pub fn of(open_threads: u32, head_moved: bool) -> Option<Self> {
        match (NonZeroU32::new(open_threads), head_moved) {
            (None, false) => None,
            (None, true) => Some(Self::Updated),
            (Some(count), false) => Some(Self::OpenThreads(count)),
            (Some(count), true) => Some(Self::UpdatedWithOpenThreads(count)),
        }
    }
}

/// Where a pull request in the review queue stands with you.
///
/// `rank` orders the `prs-for-review` pane; the thread count takes no part in
/// it, as with `Attention`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerState {
    /// Your review is requested, and you have not reviewed it before.
    Requested,
    /// Your review is requested again: you have reviewed it before.
    ReRequested,
    Revisit(Revisit),
}

impl ReviewerState {
    pub fn rank(self) -> u8 {
        match self {
            Self::Revisit(Revisit::Updated | Revisit::UpdatedWithOpenThreads(_)) => 0,
            Self::Requested | Self::ReRequested => 1,
            Self::Revisit(Revisit::OpenThreads(_)) => 2,
        }
    }

    /// The review a fresh agent should run on this pull request: a first look,
    /// or a follow-up on a review you already left.
    pub fn review_pass(self) -> ReviewPass {
        match self {
            Self::Requested => ReviewPass::First,
            Self::ReRequested | Self::Revisit(_) => ReviewPass::Again,
        }
    }
}

/// Whether a review agent looks at a pull request for the first time or
/// follows up on a review you already left. Typed rather than a bool so a call
/// site reads `Again`, not an unlabelled `true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewPass {
    First,
    Again,
}

/// Whether a review agent reviews the one pull request, or every pull request
/// in the stack it belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewScope {
    Single,
    Stack,
}

/// The review a fresh agent runs on a pull request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewRun {
    pub pass: ReviewPass,
    pub scope: ReviewScope,
}

impl ReviewRun {
    /// A review of just this pull request.
    #[must_use]
    pub const fn single(pass: ReviewPass) -> Self {
        Self {
            pass,
            scope: ReviewScope::Single,
        }
    }

    /// A review of every pull request in this one's stack.
    #[must_use]
    pub const fn stack(pass: ReviewPass) -> Self {
        Self {
            pass,
            scope: ReviewScope::Stack,
        }
    }
}

/// A row of the `prs-for-review` pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewQueueItem {
    pub pr: PullRequest,
    pub state: ReviewerState,
}

/// What the forge reports for a pull request you reviewed; `Revisit::of`
/// decides whether it is a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewedPullRequest {
    pub pr: PullRequest,
    /// Unresolved threads whose first comment is yours.
    pub my_open_threads: u32,
    /// The head differs from the commit of your latest review, or that commit is gone.
    pub head_moved: bool,
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
        // Boxed: a status is several times the size of the detached variant.
        status: Option<Box<PullRequestStatus>>,
    },
}

impl BranchView {
    pub fn on_branch(branch: BranchName, status: Option<PullRequestStatus>) -> Self {
        Self::OnBranch {
            branch,
            status: status.map(Box::new),
        }
    }

    pub fn branch(&self) -> Option<&BranchName> {
        match self {
            Self::Detached => None,
            Self::OnBranch { branch, .. } => Some(branch),
        }
    }

    pub fn status(&self) -> Option<&PullRequestStatus> {
        match self {
            Self::Detached => None,
            Self::OnBranch { status, .. } => status.as_deref(),
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
    use std::num::NonZeroU32;
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
            author: None,
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
    fn a_slug_component_that_is_a_relative_path_is_refused() {
        // `Projects::candidate` joins the name onto `projects.root`, which
        // went to some trouble to reject `..`; a name of `..` would put the
        // traversal straight back.
        for bad in ["owner/..", "owner/.", "../name", "./name", "../.."] {
            assert!(
                matches!(RepoSlug::parse(bad), Err(RepoSlugError::PathComponent(_))),
                "expected `{bad}` to be rejected"
            );
        }

        // A name that merely starts with a dot is a real repository name.
        assert!(RepoSlug::parse("owner/.github").is_ok());
        assert!(RepoSlug::parse("owner/..omega").is_ok());
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
    fn every_common_origin_spelling_parses_to_its_slug() {
        let urls = [
            "git@github.com:acme/omega.git",
            "git@github.com:acme/omega",
            "https://github.com/acme/omega",
            "https://github.com/acme/omega.git",
            "https://github.com/acme/omega.git/",
            "ssh://git@github.com/acme/omega.git",
            "ssh://git@github.com:22/acme/omega.git",
            " https://github.com/acme/omega\n",
        ];
        for url in urls {
            assert_eq!(
                RepoSlug::from_remote_url(url).map(|slug| slug.to_string()),
                Some("acme/omega".to_owned()),
                "expected `{url}` to parse"
            );
        }
    }

    #[test]
    fn a_remote_that_names_no_owner_and_name_is_none() {
        for url in [
            "/srv/git/omega.git",
            "https://github.com/acme",
            "https://github.com/a/b/c",
            "",
        ] {
            assert_eq!(
                RepoSlug::from_remote_url(url),
                None,
                "expected `{url}` to be None"
            );
        }
    }

    #[test]
    fn slugs_compare_without_regard_to_case() {
        let acme_capitalised = RepoSlug::parse("Acme/Omega").unwrap();
        let acme = RepoSlug::parse("acme/omega").unwrap();
        let someone = RepoSlug::parse("someone/omega").unwrap();

        assert!(acme_capitalised.same_repository(&acme));
        assert!(!acme.same_repository(&someone));
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
        let status = PullRequestStatus {
            pr: pr(),
            review: ReviewState::ChangesRequested,
            unresolved_threads: 3,
            mergeable: Mergeable::Conflicting,
            head: None,
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
        let status = PullRequestStatus {
            pr: pr(),
            review: ReviewState::Approved,
            unresolved_threads: 0,
            mergeable: Mergeable::Mergeable,
            head: None,
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

    fn status(review: ReviewState, threads: u32, mergeable: Mergeable) -> PullRequestStatus {
        PullRequestStatus {
            pr: pr(),
            review,
            unresolved_threads: threads,
            mergeable,
            head: None,
        }
    }

    #[test]
    fn the_ranking_is_the_declared_one() {
        // Written out rather than produced by sorting, so the test is an
        // oracle and not a second copy of the code.
        let ranked = [
            Attention::Conflicting,
            Attention::ChangesRequested,
            Attention::ChecksFailing,
            Attention::Unresolved(NonZeroU32::new(1).unwrap()),
            Attention::ReadyToMerge,
            Attention::Waiting,
            Attention::Draft,
        ];
        let ranks: Vec<u8> = ranked.iter().map(|a| a.rank()).collect();
        assert_eq!(ranks, vec![0, 1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn the_thread_count_takes_no_part_in_the_ranking() {
        // Two pull requests with open comments are equally stuck. The tie is
        // broken by age, in the use case, not by how many comments are open.
        let one = Attention::Unresolved(NonZeroU32::new(1).unwrap());
        let many = Attention::Unresolved(NonZeroU32::new(40).unwrap());
        assert_eq!(one.rank(), many.rank());
    }

    #[test]
    fn a_conflict_outranks_everything_but_a_draft() {
        let s = status(ReviewState::ChangesRequested, 5, Mergeable::Conflicting);
        assert_eq!(Attention::of(&s), Attention::Conflicting);
    }

    #[test]
    fn changes_requested_outranks_a_red_build_and_open_comments() {
        let s = status(ReviewState::ChangesRequested, 5, Mergeable::Mergeable);
        assert_eq!(Attention::of(&s), Attention::ChangesRequested);
    }

    #[test]
    fn a_red_build_outranks_open_comments() {
        let mut s = status(ReviewState::ReviewRequired, 5, Mergeable::Mergeable);
        s.pr.checks = CheckState::Failing;
        assert_eq!(Attention::of(&s), Attention::ChecksFailing);
    }

    #[test]
    fn open_comments_carry_their_count() {
        let s = status(ReviewState::ReviewRequired, 3, Mergeable::Mergeable);
        assert_eq!(
            Attention::of(&s),
            Attention::Unresolved(NonZeroU32::new(3).unwrap())
        );
    }

    #[test]
    fn no_open_comments_is_not_an_unresolved_state() {
        // `Unresolved(0)` is not a state this can be in, and NonZeroU32 is
        // what makes that true rather than a convention.
        let s = status(ReviewState::ReviewRequired, 0, Mergeable::Mergeable);
        assert_ne!(Attention::of(&s).rank(), 3);
    }

    #[test]
    fn ready_to_merge_needs_approval_a_green_build_and_a_clean_merge() {
        let ready = status(ReviewState::Approved, 0, Mergeable::Mergeable);
        assert_eq!(Attention::of(&ready), Attention::ReadyToMerge);

        // Each leg removed in turn is no longer ready, and none of them is a
        // problem that outranks it either.
        let mut pending = ready.clone();
        pending.pr.checks = CheckState::Pending;
        assert_eq!(Attention::of(&pending), Attention::Waiting);

        let unknown = status(ReviewState::Approved, 0, Mergeable::Unknown);
        assert_eq!(Attention::of(&unknown), Attention::Waiting);

        let unreviewed = status(ReviewState::ReviewRequired, 0, Mergeable::Mergeable);
        assert_eq!(Attention::of(&unreviewed), Attention::Waiting);
    }

    #[test]
    fn an_unknown_merge_state_is_not_a_conflict() {
        // GitHub reports Unknown while it computes the merge commit. Treating
        // it as a problem makes a freshly pushed pull request jump to the top
        // of the pane and fall back down again seconds later.
        let s = status(ReviewState::ReviewRequired, 0, Mergeable::Unknown);
        assert_eq!(Attention::of(&s), Attention::Waiting);
    }

    #[test]
    fn a_draft_ranks_last_whatever_else_is_wrong_with_it() {
        // A draft is work you parked. Its row still carries its own glyphs,
        // so a conflicting draft is visible even while it sits at the bottom.
        let mut s = status(ReviewState::ChangesRequested, 9, Mergeable::Conflicting);
        s.pr.is_draft = true;
        assert_eq!(Attention::of(&s), Attention::Draft);
    }

    #[test]
    fn a_pull_request_pairs_with_the_verdict_on_it() {
        let s = status(ReviewState::Approved, 0, Mergeable::Mergeable);
        let mine = MyPullRequest::new(s.clone());
        assert_eq!(mine.attention, Attention::ReadyToMerge);
        assert_eq!(mine.status, s);
    }

    fn n(count: u32) -> NonZeroU32 {
        NonZeroU32::new(count).unwrap()
    }

    #[test]
    fn revisit_of_no_threads_and_no_push_is_none() {
        assert_eq!(Revisit::of(0, false), None);
    }

    #[test]
    fn revisit_of_a_push_and_no_threads_is_updated() {
        assert_eq!(Revisit::of(0, true), Some(Revisit::Updated));
    }

    #[test]
    fn revisit_of_threads_and_no_push_carries_the_count() {
        assert_eq!(Revisit::of(3, false), Some(Revisit::OpenThreads(n(3))));
    }

    #[test]
    fn revisit_of_threads_and_a_push_is_both() {
        assert_eq!(
            Revisit::of(2, true),
            Some(Revisit::UpdatedWithOpenThreads(n(2)))
        );
    }

    #[test]
    fn the_review_queue_ranking_is_the_declared_one() {
        let ranked = [
            ReviewerState::Revisit(Revisit::Updated),
            ReviewerState::Revisit(Revisit::UpdatedWithOpenThreads(n(1))),
            ReviewerState::Requested,
            ReviewerState::ReRequested,
            ReviewerState::Revisit(Revisit::OpenThreads(n(1))),
        ];
        let ranks: Vec<u8> = ranked.iter().map(|s| s.rank()).collect();
        assert_eq!(ranks, vec![0, 0, 1, 1, 2]);
    }

    #[test]
    fn only_a_first_request_is_a_first_review_pass() {
        assert_eq!(ReviewerState::Requested.review_pass(), ReviewPass::First);
        for again in [
            ReviewerState::ReRequested,
            ReviewerState::Revisit(Revisit::Updated),
            ReviewerState::Revisit(Revisit::OpenThreads(n(1))),
            ReviewerState::Revisit(Revisit::UpdatedWithOpenThreads(n(1))),
        ] {
            assert_eq!(again.review_pass(), ReviewPass::Again);
        }
    }

    #[test]
    fn open_thread_counts_do_not_change_the_rank() {
        let one = ReviewerState::Revisit(Revisit::OpenThreads(n(1)));
        let many = ReviewerState::Revisit(Revisit::OpenThreads(n(40)));
        assert_eq!(one.rank(), many.rank());
        let one = ReviewerState::Revisit(Revisit::UpdatedWithOpenThreads(n(1)));
        let many = ReviewerState::Revisit(Revisit::UpdatedWithOpenThreads(n(40)));
        assert_eq!(one.rank(), many.rank());
    }
}
