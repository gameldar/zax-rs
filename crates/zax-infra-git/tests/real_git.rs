#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
//! Drives the adapter against a real git repository in a temporary directory.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use zax_app::GitRepo;
use zax_domain::BranchName;
use zax_exec::SystemExec;
use zax_infra_git::GitCli;

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "zax test")
        .env("GIT_AUTHOR_EMAIL", "zax@example.invalid")
        .env("GIT_COMMITTER_NAME", "zax test")
        .env("GIT_COMMITTER_EMAIL", "zax@example.invalid")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

/// A repo called `acme` with one commit on `main`.
fn fixture() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("src").join("acme");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "--initial-branch=main", "--quiet"]);
    std::fs::write(repo.join("README.md"), "zax\n").unwrap();
    git(&repo, &["add", "README.md"]);
    git(&repo, &["commit", "--quiet", "-m", "Add README"]);
    (tmp, repo)
}

#[test]
fn resolves_the_repo_name_and_branch_from_the_main_checkout() {
    let (_tmp, repo) = fixture();
    let cli = GitCli::new(SystemExec, repo.clone());

    assert_eq!(cli.root_repo_name().unwrap().as_str(), "acme");
    assert_eq!(cli.current_branch().unwrap().unwrap().as_str(), "main");
    assert_eq!(
        cli.main_worktree().unwrap().canonicalize().unwrap(),
        repo.canonicalize().unwrap()
    );
}

#[test]
fn creates_lists_and_removes_a_worktree() {
    let (tmp, repo) = fixture();
    let cli = GitCli::new(SystemExec, repo.clone());
    let branch = BranchName::parse_local("OMEGA-1963-stamp").unwrap();
    let target = tmp
        .path()
        .join("worktrees")
        .join("acme")
        .join("OMEGA-1963-stamp");

    assert!(!cli.branch_exists(&branch).unwrap());
    cli.add_worktree_new_branch(&target, &branch, None).unwrap();
    assert!(cli.branch_exists(&branch).unwrap());
    assert!(target.join("README.md").is_file());

    let listed = cli.list_worktrees().unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed[0].is_main);
    assert!(listed
        .iter()
        .any(|e| e.branch.as_ref().map(|b| b.as_str()) == Some("OMEGA-1963-stamp")));

    cli.remove_worktree(&target, false).unwrap();
    assert_eq!(cli.list_worktrees().unwrap().len(), 1);
}

#[test]
fn resolves_the_same_repo_name_from_inside_a_worktree() {
    let (tmp, repo) = fixture();
    let main_cli = GitCli::new(SystemExec, repo.clone());
    let branch = BranchName::parse_local("OMEGA-1963-stamp").unwrap();
    let target = tmp
        .path()
        .join("worktrees")
        .join("acme")
        .join("OMEGA-1963-stamp");
    main_cli
        .add_worktree_new_branch(&target, &branch, None)
        .unwrap();

    // This is the property the whole path rule rests on.
    let worktree_cli = GitCli::new(SystemExec, target);
    assert_eq!(worktree_cli.root_repo_name().unwrap().as_str(), "acme");
    assert_eq!(
        worktree_cli.current_branch().unwrap().unwrap().as_str(),
        "OMEGA-1963-stamp"
    );
}

#[test]
fn a_detached_head_reports_no_branch() {
    let (_tmp, repo) = fixture();
    git(&repo, &["checkout", "--quiet", "--detach", "HEAD"]);
    let cli = GitCli::new(SystemExec, repo);
    assert_eq!(cli.current_branch().unwrap(), None);
}

#[test]
fn a_directory_that_is_not_a_repository_fails_with_gits_own_message() {
    let tmp = TempDir::new().unwrap();
    let cli = GitCli::new(SystemExec, tmp.path().to_owned());
    let err = cli.common_dir().unwrap_err();
    assert!(
        err.to_string().contains("not a git repository"),
        "expected git's own message, got: {err}"
    );
}
