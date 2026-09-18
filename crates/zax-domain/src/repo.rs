//! Where a worktree is allowed to live.
//!
//! The project's rule - `<worktree-root>/<root-repository>/<branch>`, and never
//! inside the repository itself - is enforced by [`WorktreePath::new`]. An
//! invalid worktree location has no representation.

use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::branch::BranchName;

/// The directory name of the main working tree, which is the key everything
/// else is derived from: the worktree path, the session name and the per-repo
/// config directory.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RootRepoName(String);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RootRepoNameError {
    #[error("repository name is empty")]
    Empty,
    #[error("`{0}` is not a directory name: it contains a path separator")]
    Separator(String),
    #[error("{0} has no parent directory to take a repository name from")]
    NoParent(PathBuf),
}

impl RootRepoName {
    pub fn parse(raw: &str) -> Result<Self, RootRepoNameError> {
        if raw.is_empty() {
            return Err(RootRepoNameError::Empty);
        }
        if raw.contains('/') || raw.contains('\\') || raw.contains('\0') {
            return Err(RootRepoNameError::Separator(raw.to_owned()));
        }
        Ok(Self(raw.to_owned()))
    }

    /// Derive the name from `git rev-parse --path-format=absolute
    /// --git-common-dir`, which resolves to the main repository's git directory
    /// from the main checkout and from any worktree alike. The name is the
    /// directory containing it.
    pub fn from_common_dir(common_dir: &Path) -> Result<Self, RootRepoNameError> {
        let parent = common_dir
            .parent()
            .ok_or_else(|| RootRepoNameError::NoParent(common_dir.to_owned()))?;
        let name = parent
            .file_name()
            .ok_or_else(|| RootRepoNameError::NoParent(common_dir.to_owned()))?;
        Self::parse(&name.to_string_lossy())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RootRepoName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The configured directory that all worktrees live under, tilde already
/// expanded by the config layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeRoot(PathBuf);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorktreeRootError {
    #[error("worktree root {0} must be an absolute path")]
    NotAbsolute(PathBuf),
}

impl WorktreeRoot {
    pub fn new(path: PathBuf) -> Result<Self, WorktreeRootError> {
        if path.is_absolute() {
            Ok(Self(path))
        } else {
            Err(WorktreeRootError::NotAbsolute(path))
        }
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// A location a worktree is allowed to occupy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreePath(PathBuf);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WorktreePathError {
    #[error(
        "worktree path {path} would live inside the repository at {repo}; \
         worktrees must live outside the repo they check out"
    )]
    InsideRepo { path: PathBuf, repo: PathBuf },
}

impl WorktreePath {
    pub fn new(
        root: &WorktreeRoot,
        repo: &RootRepoName,
        branch: &BranchName,
        main_worktree: &Path,
    ) -> Result<Self, WorktreePathError> {
        let mut path = root.as_path().join(repo.as_str());
        for component in branch.components() {
            path.push(component);
        }

        // `starts_with` compares whole components, so `/a/repo-other` is not
        // considered to be inside `/a/repo`.
        if path.starts_with(main_worktree) {
            return Err(WorktreePathError::InsideRepo {
                path,
                repo: main_worktree.to_owned(),
            });
        }

        Ok(Self(path))
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::branch::BranchName;

    fn root() -> WorktreeRoot {
        WorktreeRoot::new(PathBuf::from("/Users/me/src/worktrees")).unwrap()
    }

    fn repo() -> RootRepoName {
        RootRepoName::parse("zed-ai-extensions").unwrap()
    }

    #[test]
    fn a_common_dir_yields_the_main_working_tree_directory_name() {
        let name = RootRepoName::from_common_dir(Path::new("/Users/me/src/acme/.git")).unwrap();
        assert_eq!(name.as_str(), "acme");
    }

    #[test]
    fn a_common_dir_seen_from_a_worktree_yields_the_same_name() {
        // `git rev-parse --git-common-dir` resolves to the MAIN repo's .git
        // even when run from a worktree, which is the whole point.
        let from_main =
            RootRepoName::from_common_dir(Path::new("/Users/me/src/acme/.git")).unwrap();
        let from_worktree =
            RootRepoName::from_common_dir(Path::new("/Users/me/src/acme/.git/")).unwrap();
        assert_eq!(from_main, from_worktree);
    }

    #[test]
    fn a_bare_repo_common_dir_keeps_the_directory_above_it() {
        let name = RootRepoName::from_common_dir(Path::new("/srv/git/acme.git")).unwrap();
        assert_eq!(name.as_str(), "git");
    }

    #[test]
    fn a_repo_name_may_not_contain_a_separator() {
        assert!(matches!(
            RootRepoName::parse("a/b"),
            Err(RootRepoNameError::Separator(_))
        ));
        assert_eq!(RootRepoName::parse(""), Err(RootRepoNameError::Empty));
    }

    #[test]
    fn a_worktree_root_must_be_absolute() {
        assert!(matches!(
            WorktreeRoot::new(PathBuf::from("src/worktrees")),
            Err(WorktreeRootError::NotAbsolute(_))
        ));
    }

    #[test]
    fn a_local_branch_lands_at_root_repo_branch() {
        let branch = BranchName::parse_local("OMEGA-1963-export-csv-reports").unwrap();
        let path = WorktreePath::new(
            &root(),
            &repo(),
            &branch,
            Path::new("/Users/me/src/tools/zed-ai-extensions"),
        )
        .unwrap();
        assert_eq!(
            path.as_path(),
            Path::new(
                "/Users/me/src/worktrees/zed-ai-extensions/OMEGA-1963-export-csv-reports"
            )
        );
    }

    #[test]
    fn a_slashed_foreign_branch_keeps_the_slash_as_a_directory_separator() {
        let branch = BranchName::parse_foreign("dependabot/cargo/serde-1.2.3").unwrap();
        let path = WorktreePath::new(
            &root(),
            &repo(),
            &branch,
            Path::new("/Users/me/src/tools/zed-ai-extensions"),
        )
        .unwrap();
        assert_eq!(
            path.as_path(),
            Path::new("/Users/me/src/worktrees/zed-ai-extensions/dependabot/cargo/serde-1.2.3")
        );
    }

    #[test]
    fn a_worktree_may_not_live_inside_the_repository() {
        let inside = WorktreeRoot::new(PathBuf::from(
            "/Users/me/src/tools/zed-ai-extensions/.claude/worktrees",
        ))
        .unwrap();
        let branch = BranchName::parse_local("OMEGA-1963-stamp").unwrap();
        let err = WorktreePath::new(
            &inside,
            &repo(),
            &branch,
            Path::new("/Users/me/src/tools/zed-ai-extensions"),
        )
        .unwrap_err();
        assert!(matches!(err, WorktreePathError::InsideRepo { .. }));
        assert!(
            err.to_string().contains("inside the repository"),
            "got: {err}"
        );
    }

    #[test]
    fn a_sibling_directory_with_a_shared_prefix_is_not_inside_the_repo() {
        // `/a/repo-other` must not be treated as living inside `/a/repo`.
        let sibling = WorktreeRoot::new(PathBuf::from("/Users/me/src/tools")).unwrap();
        let branch = BranchName::parse_local("topic").unwrap();
        let path = WorktreePath::new(
            &sibling,
            &RootRepoName::parse("zed-ai-extensions-other").unwrap(),
            &branch,
            Path::new("/Users/me/src/tools/zed-ai-extensions"),
        );
        assert!(path.is_ok());
    }
}
