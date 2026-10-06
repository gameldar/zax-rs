//! `zax worktree new | review | list | rm`.

use std::path::Path;

use anyhow::Result;
use zax_app::{
    queries, CreateReviewWorktree, CreateReviewWorktreeRequest, CreateWorktree,
    CreateWorktreeRequest, Editor, Provisioning,
};
use zax_domain::{BranchName, PrRef};

use crate::cli::WorktreeCommand;
use crate::warning;
use crate::wiring::{self, RealFs, RealRunner};

pub fn run(command: WorktreeCommand) -> Result<()> {
    match command {
        WorktreeCommand::New {
            branch,
            from,
            no_hooks,
            no_open,
        } => new(branch, from, no_hooks, no_open),
        WorktreeCommand::Review {
            pr,
            no_hooks,
            force,
            no_open,
        } => review(pr, no_hooks, force, no_open),
        WorktreeCommand::List => list(),
        WorktreeCommand::Rm { branch, force } => remove(branch, force),
    }
}

fn provisioning(config: &zax_config::Config, run_hooks: bool) -> Provisioning {
    Provisioning {
        copy: config.worktree.copy.clone(),
        hooks: config.worktree.hooks.clone(),
        run_hooks,
    }
}

fn new(branch: String, from: Option<String>, no_hooks: bool, no_open: bool) -> Result<()> {
    let ctx = wiring::resolve()?;
    let branch = BranchName::parse_local(&branch)?;

    let outcome =
        CreateWorktree::new(&ctx.git, &RealFs, &RealRunner).run(CreateWorktreeRequest {
            branch,
            base: from,
            root: ctx.config.worktree.root.clone(),
            provisioning: provisioning(&ctx.config, !no_hooks),
        })?;

    report(&outcome);
    warn(open_in_editor(
        &wiring::editor(&ctx.config),
        outcome.path.as_path(),
        !no_open,
    ));
    Ok(())
}

fn review(pr: String, no_hooks: bool, force: bool, no_open: bool) -> Result<()> {
    let ctx = wiring::resolve()?;
    let pr = PrRef::parse(&pr)?;

    let outcome = CreateReviewWorktree::new(&ctx.git, &ctx.gh, &RealFs, &RealRunner).run(
        CreateReviewWorktreeRequest {
            pr,
            root: ctx.config.worktree.root.clone(),
            provisioning: provisioning(&ctx.config, !no_hooks),
            force,
        },
    )?;

    // Before the path, because it is the one line here that reports something
    // lost rather than something made.
    if let Some(forced) = &outcome.forced_update {
        println!(
            "moved branch {} off its own history: {} -> {}",
            forced.branch, forced.previous_tip, forced.head
        );
    }
    report(&outcome.worktree);
    warn(open_in_editor(
        &wiring::editor(&ctx.config),
        outcome.worktree.path.as_path(),
        !no_open,
    ));
    Ok(())
}

/// Hand the finished worktree to the editor, after the copies and the hooks:
/// what opens is a worktree that is ready to work in, not a bare checkout
/// that grows its config files while you look at it.
///
/// The returned line is a warning, never an error. The worktree was made
/// either way, and the review pane reads a non-zero exit as "the review was
/// refused", which an editor that would not start does not make true.
fn open_in_editor<E: Editor>(editor: &E, path: &Path, open: bool) -> Option<String> {
    if !open {
        return None;
    }
    editor.open(path).err().map(|e| e.to_string())
}

/// Marked as zax's own, because the review pane runs this command as a
/// subprocess and an unmarked line is indistinguishable from a hook's chatter.
fn warn(message: Option<String>) {
    let Some(message) = message else { return };
    warning::print(&format!(
        "could not open the worktree in the editor: {message}"
    ));
}

fn report(outcome: &zax_app::CreateWorktreeOutcome) {
    println!("{}", outcome.path.as_path().display());
    for path in &outcome.report.copied {
        println!("  copied {}", path.display());
    }
    for path in &outcome.report.skipped {
        println!(
            "  skipped {} (not present in the source repo)",
            path.display()
        );
    }
    if outcome.report.hooks_run > 0 {
        println!("  ran {} setup hook(s)", outcome.report.hooks_run);
    }
}

fn list() -> Result<()> {
    let ctx = wiring::resolve()?;
    let entries = queries::list_worktrees(&ctx.git, &ctx.config.worktree.root, &ctx.repo)?;
    if entries.is_empty() {
        println!("no zax worktrees for {}", ctx.repo);
    }
    for entry in entries {
        let branch = entry
            .branch
            .as_ref()
            .map(|b| b.as_str().to_owned())
            .unwrap_or_else(|| "(detached)".to_owned());
        println!("{:<40} {}", branch, entry.path.display());
    }
    Ok(())
}

fn remove(branch: String, force: bool) -> Result<()> {
    let ctx = wiring::resolve()?;
    // Parsed leniently: an old branch that breaks the rule can still be removed.
    let branch = BranchName::parse_foreign(&branch)?;
    let path =
        queries::remove_worktree(&ctx.git, &RealFs, &ctx.config.worktree.root, &branch, force)?;
    println!("removed {}", path.as_path().display());
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::{Path, PathBuf};

    use zax_app::testing::FakeEditor;
    use zax_app::PortError;

    use super::open_in_editor;

    #[test]
    fn a_finished_worktree_is_handed_to_the_editor() {
        let editor = FakeEditor::default();

        let warning = open_in_editor(&editor, Path::new("/src/worktrees/acme/patch-1"), true);

        assert_eq!(warning, None);
        assert_eq!(
            *editor.opened.borrow(),
            vec![PathBuf::from("/src/worktrees/acme/patch-1")]
        );
    }

    #[test]
    fn no_open_leaves_the_editor_alone() {
        let editor = FakeEditor::default();

        open_in_editor(&editor, Path::new("/src/worktrees/acme/patch-1"), false);

        assert!(editor.opened.borrow().is_empty());
    }

    #[test]
    fn an_editor_that_will_not_start_is_a_warning_and_not_a_failed_command() {
        // The worktree exists either way, and the pane reads a non-zero exit
        // as "the review was refused", which would be a lie.
        let editor = FakeEditor::failing(PortError::MissingProgram {
            program: "zed".to_owned(),
            hint: "run `cli: install`".to_owned(),
        });

        let warning = open_in_editor(&editor, Path::new("/src/worktrees/acme/patch-1"), true)
            .expect("a failure the user cannot see is a keystroke that did nothing");

        assert!(warning.contains("zed"), "got: {warning}");
        assert!(warning.contains("cli: install"), "got: {warning}");
    }
}
