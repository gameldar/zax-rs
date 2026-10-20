#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
//! Drives the adapter against a real git repository in a temporary directory.

use std::path::Path;
use std::process::Command;

use tempfile::TempDir;
use zax_app::{CheckoutInspector, GitRepo};
use zax_domain::{BranchName, CheckoutProbe};
use zax_exec::SystemExec;
use zax_infra_git::{GitCheckouts, GitCli};

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

/// The classification `wiring::resolve` leans on, against the real binary:
/// a directory outside any repository has to read as outside one, or a
/// repo-less session would fail where it is meant to start.
#[test]
fn a_directory_outside_any_repository_reads_as_outside_one() {
    let tmp = TempDir::new().unwrap();
    let cli = GitCli::new(SystemExec, tmp.path().to_owned());

    let err = cli.repo_name().unwrap_err();

    assert!(
        zax_infra_git::is_outside_repository(&err),
        "expected git's no-repository answer, got: {err}"
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
/// way OpenSSH fails when the question has been kept off the terminal, and says
/// so plainly when it has not been: a test that hangs waiting for a passphrase
/// reports nothing, and that silence is the bug itself.
///
/// Only `SSH_ASKPASS_REQUIRE` is inspected, because that is the part of the
/// policy that does not depend on the machine. Whether `SSH_ASKPASS` names a
/// real helper is a question about what is installed, and a test that asserted
/// an answer would pass here and fail on a desktop.
const STUB_SSH: &str = "#!/bin/sh\n\
    if [ \"$SSH_ASKPASS_REQUIRE\" = force ]; then\n\
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

#[test]
fn real_git_reports_the_origin_url_and_its_absence() {
    let (_tmp, repo) = fixture();
    let cli = GitCli::new(SystemExec, repo.clone());

    // The gate on the doctor's ssh check reads this, so "no origin" has to be
    // an answer rather than an error.
    assert_eq!(cli.remote_url("origin").unwrap(), None);

    git(
        &repo,
        &["remote", "add", "origin", "git@github.com:gameldar/zax.git"],
    );

    assert_eq!(
        cli.remote_url("origin").unwrap(),
        Some("git@github.com:gameldar/zax.git".to_owned())
    );
}

#[test]
fn a_main_checkout_reports_its_toplevel_and_origin() {
    let (_tmp, repo) = fixture();
    git(
        &repo,
        &["remote", "add", "origin", "git@github.com:acme/omega.git"],
    );
    let checkouts = GitCheckouts::new(SystemExec);

    assert_eq!(
        checkouts.inspect(&repo).unwrap(),
        CheckoutProbe::Main {
            top: repo.canonicalize().unwrap(),
            origin_url: Some("git@github.com:acme/omega.git".to_owned()),
        }
    );
}

#[test]
fn a_main_checkout_without_origin_has_none() {
    let (_tmp, repo) = fixture();
    let checkouts = GitCheckouts::new(SystemExec);

    assert_eq!(
        checkouts.inspect(&repo).unwrap(),
        CheckoutProbe::Main {
            top: repo.canonicalize().unwrap(),
            origin_url: None,
        }
    );
}

#[test]
fn a_linked_worktree_is_reported_as_one() {
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

    let checkouts = GitCheckouts::new(SystemExec);
    assert_eq!(
        checkouts.inspect(&target).unwrap(),
        CheckoutProbe::LinkedWorktree
    );
}

#[test]
fn a_plain_file_under_the_root_is_not_a_checkout() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("notes.txt");
    std::fs::write(&file, "hi\n").unwrap();
    let checkouts = GitCheckouts::new(SystemExec);

    assert_eq!(
        checkouts.inspect(&file).unwrap(),
        CheckoutProbe::NotACheckout
    );
}

#[test]
fn a_missing_path_is_absent() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("nope");
    let checkouts = GitCheckouts::new(SystemExec);

    assert_eq!(checkouts.inspect(&missing).unwrap(), CheckoutProbe::Absent);
}

#[test]
fn a_directory_inside_another_repository_is_not_a_checkout() {
    // A plain subdirectory of a repo still answers `--show-toplevel`
    // successfully, naming the enclosing repo. Without the toplevel
    // comparison this would be misread as that repo's main checkout.
    let (_tmp, repo) = fixture();
    let sub = repo.join("sub");
    std::fs::create_dir(&sub).unwrap();
    let checkouts = GitCheckouts::new(SystemExec);

    assert_eq!(
        checkouts.inspect(&sub).unwrap(),
        CheckoutProbe::NotACheckout
    );
}

#[test]
fn the_probe_reports_the_canonical_toplevel() {
    let (tmp, repo) = fixture();
    let link = tmp.path().join("link-to-acme");
    std::os::unix::fs::symlink(&repo, &link).unwrap();
    let checkouts = GitCheckouts::new(SystemExec);

    match checkouts.inspect(&link).unwrap() {
        CheckoutProbe::Main { top, .. } => assert_eq!(top, repo.canonicalize().unwrap()),
        other => panic!("expected a main checkout, got {other:?}"),
    }
}

/// A `projects.root` of symlinks into other volumes is an ordinary way to
/// keep checkouts, and the probe already follows a symlink to its canonical
/// toplevel - so dropping one here would lose the checkout before anything
/// got the chance to inspect it.
#[test]
fn subdirectories_lists_a_symlink_to_a_directory() {
    let tmp = TempDir::new().unwrap();
    let real = tmp.path().join("elsewhere");
    std::fs::create_dir(&real).unwrap();
    let link = tmp.path().join("omega");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let dangling = tmp.path().join("gone");
    std::os::unix::fs::symlink(tmp.path().join("nowhere"), &dangling).unwrap();
    let checkouts = GitCheckouts::new(SystemExec);

    let found = checkouts.subdirectories(tmp.path()).unwrap();

    assert!(found.contains(&link), "got: {found:?}");
    // A link that leads nowhere is not a directory, which is an answer and
    // not a reason to lose the rest of the listing.
    assert!(!found.contains(&dangling), "got: {found:?}");
}

#[test]
fn subdirectories_lists_only_directories() {
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir(tmp.path().join("a")).unwrap();
    std::fs::write(tmp.path().join("b"), "file\n").unwrap();
    std::fs::create_dir(tmp.path().join("c")).unwrap();
    let checkouts = GitCheckouts::new(SystemExec);

    let mut found = checkouts.subdirectories(tmp.path()).unwrap();
    found.sort();

    assert_eq!(found, vec![tmp.path().join("a"), tmp.path().join("c")]);
}
