//! `zax worktree new`: a rule-enforcing `git worktree add`, then copy and hooks.

use std::path::PathBuf;

use thiserror::Error;
use zax_domain::{BranchName, WorktreePath, WorktreePathError, WorktreeRoot};

use crate::ports::{FileSystem, GitRepo, PortError, ProcessRunner};
use crate::provision::{self, ProvisionError, ProvisionReport, Provisioning};

pub struct CreateWorktree<'a, G, F, P> {
    git: &'a G,
    fs: &'a F,
    runner: &'a P,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorktreeRequest {
    pub branch: BranchName,
    /// What to branch from. `None` means git's default, the current HEAD.
    pub base: Option<String>,
    pub root: WorktreeRoot,
    pub provisioning: Provisioning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateWorktreeOutcome {
    pub path: WorktreePath,
    pub report: ProvisionReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CreateWorktreeError {
    #[error("branch `{0}` already exists; use `zax worktree new` with a new name")]
    BranchExists(String),
    #[error("{0} already exists")]
    TargetExists(PathBuf),
    #[error(transparent)]
    Path(#[from] WorktreePathError),
    #[error(transparent)]
    Provision(#[from] ProvisionError),
    #[error(transparent)]
    Port(#[from] PortError),
}

impl<'a, G: GitRepo, F: FileSystem, P: ProcessRunner> CreateWorktree<'a, G, F, P> {
    pub fn new(git: &'a G, fs: &'a F, runner: &'a P) -> Self {
        Self { git, fs, runner }
    }

    pub fn run(
        &self,
        req: CreateWorktreeRequest,
    ) -> Result<CreateWorktreeOutcome, CreateWorktreeError> {
        let main = self.git.main_worktree()?;
        let repo = self.git.root_repo_name()?;
        let path = WorktreePath::new(&req.root, &repo, &req.branch, &main)?;

        if self.git.branch_exists(&req.branch)? {
            return Err(CreateWorktreeError::BranchExists(
                req.branch.as_str().to_owned(),
            ));
        }
        if self.fs.exists(path.as_path()) {
            return Err(CreateWorktreeError::TargetExists(path.as_path().to_owned()));
        }

        self.git
            .add_worktree_new_branch(path.as_path(), &req.branch, req.base.as_deref())?;

        let report = provision::apply(
            self.fs,
            self.runner,
            &main,
            &path,
            &req.branch,
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

    use zax_domain::{BranchName, Command, WorktreeRoot};

    use super::*;
    use crate::provision::Provisioning;
    use crate::testing::{FakeFs, FakeGitRepo, FakeRunner};

    fn root() -> WorktreeRoot {
        WorktreeRoot::new(PathBuf::from("/Users/me/src/worktrees")).unwrap()
    }

    fn request(branch: &str) -> CreateWorktreeRequest {
        CreateWorktreeRequest {
            branch: BranchName::parse_local(branch).unwrap(),
            base: None,
            root: root(),
            provisioning: Provisioning::default(),
        }
    }

    #[test]
    fn creates_the_worktree_at_the_canonical_path() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();

        let outcome = CreateWorktree::new(&git, &fs, &runner)
            .run(request("OMEGA-1963-stamp"))
            .unwrap();

        assert_eq!(
            outcome.path.as_path(),
            Path::new("/Users/me/src/worktrees/acme/OMEGA-1963-stamp")
        );
        assert_eq!(
            *git.added.borrow(),
            vec![(
                PathBuf::from("/Users/me/src/worktrees/acme/OMEGA-1963-stamp"),
                "OMEGA-1963-stamp".to_owned(),
                None,
                true,
            )]
        );
    }

    #[test]
    fn passes_the_base_through_to_git() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();

        CreateWorktree::new(&git, &fs, &runner)
            .run(CreateWorktreeRequest {
                base: Some("origin/main".to_owned()),
                ..request("OMEGA-1963-stamp")
            })
            .unwrap();

        assert_eq!(git.added.borrow()[0].2, Some("origin/main".to_owned()));
    }

    #[test]
    fn refuses_a_branch_that_already_exists() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main")
            .with_existing_branch("OMEGA-1963-stamp");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();

        let err = CreateWorktree::new(&git, &fs, &runner)
            .run(request("OMEGA-1963-stamp"))
            .unwrap_err();

        assert_eq!(
            err,
            CreateWorktreeError::BranchExists("OMEGA-1963-stamp".to_owned())
        );
        assert!(git.added.borrow().is_empty());
    }

    #[test]
    fn refuses_when_something_is_already_at_the_target_path() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::with_existing(["/Users/me/src/worktrees/acme/OMEGA-1963-stamp"]);
        let runner = FakeRunner::default();

        let err = CreateWorktree::new(&git, &fs, &runner)
            .run(request("OMEGA-1963-stamp"))
            .unwrap_err();

        assert!(matches!(err, CreateWorktreeError::TargetExists(_)));
        assert!(git.added.borrow().is_empty());
    }

    #[test]
    fn refuses_a_worktree_root_inside_the_repository() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::default();
        let runner = FakeRunner::default();

        let err = CreateWorktree::new(&git, &fs, &runner)
            .run(CreateWorktreeRequest {
                root: WorktreeRoot::new(PathBuf::from("/Users/me/src/acme/.claude/worktrees"))
                    .unwrap(),
                ..request("OMEGA-1963-stamp")
            })
            .unwrap_err();

        assert!(matches!(err, CreateWorktreeError::Path(_)));
        assert!(git.added.borrow().is_empty());
    }

    #[test]
    fn provisions_after_the_worktree_exists() {
        let git = FakeGitRepo::new("/Users/me/src/acme", "main");
        let fs = FakeFs::with_existing(["/Users/me/src/acme/.env"]);
        let runner = FakeRunner::default();

        let outcome = CreateWorktree::new(&git, &fs, &runner)
            .run(CreateWorktreeRequest {
                provisioning: Provisioning {
                    copy: vec![PathBuf::from(".env")],
                    hooks: vec![Command::shell("direnv allow").unwrap()],
                    run_hooks: true,
                },
                ..request("OMEGA-1963-stamp")
            })
            .unwrap();

        assert_eq!(outcome.report.copied, vec![PathBuf::from(".env")]);
        assert_eq!(outcome.report.hooks_run, 1);
    }
}
