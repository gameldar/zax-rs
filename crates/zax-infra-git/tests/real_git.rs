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

/// Reads a revision with the `git` binary rather than through the adapter, so
/// the expectations below do not come from the code they are checking.
fn rev(dir: &Path, revision: &str) -> String {
    let out = Command::new("git")
        .args(["rev-parse", revision])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(out.status.success(), "git rev-parse {revision} failed");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[test]
fn fetches_a_refspec_and_answers_ancestry_against_real_git() {
    // The whole review guard rests on these three: a fetch with no destination
    // writes FETCH_HEAD and nothing else, the two tips resolve, and git decides
    // which way the ancestry runs.
    let (tmp, upstream) = fixture();
    let clone = tmp.path().join("src").join("clone");
    let status = Command::new("git")
        .args(["clone", "--quiet"])
        .arg(&upstream)
        .arg(&clone)
        .status()
        .unwrap();
    assert!(status.success(), "clone failed");

    // One more commit upstream, which the clone has not seen.
    std::fs::write(upstream.join("NEW.md"), "newer\n").unwrap();
    git(&upstream, &["add", "NEW.md"]);
    git(&upstream, &["commit", "--quiet", "-m", "Add NEW"]);

    let cli = GitCli::new(SystemExec, clone.clone());
    let before = rev(&clone, "HEAD");
    cli.fetch("origin", "refs/heads/main").unwrap();

    let fetched = cli.resolve_commit("FETCH_HEAD").unwrap();
    assert_eq!(fetched, rev(&upstream, "HEAD"));
    assert_eq!(
        rev(&clone, "HEAD"),
        before,
        "a refspec with no destination must move nothing local"
    );

    let local = cli.resolve_commit("refs/heads/main").unwrap();
    assert_eq!(local, before);
    assert!(cli.is_ancestor(&local, &fetched).unwrap());
    assert!(!cli.is_ancestor(&fetched, &local).unwrap());

    // A divergent tip: commit locally on top of the old base.
    std::fs::write(clone.join("MINE.md"), "mine\n").unwrap();
    git(&clone, &["add", "MINE.md"]);
    git(&clone, &["commit", "--quiet", "-m", "Add MINE"]);
    let mine = cli.resolve_commit("refs/heads/main").unwrap();
    assert!(
        !cli.is_ancestor(&mine, &fetched).unwrap(),
        "a divergent local tip is what the review guard must refuse"
    );
}

#[test]
fn an_unknown_revision_is_an_error_not_an_echo() {
    let (_tmp, repo) = fixture();
    let cli = GitCli::new(SystemExec, repo);
    assert!(cli.resolve_commit("refs/heads/nope").is_err());
}

/// Stands in for ssh, which is the program that actually prompts. It fails the
/// way OpenSSH fails when it has been denied every way of asking, and says so
/// plainly when it has not been: a test that hangs waiting for a passphrase
/// reports nothing, and that silence is the bug itself.
const STUB_SSH: &str = "#!/bin/sh\n\
    if [ \"$SSH_ASKPASS_REQUIRE\" = force ] && [ ! -x \"$SSH_ASKPASS\" ]; then\n\
    \techo 'git@github.com: Permission denied (publickey).' >&2\n\
    \texit 255\n\
    fi\n\
    echo 'nothing stopped this from prompting on the terminal' >&2\n\
    exit 1\n";

#[test]
fn a_fetch_that_would_prompt_fails_with_a_line_instead_of_waiting() {
    use std::os::unix::fs::PermissionsExt;

    let (tmp, repo) = fixture();
    let ssh = tmp.path().join("ssh");
    std::fs::write(&ssh, STUB_SSH).unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755)).unwrap();

    git(
        &repo,
        &["remote", "add", "origin", "git@github.com:gameldar/zax.git"],
    );
    git(&repo, &["config", "core.sshCommand", ssh.to_str().unwrap()]);

    let cli = GitCli::new(SystemExec, repo);
    let error = cli
        .fetch("origin", "refs/pull/1963/head")
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("Permission denied"),
        "the fetch should have been refused, not asked: {error}"
    );
}
