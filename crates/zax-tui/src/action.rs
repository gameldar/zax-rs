//! What a keystroke asks the host process to do. Panes never perform I/O
//! themselves; they return an action.

use std::path::PathBuf;

use zax_domain::{BranchName, RepoSlug, TicketKey, TodoEdit};

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
    },
    /// A change to the todo file, sent to the broker.
    Todo(TodoEdit),
    /// Open a file in the editor.
    OpenFile(PathBuf),
    Quit,
    /// `zax worktree new <branch>` in the current repository.
    StartWork {
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
    /// `git worktree remove` on a worktree the popup listed. Never forced:
    /// git refuses one with uncommitted changes, and the branch survives.
    RemoveWorktree {
        path: PathBuf,
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
            },
            PaneAction::ReviewWorktree {
                repo: RepoSlug::parse("acme/omega").unwrap(),
                number: 1963,
            }
        );
    }

    #[test]
    fn doing_nothing_is_an_action_so_key_handlers_never_return_an_option() {
        assert_eq!(PaneAction::default(), PaneAction::None);
    }
}
