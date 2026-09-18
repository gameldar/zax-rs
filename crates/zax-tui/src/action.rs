//! What a keystroke asks the host process to do. Panes never perform I/O
//! themselves; they return an action.

use zax_domain::RepoSlug;

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
    Quit,
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
