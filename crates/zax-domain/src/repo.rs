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
    #[error("`{0}` is not a directory name: it contains a NUL byte")]
    InteriorNul(String),
    #[error("`{0}` is not a repository name: it is a path traversal component")]
    Traversal(String),
    #[error("{0} has no parent directory to take a repository name from")]
    NoParent(PathBuf),
}

impl RootRepoName {
    pub fn parse(raw: &str) -> Result<Self, RootRepoNameError> {
        if raw.trim().is_empty() {
            return Err(RootRepoNameError::Empty);
        }
        if raw.contains('/') || raw.contains('\\') {
            return Err(RootRepoNameError::Separator(raw.to_owned()));
        }
        if raw.contains('\0') {
            return Err(RootRepoNameError::InteriorNul(raw.to_owned()));
        }
        // `.` and `..` are legal directory entries to name but are traversal
        // when joined into a path, which would move the worktree somewhere
        // other than where the caller asked for.
        if raw == "." || raw == ".." {
            return Err(RootRepoNameError::Traversal(raw.to_owned()));
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
    #[error(
        "worktree root {0} contains a `..` component; give the path it resolves \
         to, because containment is checked without touching the disk"
    )]
    Traversal(PathBuf),
}

impl WorktreeRoot {
    pub fn new(path: PathBuf) -> Result<Self, WorktreeRootError> {
        if !path.is_absolute() {
            return Err(WorktreeRootError::NotAbsolute(path));
        }
        // Without this, `/a/repo-other/../repo` passes the containment check in
        // `WorktreePath::new` and then resolves to a path inside the repo. The
        // check is lexical by design, so the traversal has to be refused here
        // rather than resolved later.
        if has_parent_traversal(&path) {
            return Err(WorktreeRootError::Traversal(path));
        }
        Ok(Self(path))
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
    #[error("repository path {0} must be absolute before containment can be checked")]
    RepoNotAbsolute(PathBuf),
    #[error(
        "repository path {0} contains a `..` component; give the path it \
         resolves to, because containment is checked without touching the disk"
    )]
    RepoTraversal(PathBuf),
}

impl WorktreePath {
    /// # Containment
    ///
    /// The "not inside the repository" rule is enforced lexically, on whole
    /// path components. A lexical comparison is only sound when BOTH sides are
    /// already normalised, so both are checked here: [`WorktreeRoot`] refuses a
    /// `..` component, and this function refuses a `main_worktree` that is
    /// relative or contains one. Guarding only one side is not enough - a
    /// `main_worktree` of `/a/other/../repo` resolves to `/a/repo` while
    /// sharing no literal component prefix with it, so the check would pass a
    /// worktree straight into the repository.
    ///
    /// The third input needs no check: [`BranchName`] cannot contribute a `..`
    /// component, because its ref-syntax validation rejects any `..` in the raw
    /// name. See the note in `branch.rs`.
    ///
    /// Two spellings still get through, and both need the filesystem to
    /// detect, which this crate deliberately does not touch:
    ///
    /// - A symbolic link. A root that is a symlink resolving into the
    ///   repository is accepted, because only the link's own path is compared.
    /// - Case folding. On a case-insensitive volume, which is the macOS
    ///   default, `/A/REPO` and `/a/repo` are one directory but two different
    ///   sequences of components, so a repository given in a different case
    ///   than the root is accepted.
    ///
    /// Both are the same shape as the bugs this function does catch: a path
    /// that resolves into the repository while sharing no literal component
    /// prefix with it. The difference is that resolving them requires asking
    /// the filesystem, so the adapter that actually creates the worktree is
    /// where those checks belong. Do not add a case-folding comparison here
    /// instead: whether a volume folds case is a property of the volume, not
    /// of the string, and guessing it would break case-sensitive volumes.
    pub fn new(
        root: &WorktreeRoot,
        repo: &RootRepoName,
        branch: &BranchName,
        main_worktree: &Path,
    ) -> Result<Self, WorktreePathError> {
        // A relative repo path can never share a component prefix with an
        // absolute candidate, so the check below would silently never fire.
        if !main_worktree.is_absolute() {
            return Err(WorktreePathError::RepoNotAbsolute(main_worktree.to_owned()));
        }
        // And a `..` on this side defeats the comparison just as surely as one
        // on the root's side does.
        if has_parent_traversal(main_worktree) {
            return Err(WorktreePathError::RepoTraversal(main_worktree.to_owned()));
        }

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

/// Does this path contain a `..` component?
///
/// Such a path cannot be compared lexically for containment, because
/// `Path::starts_with` matches components literally and never resolves `..`.
///
/// There is deliberately no `.` case. `Path::components` normalises away every
/// `.` except a leading one, so a mid-path `.` never reaches this function, and
/// a leading `.` only occurs in a relative path, which the caller has already
/// rejected. A `.` is inert anyway: `/a/./b` resolves to `/a/b` and cannot
/// escape the directory the caller asked for.
pub(crate) fn has_parent_traversal(path: &Path) -> bool {
    use std::path::Component;

    path.components().any(|c| matches!(c, Component::ParentDir))
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
    fn a_repo_name_may_not_be_a_traversal_component() {
        // `..` as a repo name would move the worktree up out of its root.
        assert!(matches!(
            RootRepoName::parse(".."),
            Err(RootRepoNameError::Traversal(_))
        ));
        assert!(matches!(
            RootRepoName::parse("."),
            Err(RootRepoNameError::Traversal(_))
        ));
    }

    #[test]
    fn a_whitespace_only_repo_name_is_empty_not_a_name() {
        assert_eq!(RootRepoName::parse("   "), Err(RootRepoNameError::Empty));
    }

    #[test]
    fn a_repo_name_may_not_contain_a_nul() {
        assert!(matches!(
            RootRepoName::parse("re\0po"),
            Err(RootRepoNameError::InteriorNul(_))
        ));
    }

    #[test]
    fn a_worktree_root_must_be_absolute() {
        assert!(matches!(
            WorktreeRoot::new(PathBuf::from("src/worktrees")),
            Err(WorktreeRootError::NotAbsolute(_))
        ));
    }

    #[test]
    fn a_worktree_root_may_not_contain_a_parent_traversal_component() {
        // This is the bypass the lexical containment check cannot see:
        // `/a/repo-other/../repo` resolves to `/a/repo`, so accepting it would
        // put the worktree inside the repository.
        assert!(matches!(
            WorktreeRoot::new(PathBuf::from(
                "/Users/me/src/tools/zed-ai-extensions-other/../zed-ai-extensions"
            )),
            Err(WorktreeRootError::Traversal(_))
        ));
    }

    #[test]
    fn a_mid_path_dot_is_harmless_and_accepted() {
        // `Path::components` normalises a non-leading `.` away, so it can never
        // reach the traversal check - and it does not need to, because `/a/./b`
        // resolves to `/a/b` and cannot escape anywhere.
        let root = WorktreeRoot::new(PathBuf::from("/Users/me/./src/worktrees"))
            .expect("a mid-path dot is inert");
        assert_eq!(
            root.as_path(),
            Path::new("/Users/me/./src/worktrees"),
            "the path is kept verbatim; only the containment comparison normalises"
        );
    }

    #[test]
    fn containment_cannot_be_checked_against_a_traversing_repository_path() {
        // `/a/other/../repo` resolves to `/a/repo`, so a worktree at
        // `/a/repo/topic` is inside it - but the two share no literal component
        // prefix, so a lexical check would wave it through. Guarding only the
        // worktree root is not enough; both sides have to be normalised.
        let root = WorktreeRoot::new(PathBuf::from("/a")).unwrap();
        let branch = BranchName::parse_local("topic").unwrap();
        assert!(matches!(
            WorktreePath::new(
                &root,
                &RootRepoName::parse("repo").unwrap(),
                &branch,
                Path::new("/a/other/../repo"),
            ),
            Err(WorktreePathError::RepoTraversal(_))
        ));
    }

    #[test]
    fn containment_cannot_be_checked_against_a_relative_repository_path() {
        // A relative repo path shares no component prefix with an absolute
        // candidate, so the containment check would silently never fire.
        let branch = BranchName::parse_local("topic").unwrap();
        assert!(matches!(
            WorktreePath::new(&root(), &repo(), &branch, Path::new("relative/repo")),
            Err(WorktreePathError::RepoNotAbsolute(_))
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
