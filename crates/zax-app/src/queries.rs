//! The read-only use cases, plus worktree removal and `zax doctor`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::SystemTime;

use thiserror::Error;
use zax_domain::{
    BranchName, BranchView, MyPullRequest, PrFilter, RepoSlug, ReviewQueueItem, ReviewerState,
    Revisit, RootRepoName, Transport, WorkIndex, WorktreePath, WorktreePathError, WorktreeRoot,
};

use crate::ports::{
    Editor, FileSystem, Forge, GitRepo, Multiplexer, PortError, SshAgent, WorktreeEntry,
    WorktreeOpener, WorktreeScanner,
};

/// The `prs-for-review` pane's data source: pending requests plus revisits.
///
/// The filter is applied here even though the forge was given it too. A forge
/// can only narrow what it is able to express, and its search is capped, so
/// asking it to narrow is worth doing - but this is the only place that decides
/// what the pane shows. `max_age` applies to requests only.
pub fn list_review_queue<Fg: Forge>(
    forge: &Fg,
    filter: &PrFilter,
    now: SystemTime,
) -> Result<Vec<ReviewQueueItem>, PortError> {
    let requests = filter.apply(forge.review_requests(filter)?, now);
    let reviewed = forge.reviewed_pull_requests(filter)?;

    let mut queue: Vec<ReviewQueueItem> = requests
        .into_iter()
        .map(|pr| ReviewQueueItem {
            pr,
            state: ReviewerState::Requested,
        })
        .collect();
    let requested: HashSet<(RepoSlug, u32)> = queue
        .iter()
        .map(|item| (item.pr.repo.clone(), item.pr.number))
        .collect();
    queue.extend(
        reviewed
            .into_iter()
            .filter(|r| filter.admits_repo(&r.pr.repo))
            .filter(|r| !requested.contains(&(r.pr.repo.clone(), r.pr.number)))
            .filter_map(|r| {
                Revisit::of(r.my_open_threads, r.head_moved).map(|revisit| ReviewQueueItem {
                    pr: r.pr,
                    state: ReviewerState::Revisit(revisit),
                })
            }),
    );
    queue.sort_by(|a, b| {
        a.state
            .rank()
            .cmp(&b.state.rank())
            .then_with(|| a.pr.created_at.cmp(&b.pr.created_at))
    });
    Ok(queue)
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
///
/// The ssh agent is the one conditional check. It belongs with `gh auth`
/// rather than with the tools, because both are credentials zax cannot supply
/// for itself - but a key is only needed when `origin` is reached over ssh,
/// and a doctor that fails on a check the reader cannot act on is a doctor
/// they learn to ignore.
pub fn doctor<M: Multiplexer, G: GitRepo, Fg: Forge, O: WorktreeOpener, E: Editor, S: SshAgent>(
    mux: &M,
    git: &G,
    forge: &Fg,
    opener: &O,
    editor: &E,
    ssh: &S,
) -> Vec<DoctorCheck> {
    let mut checks = vec![
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
    ];

    if origin_needs_ssh_key(git) {
        checks.push(DoctorCheck {
            name: "ssh agent",
            result: ssh.probe(),
        });
    }

    checks.push(DoctorCheck {
        name: "open",
        result: opener.probe(),
    });
    checks.push(DoctorCheck {
        name: "editor",
        result: editor.probe(),
    });
    checks
}

/// Whether a fetch from `origin` would need an ssh key.
///
/// A remote that cannot be read is not claimed to need one: that failure is
/// the git check's to report, and guessing here would turn one broken thing
/// into two.
fn origin_needs_ssh_key<G: GitRepo>(git: &G) -> bool {
    git.remote_url("origin")
        .ok()
        .flatten()
        .is_some_and(|url| Transport::of(&url).needs_ssh_key())
}

/// The `work` source's data: what exists across every repository, for any
/// pane to join against.
///
/// Pull requests are not filtered by `[prs]`: that narrows the pull request
/// panes, and a ticket's own pull request must not disappear because of a
/// review-queue setting.
pub fn list_work<S: WorktreeScanner, M: Multiplexer, Fg: Forge>(
    scanner: &S,
    mux: &M,
    forge: &Fg,
    root: &WorktreeRoot,
) -> Result<WorkIndex, PortError> {
    Ok(WorkIndex {
        worktrees: scanner.scan(root)?,
        sessions: mux.list_sessions()?,
        prs: list_my_pull_requests(forge, &PrFilter::default())?,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::num::NonZeroU32;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, UNIX_EPOCH};

    use zax_domain::{
        Attention, BranchName, BranchView, CheckState, Mergeable, PrFilter, PullRequest,
        PullRequestStatus, RepoPattern, RepoSlug, ReviewState, ReviewedPullRequest, ReviewerState,
        Revisit, RootRepoName, WorktreeRoot,
    };

    use super::*;
    use crate::ports::{PortError, WorktreeEntry};
    use crate::testing::{
        FakeEditor, FakeForge, FakeFs, FakeGitRepo, FakeMultiplexer, FakeOpener, FakeSshAgent,
    };

    fn pr(number: u32) -> PullRequest {
        PullRequest {
            repo: RepoSlug::parse("acme/omega").unwrap(),
            number,
            title: format!("PR {number}"),
            url: format!("https://github.com/acme/omega/pull/{number}"),
            is_draft: false,
            created_at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            checks: CheckState::Passing,
            author: None,
        }
    }

    fn reviewed(number: u32, threads: u32, moved: bool) -> ReviewedPullRequest {
        ReviewedPullRequest {
            pr: pr(number),
            my_open_threads: threads,
            head_moved: moved,
        }
    }

    fn aged(mut pr: PullRequest, secs: u64) -> PullRequest {
        pr.created_at = UNIX_EPOCH + Duration::from_secs(secs);
        pr
    }

    fn numbers(items: &[ReviewQueueItem]) -> Vec<u32> {
        items.iter().map(|i| i.pr.number).collect()
    }

    fn threads(count: u32) -> NonZeroU32 {
        NonZeroU32::new(count).unwrap()
    }

    const NOW: u64 = 1_700_000_100;

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(NOW)
    }

    #[test]
    fn review_requests_pass_through_from_the_forge_as_requested() {
        let forge = FakeForge::with_review_requests(vec![pr(1), pr(2)]);
        let listed = list_review_queue(&forge, &PrFilter::default(), now()).unwrap();
        assert_eq!(numbers(&listed), vec![1, 2]);
        assert!(listed.iter().all(|i| i.state == ReviewerState::Requested));
    }

    #[test]
    fn review_requests_drop_the_rows_the_filter_refuses() {
        let stale = aged(pr(3), 1_600_000_000);
        let forge = FakeForge::with_review_requests(vec![pr(1), stale]);
        let filter = PrFilter {
            max_age: Some(Duration::from_secs(14 * 86_400)),
            ..PrFilter::default()
        };

        let listed = list_review_queue(&forge, &filter, now()).unwrap();

        assert_eq!(numbers(&listed), vec![1]);
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

        list_review_queue(&forge, &filter, now()).unwrap();

        assert_eq!(*forge.asked_with.borrow(), vec![filter]);
    }

    #[test]
    fn the_reviewed_search_is_given_the_filter() {
        let forge = FakeForge::default();
        let filter = PrFilter {
            exclude: vec![RepoPattern::parse("acme/legacy-api").unwrap()],
            ..PrFilter::default()
        };

        list_review_queue(&forge, &filter, now()).unwrap();

        assert_eq!(*forge.asked_reviewed_with.borrow(), vec![filter]);
    }

    #[test]
    fn a_forge_failure_is_surfaced_rather_than_swallowed_into_an_empty_list() {
        // An empty review queue and a broken gh must never look the same.
        let forge = FakeForge::failing(PortError::MissingTool { tool: "gh" });
        assert!(list_review_queue(&forge, &PrFilter::default(), now()).is_err());
    }

    #[test]
    fn the_request_search_failing_fails_the_whole_queue() {
        let forge = FakeForge::failing(PortError::MissingTool { tool: "gh" })
            .and_reviewed(vec![reviewed(5, 1, false)]);
        assert!(list_review_queue(&forge, &PrFilter::default(), now()).is_err());
    }

    #[test]
    fn the_reviewed_search_failing_fails_the_whole_queue() {
        let forge = FakeForge::with_review_requests(vec![pr(1)])
            .failing_reviewed(PortError::MissingTool { tool: "gh" });
        assert!(list_review_queue(&forge, &PrFilter::default(), now()).is_err());
    }

    #[test]
    fn a_reviewed_pr_with_an_open_thread_of_yours_is_a_revisit() {
        let forge = FakeForge::with_reviewed(vec![reviewed(5, 2, false)]);
        let listed = list_review_queue(&forge, &PrFilter::default(), now()).unwrap();
        assert_eq!(
            listed,
            vec![ReviewQueueItem {
                pr: pr(5),
                state: ReviewerState::Revisit(Revisit::OpenThreads(threads(2))),
            }]
        );
    }

    #[test]
    fn a_reviewed_pr_with_no_open_threads_and_no_push_is_not_listed() {
        let forge = FakeForge::with_reviewed(vec![reviewed(5, 0, false)]);
        let listed = list_review_queue(&forge, &PrFilter::default(), now()).unwrap();
        assert!(listed.is_empty());
    }

    #[test]
    fn a_reviewed_pr_whose_head_moved_is_an_updated_revisit() {
        let forge = FakeForge::with_reviewed(vec![reviewed(5, 0, true), reviewed(6, 3, true)]);
        let listed = list_review_queue(&forge, &PrFilter::default(), now()).unwrap();
        let states: Vec<ReviewerState> = listed.iter().map(|i| i.state).collect();
        assert_eq!(
            states,
            vec![
                ReviewerState::Revisit(Revisit::Updated),
                ReviewerState::Revisit(Revisit::UpdatedWithOpenThreads(threads(3))),
            ]
        );
    }

    #[test]
    fn a_pr_in_both_searches_is_one_requested_row() {
        let forge =
            FakeForge::with_review_requests(vec![pr(5)]).and_reviewed(vec![reviewed(5, 2, true)]);
        let listed = list_review_queue(&forge, &PrFilter::default(), now()).unwrap();
        assert_eq!(
            listed,
            vec![ReviewQueueItem {
                pr: pr(5),
                state: ReviewerState::Requested,
            }]
        );
    }

    #[test]
    fn the_same_number_in_another_repository_is_a_separate_row() {
        let mut elsewhere = reviewed(5, 1, false);
        elsewhere.pr.repo = RepoSlug::parse("acme/south").unwrap();
        let forge = FakeForge::with_review_requests(vec![pr(5)]).and_reviewed(vec![elsewhere]);
        let listed = list_review_queue(&forge, &PrFilter::default(), now()).unwrap();
        assert_eq!(listed.len(), 2);
    }

    #[test]
    fn max_age_never_drops_a_revisit() {
        let old_request = aged(pr(1), 1_600_000_000);
        let mut old_revisit = reviewed(2, 1, false);
        old_revisit.pr = aged(old_revisit.pr, 1_600_000_000);
        let forge =
            FakeForge::with_review_requests(vec![old_request]).and_reviewed(vec![old_revisit]);
        let filter = PrFilter {
            max_age: Some(Duration::from_secs(14 * 86_400)),
            ..PrFilter::default()
        };

        let listed = list_review_queue(&forge, &filter, now()).unwrap();

        assert_eq!(numbers(&listed), vec![2]);
    }

    #[test]
    fn an_excluded_repository_is_dropped_from_both_searches() {
        let mut request = pr(1);
        request.repo = RepoSlug::parse("acme/legacy-api").unwrap();
        let mut revisit = reviewed(2, 1, true);
        revisit.pr.repo = RepoSlug::parse("acme/legacy-api").unwrap();
        let forge = FakeForge::with_review_requests(vec![request, pr(3)])
            .and_reviewed(vec![revisit, reviewed(4, 1, true)]);
        let filter = PrFilter {
            exclude: vec![RepoPattern::parse("acme/legacy-api").unwrap()],
            ..PrFilter::default()
        };

        let listed = list_review_queue(&forge, &filter, now()).unwrap();

        assert_eq!(numbers(&listed), vec![4, 3]);
    }

    #[test]
    fn the_queue_is_ordered_updated_then_requested_then_open_threads_oldest_first() {
        let at = |mut r: ReviewedPullRequest, secs: u64| {
            r.pr = aged(r.pr, secs);
            r
        };
        let forge = FakeForge::with_review_requests(vec![
            aged(pr(20), 1_650_000_000),
            aged(pr(21), 1_640_000_000),
        ])
        .and_reviewed(vec![
            at(reviewed(30, 1, false), 1_600_000_000),
            at(reviewed(10, 0, true), 1_690_000_000),
            at(reviewed(11, 4, true), 1_680_000_000),
            at(reviewed(31, 2, false), 1_590_000_000),
        ]);

        let listed = list_review_queue(&forge, &PrFilter::default(), now()).unwrap();

        assert_eq!(numbers(&listed), vec![11, 10, 21, 20, 31, 30]);
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
                head: None,
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
    fn an_ssh_origin_gets_its_agent_checked() {
        // A locked key stops `SPC w` as surely as a missing gh does, and until the
        // doctor says so the only symptom is a keystroke that did nothing.
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "main")
            .with_remote("origin", "git@github.com:acme/omega.git");
        let forge = FakeForge::default();
        let editor = FakeEditor::default();
        let agent = FakeSshAgent::locked(PortError::NotAuthenticated {
            tool: "ssh agent",
            detail: "the agent is running but holds no keys".to_owned(),
        });

        let checks = doctor(&mux, &git, &forge, &FakeOpener::default(), &editor, &agent);

        let ssh = checks
            .iter()
            .find(|c| c.name == "ssh agent")
            .expect("an ssh remote needs a key, so the agent is part of the diagnosis");
        assert!(!ssh.is_ok());
    }

    #[test]
    fn an_https_origin_is_not_asked_about_ssh_keys() {
        // The check would always fail for someone who never uses ssh, and a
        // doctor that fails on an irrelevant check teaches you to ignore it.
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "main")
            .with_remote("origin", "https://github.com/acme/omega.git");
        let forge = FakeForge::default();
        let editor = FakeEditor::default();
        let agent = FakeSshAgent::locked(PortError::MissingTool { tool: "ssh-add" });

        let checks = doctor(&mux, &git, &forge, &FakeOpener::default(), &editor, &agent);

        assert!(
            !checks.iter().any(|c| c.name == "ssh agent"),
            "an https remote needs a token, not a key"
        );
        assert!(checks.iter().all(DoctorCheck::is_ok));
    }

    #[test]
    fn a_repository_with_no_origin_is_not_asked_about_ssh_keys() {
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let forge = FakeForge::default();
        let editor = FakeEditor::default();
        let agent = FakeSshAgent::locked(PortError::MissingTool { tool: "ssh-add" });

        let checks = doctor(&mux, &git, &forge, &FakeOpener::default(), &editor, &agent);

        assert!(!checks.iter().any(|c| c.name == "ssh agent"));
    }

    #[test]
    fn doctor_reports_one_check_per_tool() {
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let forge = FakeForge::default();
        let editor = FakeEditor::default();

        let checks = doctor(
            &mux,
            &git,
            &forge,
            &FakeOpener::default(),
            &editor,
            &FakeSshAgent::default(),
        );

        assert_eq!(
            checks.iter().map(|c| c.name).collect::<Vec<_>>(),
            vec!["tmux", "git", "gh", "gh auth", "open", "editor"]
        );
        assert!(checks.iter().all(DoctorCheck::is_ok));
    }

    #[test]
    fn doctor_keeps_going_after_a_failing_check() {
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let forge = FakeForge::failing(PortError::MissingTool { tool: "gh" });
        let editor = FakeEditor::default();

        let checks = doctor(
            &mux,
            &git,
            &forge,
            &FakeOpener::default(),
            &editor,
            &FakeSshAgent::default(),
        );

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
                ("gh auth", false),
                ("open", true),
                ("editor", true)
            ]
        );
    }

    #[test]
    fn doctor_reports_an_editor_that_is_not_installed() {
        // The `SPC w` row action ends in an editor window, so an editor zax cannot
        // start is exactly the kind of thing doctor exists to find before the
        // keystroke does.
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let forge = FakeForge::default();
        let editor = FakeEditor::failing(PortError::MissingProgram {
            program: "zed".to_owned(),
            hint: "install the CLI from the app".to_owned(),
        });

        let checks = doctor(
            &mux,
            &git,
            &forge,
            &FakeOpener::default(),
            &editor,
            &FakeSshAgent::default(),
        );

        let editor_check = checks.iter().find(|c| c.name == "editor").unwrap();
        let message = editor_check.result.as_ref().unwrap_err().to_string();
        assert!(message.contains("zed"), "got: {message}");
        assert!(message.contains("install the CLI"), "got: {message}");
    }

    #[test]
    fn doctor_reports_an_opener_that_cannot_start_apart_from_the_editor() {
        // Opening a worktree and editing a file are configured separately, so
        // a broken one must not be reported as the other.
        let mux = FakeMultiplexer::default();
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let forge = FakeForge::default();
        let opener = FakeOpener::failing(PortError::MissingProgram {
            program: "herdr".to_owned(),
            hint: "install herdr".to_owned(),
        });

        let checks = doctor(
            &mux,
            &git,
            &forge,
            &opener,
            &FakeEditor::default(),
            &FakeSshAgent::default(),
        );

        let open = checks.iter().find(|c| c.name == "open").unwrap();
        assert!(open
            .result
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("herdr"));
        assert!(checks.iter().find(|c| c.name == "editor").unwrap().is_ok());
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
            head: None,
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

    #[test]
    fn work_gathers_worktrees_sessions_and_unfiltered_pull_requests() {
        use crate::testing::FakeScanner;
        use zax_domain::{SessionName, Worktree};

        let root = WorktreeRoot::new(PathBuf::from("/w")).unwrap();
        let scanner = FakeScanner(vec![Worktree {
            repo: RootRepoName::parse("omega").unwrap(),
            branch: BranchName::parse_foreign("OMEGA-1-a").unwrap(),
            path: PathBuf::from("/w/omega/OMEGA-1-a"),
        }]);
        let mux = FakeMultiplexer::with_session_at(
            SessionName::parse("zax-omega-x-00000000").unwrap(),
            "/w/omega/OMEGA-1-a",
            true,
        );
        let mut status = PullRequestStatus {
            pr: pr(5),
            review: ReviewState::None,
            unresolved_threads: 0,
            mergeable: Mergeable::Mergeable,
            head: None,
        };
        status.pr.repo = RepoSlug::parse("other/repo").unwrap();
        let forge = FakeForge::with_authored(vec![status]);

        let work = list_work(&scanner, &mux, &forge, &root).unwrap();
        assert_eq!(work.worktrees.len(), 1);
        assert_eq!(work.sessions.len(), 1);
        assert_eq!(
            work.prs.len(),
            1,
            "no [prs] filter applies to the work index"
        );
    }
}
