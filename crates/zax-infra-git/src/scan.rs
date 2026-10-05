//! Finding worktrees by walking the worktree root.
//!
//! zax puts every worktree at `<root>/<repo>/<branch>`, with a slashed branch
//! becoming nested directories, so the path is the branch. A directory with a
//! `.git` file is a worktree and is not searched further. A directory holding a
//! `.git` directory is a full checkout, not a worktree, and is left alone.

use std::path::{Path, PathBuf};

use zax_app::{PortError, WorktreeScanner};
use zax_domain::{BranchName, RootRepoName, Worktree, WorktreeRoot};

/// Deep enough for any branch with a few slashes; bounded so a mistaken root
/// such as `~` cannot walk the whole disk.
const MAX_DEPTH: usize = 6;

pub struct FsWorktreeScanner;

impl WorktreeScanner for FsWorktreeScanner {
    fn scan(&self, root: &WorktreeRoot) -> Result<Vec<Worktree>, PortError> {
        let mut found = Vec::new();
        for repo_dir in subdirectories(root.as_path())? {
            let Some(repo) = repo_dir
                .file_name()
                .and_then(|n| RootRepoName::parse(&n.to_string_lossy()).ok())
            else {
                continue;
            };
            walk(&repo_dir, &repo_dir, &repo, 1, &mut found);
        }
        Ok(found)
    }
}

fn walk(repo_dir: &Path, dir: &Path, repo: &RootRepoName, depth: usize, found: &mut Vec<Worktree>) {
    if depth > MAX_DEPTH {
        return;
    }
    for child in readable_subdirectories(dir) {
        match std::fs::metadata(child.join(".git")) {
            Ok(git) if git.is_file() => {
                let branch = child
                    .strip_prefix(repo_dir)
                    .ok()
                    .map(|rel| {
                        rel.to_string_lossy()
                            .replace(std::path::MAIN_SEPARATOR, "/")
                    })
                    .and_then(|raw| BranchName::parse_foreign(&raw).ok());
                if let Some(branch) = branch {
                    found.push(Worktree {
                        repo: repo.clone(),
                        branch,
                        path: child,
                    });
                }
            }
            // A `.git` directory is a full checkout, which owns its own tree.
            Ok(_) => {}
            Err(_) => walk(repo_dir, &child, repo, depth + 1, found),
        }
    }
}

/// The worktree root's own subdirectories. A root that does not exist has
/// none, but one that exists and cannot be read is a real problem the user
/// should hear about, so only this read reports its failure.
fn subdirectories(dir: &Path) -> Result<Vec<PathBuf>, PortError> {
    match std::fs::read_dir(dir) {
        Ok(entries) => Ok(directories(entries)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(PortError::Io {
            operation: "scan worktrees",
            path: dir.to_owned(),
            detail: e.to_string(),
        }),
    }
}

/// Below the root, a directory that cannot be read is skipped with its
/// subtree: one unreadable folder must not blank every other worktree.
fn readable_subdirectories(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(directories).unwrap_or_default()
}

fn directories(entries: std::fs::ReadDir) -> Vec<PathBuf> {
    entries
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::fs;
    use std::path::{Path, PathBuf};

    use zax_app::WorktreeScanner;
    use zax_domain::WorktreeRoot;

    use super::*;

    fn worktree_at(root: &Path, rel: &str) {
        let dir = root.join(rel);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(".git"), "gitdir: /somewhere\n").unwrap();
    }

    #[test]
    fn every_directory_holding_a_git_file_is_a_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        worktree_at(tmp.path(), "omega/OMEGA-1-a");
        worktree_at(tmp.path(), "api/feature/slashed");
        fs::create_dir_all(tmp.path().join("omega/not-a-worktree")).unwrap();
        let root = WorktreeRoot::new(tmp.path().to_owned()).unwrap();

        let mut found = FsWorktreeScanner.scan(&root).unwrap();
        found.sort_by(|a, b| a.path.cmp(&b.path));
        let pairs: Vec<(String, String)> = found
            .iter()
            .map(|w| (w.repo.as_str().to_owned(), w.branch.as_str().to_owned()))
            .collect();
        assert_eq!(
            pairs,
            [
                ("api".to_owned(), "feature/slashed".to_owned()),
                ("omega".to_owned(), "OMEGA-1-a".to_owned()),
            ]
        );
    }

    #[test]
    fn a_missing_root_is_no_worktrees_rather_than_an_error() {
        let root = WorktreeRoot::new(PathBuf::from("/definitely/not/here/zax")).unwrap();
        assert!(FsWorktreeScanner.scan(&root).unwrap().is_empty());
    }

    #[test]
    fn a_worktree_is_not_searched_for_more_worktrees() {
        let tmp = tempfile::tempdir().unwrap();
        worktree_at(tmp.path(), "omega/OMEGA-1-a");
        worktree_at(tmp.path(), "omega/OMEGA-1-a/vendor/sub");
        let root = WorktreeRoot::new(tmp.path().to_owned()).unwrap();
        assert_eq!(FsWorktreeScanner.scan(&root).unwrap().len(), 1);
    }

    #[test]
    fn a_checkout_with_a_git_directory_is_not_a_worktree() {
        let tmp = tempfile::tempdir().unwrap();
        worktree_at(tmp.path(), "omega/OMEGA-1-a");
        fs::create_dir_all(tmp.path().join("omega/full/.git")).unwrap();
        worktree_at(tmp.path(), "omega/full/inner");
        let root = WorktreeRoot::new(tmp.path().to_owned()).unwrap();
        let found = FsWorktreeScanner.scan(&root).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].branch.as_str(), "OMEGA-1-a");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_directory_is_skipped_not_fatal() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        worktree_at(tmp.path(), "omega/OMEGA-1-a");
        let locked = tmp.path().join("omega/locked");
        fs::create_dir_all(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let root = WorktreeRoot::new(tmp.path().to_owned()).unwrap();

        let result = FsWorktreeScanner.scan(&root);
        let still_readable = fs::read_dir(&locked).is_ok();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        if !still_readable {
            assert_eq!(result.unwrap().len(), 1);
        }
    }
}
