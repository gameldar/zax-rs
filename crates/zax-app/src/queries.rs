//! The read-only use cases, plus worktree removal and `zax doctor`.

use std::path::PathBuf;
use std::time::SystemTime;

use thiserror::Error;
use zax_domain::{
    BranchName, BranchView, MyPullRequest, PrFilter, PullRequest, RootRepoName, WorktreePath,
    WorktreePathError, WorktreeRoot,
};

use crate::ports::{FileSystem, Forge, GitRepo, Multiplexer, PortError, WorktreeEntry};

/// The `prs-for-review` pane's data source.
///
/// The filter is applied here even though the forge was given it too. A forge
/// can only narrow what it is able to express, and its search is capped, so
/// asking it to narrow is worth doing - but this is the only place that decides
/// what the pane shows.
pub fn list_review_requests<Fg: Forge>(
    forge: &Fg,
    filter: &PrFilter,
    now: SystemTime,
) -> Result<Vec<PullRequest>, PortError> {
    Ok(filter.apply(forge.review_requests(filter)?, now))
}

/// The `my-prs` pane's data source.
///
/// It takes no clock, and the absent parameter is the point: `[prs] max_age`
/// plays no part here, so `admits_repo` is what this calls rather than
/// `admits`. Your own three-week-old pull request is the one that is rotting.
pub fn list_my_pull_requests<Fg: Forge>(
    forge: &Fg,
    filter: &PrFilter,
) -> Result<Vec<MyPullRequest>, PortError> {
    let mut mine: Vec<MyPullRequest> = forge
        .authored_pull_requests(filter)?
        .into_iter()
        .filter(|status| filter.admits_repo(&status.pr.repo))
        .map(MyPullRequest::new)
        .collect();
    mine.sort_by(|a, b| {
        a.attention
            .rank()
            .cmp(&b.attention.rank())
            .then_with(|| a.status.pr.created_at.cmp(&b.status.pr.created_at))
    });
    Ok(mine)
}

/// The `branch-status` pane's data source. A detached HEAD has no branch and so
/// no status, which is not an error.
pub fn branch_view<G: GitRepo, Fg: Forge>(git: &G, forge: &Fg) -> Result<BranchView, PortError> {
    match git.current_branch()? {
        None => Ok(BranchView::Detached),
        Some(branch) => {
            let status = forge.branch_status(&branch)?;
            Ok(BranchView::on_branch(branch, status))
        }
    }
}

/// Only the worktrees living under this repo's directory in the configured
/// root. The main checkout and hand-made worktrees elsewhere are not ours.
pub fn list_worktrees<G: GitRepo>(
    git: &G,
    root: &WorktreeRoot,
    repo: &RootRepoName,
) -> Result<Vec<WorktreeEntry>, PortError> {
    let managed = root.as_path().join(repo.as_str());
    Ok(git
        .list_worktrees()?
        .into_iter()
        .filter(|entry| !entry.is_main && entry.path.starts_with(&managed))
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RemoveWorktreeError {
    #[error("{0} is not a worktree zax created")]
    NotAWorktree(PathBuf),
    #[error(transparent)]
    Path(#[from] WorktreePathError),
    #[error(transparent)]
    Port(#[from] PortError),
}

pub fn remove_worktree<G: GitRepo, F: FileSystem>(
    git: &G,
    fs: &F,
    root: &WorktreeRoot,
    branch: &BranchName,
    force: bool,
) -> Result<WorktreePath, RemoveWorktreeError> {
    let main = git.main_worktree()?;
    let repo = git.root_repo_name()?;
    let path = WorktreePath::new(root, &repo, branch, &main)?;

    if !fs.exists(path.as_path()) {
        return Err(RemoveWorktreeError::NotAWorktree(path.as_path().to_owned()));
    }

    git.remove_worktree(path.as_path(), force)?;
    Ok(path)
}

#[derive(Debug)]
pub struct DoctorCheck {
    pub name: &'static str,
    pub result: Result<String, PortError>,
}

impl DoctorCheck {
    pub fn is_ok(&self) -> bool {
        self.result.is_ok()
    }
}

/// Every check runs, so one missing tool does not hide the next.
pub fn doctor<M: Multiplexer, G: GitRepo, Fg: Forge>(
    mux: &M,
    git: &G,
    forge: &Fg,
) -> Vec<DoctorCheck> {
    vec![
        DoctorCheck {
            name: "tmux",
            result: mux.probe(),
        },
        DoctorCheck {
            name: "git",
            result: git.probe(),
        },
        DoctorCheck {
            name: "gh",
            result: forge.probe(),
        },
        DoctorCheck {
            name: "gh auth",
            result: forge.auth_status(),
        },
    ]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::{Path, PathBuf};
    use std::time::{Duration, UNIX_EPOCH};

    use zax_domain::{
        Attention, BranchName, BranchView, CheckState, Mergeable, PrFilter, PullRequest,
        PullRequestStatus, RepoPattern, RepoSlug, ReviewState, RootRepoName, WorktreeRoot,
    };

    use super::*;
    use crate::ports::{PortError, WorktreeEntry};
    use crate::testing::{FakeForge, FakeFs, FakeGitRepo, FakeMultiplexer};

    fn pr(number: u32) -> PullRequest {
        PullRequest {
            repo: RepoSlug::parse("acme/omega").unwrap(),
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/acme/omega/pull/{number}"),
            is_draft: false,
            created_at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            checks: CheckState::Passing,
        }
    }

    #[test]
    fn review_requests_pass_through_from_the_forge() {
        let forge = FakeForge::with_review_requests(vec![pr(1), pr(2)]);
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_100);
        assert_eq!(
            list_review_requests(&forge, &PrFilter::default(), now)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn review_requests_drop_the_rows_the_filter_refuses() {
        let mut stale = pr(3);
        stale.created_at = UNIX_EPOCH + Duration::from_secs(1_600_000_000);
        let forge = FakeForge::with_review_requests(vec![pr(1), stale]);
        let filter = PrFilter {
            max_age: Some(Duration::from_secs(14 * 86_400)),
            ..PrFilter::default()
        };
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_100);

        let listed = list_review_requests(&forge, &filter, now).unwrap();

        assert_eq!(listed.iter().map(|p| p.number).collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn the_filter_reaches_the_forge_so_it_can_narrow_its_own_search() {
        // The forge's search is capped, so a filter that only ran afterwards
        // would still let an excluded repository crowd the queue out.
        let forge = FakeForge::with_review_requests(vec![pr(1)]);
        let filter = PrFilter {
            exclude: vec![RepoPattern::parse("acme/legacy-api").unwrap()],
            ..PrFilter::default()
        };
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_100);

        list_review_requests(&forge, &filter, now).unwrap();

        assert_eq!(*forge.asked_with.borrow(), vec![filter]);
    }

    #[test]
    fn a_forge_failure_is_surfaced_rather_than_swallowed_into_an_empty_list() {
        // An empty review queue and a broken gh must never look the same.
        let forge = FakeForge::failing(PortError::MissingTool { tool: "gh" });
        let now = UNIX_EPOCH + Duration::from_secs(1_700_000_100);
        assert!(list_review_requests(&forge, &PrFilter::default(), now).is_err());
    }

    #[test]
    fn the_branch_view_asks_the_forge_about_the_current_branch() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        let forge = FakeForge {
            branch_status: Some(PullRequestStatus {
                pr: pr(1963),
                review: ReviewState::ReviewRequired,
                unresolved_threads: 2,
                mergeable: Mergeable::Mergeable,
            }),
            ..FakeForge::default()
        };

        let view = branch_view(&git, &forge).unwrap();

        assert_eq!(view.branch().unwrap().as_str(), "OMEGA-1963-stamp");
        assert_eq!(view.status().unwrap().unresolved_threads, 2);
    }

    #[test]
    fn the_branch_view_still_names_the_branch_when_it_has_no_pull_request() {
        // The pane has to say where HEAD is even when the forge has nothing to
        // report, so an absent pull request must not erase the branch.
        let git = FakeGitRepo::new("/Users/me/src/acme", "OMEGA-1963-stamp");
        let forge = FakeForge::default();

        let view = branch_view(&git, &forge).unwrap();

        assert_eq!(view.branch().unwrap().as_str(), "OMEGA-1963-stamp");
        assert_eq!(view.status(), None);
    }

    #[test]
    fn the_branch_view_is_detached_on_a_detached_head_rather_than_an_error() {
        let git = FakeGitRepo::detached("/Users/me/src/acme");
        let forge = FakeForge::default();
        assert_eq!(branch_view(&git, &forge).unwrap(), BranchView::Detached);
    }

    #[test]
    fn worktree_listing_keeps_only_the_ones_zax_manages() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        git.worktrees.borrow_mut().extend([
            WorktreeEntry {
                path: PathBuf::from("/Users/me/src/acme"),
                branch: Some(BranchName::parse_local("main").unwrap()),
                is_main: true,
                is_detached: false,
            },
            WorktreeEntry {
                path: PathBuf::from("/Users/me/src/worktrees/acme/OMEGA-1-a"),
                branch: Some(BranchName::parse_local("OMEGA-1-a").unwrap()),
                is_main: false,
                is_detached: false,
            },
            WorktreeEntry {
                path: PathBuf::from("/tmp/somewhere-else"),
                branch: Some(BranchName::parse_local("other").unwrap()),
                is_main: false,
                is_detached: false,
            },
        ]);
        let root = WorktreeRoot::new(PathBuf::from("/Users/me/src/worktrees")).unwrap();
        let repo = RootRepoName::parse("acme").unwrap();

        let listed = list_worktrees(&git, &root, &repo).unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].path,
            PathBuf::from("/Users/me/src/worktrees/acme/OMEGA-1-a")
        );
    }

    #[test]
    fn removing_a_worktree_resolves_the_branch_to_its_canonical_path() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::with_existing(["/Users/me/src/worktrees/acme/OMEGA-1963-stamp"]);
        let root = WorktreeRoot::new(PathBuf::from("/Users/me/src/worktrees")).unwrap();
        let branch = BranchName::parse_local("OMEGA-1963-stamp").unwrap();

        let path = remove_worktree(&git, &fs, &root, &branch, false).unwrap();

        assert_eq!(
            path.as_path(),
            Path::new("/Users/me/src/worktrees/acme/OMEGA-1963-stamp")
        );
        assert_eq!(
            *git.removed.borrow(),
            vec![(
                PathBuf::from("/Users/me/src/worktrees/acme/OMEGA-1963-stamp"),
                false
            )]
        );
    }

    #[test]
    fn removing_a_worktree_that_is_not_there_says_so() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let root = WorktreeRoot::new(PathBuf::from("/Users/me/src/worktrees")).unwrap();
        let branch = BranchName::parse_local("OMEGA-1963-stamp").unwrap();

        let err = remove_worktree(&git, &fs, &root, &branch, false).unwrap_err();

        assert!(matches!(err, RemoveWorktreeError::NotAWorktree(_)));
        assert!(git.removed.borrow().is_empty());
    }

    #[test]
    fn doctor_reports_one_check_per_tool() {
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let forge = FakeForge::default();

        let checks = doctor(&mux, &git, &forge);

        assert_eq!(
            checks.iter().map(|c| c.name).collect::<Vec<_>>(),
            vec!["tmux", "git", "gh", "gh auth"]
        );
        assert!(checks.iter().all(DoctorCheck::is_ok));
    }

    #[test]
    fn doctor_keeps_going_after_a_failing_check() {
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let forge = FakeForge::failing(PortError::MissingTool { tool: "gh" });

        let checks = doctor(&mux, &git, &forge);

        // One missing tool must not hide the state of the others: every check
        // still runs, and each reports its own verdict.
        assert_eq!(
            checks
                .iter()
                .map(|c| (c.name, c.is_ok()))
                .collect::<Vec<_>>(),
            vec![
                ("tmux", true),
                ("git", true),
                ("gh", false),
                ("gh auth", false)
            ]
        );
    }

    fn authored(
        number: u32,
        created: u64,
        review: ReviewState,
        threads: u32,
        mergeable: Mergeable,
    ) -> PullRequestStatus {
        let mut pr = pr(number);
        pr.created_at = UNIX_EPOCH + Duration::from_secs(created);
        PullRequestStatus {
            pr,
            review,
            unresolved_threads: threads,
            mergeable,
        }
    }

    #[test]
    fn my_pull_requests_come_back_worst_first() {
        // Numbers chosen so the expected order is nothing like the input one.
        let forge = FakeForge::with_authored(vec![
            authored(
                1,
                1_700_000_000,
                ReviewState::Approved,
                0,
                Mergeable::Mergeable,
            ),
            authored(
                2,
                1_700_000_000,
                ReviewState::ReviewRequired,
                2,
                Mergeable::Mergeable,
            ),
            authored(
                3,
                1_700_000_000,
                ReviewState::ChangesRequested,
                0,
                Mergeable::Mergeable,
            ),
            authored(
                4,
                1_700_000_000,
                ReviewState::ReviewRequired,
                0,
                Mergeable::Conflicting,
            ),
            authored(
                5,
                1_700_000_000,
                ReviewState::ReviewRequired,
                0,
                Mergeable::Mergeable,
            ),
        ]);

        let mine = list_my_pull_requests(&forge, &PrFilter::default()).unwrap();

        assert_eq!(
            mine.iter().map(|m| m.status.pr.number).collect::<Vec<_>>(),
            vec![4, 3, 2, 1, 5]
        );
        assert_eq!(mine[0].attention, Attention::Conflicting);
    }

    #[test]
    fn equally_stuck_pull_requests_put_the_oldest_first() {
        let forge = FakeForge::with_authored(vec![
            authored(
                1,
                1_700_000_900,
                ReviewState::ChangesRequested,
                0,
                Mergeable::Mergeable,
            ),
            authored(
                2,
                1_700_000_100,
                ReviewState::ChangesRequested,
                0,
                Mergeable::Mergeable,
            ),
        ]);

        let mine = list_my_pull_requests(&forge, &PrFilter::default()).unwrap();

        assert_eq!(
            mine.iter().map(|m| m.status.pr.number).collect::<Vec<_>>(),
            vec![2, 1]
        );
    }

    #[test]
    fn an_excluded_repository_is_dropped_from_my_pull_requests() {
        let mut elsewhere = authored(
            9,
            1_700_000_000,
            ReviewState::ReviewRequired,
            0,
            Mergeable::Mergeable,
        );
        elsewhere.pr.repo = RepoSlug::parse("acme/legacy-api").unwrap();
        let forge = FakeForge::with_authored(vec![
            authored(
                1,
                1_700_000_000,
                ReviewState::ReviewRequired,
                0,
                Mergeable::Mergeable,
            ),
            elsewhere,
        ]);
        let filter = PrFilter {
            exclude: vec![RepoPattern::parse("acme/legacy-api").unwrap()],
            ..PrFilter::default()
        };

        let mine = list_my_pull_requests(&forge, &filter).unwrap();

        assert_eq!(
            mine.iter().map(|m| m.status.pr.number).collect::<Vec<_>>(),
            vec![1]
        );
    }

    #[test]
    fn a_pull_request_older_than_max_age_is_still_mine() {
        // max_age hides a stale review request. Your own three-week-old pull
        // request is the one that is rotting, so it must survive.
        let forge = FakeForge::with_authored(vec![authored(
            1,
            1_600_000_000,
            ReviewState::ReviewRequired,
            0,
            Mergeable::Mergeable,
        )]);
        let filter = PrFilter {
            max_age: Some(Duration::from_secs(14 * 86_400)),
            ..PrFilter::default()
        };

        assert_eq!(list_my_pull_requests(&forge, &filter).unwrap().len(), 1);
    }

    #[test]
    fn the_filter_reaches_the_forge_for_my_pull_requests_too() {
        let forge = FakeForge::with_authored(vec![]);
        let filter = PrFilter {
            exclude: vec![RepoPattern::parse("acme/legacy-api").unwrap()],
            ..PrFilter::default()
        };

        list_my_pull_requests(&forge, &filter).unwrap();

        assert_eq!(*forge.asked_authored_with.borrow(), vec![filter]);
    }

    #[test]
    fn a_broken_forge_is_not_an_empty_list_of_my_pull_requests() {
        let forge = FakeForge::failing(PortError::MissingTool { tool: "gh" });
        assert!(list_my_pull_requests(&forge, &PrFilter::default()).is_err());
    }
}
