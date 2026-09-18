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
    ///
    /// The readable part of the name cannot be relied on to be unique.
    /// Sanitising maps several characters onto `-`, so `bar.baz` and `bar-baz`
    /// produce the same text, and the `-` joining repo to branch is
    /// indistinguishable from a `-` inside either of them: repo `foo-bar` with
    /// branch `baz` reads exactly like repo `foo` with branch `bar-baz`. Two
    /// worktrees sharing a session would mean `zax session kill` tearing down
    /// the wrong agent, so a fingerprint of the raw, unsanitised pair is
    /// appended to make the name injective in practice.
    pub fn derive(repo: &RootRepoName, branch: &BranchName) -> Self {
        let name = format!(
            "{}{}-{}-{:08x}",
            Self::PREFIX,
            sanitize(repo.as_str()),
            sanitize(branch.as_str()),
            fingerprint(repo.as_str(), branch.as_str()),
        );
        Self(name)
    }

    pub fn parse(raw: &str) -> Result<Self, SessionNameError> {
        let rest = raw
            .strip_prefix(Self::PREFIX)
            .ok_or_else(|| SessionNameError::NotAZaxSession(raw.to_owned()))?;
        if rest.is_empty() {
            return Err(SessionNameError::NotAZaxSession(raw.to_owned()));
        }
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

/// FNV-1a over `repo`, a NUL, then `branch`, folded to 32 bits.
///
/// Written out rather than taken from `DefaultHasher`, whose output is
/// explicitly not guaranteed stable between Rust releases. This value ends up
/// in a session name that must still match after zax is rebuilt, or
/// `session start` would miss a running agent and build a second session
/// beside it.
fn fingerprint(repo: &str, branch: &str) -> u32 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    // The NUL separator is what stops `("ab", "c")` and `("a", "bc")` hashing
    // alike, and neither input can contain one.
    for byte in repo.bytes().chain(std::iter::once(0)).chain(branch.bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    ((hash >> 32) ^ hash) as u32
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
        assert_eq!(
            name.as_str(),
            "zax-zed-ai-extensions-OMEGA-1963-stamp-218dfc7b"
        );
    }

    #[test]
    fn characters_tmux_treats_as_targets_are_replaced() {
        // tmux reads `.` and `:` in a target as pane and window separators, and
        // `/` would be read as a path in some contexts.
        let repo = RootRepoName::parse("acme").unwrap();
        let branch = BranchName::parse_foreign("dependabot/cargo/serde-1.2.3").unwrap();
        let name = SessionName::derive(&repo, &branch);
        assert_eq!(
            name.as_str(),
            "zax-acme-dependabot-cargo-serde-1-2-3-2c36a554"
        );
    }

    #[test]
    fn derived_names_are_always_parseable() {
        let repo = RootRepoName::parse("acme").unwrap();
        let branch = BranchName::parse_foreign("weird/branch.name").unwrap();
        let derived = SessionName::derive(&repo, &branch);
        assert_eq!(SessionName::parse(derived.as_str()).unwrap(), derived);
    }

    #[test]
    fn two_different_repo_and_branch_pairs_never_share_a_session_name() {
        // The join `-` is indistinguishable from a `-` inside either side, so
        // these two read identically before the fingerprint is appended.
        let a = SessionName::derive(
            &RootRepoName::parse("foo-bar").unwrap(),
            &BranchName::parse_local("baz").unwrap(),
        );
        let b = SessionName::derive(
            &RootRepoName::parse("foo").unwrap(),
            &BranchName::parse_local("bar-baz").unwrap(),
        );
        assert_ne!(a, b, "one worktree, one session");
    }

    #[test]
    fn characters_that_sanitise_alike_still_get_distinct_session_names() {
        // Sanitising is lossy: `bar.baz` and `bar-baz` both render as
        // `bar-baz`, so only the fingerprint tells them apart.
        let repo = RootRepoName::parse("repo").unwrap();
        let dotted = SessionName::derive(&repo, &BranchName::parse_foreign("bar.baz").unwrap());
        let hyphenated = SessionName::derive(&repo, &BranchName::parse_local("bar-baz").unwrap());
        assert_ne!(dotted, hyphenated);
    }

    #[test]
    fn the_fingerprint_is_stable_across_builds() {
        // This value must not drift. A session outlives the binary that made
        // it, and a changed fingerprint would leave `session start` unable to
        // find a running agent, silently building a second session beside it.
        let name = SessionName::derive(
            &RootRepoName::parse("acme").unwrap(),
            &BranchName::parse_foreign("weird/branch.name").unwrap(),
        );
        assert!(
            name.as_str().ends_with("-684a3578"),
            "fingerprint drifted: {name}"
        );
    }

    #[test]
    fn deriving_the_same_pair_twice_gives_the_same_name() {
        let repo = RootRepoName::parse("acme").unwrap();
        let branch = BranchName::parse_local("OMEGA-1-topic").unwrap();
        assert_eq!(
            SessionName::derive(&repo, &branch),
            SessionName::derive(&repo, &branch)
        );
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
