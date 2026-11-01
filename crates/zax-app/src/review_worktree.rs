//! `zax worktree review`: a pull request's head branch, at the canonical path.
//!
//! This is the one place the naming rule bends. A PR's head branch is not ours
//! to name, so it is parsed with [`BranchName::parse_foreign`] and a slash in it
//! becomes a real directory separator, exactly as the rule prescribes for older
//! branches.

use std::path::PathBuf;

use thiserror::Error;
use zax_domain::{BranchName, PrRef, RepoSlug, WorktreePath, WorktreePathError, WorktreeRoot};

use crate::create_worktree::CreateWorktreeOutcome;
use crate::ports::{FileSystem, Forge, GitRepo, PortError, ProcessRunner};
use crate::provision::{self, ProvisionError, Provisioning};

pub struct CreateReviewWorktree<'a, G, Fg, Fs, P> {
    git: &'a G,
    forge: &'a Fg,
    fs: &'a Fs,
    runner: &'a P,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateReviewWorktreeRequest {
    pub pr: PrRef,
    pub root: WorktreeRoot,
    pub provisioning: Provisioning,
    /// Permit moving an existing local branch to a head that is not a
    /// descendant of it.
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateReviewWorktreeOutcome {
    pub worktree: CreateWorktreeOutcome,
    /// The pull request being reviewed, as the forge spells its URL, so
    /// whatever opens the worktree can start the review itself.
    pub pr_url: String,
    /// `Some` when an existing local branch was moved off its own history,
    /// which only happens under `force`. The caller reports it: a discarded
    /// commit the user is not told about is indistinguishable from one that
    /// was never made.
    pub forced_update: Option<ForcedBranchUpdate>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForcedBranchUpdate {
    pub branch: String,
    pub previous_tip: String,
    pub head: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CreateReviewWorktreeError {
    #[error(
        "pull request #{number} belongs to {pull_request_repo}, but this is \
         {current_repo}; reviewing a pull request from another repository is \
         not supported"
    )]
    WrongRepository {
        number: u32,
        pull_request_repo: RepoSlug,
        current_repo: RepoSlug,
    },
    #[error(
        "branch `{branch}` is at {local_tip} and the pull request head is \
         {incoming_head}, which is not a descendant of it; fetching would \
         discard commits only this branch has. Pass --force to move it anyway, \
         or delete the branch"
    )]
    NotFastForward {
        branch: String,
        local_tip: String,
        incoming_head: String,
    },
    #[error(
        "the forge did not say which repository this pull request is in, so zax \
         cannot tell whether it is this one; give the pull request's URL rather \
         than its number"
    )]
    UnknownRepository,
    #[error("the pull request's head branch is unusable: {0}")]
    Branch(String),
    #[error("{0} already exists")]
    TargetExists(PathBuf),
    #[error(transparent)]
    Path(#[from] WorktreePathError),
    #[error(transparent)]
    Provision(#[from] ProvisionError),
    #[error(transparent)]
    Port(#[from] PortError),
}

impl<'a, G, Fg, Fs, P> CreateReviewWorktree<'a, G, Fg, Fs, P>
where
    G: GitRepo,
    Fg: Forge,
    Fs: FileSystem,
    P: ProcessRunner,
{
    pub fn new(git: &'a G, forge: &'a Fg, fs: &'a Fs, runner: &'a P) -> Self {
        Self {
            git,
            forge,
            fs,
            runner,
        }
    }

    pub fn run(
        &self,
        req: CreateReviewWorktreeRequest,
    ) -> Result<CreateReviewWorktreeOutcome, CreateReviewWorktreeError> {
        let head = self.forge.pull_request_head(&req.pr)?;
        self.refuse_another_repository(&req.pr, &head)?;

        let branch = BranchName::parse_foreign(&head.head_ref)
            .map_err(|e| CreateReviewWorktreeError::Branch(e.to_string()))?;

        let main = self.git.main_worktree()?;
        let repo = self.git.root_repo_name()?;
        let path = WorktreePath::new(&req.root, &repo, &branch, &main)?;

        if self.fs.exists(path.as_path()) {
            return Err(CreateReviewWorktreeError::TargetExists(
                path.as_path().to_owned(),
            ));
        }

        let forced_update = self.guard_existing_branch(head.number, &branch, req.force)?;

        // `refs/pull/<n>/head` exists on the upstream remote even for a fork, so
        // one refspec covers both cases. The leading `+` makes it a forced
        // update, so re-reviewing a force-pushed PR works.
        self.git.fetch(
            "origin",
            &format!(
                "+refs/pull/{}/head:refs/heads/{}",
                head.number,
                branch.as_str()
            ),
        )?;

        self.git
            .add_worktree_existing_branch(path.as_path(), &branch)?;

        let report = provision::apply(
            self.fs,
            self.runner,
            &main,
            &path,
            &branch,
            &main,
            &req.provisioning,
        )?;

        Ok(CreateReviewWorktreeOutcome {
            worktree: CreateWorktreeOutcome { path, report },
            pr_url: head.url,
            forced_update,
        })
    }

    /// The refspec above is forced, so it moves a local branch of that name
    /// wherever the pull request head is, discarding anything only the branch
    /// had. `zax worktree new` refuses outright when a branch exists; this
    /// refuses only the updates that can lose work, because re-reviewing a
    /// force-pushed pull request is the case this command is for and a
    /// force-pushed head is exactly not a descendant of the old tip.
    ///
    /// The probe fetch has no destination, so it writes FETCH_HEAD and nothing
    /// else: the question can be asked before anything is at risk.
    fn guard_existing_branch(
        &self,
        number: u32,
        branch: &BranchName,
        force: bool,
    ) -> Result<Option<ForcedBranchUpdate>, CreateReviewWorktreeError> {
        if !self.git.branch_exists(branch)? {
            return Ok(None);
        }

        self.git
            .fetch("origin", &format!("refs/pull/{number}/head"))?;
        let incoming_head = self.git.resolve_commit("FETCH_HEAD")?;
        let local_tip = self
            .git
            .resolve_commit(&format!("refs/heads/{}", branch.as_str()))?;

        if self.git.is_ancestor(&local_tip, &incoming_head)? {
            return Ok(None);
        }
        if !force {
            return Err(CreateReviewWorktreeError::NotFastForward {
                branch: branch.as_str().to_owned(),
                local_tip,
                incoming_head,
            });
        }
        Ok(Some(ForcedBranchUpdate {
            branch: branch.as_str().to_owned(),
            previous_tip: local_tip,
            head: incoming_head,
        }))
    }

    /// The review queue is cross-repository by construction, so a pull request
    /// reaching this use case need not be one of this repository's. Everything
    /// below is single-repository: the worktree path comes from this repo's
    /// name and the head is fetched from this repo's `origin`. Resolving
    /// another repository to a checkout would be a feature; refusing is the
    /// honest answer, and it is made here rather than in the CLI so every
    /// caller gets it.
    fn refuse_another_repository(
        &self,
        pr: &PrRef,
        head: &crate::ports::PullRequestHead,
    ) -> Result<(), CreateReviewWorktreeError> {
        // Where the pull request lives, when that can be known rather than
        // guessed. A URL states it outright. A bare number is resolved by the
        // forge against the current directory, and when the head is not in a
        // fork the head repository is that same repository, which catches a
        // forge pointed somewhere other than `origin`. A fork's head says
        // nothing about where its pull request is, so it is not consulted.
        let pull_request_repo = match pr.repository() {
            Some(repo) => repo,
            // A fork's head names the fork, not the pull request's repository,
            // so it is no help; a URL is the only other source and there was
            // none.
            None if head.is_cross_repository => return Ok(()),
            None => head
                .head_repo
                .clone()
                .ok_or(CreateReviewWorktreeError::UnknownRepository)?,
        };

        let current_repo = self.forge.current_repo()?;
        // Case-insensitively: a URL keeps the casing it was pasted with and
        // the forge answers with the canonical one, so the same repository
        // routinely arrives spelled two ways.
        if pull_request_repo.same_repository(&current_repo) {
            return Ok(());
        }
        Err(CreateReviewWorktreeError::WrongRepository {
            number: head.number,
            pull_request_repo,
            current_repo,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::{Path, PathBuf};

    use zax_domain::{PrRef, RepoSlug, WorktreeRoot};

    use super::*;
    use crate::ports::PullRequestHead;
    use crate::provision::Provisioning;
    use crate::testing::{FakeForge, FakeFs, FakeGitRepo, FakeRunner};

    fn request(head_ref: &str, cross: bool) -> (FakeForge, CreateReviewWorktreeRequest) {
        let forge = FakeForge::with_head(PullRequestHead {
            number: 1963,
            url: "https://github.com/acme/omega/pull/1963".to_owned(),
            head_ref: head_ref.to_owned(),
            head_repo: Some(RepoSlug::parse("acme/omega").unwrap()),
            is_cross_repository: cross,
        })
        .in_repository("acme/omega");
        let req = CreateReviewWorktreeRequest {
            pr: PrRef::Number(1963),
            root: WorktreeRoot::new(PathBuf::from("/Users/me/src/worktrees")).unwrap(),
            provisioning: Provisioning::default(),
            force: false,
        };
        (forge, req)
    }

    #[test]
    fn fetches_the_pull_request_head_into_a_local_branch() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (forge, req) = request("OMEGA-2000-their-work", false);

        CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap();

        // A forced refspec, so re-reviewing a force-pushed PR does not fail.
        assert_eq!(
            *git.fetched.borrow(),
            vec![(
                "origin".to_owned(),
                "+refs/pull/1963/head:refs/heads/OMEGA-2000-their-work".to_owned(),
            )]
        );
    }

    #[test]
    fn adds_the_worktree_for_an_existing_branch_not_a_new_one() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (forge, req) = request("OMEGA-2000-their-work", false);

        CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap();

        let added = git.added.borrow();
        assert_eq!(added.len(), 1);
        assert!(!added[0].3, "must not pass -b for a branch fetch created");
    }

    #[test]
    fn a_slashed_head_branch_keeps_the_slash_as_a_directory_separator() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (forge, req) = request("dependabot/cargo/serde-1.2.3", false);

        let outcome = CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap();

        assert_eq!(
            outcome.worktree.path.as_path(),
            Path::new("/Users/me/src/worktrees/acme/dependabot/cargo/serde-1.2.3")
        );
    }

    #[test]
    fn a_fork_works_because_the_refspec_is_the_pull_ref_not_the_fork_remote() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (forge, req) = request("patch-1", true);

        CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap();

        assert_eq!(
            *git.fetched.borrow(),
            vec![(
                "origin".to_owned(),
                "+refs/pull/1963/head:refs/heads/patch-1".to_owned(),
            )]
        );
    }

    #[test]
    fn a_head_branch_that_is_not_a_valid_ref_is_reported_not_panicked_on() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (forge, req) = request("bad..ref", false);

        let err = CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap_err();

        assert!(matches!(err, CreateReviewWorktreeError::Branch(_)));
        assert!(git.fetched.borrow().is_empty());
    }

    #[test]
    fn refuses_a_pull_request_that_belongs_to_another_repository() {
        // The review queue spans repositories, so the `SPC w` row action can hand
        // this a pull request from one zax is not standing in. Building the
        // worktree anyway derives the path from this repository and fetches
        // `refs/pull/<n>/head` from this repository's origin, which either
        // fails obscurely or, when the number happens to exist here too,
        // silently produces the wrong diff.
        let git = FakeGitRepo::new("/Users/me/src/zed-ai-extensions", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (forge, req) = request("fix-ingest", false);
        let forge = forge.in_repository("someone/zed-ai-extensions");
        let req = CreateReviewWorktreeRequest {
            pr: PrRef::parse("https://github.com/acme/omega/pull/42").unwrap(),
            ..req
        };

        let err = CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("acme/omega"), "got: {message}");
        assert!(
            message.contains("someone/zed-ai-extensions"),
            "got: {message}"
        );
        assert!(
            git.fetched.borrow().is_empty(),
            "nothing may be fetched into this repository"
        );
        assert!(git.added.borrow().is_empty());
    }

    #[test]
    fn a_pull_request_url_in_another_case_is_still_this_repository() {
        // The URL keeps whatever casing was pasted; `gh repo view` answers
        // with GitHub's canonical casing. GitHub reads the two as one
        // repository, so comparing them literally would refuse a pull request
        // of the very repository zax is standing in.
        let git = FakeGitRepo::new("/Users/me/src/omega", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (forge, req) = request("fix-ingest", false);
        let forge = forge.in_repository("Acme/Omega");
        let req = CreateReviewWorktreeRequest {
            pr: PrRef::parse("https://github.com/acme/omega/pull/42").unwrap(),
            ..req
        };

        CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap();

        assert_eq!(git.fetched.borrow().len(), 1);
    }

    #[test]
    fn refuses_when_a_numbered_pull_request_resolves_somewhere_else() {
        // A bare number is resolved by the forge, which does not have to agree
        // with the repository zax will fetch from. For a pull request whose
        // head is in its own repository, the head repository names where it
        // was resolved, so a mismatch is visible without a URL.
        let git = FakeGitRepo::new("/Users/me/src/zed-ai-extensions", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (forge, req) = request("fix-ingest", false);
        let forge = forge.in_repository("someone/zed-ai-extensions");

        let err = CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap_err();

        assert!(matches!(
            err,
            CreateReviewWorktreeError::WrongRepository { .. }
        ));
        assert!(git.fetched.borrow().is_empty());
    }

    #[test]
    fn a_fork_pull_request_in_the_current_repository_is_still_reviewable() {
        // The head of a fork's pull request lives in the fork, which is not the
        // current repository and must not be read as one. What matters is where
        // the pull request itself is, which the URL states.
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (_, req) = request("patch-1", true);
        let forge = FakeForge::with_head(PullRequestHead {
            number: 1963,
            url: "https://github.com/acme/omega/pull/1963".to_owned(),
            head_ref: "patch-1".to_owned(),
            head_repo: Some(RepoSlug::parse("contributor/omega").unwrap()),
            is_cross_repository: true,
        })
        .in_repository("acme/omega");
        let req = CreateReviewWorktreeRequest {
            pr: PrRef::parse("https://github.com/acme/omega/pull/1963").unwrap(),
            ..req
        };

        CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap();

        assert_eq!(git.fetched.borrow().len(), 1);
    }

    /// The pull ref probe every guarded run makes before touching a branch.
    const PROBE: &str = "refs/pull/1963/head";
    const UPDATE: &str = "+refs/pull/1963/head:refs/heads/patch-1";

    fn existing_branch_repo(local: &str, incoming: &str) -> FakeGitRepo {
        FakeGitRepo::new("/Users/me/src/acme", "main")
            .with_existing_branch("patch-1")
            .with_commit("refs/heads/patch-1", local)
            .with_commit("FETCH_HEAD", incoming)
    }

    #[test]
    fn refuses_to_move_an_existing_branch_off_its_own_history() {
        // `zax worktree rm` leaves the branch behind, so a name collision or
        // your own review fixups on a branch whose worktree you removed sit in
        // exactly the state a forced refspec destroys: a local branch that is
        // not checked out anywhere. git force-updates it without a word.
        let git = existing_branch_repo("aaaaaaa", "bbbbbbb");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (forge, req) = request("patch-1", false);

        let err = CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap_err();

        let message = err.to_string();
        assert!(message.contains("patch-1"), "got: {message}");
        assert!(message.contains("aaaaaaa"), "got: {message}");
        assert!(message.contains("bbbbbbb"), "got: {message}");
        assert!(message.contains("--force"), "got: {message}");

        let fetched = git.fetched.borrow();
        assert!(
            fetched.iter().all(|(_, refspec)| refspec != UPDATE),
            "the branch must not have been moved: {fetched:?}"
        );
        assert!(git.added.borrow().is_empty());
    }

    #[test]
    fn a_fast_forward_of_an_existing_branch_needs_no_permission() {
        // Nothing is lost when the branch only moves forward, which is the
        // ordinary "the pull request gained a commit" case.
        let git = existing_branch_repo("aaaaaaa", "bbbbbbb").with_ancestry("aaaaaaa", "bbbbbbb");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (forge, req) = request("patch-1", false);

        let outcome = CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap();

        assert_eq!(outcome.forced_update, None);
        assert_eq!(
            *git.fetched.borrow(),
            vec![
                ("origin".to_owned(), PROBE.to_owned()),
                ("origin".to_owned(), UPDATE.to_owned()),
            ]
        );
    }

    #[test]
    fn a_forced_divergent_update_happens_but_is_reported() {
        // Re-reviewing a force-pushed pull request is the case this must keep
        // serving, and a force-pushed head is precisely not a descendant of
        // the old tip. It is allowed with `--force`, and never silently.
        let git = existing_branch_repo("aaaaaaa", "bbbbbbb");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (forge, req) = request("patch-1", false);
        let req = CreateReviewWorktreeRequest { force: true, ..req };

        let outcome = CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap();

        assert_eq!(
            outcome.forced_update,
            Some(ForcedBranchUpdate {
                branch: "patch-1".to_owned(),
                previous_tip: "aaaaaaa".to_owned(),
                head: "bbbbbbb".to_owned(),
            })
        );
        assert_eq!(
            *git.fetched.borrow(),
            vec![
                ("origin".to_owned(), PROBE.to_owned()),
                ("origin".to_owned(), UPDATE.to_owned()),
            ]
        );
    }

    #[test]
    fn a_branch_that_does_not_exist_yet_is_fetched_without_a_probe() {
        // Nothing can be lost, so nothing is asked.
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (forge, req) = request("patch-1", false);

        CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap();

        assert_eq!(
            *git.fetched.borrow(),
            vec![("origin".to_owned(), UPDATE.to_owned())]
        );
    }

    #[test]
    fn a_pull_request_whose_fork_was_deleted_is_still_reviewable() {
        // GitHub nulls the head repository once the fork is gone, and
        // refs/pull/<n>/head outlives it, so the review works.
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (_, req) = request("patch-1", true);
        let forge = FakeForge::with_head(PullRequestHead {
            number: 1963,
            url: "https://github.com/acme/omega/pull/1963".to_owned(),
            head_ref: "patch-1".to_owned(),
            head_repo: None,
            is_cross_repository: true,
        })
        .in_repository("acme/omega");

        CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap();

        assert_eq!(git.fetched.borrow().len(), 1);
    }

    #[test]
    fn a_pull_request_with_no_repository_anywhere_is_an_error_not_a_guess() {
        // Nothing says where this pull request is: no URL, and a head
        // repository the forge would not name. Proceeding would fetch from
        // whatever origin happens to be.
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let (_, req) = request("patch-1", false);
        let forge = FakeForge::with_head(PullRequestHead {
            number: 1963,
            url: "https://github.com/acme/omega/pull/1963".to_owned(),
            head_ref: "patch-1".to_owned(),
            head_repo: None,
            is_cross_repository: false,
        })
        .in_repository("acme/omega");

        let err = CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap_err();

        assert!(matches!(err, CreateReviewWorktreeError::UnknownRepository));
        assert!(git.fetched.borrow().is_empty());
    }

    #[test]
    fn refuses_when_the_target_path_is_already_occupied() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::with_existing(["/Users/me/src/worktrees/acme/OMEGA-2000-their-work"]);
        let runner = FakeRunner::default();
        let (forge, req) = request("OMEGA-2000-their-work", false);

        let err = CreateReviewWorktree::new(&git, &forge, &fs, &runner)
            .run(req)
            .unwrap_err();

        assert!(matches!(err, CreateReviewWorktreeError::TargetExists(_)));
        assert!(git.fetched.borrow().is_empty());
    }
}
