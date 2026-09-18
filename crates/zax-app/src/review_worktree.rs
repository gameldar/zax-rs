//! `zax worktree review`: a pull request's head branch, at the canonical path.
//!
//! This is the one place the naming rule bends. A PR's head branch is not ours
//! to name, so it is parsed with [`BranchName::parse_foreign`] and a slash in it
//! becomes a real directory separator, exactly as the rule prescribes for older
//! branches.

use std::path::PathBuf;

use thiserror::Error;
use zax_domain::{BranchName, PrRef, WorktreePath, WorktreePathError, WorktreeRoot};

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
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CreateReviewWorktreeError {
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
    ) -> Result<CreateWorktreeOutcome, CreateReviewWorktreeError> {
        let head = self.forge.pull_request_head(&req.pr)?;
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

        Ok(CreateWorktreeOutcome { path, report })
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
            head_ref: head_ref.to_owned(),
            head_repo: RepoSlug::parse("acme/omega").unwrap(),
            is_cross_repository: cross,
        });
        let req = CreateReviewWorktreeRequest {
            pr: PrRef::Number(1963),
            root: WorktreeRoot::new(PathBuf::from("/Users/me/src/worktrees")).unwrap(),
            provisioning: Provisioning::default(),
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
            outcome.path.as_path(),
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
