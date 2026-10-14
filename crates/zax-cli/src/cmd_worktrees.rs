//! `zax pane worktrees <TICKET>`: a ticket's worktrees, for cleaning up.
//!
//! The tickets pane opens this in a tmux popup over the whole window. It
//! scans the worktree root itself rather than subscribing to the broker:
//! the scan is a directory walk, and the popup has to show what is on disk
//! now, including after its own removals.

use std::path::Path;
use std::time::Duration;

use anyhow::{anyhow, Result};
use ratatui::crossterm::event::{self, Event as TermEvent, KeyEventKind};
use zax_app::WorktreeScanner;
use zax_domain::{TicketKey, Worktree};
use zax_exec::{Exec, SystemExec};
use zax_infra_git::FsWorktreeScanner;
use zax_tui::{WorktreesAction, WorktreesModal};

use crate::screen::Screen;
use crate::wiring;

/// How long the loop waits for a keystroke.
const TICK: Duration = Duration::from_millis(250);

pub fn run(ticket: &str) -> Result<()> {
    let key = TicketKey::parse(ticket).map_err(|e| anyhow!("{ticket}: {e}"))?;
    let ctx = wiring::resolve()?;
    let root = ctx.config.worktree.root.clone();
    let list = |key: &TicketKey| -> Result<Vec<Worktree>> {
        Ok(FsWorktreeScanner
            .scan(&root)
            .map_err(|e| anyhow!(e))?
            .into_iter()
            .filter(|w| w.branch.ticket().as_ref() == Some(key))
            .collect())
    };

    let mut modal = WorktreesModal::new(key.clone(), list(&key)?);
    if modal.is_empty() {
        return Ok(());
    }
    let mut screen = Screen::enter()?;
    loop {
        screen.terminal.draw(|f| modal.render(f, f.area()))?;
        if !event::poll(TICK)? {
            continue;
        }
        let TermEvent::Key(press) = event::read()? else {
            continue;
        };
        if press.kind != KeyEventKind::Press {
            continue;
        }
        match modal.on_key(press) {
            WorktreesAction::Close => return Ok(()),
            WorktreesAction::None => {}
            WorktreesAction::Remove(path) => {
                if let Some(message) = remove_worktree(&SystemExec, &path) {
                    modal.notice(message);
                }
                modal.set_worktrees(list(&key)?);
                // Nothing left to clean up, so there is nothing to show.
                if modal.is_empty() {
                    return Ok(());
                }
            }
        }
    }
}

/// `git worktree remove <path>`, unforced, run from the worktree's own
/// repository. The pane serves every repository under the worktree root, so
/// the repository is found from the worktree: its common dir's parent is the
/// main checkout. Git refuses a worktree with uncommitted changes, and that
/// refusal is the message. The branch is left alone.
fn remove_worktree<E: Exec>(exec: &E, path: &Path) -> Option<String> {
    let common = exec.exec(
        "git",
        &[
            "-C".to_owned(),
            path.to_string_lossy().into_owned(),
            "rev-parse".to_owned(),
            "--path-format=absolute".to_owned(),
            "--git-common-dir".to_owned(),
        ],
        None,
        &[],
    );
    let main = match common {
        Ok(out) if out.ok() => Path::new(out.trimmed_stdout()).parent().map(Path::to_owned),
        Ok(out) => return Some(git_failure(&out.stderr, out.status)),
        Err(e) => return Some(format!("git: {e}")),
    };
    let Some(main) = main else {
        return Some("git: no main worktree found".to_owned());
    };
    let removed = exec.exec(
        "git",
        &[
            "worktree".to_owned(),
            "remove".to_owned(),
            path.to_string_lossy().into_owned(),
        ],
        Some(&main),
        &[],
    );
    match removed {
        Ok(out) if out.ok() => None,
        Ok(out) => Some(git_failure(&out.stderr, out.status)),
        Err(e) => Some(format!("git: {e}")),
    }
}

fn git_failure(stderr: &str, status: i32) -> String {
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map_or_else(|| format!("exited with status {status}"), str::to_owned);
    format!("git: {line}")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::Path;

    use zax_exec::{Output, RecordingExec};

    use super::remove_worktree;

    #[test]
    fn a_worktree_is_removed_from_its_own_repository_without_force() {
        let exec = RecordingExec::with_responses(vec![
            Output {
                status: 0,
                stdout: "/src/omega/.git\n".into(),
                stderr: String::new(),
            },
            Output {
                status: 0,
                stdout: String::new(),
                stderr: String::new(),
            },
        ]);
        assert_eq!(
            remove_worktree(&exec, Path::new("/w/omega/OMEGA-1-a")),
            None
        );
        assert_eq!(
            exec.args(0),
            [
                "-C",
                "/w/omega/OMEGA-1-a",
                "rev-parse",
                "--path-format=absolute",
                "--git-common-dir"
            ]
        );
        assert_eq!(exec.args(1), ["worktree", "remove", "/w/omega/OMEGA-1-a"]);
        assert_eq!(
            exec.calls()[1].cwd.as_deref(),
            Some(Path::new("/src/omega"))
        );
    }

    #[test]
    fn git_refusing_a_dirty_worktree_reaches_the_pane() {
        let exec = RecordingExec::with_responses(vec![
            Output {
                status: 0,
                stdout: "/src/omega/.git\n".into(),
                stderr: String::new(),
            },
            Output {
                status: 128,
                stdout: String::new(),
                stderr: "fatal: '/w/omega/x' contains modified or untracked files, use --force to delete it\n".into(),
            },
        ]);
        let message = remove_worktree(&exec, Path::new("/w/omega/x")).unwrap();
        assert!(message.starts_with("git: fatal:"), "{message}");
        assert!(message.contains("modified or untracked"), "{message}");
    }
}
