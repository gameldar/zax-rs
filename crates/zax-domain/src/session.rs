//! tmux session names, derived so that one worktree maps to one session.

use std::fmt;

use thiserror::Error;

use crate::branch::BranchName;
use crate::repo::RootRepoName;

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionName(String);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SessionNameError {
    #[error("`{0}` is not a zax session; zax sessions are named zax-<repo>-<branch>")]
    NotAZaxSession(String),
    #[error("session name `{name}` contains `{ch}`, which tmux reads as a target separator")]
    IllegalCharacter { name: String, ch: char },
}

impl SessionName {
    pub const PREFIX: &'static str = "zax-";

    /// One worktree, one session.
    pub fn derive(repo: &RootRepoName, branch: &BranchName) -> Self {
        let mut name = String::with_capacity(
            Self::PREFIX.len() + repo.as_str().len() + branch.as_str().len() + 1,
        );
        name.push_str(Self::PREFIX);
        name.push_str(&sanitize(repo.as_str()));
        name.push('-');
        name.push_str(&sanitize(branch.as_str()));
        Self(name)
    }

    pub fn parse(raw: &str) -> Result<Self, SessionNameError> {
        let rest = raw
            .strip_prefix(Self::PREFIX)
            .filter(|rest| !rest.is_empty())
            .ok_or_else(|| SessionNameError::NotAZaxSession(raw.to_owned()))?;
        let _ = rest;
        if let Some(ch) = raw.chars().find(|c| !is_allowed(*c)) {
            return Err(SessionNameError::IllegalCharacter {
                name: raw.to_owned(),
                ch,
            });
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for SessionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_allowed(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

/// Map everything tmux could misread onto `-`. Runs are deliberately not
/// collapsed: collapsing could map two different branches onto one session.
fn sanitize(raw: &str) -> String {
    raw.chars()
        .map(|c| if is_allowed(c) { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::branch::BranchName;
    use crate::repo::RootRepoName;

    #[test]
    fn a_session_name_is_derived_from_repo_and_branch() {
        let repo = RootRepoName::parse("zed-ai-extensions").unwrap();
        let branch = BranchName::parse_local("OMEGA-1963-stamp").unwrap();
        let name = SessionName::derive(&repo, &branch);
        assert_eq!(name.as_str(), "zax-zed-ai-extensions-OMEGA-1963-stamp");
    }

    #[test]
    fn characters_tmux_treats_as_targets_are_replaced() {
        // tmux reads `.` and `:` in a target as pane and window separators, and
        // `/` would be read as a path in some contexts.
        let repo = RootRepoName::parse("acme").unwrap();
        let branch = BranchName::parse_foreign("dependabot/cargo/serde-1.2.3").unwrap();
        let name = SessionName::derive(&repo, &branch);
        assert_eq!(name.as_str(), "zax-acme-dependabot-cargo-serde-1-2-3");
    }

    #[test]
    fn derived_names_are_always_parseable() {
        let repo = RootRepoName::parse("acme").unwrap();
        let branch = BranchName::parse_foreign("weird/branch.name").unwrap();
        let derived = SessionName::derive(&repo, &branch);
        assert_eq!(SessionName::parse(derived.as_str()).unwrap(), derived);
    }

    #[test]
    fn parse_requires_the_zax_prefix_so_kill_cannot_touch_a_personal_session() {
        assert!(matches!(
            SessionName::parse("my-work"),
            Err(SessionNameError::NotAZaxSession(_))
        ));
        assert!(SessionName::parse("zax-acme-topic").is_ok());
    }

    #[test]
    fn parse_rejects_target_separators() {
        assert!(matches!(
            SessionName::parse("zax-acme:0"),
            Err(SessionNameError::IllegalCharacter { .. })
        ));
        assert!(matches!(
            SessionName::parse("zax-acme.0"),
            Err(SessionNameError::IllegalCharacter { .. })
        ));
    }

    #[test]
    fn parse_rejects_the_bare_prefix() {
        assert!(matches!(
            SessionName::parse("zax-"),
            Err(SessionNameError::NotAZaxSession(_))
        ));
    }
}
