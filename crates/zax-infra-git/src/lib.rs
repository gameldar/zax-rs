//! `GitRepo` via the git CLI.

use std::path::{Path, PathBuf};

use zax_app::{GitRepo, PortError, WorktreeEntry};
use zax_domain::{BranchName, RootRepoName};
use zax_exec::{Exec, ExecError, Output};

const TOOL: &str = "git";

pub struct GitCli<E: Exec> {
    exec: E,
    cwd: PathBuf,
    program: String,
}

impl<E: Exec> GitCli<E> {
    pub fn new(exec: E, cwd: PathBuf) -> Self {
        Self::with_program(exec, cwd, TOOL.to_owned())
    }

    pub fn with_program(exec: E, cwd: PathBuf, program: String) -> Self {
        Self { exec, cwd, program }
    }

    /// For tests that assert on the recorded calls.
    pub fn exec_ref(&self) -> &E {
        &self.exec
    }

    /// Run and accept any exit status.
    fn run(&self, args: Vec<String>) -> Result<Output, PortError> {
        self.exec
            .exec(&self.program, &args, Some(&self.cwd), &[])
            .map_err(map_exec_error)
    }

    /// Run and require success.
    fn run_ok(&self, args: Vec<String>) -> Result<Output, PortError> {
        let out = self.run(args)?;
        if out.ok() {
            Ok(out)
        } else {
            Err(PortError::CommandFailed {
                tool: TOOL,
                status: out.status,
                stderr: out.stderr.trim().to_owned(),
            })
        }
    }
}

fn map_exec_error(error: ExecError) -> PortError {
    match error {
        ExecError::NotFound { .. } => PortError::MissingTool { tool: TOOL },
        ExecError::Io { detail, .. } => PortError::Unparseable { tool: TOOL, detail },
    }
}

impl<E: Exec> GitRepo for GitCli<E> {
    fn common_dir(&self) -> Result<PathBuf, PortError> {
        Ok(PathBuf::from(
            self.run_ok(argv::common_dir())?.trimmed_stdout(),
        ))
    }

    fn main_worktree(&self) -> Result<PathBuf, PortError> {
        let common = self.common_dir()?;
        common
            .parent()
            .map(Path::to_owned)
            .ok_or_else(|| PortError::Unparseable {
                tool: TOOL,
                detail: format!("{} has no parent directory", common.display()),
            })
    }

    fn root_repo_name(&self) -> Result<RootRepoName, PortError> {
        RootRepoName::from_common_dir(&self.common_dir()?).map_err(|e| PortError::Unparseable {
            tool: TOOL,
            detail: e.to_string(),
        })
    }

    fn current_branch(&self) -> Result<Option<BranchName>, PortError> {
        let out = self.run(argv::current_branch())?;
        if !out.ok() {
            // `symbolic-ref --quiet` exits non-zero on a detached HEAD.
            return Ok(None);
        }
        let name = out.trimmed_stdout();
        if name.is_empty() {
            return Ok(None);
        }
        // An existing branch may predate the project's rule, so parse leniently.
        BranchName::parse_foreign(name)
            .map(Some)
            .map_err(|e| PortError::Unparseable {
                tool: TOOL,
                detail: e.to_string(),
            })
    }

    fn branch_exists(&self, branch: &BranchName) -> Result<bool, PortError> {
        Ok(self.run(argv::branch_exists(branch))?.ok())
    }

    fn add_worktree_new_branch(
        &self,
        path: &Path,
        branch: &BranchName,
        base: Option<&str>,
    ) -> Result<(), PortError> {
        self.run_ok(argv::add_worktree_new_branch(path, branch, base))
            .map(|_| ())
    }

    fn add_worktree_existing_branch(
        &self,
        path: &Path,
        branch: &BranchName,
    ) -> Result<(), PortError> {
        self.run_ok(argv::add_worktree_existing_branch(path, branch))
            .map(|_| ())
    }

    fn list_worktrees(&self) -> Result<Vec<WorktreeEntry>, PortError> {
        let out = self.run_ok(argv::list_worktrees())?;
        porcelain::parse_worktree_list(&out.stdout)
            .map_err(|detail| PortError::Unparseable { tool: TOOL, detail })
    }

    fn remove_worktree(&self, path: &Path, force: bool) -> Result<(), PortError> {
        self.run_ok(argv::remove_worktree(path, force)).map(|_| ())
    }

    fn fetch(&self, remote: &str, refspec: &str) -> Result<(), PortError> {
        self.run_ok(argv::fetch(remote, refspec)).map(|_| ())
    }

    fn probe(&self) -> Result<String, PortError> {
        Ok(self.run_ok(argv::version())?.trimmed_stdout().to_owned())
    }
}

pub mod argv;
pub mod porcelain;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::{Path, PathBuf};

    use zax_app::GitRepo;
    use zax_exec::{Output, RecordingExec};

    use super::*;

    fn cli(exec: RecordingExec) -> GitCli<RecordingExec> {
        GitCli::new(
            exec,
            PathBuf::from("/Users/me/src/worktrees/acme/topic"),
        )
    }

    #[test]
    fn the_common_dir_comes_back_as_a_path() {
        let git = cli(RecordingExec::returning("/Users/me/src/acme/.git\n"));
        assert_eq!(
            git.common_dir().unwrap(),
            PathBuf::from("/Users/me/src/acme/.git")
        );
        assert_eq!(
            git.exec_ref().args(0),
            ["rev-parse", "--path-format=absolute", "--git-common-dir"]
        );
    }

    #[test]
    fn every_call_runs_in_the_configured_directory() {
        let git = cli(RecordingExec::returning("/Users/me/src/acme/.git\n"));
        git.common_dir().unwrap();
        assert_eq!(
            git.exec_ref().calls()[0].cwd,
            Some(PathBuf::from("/Users/me/src/worktrees/acme/topic"))
        );
    }

    #[test]
    fn the_repo_name_is_the_directory_above_the_common_dir() {
        let git = cli(RecordingExec::returning("/Users/me/src/acme/.git\n"));
        assert_eq!(git.root_repo_name().unwrap().as_str(), "acme");
    }

    #[test]
    fn the_main_worktree_is_the_parent_of_the_common_dir() {
        let git = cli(RecordingExec::returning("/Users/me/src/acme/.git\n"));
        assert_eq!(
            git.main_worktree().unwrap(),
            PathBuf::from("/Users/me/src/acme")
        );
    }

    #[test]
    fn a_detached_head_is_none_rather_than_an_error() {
        let git = cli(RecordingExec::with_responses(vec![Output {
            status: 1,
            stdout: String::new(),
            stderr: String::new(),
        }]));
        assert_eq!(git.current_branch().unwrap(), None);
    }

    #[test]
    fn a_branch_name_is_trimmed_of_its_newline() {
        let git = cli(RecordingExec::returning("OMEGA-1963-stamp\n"));
        assert_eq!(
            git.current_branch().unwrap().unwrap().as_str(),
            "OMEGA-1963-stamp"
        );
    }

    #[test]
    fn branch_existence_reads_the_exit_status() {
        let present = cli(RecordingExec::with_responses(vec![Output {
            status: 0,
            stdout: String::new(),
            stderr: String::new(),
        }]));
        let branch = zax_domain::BranchName::parse_local("topic").unwrap();
        assert!(present.branch_exists(&branch).unwrap());

        let absent = cli(RecordingExec::with_responses(vec![Output {
            status: 1,
            stdout: String::new(),
            stderr: String::new(),
        }]));
        assert!(!absent.branch_exists(&branch).unwrap());
    }

    #[test]
    fn a_failing_git_command_carries_its_stderr() {
        let git = cli(RecordingExec::with_responses(vec![Output {
            status: 128,
            stdout: String::new(),
            stderr: "fatal: not a git repository".to_owned(),
        }]));
        let err = git.common_dir().unwrap_err();
        assert!(
            err.to_string().contains("not a git repository"),
            "got: {err}"
        );
    }

    #[test]
    fn a_missing_git_binary_is_reported_as_a_missing_tool() {
        let git = GitCli::with_program(
            zax_exec::SystemExec,
            PathBuf::from("/"),
            "definitely-not-git-zax".to_owned(),
        );
        assert!(matches!(
            git.common_dir().unwrap_err(),
            zax_app::PortError::MissingTool { tool: "git" }
        ));
    }

    #[test]
    fn worktree_addition_builds_the_expected_command_line() {
        let git = cli(RecordingExec::returning(""));
        let branch = zax_domain::BranchName::parse_local("OMEGA-1963-stamp").unwrap();
        git.add_worktree_new_branch(
            Path::new("/Users/me/src/worktrees/acme/OMEGA-1963-stamp"),
            &branch,
            Some("origin/main"),
        )
        .unwrap();
        assert_eq!(
            git.exec_ref().args(0),
            [
                "worktree",
                "add",
                "-b",
                "OMEGA-1963-stamp",
                "--",
                "/Users/me/src/worktrees/acme/OMEGA-1963-stamp",
                "origin/main",
            ]
        );
    }
}
