//! zax will not nest a multiplexer inside a multiplexer.

/// `$TMUX` is set to a non-empty value.
pub fn inside_multiplexer(tmux_env: Option<&str>) -> bool {
    tmux_env.is_some_and(|value| !value.is_empty())
}

/// Read it from the real environment.
pub fn inside_multiplexer_now() -> bool {
    inside_multiplexer(std::env::var("TMUX").ok().as_deref())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn no_tmux_variable_means_we_are_not_nested() {
        assert!(!inside_multiplexer(None));
    }

    #[test]
    fn an_empty_tmux_variable_is_treated_as_not_set() {
        // Some shells export TMUX as empty rather than unsetting it.
        assert!(!inside_multiplexer(Some("")));
    }

    #[test]
    fn a_tmux_variable_means_we_are_nested() {
        assert!(inside_multiplexer(Some("/tmp/tmux-501/default,12345,0")));
    }

    #[test]
    fn even_our_own_socket_counts_as_nested() {
        // A session inside a session is still a session inside a session.
        assert!(inside_multiplexer(Some("/tmp/tmux-501/zax,12345,0")));
    }
}
