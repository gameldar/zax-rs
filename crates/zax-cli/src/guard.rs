//! zax will not nest a multiplexer inside a multiplexer.

/// `$TMUX` or `$TMUX_PANE` is set to a non-empty value.
///
/// `TMUX_PANE` counts on its own because zax starts the agent with `TMUX`
/// removed (see the tmux adapter's `new_session`), so an agent that runs
/// `zax session start` carries only `TMUX_PANE` - and is still inside tmux.
pub fn inside_multiplexer(tmux_env: Option<&str>, tmux_pane_env: Option<&str>) -> bool {
    [tmux_env, tmux_pane_env]
        .into_iter()
        .any(|value| value.is_some_and(|value| !value.is_empty()))
}

/// Read it from the real environment.
pub fn inside_multiplexer_now() -> bool {
    inside_multiplexer(
        std::env::var("TMUX").ok().as_deref(),
        std::env::var("TMUX_PANE").ok().as_deref(),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn no_tmux_variable_means_we_are_not_nested() {
        assert!(!inside_multiplexer(None, None));
    }

    #[test]
    fn an_empty_tmux_variable_is_treated_as_not_set() {
        // Some shells export TMUX as empty rather than unsetting it.
        assert!(!inside_multiplexer(Some(""), Some("")));
    }

    #[test]
    fn a_tmux_variable_means_we_are_nested() {
        assert!(inside_multiplexer(
            Some("/tmp/tmux-501/default,12345,0"),
            None
        ));
    }

    #[test]
    fn even_our_own_socket_counts_as_nested() {
        // A session inside a session is still a session inside a session.
        assert!(inside_multiplexer(Some("/tmp/tmux-501/zax,12345,0"), None));
    }

    #[test]
    fn a_tmux_pane_alone_means_we_are_nested() {
        // The agent pane: zax strips TMUX from it, tmux's TMUX_PANE remains.
        assert!(inside_multiplexer(None, Some("%3")));
    }
}
