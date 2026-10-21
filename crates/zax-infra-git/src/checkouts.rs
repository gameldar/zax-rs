//! `CheckoutInspector` via the git CLI: observes whether a path is a main
//! checkout, a linked worktree, or not a checkout at all. The verdict on what
//! that means for a particular repository is drawn elsewhere, in the domain
//! (`verify`/`accept`); this adapter only reports what it observed.

use std::path::{Path, PathBuf};

use zax_app::{CheckoutInspector, PortError};
use zax_domain::CheckoutProbe;
use zax_exec::Exec;

use crate::{argv, map_exec_error, parse_remote_url, TOOL};

pub struct GitCheckouts<E: Exec> {
    exec: E,
}

impl<E: Exec> GitCheckouts<E> {
    pub fn new(exec: E) -> Self {
        Self { exec }
    }

    /// For tests that assert on the recorded calls.
    pub fn exec_ref(&self) -> &E {
        &self.exec
    }

    fn remote_url(&self, path: &Path) -> Result<Option<String>, PortError> {
        let out = self
            .exec
            .exec(TOOL, &argv::remote_url("origin"), Some(path), &[])
            .map_err(map_exec_error)?;
        Ok(parse_remote_url(&out))
    }
}

impl<E: Exec> CheckoutInspector for GitCheckouts<E> {
    fn inspect(&self, path: &Path) -> Result<CheckoutProbe, PortError> {
        if !path.exists() {
            return Ok(CheckoutProbe::Absent);
        }
        if !path.is_dir() {
            return Ok(CheckoutProbe::NotACheckout);
        }

        let out = self
            .exec
            .exec(TOOL, &argv::checkout_probe(), Some(path), &[])
            .map_err(map_exec_error)?;
        if !out.ok() {
            return Ok(CheckoutProbe::NotACheckout);
        }

        let mut lines = out.stdout.lines();
        let toplevel = lines.next().unwrap_or_default();
        let git_dir = lines.next().unwrap_or_default();
        let git_common_dir = lines.next().unwrap_or_default();

        let (Ok(top), Ok(here)) = (Path::new(toplevel).canonicalize(), path.canonicalize()) else {
            // A race or a permissions problem leaves the path unusable as a
            // checkout either way, so this is a rejection, not an error.
            return Ok(CheckoutProbe::NotACheckout);
        };
        if top != here {
            // A plain directory inside another repository (a dotfiles repo at
            // `~`, say) still answers `--show-toplevel` successfully, naming
            // the enclosing repo. This check must run before the worktree
            // check below, or such a directory would be misread as that
            // repo's own main checkout.
            return Ok(CheckoutProbe::NotACheckout);
        }
        if git_dir != git_common_dir {
            return Ok(CheckoutProbe::LinkedWorktree);
        }

        Ok(CheckoutProbe::Main {
            top,
            origin_url: self.remote_url(path)?,
        })
    }

    fn subdirectories(&self, dir: &Path) -> Result<Vec<PathBuf>, PortError> {
        let entries = std::fs::read_dir(dir).map_err(|e| io_error(dir, &e))?;
        let mut found = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| io_error(dir, &e))?;
            // `DirEntry::metadata()` does not follow a symlink on unix, nor
            // does `file_type()`, so a symlinked checkout - an ordinary way
            // to keep one on another volume - would be dropped before
            // anything could inspect it. `fs::metadata` follows it, and a
            // link that leads nowhere is simply not a directory rather than
            // a reason to fail the whole listing.
            if std::fs::metadata(entry.path()).is_ok_and(|meta| meta.is_dir()) {
                found.push(entry.path());
            }
        }
        Ok(found)
    }
}

fn io_error(path: &Path, error: &std::io::Error) -> PortError {
    PortError::Io {
        operation: "list directory",
        path: path.to_owned(),
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use tempfile::TempDir;
    use zax_exec::{Output, RecordingExec};

    use super::*;

    #[test]
    fn the_probe_is_a_single_combined_rev_parse_call() {
        let tmp = TempDir::new().expect("tmp dir");
        let top = tmp.path().canonicalize().expect("canonicalize");
        let exec = RecordingExec::with_responses(vec![
            Output {
                status: 0,
                stdout: format!("{top}\n{top}/.git\n{top}/.git\n", top = top.display()),
                stderr: String::new(),
            },
            Output {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
        ]);
        let checkouts = GitCheckouts::new(exec);

        checkouts.inspect(tmp.path()).unwrap();

        assert_eq!(
            checkouts.exec_ref().args(0),
            [
                "rev-parse",
                "--path-format=absolute",
                "--show-toplevel",
                "--git-dir",
                "--git-common-dir",
            ]
        );
    }

    #[test]
    fn a_main_checkout_asks_for_its_origin_at_its_own_path() {
        let tmp = TempDir::new().expect("tmp dir");
        let top = tmp.path().canonicalize().expect("canonicalize");
        let exec = RecordingExec::with_responses(vec![
            Output {
                status: 0,
                stdout: format!("{top}\n{top}/.git\n{top}/.git\n", top = top.display()),
                stderr: String::new(),
            },
            Output {
                status: 0,
                stdout: "git@github.com:acme/omega.git\n".to_owned(),
                stderr: String::new(),
            },
        ]);
        let checkouts = GitCheckouts::new(exec);

        let probe = checkouts.inspect(tmp.path()).unwrap();

        assert_eq!(
            probe,
            CheckoutProbe::Main {
                top,
                origin_url: Some("git@github.com:acme/omega.git".to_owned()),
            }
        );
        assert_eq!(
            checkouts.exec_ref().args(1),
            ["remote", "get-url", "origin"]
        );
    }

    #[test]
    fn a_non_zero_exit_is_not_a_checkout() {
        let tmp = TempDir::new().expect("tmp dir");
        let exec = RecordingExec::with_responses(vec![Output {
            status: 128,
            stdout: String::new(),
            stderr: "fatal: not a git repository".to_owned(),
        }]);
        let checkouts = GitCheckouts::new(exec);

        assert_eq!(
            checkouts.inspect(tmp.path()).unwrap(),
            CheckoutProbe::NotACheckout
        );
    }

    #[test]
    fn a_toplevel_that_differs_from_the_path_is_not_a_checkout() {
        // The enclosing-repository case: git answers successfully, but names
        // a toplevel other than the path asked about.
        let tmp = TempDir::new().expect("tmp dir");
        let elsewhere = TempDir::new().expect("another tmp dir");
        let other_top = elsewhere.path().canonicalize().expect("canonicalize");
        let exec = RecordingExec::returning(&format!(
            "{top}\n{top}/.git\n{top}/.git\n",
            top = other_top.display()
        ));
        let checkouts = GitCheckouts::new(exec);

        assert_eq!(
            checkouts.inspect(tmp.path()).unwrap(),
            CheckoutProbe::NotACheckout
        );
    }

    #[test]
    fn mismatched_git_and_common_dirs_mean_a_linked_worktree() {
        let tmp = TempDir::new().expect("tmp dir");
        let top = tmp.path().canonicalize().expect("canonicalize");
        let exec = RecordingExec::returning(&format!(
            "{top}\n{top}/.git/worktrees/topic\n/elsewhere/.git\n",
            top = top.display()
        ));
        let checkouts = GitCheckouts::new(exec);

        assert_eq!(
            checkouts.inspect(tmp.path()).unwrap(),
            CheckoutProbe::LinkedWorktree
        );
    }

    #[test]
    fn subdirectories_of_a_missing_directory_is_an_io_error() {
        let tmp = TempDir::new().expect("tmp dir");
        let missing = tmp.path().join("nope");
        let checkouts = GitCheckouts::new(RecordingExec::returning(""));

        let err = checkouts.subdirectories(&missing).unwrap_err();
        assert!(
            matches!(
                err,
                PortError::Io {
                    operation: "list directory",
                    ..
                }
            ),
            "got: {err:?}"
        );
    }
}
