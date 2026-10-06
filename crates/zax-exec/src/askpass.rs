//! Who answers when a captured child needs a passphrase.
//!
//! Keeping a question off the terminal and leaving it unanswerable are two
//! different things, and conflating them is what made a locked ssh key look
//! like a broken keystroke. `SSH_ASKPASS_REQUIRE=force` is enough to move the
//! question off the terminal a pane's TUI is holding; what the question is
//! then shown on is this decision.

use std::path::{Path, PathBuf};

/// Where a captured child's question goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Askpass {
    /// The helper the user named. Their own `SSH_ASKPASS` already answers this
    /// question, and zax overriding it is how a working setup breaks.
    Configured(String),
    /// A helper found on this machine, drawn on the display that is there.
    Discovered(PathBuf),
    /// Nobody can ask, so a locked key fails fast rather than hanging.
    Nobody,
}

/// Named rather than installed, because it has to be a program that cannot be
/// run. An askpass that might exist one day is a prompt waiting to come back.
const NO_ASKPASS: &str = "/nonexistent/zax-has-nobody-to-ask";

impl Askpass {
    /// What to put in `SSH_ASKPASS` or `GIT_ASKPASS`. `Nobody` still names a
    /// program, because the variable being unset is what sends ssh back to
    /// the terminal.
    pub fn program(&self) -> &str {
        match self {
            Self::Configured(program) => program,
            Self::Discovered(path) => path.to_str().unwrap_or(NO_ASKPASS),
            Self::Nobody => NO_ASKPASS,
        }
    }
}

/// Where OpenSSH's askpass helper lives, which is nowhere in particular: the
/// path is a packaging decision and every distribution made a different one.
const CANDIDATES: [&str; 7] = [
    // Debian and Ubuntu, where it is an alternatives symlink to whichever
    // helper is installed.
    "/usr/lib/openssh/ssh-askpass",
    "/usr/lib/ssh/ssh-askpass",
    "/usr/libexec/openssh/ssh-askpass",
    "/usr/libexec/seahorse/ssh-askpass",
    "/usr/bin/ssh-askpass",
    "/usr/local/bin/ssh-askpass",
    "/opt/homebrew/bin/ssh-askpass",
];

/// What the decision depends on, gathered so that it can be made without
/// reading the real environment.
pub struct Environment {
    /// The variable being decided, as the user's own environment set it.
    pub configured: Option<String>,
    /// Whether there is anywhere to draw a window.
    pub has_display: bool,
}

/// An explicit setting, which wins even with no display: the user may have
/// named a helper that needs none, and second-guessing it is the bug being
/// fixed. An exported-but-empty variable names no program.
fn configured(env: &Environment) -> Option<Askpass> {
    env.configured
        .as_deref()
        .filter(|program| !program.is_empty())
        .map(|program| Askpass::Configured(program.to_owned()))
}

/// `SSH_ASKPASS`, which is searched for because an ssh key passphrase has
/// nowhere else to go: an agent is the only other mechanism, and unlocking it
/// is the very thing being asked about.
pub fn for_ssh(env: &Environment, runnable: &impl Fn(&Path) -> bool) -> Askpass {
    if let Some(chosen) = configured(env) {
        return chosen;
    }
    // A graphical helper with nothing to draw on is a prompt that never
    // appears, which is the hang this policy exists to prevent.
    if !env.has_display {
        return Askpass::Nobody;
    }
    CANDIDATES
        .iter()
        .map(Path::new)
        .find(|path| runnable(path))
        .map_or(Askpass::Nobody, |path| Askpass::Discovered(path.to_owned()))
}

/// `GIT_ASKPASS`, which is respected but never searched for. git's own
/// credential prompts have a real answer in a credential helper, so a dialog
/// zax went looking for would be answering a question that should not have
/// been asked - unlike a passphrase, which has no other mechanism.
pub fn for_git(env: &Environment) -> Askpass {
    configured(env).unwrap_or(Askpass::Nobody)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn nothing_runs(_: &Path) -> bool {
        false
    }

    fn everything_runs(_: &Path) -> bool {
        true
    }

    #[test]
    fn the_helper_the_user_named_is_the_one_used() {
        // Overriding this is what broke a machine that had a working askpass
        // installed all along.
        let env = Environment {
            configured: Some("/usr/lib/openssh/ssh-askpass".to_owned()),
            has_display: true,
        };

        assert_eq!(
            for_ssh(&env, &nothing_runs),
            Askpass::Configured("/usr/lib/openssh/ssh-askpass".to_owned())
        );
    }

    #[test]
    fn an_explicit_helper_is_trusted_even_with_no_display() {
        // It may be one that needs no display. The user said so either way.
        let env = Environment {
            configured: Some("/opt/my-askpass".to_owned()),
            has_display: false,
        };

        assert_eq!(
            for_ssh(&env, &nothing_runs),
            Askpass::Configured("/opt/my-askpass".to_owned())
        );
    }

    #[test]
    fn a_machine_with_a_display_uses_the_helper_installed_on_it() {
        let env = Environment {
            configured: None,
            has_display: true,
        };

        assert_eq!(
            for_ssh(&env, &everything_runs),
            Askpass::Discovered(PathBuf::from("/usr/lib/openssh/ssh-askpass"))
        );
    }

    #[test]
    fn the_variable_being_empty_is_not_a_choice() {
        // An exported-but-empty `SSH_ASKPASS` names no program; treating it as
        // one hands ssh an empty path to run.
        let env = Environment {
            configured: Some(String::new()),
            has_display: true,
        };

        assert_eq!(
            for_ssh(&env, &everything_runs),
            Askpass::Discovered(PathBuf::from("/usr/lib/openssh/ssh-askpass"))
        );
    }

    #[test]
    fn with_no_display_there_is_nobody_to_ask() {
        // Over plain ssh with no display, failing fast is the whole point.
        let env = Environment {
            configured: None,
            has_display: false,
        };

        assert_eq!(for_ssh(&env, &everything_runs), Askpass::Nobody);
    }

    #[test]
    fn a_machine_with_no_helper_installed_has_nobody_to_ask() {
        let env = Environment {
            configured: None,
            has_display: true,
        };

        assert_eq!(for_ssh(&env, &nothing_runs), Askpass::Nobody);
    }

    #[test]
    fn nobody_still_names_a_program_that_cannot_be_run() {
        // Leaving the variable unset is what sends ssh back to the terminal,
        // so there has to be a program, and it has to not exist.
        assert!(!Path::new(Askpass::Nobody.program()).exists());
    }

    #[test]
    fn git_is_never_given_a_helper_zax_went_looking_for() {
        // A credential helper is the answer to git's prompts; a dialog here
        // would be answering a question that should not have been asked.
        let env = Environment {
            configured: None,
            has_display: true,
        };

        assert_eq!(for_git(&env), Askpass::Nobody);
    }

    #[test]
    fn git_still_respects_the_helper_the_user_named() {
        let env = Environment {
            configured: Some("/usr/bin/my-git-askpass".to_owned()),
            has_display: false,
        };

        assert_eq!(
            for_git(&env),
            Askpass::Configured("/usr/bin/my-git-askpass".to_owned())
        );
    }
}
