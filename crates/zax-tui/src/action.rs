//! What a keystroke asks the host process to do. Panes never perform I/O
//! themselves; they return an action.

use std::path::PathBuf;

use zax_domain::{BranchName, RepoSlug, TicketKey, TodoEdit};

/// What to do once a pull request's worktree exists. Typed rather than a
/// bool so a call site reads `Open`, not an unlabelled `false`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMode {
    /// Just open the worktree in the editor.
    Open,
    /// Open it and start a Claude review of the pull request.
    OpenAndReview,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PaneAction {
    #[default]
    None,
    OpenUrl(String),
    CopyUrl(String),
    Refresh,
    /// `zax worktree review` for the selected row.
    ReviewWorktree {
        repo: RepoSlug,
        number: u32,
        mode: ReviewMode,
    },
    /// A change to the todo file, sent to the broker.
    Todo(TodoEdit),
    /// Open a file in the editor.
    OpenFile(PathBuf),
    Quit,
    /// `zax worktree new <branch> [--repo <path>]`. `repo` is `None` in
    /// today's single-repository behaviour, and the checkout chosen by
    /// `ChooseRepo` otherwise.
    StartWork {
        branch: BranchName,
        repo: Option<PathBuf>,
    },
    /// A ticket belongs to no repository, so before starting work the host
    /// is asked which checkout under `projects.root` to put it in.
    ChooseRepo {
        branch: BranchName,
    },
    /// Open an existing worktree in the editor. `others` counts the ticket's
    /// other worktrees, so the pane can say it picked one.
    OpenWorktree {
        path: PathBuf,
        others: usize,
    },
    /// Ask the provider which transitions the ticket has.
    ListTransitions {
        key: TicketKey,
    },
    Transition {
        key: TicketKey,
        id: String,
    },
    /// Open the ticket's worktrees in a popup over the whole window, where
    /// they can be removed.
    ListWorktrees {
        key: TicketKey,
    },
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use zax_domain::RepoSlug;

    use super::*;

    #[test]
    fn actions_compare_by_value_so_key_handling_is_testable() {
        assert_eq!(PaneAction::Refresh, PaneAction::Refresh);
        assert_ne!(PaneAction::Refresh, PaneAction::Quit);
        assert_eq!(
            PaneAction::ReviewWorktree {
                repo: RepoSlug::parse("acme/omega").unwrap(),
                number: 1963,
                mode: ReviewMode::Open,
            },
            PaneAction::ReviewWorktree {
                repo: RepoSlug::parse("acme/omega").unwrap(),
                number: 1963,
                mode: ReviewMode::Open,
            }
        );
    }

    #[test]
    fn doing_nothing_is_an_action_so_key_handlers_never_return_an_option() {
        assert_eq!(PaneAction::default(), PaneAction::None);
    }
}
