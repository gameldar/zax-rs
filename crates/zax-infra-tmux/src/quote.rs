//! tmux takes a shell command as one string, so argv has to be re-quoted.

/// POSIX single-quote each argument that is not obviously safe, and join them.
pub fn shell_quote(argv: &[String]) -> String {
    argv.iter()
        .map(|arg| quote_one(arg))
        .collect::<Vec<_>>()
        .join(" ")
}

fn quote_one(arg: &str) -> String {
    if !arg.is_empty() && arg.chars().all(is_safe) {
        return arg.to_owned();
    }
    // Inside single quotes everything is literal, so a single quote is closed,
    // escaped and reopened.
    format!("'{}'", arg.replace('\'', r"'\''"))
}

fn is_safe(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '_' | '@' | '%' | '+' | '=' | ':' | ',' | '.' | '/' | '-')
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn q(args: &[&str]) -> String {
        shell_quote(&args.iter().map(|a| (*a).to_owned()).collect::<Vec<_>>())
    }

    #[test]
    fn plain_words_are_left_alone() {
        assert_eq!(q(&["claude"]), "claude");
        assert_eq!(
            q(&["zax", "pane", "prs-for-review"]),
            "zax pane prs-for-review"
        );
    }

    #[test]
    fn paths_and_flags_are_left_alone() {
        assert_eq!(
            q(&["/nix/store/abc/bin/zax", "--every", "60s"]),
            "/nix/store/abc/bin/zax --every 60s"
        );
    }

    #[test]
    fn anything_with_a_space_is_quoted() {
        assert_eq!(q(&["sh", "-c", "direnv allow"]), "sh -c 'direnv allow'");
    }

    #[test]
    fn a_single_quote_is_escaped_the_posix_way() {
        assert_eq!(q(&["echo", "it's"]), r#"echo 'it'\''s'"#);
    }

    #[test]
    fn shell_metacharacters_are_quoted_not_interpreted() {
        assert_eq!(q(&["echo", "a;rm -rf /"]), "echo 'a;rm -rf /'");
        assert_eq!(q(&["echo", "$HOME"]), "echo '$HOME'");
        assert_eq!(q(&["echo", "a&b"]), "echo 'a&b'");
        assert_eq!(q(&["echo", "`id`"]), "echo '`id`'");
    }

    #[test]
    fn an_empty_argument_survives_as_an_empty_quoted_string() {
        assert_eq!(q(&["echo", ""]), "echo ''");
    }
}
