//! Making a fresh worktree usable: the gitignored-but-required files, and the
//! repo's own setup hooks.

use std::path::{Path, PathBuf};

use thiserror::Error;
use zax_domain::{BranchName, Command, WorktreePath};

use crate::ports::{FileSystem, PortError, ProcessRunner};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Provisioning {
    /// Paths relative to the main working tree.
    pub copy: Vec<PathBuf>,
    pub hooks: Vec<Command>,
    pub run_hooks: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProvisionReport {
    pub copied: Vec<PathBuf>,
    /// Configured but absent in the source tree, which is not an error: a repo
    /// config can list `.envrc` for the people who use direnv.
    pub skipped: Vec<PathBuf>,
    pub hooks_run: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProvisionError {
    #[error("could not copy {path} into the new worktree: {source}")]
    Copy { path: PathBuf, source: PortError },
    #[error("setup hook {index} (`{script}`) failed: {source}")]
    Hook {
        index: usize,
        script: String,
        source: PortError,
    },
}

/// The environment every hook sees.
pub fn hook_env(
    target: &WorktreePath,
    branch: &BranchName,
    source_repo: &Path,
) -> Vec<(String, String)> {
    vec![
        (
            "ZAX_WORKTREE".to_owned(),
            target.as_path().to_string_lossy().into_owned(),
        ),
        ("ZAX_BRANCH".to_owned(), branch.as_str().to_owned()),
        (
            "ZAX_TICKET".to_owned(),
            branch
                .ticket()
                .map(|t| t.as_str().to_owned())
                .unwrap_or_default(),
        ),
        (
            "ZAX_SOURCE_REPO".to_owned(),
            source_repo.to_string_lossy().into_owned(),
        ),
    ]
}

/// Copy, then run hooks. Copying happens first so a hook can depend on `.env`.
pub fn apply<F: FileSystem, P: ProcessRunner>(
    fs: &F,
    runner: &P,
    source: &Path,
    target: &WorktreePath,
    branch: &BranchName,
    source_repo: &Path,
    plan: &Provisioning,
) -> Result<ProvisionReport, ProvisionError> {
    let mut report = ProvisionReport::default();

    for relative in &plan.copy {
        let from = source.join(relative);
        if !fs.exists(&from) {
            report.skipped.push(relative.clone());
            continue;
        }
        let to = target.as_path().join(relative);
        if let Some(parent) = to.parent() {
            fs.create_dir_all(parent)
                .map_err(|source| ProvisionError::Copy {
                    path: relative.clone(),
                    source,
                })?;
        }
        fs.copy_file(&from, &to)
            .map_err(|source| ProvisionError::Copy {
                path: relative.clone(),
                source,
            })?;
        report.copied.push(relative.clone());
    }

    if !plan.run_hooks {
        return Ok(report);
    }

    let env = hook_env(target, branch, source_repo);
    for (index, hook) in plan.hooks.iter().enumerate() {
        runner
            .run(target.as_path(), hook, &env)
            .map_err(|source| ProvisionError::Hook {
                index,
                script: script_of(hook),
                source,
            })?;
        report.hooks_run += 1;
    }

    Ok(report)
}

/// The human-readable form of a hook, for an error message.
///
/// Hooks that came from configuration are built by `Command::shell` and look
/// like `["sh", "-c", script]`, where the script is what the user actually
/// wrote and is the only useful thing to show. But `Provisioning::hooks` is a
/// `Vec<Command>` and nothing in the type system says a hook must have that
/// shape, so anything else is rendered in full rather than by guessing which
/// argument is the interesting one. Taking the last argument unconditionally
/// reported `bundle install --quiet` as the bare flag `--quiet`.
fn script_of(hook: &Command) -> String {
    match hook.argv() {
        [program, flag, script] if program == "sh" && flag == "-c" => script.clone(),
        argv => argv.join(" "),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::{Path, PathBuf};

    use zax_domain::{BranchName, Command, RootRepoName, WorktreePath, WorktreeRoot};

    use super::*;
    use crate::testing::{FakeFs, FakeRunner};

    fn target() -> WorktreePath {
        WorktreePath::new(
            &WorktreeRoot::new(PathBuf::from("/Users/me/src/worktrees")).unwrap(),
            &RootRepoName::parse("acme").unwrap(),
            &BranchName::parse_local("OMEGA-1963-stamp").unwrap(),
            Path::new("/Users/me/src/acme"),
        )
        .unwrap()
    }

    fn branch() -> BranchName {
        BranchName::parse_local("OMEGA-1963-stamp").unwrap()
    }

    #[test]
    fn copies_the_gitignored_files_that_exist_and_skips_the_rest() {
        let fs = FakeFs::with_existing(["/Users/me/src/acme/.env"]);
        let runner = FakeRunner::default();
        let plan = Provisioning {
            copy: vec![PathBuf::from(".env"), PathBuf::from(".envrc")],
            hooks: vec![],
            run_hooks: true,
        };

        let report = apply(
            &fs,
            &runner,
            Path::new("/Users/me/src/acme"),
            &target(),
            &branch(),
            Path::new("/Users/me/src/acme"),
            &plan,
        )
        .unwrap();

        assert_eq!(report.copied, vec![PathBuf::from(".env")]);
        assert_eq!(report.skipped, vec![PathBuf::from(".envrc")]);
        assert_eq!(
            fs.copies(),
            vec![(
                PathBuf::from("/Users/me/src/acme/.env"),
                PathBuf::from("/Users/me/src/worktrees/acme/OMEGA-1963-stamp/.env"),
            )]
        );
    }

    #[test]
    fn creates_the_parent_directory_for_a_nested_copy_target() {
        let fs = FakeFs::with_existing(["/Users/me/src/acme/config/secrets.yml"]);
        let runner = FakeRunner::default();
        let plan = Provisioning {
            copy: vec![PathBuf::from("config/secrets.yml")],
            hooks: vec![],
            run_hooks: true,
        };

        apply(
            &fs,
            &runner,
            Path::new("/Users/me/src/acme"),
            &target(),
            &branch(),
            Path::new("/Users/me/src/acme"),
            &plan,
        )
        .unwrap();

        assert!(fs.created_dirs().contains(&PathBuf::from(
            "/Users/me/src/worktrees/acme/OMEGA-1963-stamp/config"
        )));
    }

    #[test]
    fn runs_hooks_in_the_new_worktree_with_the_documented_environment() {
        let fs = FakeFs::default();
        let runner = FakeRunner::default();
        let plan = Provisioning {
            copy: vec![],
            hooks: vec![
                Command::shell("direnv allow").unwrap(),
                Command::shell("bundle install").unwrap(),
            ],
            run_hooks: true,
        };

        let report = apply(
            &fs,
            &runner,
            Path::new("/Users/me/src/acme"),
            &target(),
            &branch(),
            Path::new("/Users/me/src/acme"),
            &plan,
        )
        .unwrap();

        assert_eq!(report.hooks_run, 2);
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert_eq!(
            calls[0].cwd,
            PathBuf::from("/Users/me/src/worktrees/acme/OMEGA-1963-stamp")
        );
        assert_eq!(
            calls[0].env,
            vec![
                (
                    "ZAX_WORKTREE".to_owned(),
                    "/Users/me/src/worktrees/acme/OMEGA-1963-stamp".to_owned()
                ),
                ("ZAX_BRANCH".to_owned(), "OMEGA-1963-stamp".to_owned()),
                ("ZAX_TICKET".to_owned(), "OMEGA-1963".to_owned()),
                (
                    "ZAX_SOURCE_REPO".to_owned(),
                    "/Users/me/src/acme".to_owned()
                ),
            ]
        );
    }

    #[test]
    fn zax_ticket_is_empty_when_the_branch_carries_no_ticket() {
        let env = hook_env(
            &target(),
            &BranchName::parse_local("branded-primitive-helper").unwrap(),
            Path::new("/Users/me/src/acme"),
        );
        assert_eq!(env[2], ("ZAX_TICKET".to_owned(), String::new()));
    }

    #[test]
    fn no_hooks_runs_none_of_them_but_still_copies() {
        let fs = FakeFs::with_existing(["/Users/me/src/acme/.env"]);
        let runner = FakeRunner::default();
        let plan = Provisioning {
            copy: vec![PathBuf::from(".env")],
            hooks: vec![Command::shell("direnv allow").unwrap()],
            run_hooks: false,
        };

        let report = apply(
            &fs,
            &runner,
            Path::new("/Users/me/src/acme"),
            &target(),
            &branch(),
            Path::new("/Users/me/src/acme"),
            &plan,
        )
        .unwrap();

        assert_eq!(report.hooks_run, 0);
        assert_eq!(report.copied.len(), 1);
        assert!(runner.calls().is_empty());
    }

    #[test]
    fn files_are_copied_before_any_hook_runs() {
        // A hook such as `direnv allow` or `bundle install` may need a file that
        // was just copied, so the order is part of the contract. Nothing else in
        // this suite would fail if the two loops were swapped.
        let fs = FakeFs::with_existing(["/Users/me/src/acme/.env"]);
        // Fail the very first hook. If hooks ran before copying, `apply` would
        // return here having copied nothing, which is what makes this assert
        // the ordering rather than merely observe it.
        let runner = FakeRunner::failing_at(0, "no direnv here");
        let plan = Provisioning {
            copy: vec![PathBuf::from(".env")],
            hooks: vec![Command::shell("direnv allow").unwrap()],
            run_hooks: true,
        };

        let err = apply(
            &fs,
            &runner,
            Path::new("/Users/me/src/acme"),
            &target(),
            &branch(),
            Path::new("/Users/me/src/acme"),
            &plan,
        )
        .unwrap_err();

        assert!(
            matches!(err, ProvisionError::Hook { index: 0, .. }),
            "got: {err:?}"
        );
        assert_eq!(
            fs.copies().len(),
            1,
            "the file must already be copied when the first hook runs"
        );
    }

    #[test]
    fn an_error_names_a_hook_that_was_not_built_from_a_shell_script() {
        // `script_of` must not assume every hook is `sh -c <script>`; taking the
        // last argument blindly once reported this hook as `--quiet`.
        let fs = FakeFs::default();
        let runner = FakeRunner::failing_at(0, "no bundler here");
        let plan = Provisioning {
            copy: vec![],
            hooks: vec![Command::new(vec![
                "bundle".to_owned(),
                "install".to_owned(),
                "--quiet".to_owned(),
            ])
            .unwrap()],
            run_hooks: true,
        };

        let err = apply(
            &fs,
            &runner,
            Path::new("/Users/me/src/acme"),
            &target(),
            &branch(),
            Path::new("/Users/me/src/acme"),
            &plan,
        )
        .unwrap_err();

        match err {
            ProvisionError::Hook { ref script, .. } => {
                assert_eq!(script, "bundle install --quiet");
            }
            other => panic!("expected a hook failure, got {other:?}"),
        }
    }

    #[test]
    fn a_failing_hook_stops_the_run_and_says_which_one() {
        let fs = FakeFs::default();
        let runner = FakeRunner::failing_at(1, "bundler exploded");
        let plan = Provisioning {
            copy: vec![],
            hooks: vec![
                Command::shell("direnv allow").unwrap(),
                Command::shell("bundle install").unwrap(),
                Command::shell("never reached").unwrap(),
            ],
            run_hooks: true,
        };

        let err = apply(
            &fs,
            &runner,
            Path::new("/Users/me/src/acme"),
            &target(),
            &branch(),
            Path::new("/Users/me/src/acme"),
            &plan,
        )
        .unwrap_err();

        match err {
            ProvisionError::Hook {
                index, ref script, ..
            } => {
                assert_eq!(index, 1);
                assert_eq!(script, "bundle install");
            }
            other => panic!("expected a hook failure, got {other:?}"),
        }
        assert_eq!(runner.calls().len(), 2, "the third hook must not run");
    }
}
