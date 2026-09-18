//! Pure argv construction, so the adapter tests assert the exact command line.

use std::path::Path;

use zax_domain::BranchName;

fn s(value: &str) -> String {
    value.to_owned()
}

fn p(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn version() -> Vec<String> {
    vec![s("--version")]
}

pub fn common_dir() -> Vec<String> {
    vec![
        s("rev-parse"),
        s("--path-format=absolute"),
        s("--git-common-dir"),
    ]
}

pub fn current_branch() -> Vec<String> {
    vec![s("symbolic-ref"), s("--quiet"), s("--short"), s("HEAD")]
}

pub fn branch_exists(branch: &BranchName) -> Vec<String> {
    vec![
        s("show-ref"),
        s("--verify"),
        s("--quiet"),
        format!("refs/heads/{}", branch.as_str()),
    ]
}

pub fn add_worktree_new_branch(
    path: &Path,
    branch: &BranchName,
    base: Option<&str>,
) -> Vec<String> {
    let mut argv = vec![
        s("worktree"),
        s("add"),
        s("-b"),
        s(branch.as_str()),
        p(path),
    ];
    if let Some(base) = base {
        argv.push(s(base));
    }
    argv
}

/// Note the `--`. The branch comes from a pull request and is not ours to name,
/// so it ends option parsing before any value git could otherwise read as a
/// flag. `BranchName` already refuses a leading dash, which is the real
/// defence; this is the second one, in the place where a mistake would matter.
pub fn add_worktree_existing_branch(path: &Path, branch: &BranchName) -> Vec<String> {
    vec![
        s("worktree"),
        s("add"),
        s("--"),
        p(path),
        s(branch.as_str()),
    ]
}

pub fn list_worktrees() -> Vec<String> {
    vec![s("worktree"), s("list"), s("--porcelain")]
}

pub fn remove_worktree(path: &Path, force: bool) -> Vec<String> {
    let mut argv = vec![s("worktree"), s("remove")];
    if force {
        argv.push(s("--force"));
    }
    argv.push(p(path));
    argv
}

pub fn fetch(remote: &str, refspec: &str) -> Vec<String> {
    vec![s("fetch"), s(remote), s(refspec)]
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::Path;

    use zax_domain::BranchName;

    use super::*;

    #[test]
    fn the_common_dir_query_asks_for_an_absolute_path() {
        assert_eq!(
            common_dir(),
            ["rev-parse", "--path-format=absolute", "--git-common-dir"]
        );
    }

    #[test]
    fn the_current_branch_query_is_quiet_so_a_detached_head_is_not_noise() {
        assert_eq!(
            current_branch(),
            ["symbolic-ref", "--quiet", "--short", "HEAD"]
        );
    }

    #[test]
    fn branch_existence_is_a_verified_ref_lookup() {
        let branch = BranchName::parse_local("OMEGA-1963-stamp").unwrap();
        assert_eq!(
            branch_exists(&branch),
            [
                "show-ref",
                "--verify",
                "--quiet",
                "refs/heads/OMEGA-1963-stamp"
            ]
        );
    }

    #[test]
    fn adding_a_new_branch_worktree_passes_dash_b() {
        let branch = BranchName::parse_local("OMEGA-1963-stamp").unwrap();
        assert_eq!(
            add_worktree_new_branch(
                Path::new("/Users/me/src/worktrees/acme/OMEGA-1963-stamp"),
                &branch,
                None
            ),
            [
                "worktree",
                "add",
                "-b",
                "OMEGA-1963-stamp",
                "/Users/me/src/worktrees/acme/OMEGA-1963-stamp",
            ]
        );
    }

    #[test]
    fn a_base_is_appended_after_the_path() {
        let branch = BranchName::parse_local("OMEGA-1963-stamp").unwrap();
        assert_eq!(
            add_worktree_new_branch(
                Path::new("/w/OMEGA-1963-stamp"),
                &branch,
                Some("origin/main")
            ),
            [
                "worktree",
                "add",
                "-b",
                "OMEGA-1963-stamp",
                "/w/OMEGA-1963-stamp",
                "origin/main"
            ]
        );
    }

    #[test]
    fn adding_an_existing_branch_worktree_omits_dash_b() {
        let branch = BranchName::parse_foreign("dependabot/cargo/serde-1.2.3").unwrap();
        assert_eq!(
            add_worktree_existing_branch(Path::new("/w/dependabot/cargo/serde-1.2.3"), &branch),
            [
                "worktree",
                "add",
                "--",
                "/w/dependabot/cargo/serde-1.2.3",
                "dependabot/cargo/serde-1.2.3",
            ]
        );
    }

    #[test]
    fn removal_only_forces_when_asked() {
        assert_eq!(
            remove_worktree(Path::new("/w/a"), false),
            ["worktree", "remove", "/w/a"]
        );
        assert_eq!(
            remove_worktree(Path::new("/w/a"), true),
            ["worktree", "remove", "--force", "/w/a"]
        );
    }

    #[test]
    fn fetch_passes_remote_and_refspec_verbatim() {
        assert_eq!(
            fetch("origin", "+refs/pull/1963/head:refs/heads/patch-1"),
            ["fetch", "origin", "+refs/pull/1963/head:refs/heads/patch-1"]
        );
    }

    #[test]
    fn the_worktree_listing_is_machine_readable() {
        assert_eq!(list_worktrees(), ["worktree", "list", "--porcelain"]);
    }
}
