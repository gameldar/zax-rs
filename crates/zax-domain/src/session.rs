//! tmux session names, derived so that one worktree maps to one session.

use std::fmt;
use std::path::PathBuf;

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

    /// One of a worktree's sessions. A worktree can run several agents side
    /// by side, one per slot, so each Zed agent thread gets its own.
    ///
    /// The first slot is exactly [`SessionName::derive`], so sessions started
    /// before slots existed are still found. Later slots append `-<n>`. That
    /// cannot collide with another worktree's name: every derived name ends in
    /// eight hex digits with no `-` among them, and a slot suffix puts a `-`
    /// inside the last eight characters.
    pub fn derive_slot(repo: &RootRepoName, branch: &BranchName, slot: SessionSlot) -> Self {
        let first = Self::derive(repo, branch);
        if slot == SessionSlot::FIRST {
            return first;
        }
        Self(format!("{}-{}", first.0, slot.get()))
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

/// Which of a worktree's sessions a name refers to, from 1 to
/// [`SessionSlot::MAX`].
///
/// The ceiling keeps the slot suffix short enough for the naming argument in
/// [`SessionName::derive_slot`] to hold, and bounds the search for a free one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SessionSlot(u8);

impl SessionSlot {
    pub const FIRST: Self = Self(1);
    pub const MAX: u8 = 99;

    pub fn new(n: u8) -> Option<Self> {
        (1..=Self::MAX).contains(&n).then_some(Self(n))
    }

    pub fn get(self) -> u8 {
        self.0
    }

    /// Every slot, lowest first.
    pub fn all() -> impl Iterator<Item = Self> {
        (1..=Self::MAX).map(Self)
    }
}

/// A session as the multiplexer reports it right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSession {
    pub name: SessionName,
    /// At least one client is looking at it.
    pub attached: bool,
    /// The directory the session was started in. A session is matched to a
    /// worktree through this, because its name is sanitised and fingerprinted
    /// and cannot be read back into a branch.
    pub path: PathBuf,
}

/// What `zax session start` should do in a worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionChoice {
    /// An agent nobody is looking at: Zed restarted, or its terminal closed.
    Reattach(SessionName),
    /// Every session is in use, or there are none. Build this one.
    Create(SessionName),
    /// Every slot is taken and attached.
    Full,
}

/// Reattach to the lowest detached session in this worktree, otherwise create
/// the lowest free slot.
///
/// A detached session is preferred over a gap, because a detached session is a
/// live agent that someone left and wants back, and a gap is only a name.
pub fn choose_session(
    repo: &RootRepoName,
    branch: &BranchName,
    live: &[LiveSession],
) -> SessionChoice {
    let state = |name: &SessionName| live.iter().find(|s| &s.name == name).map(|s| s.attached);
    let mut free = None;
    for slot in SessionSlot::all() {
        let name = SessionName::derive_slot(repo, branch, slot);
        match state(&name) {
            Some(false) => return SessionChoice::Reattach(name),
            Some(true) => {}
            None => {
                free.get_or_insert(name);
            }
        }
    }
    free.map_or(SessionChoice::Full, SessionChoice::Create)
}

/// This worktree's sessions among `live`, lowest slot first.
pub fn worktree_sessions(
    repo: &RootRepoName,
    branch: &BranchName,
    live: &[LiveSession],
) -> Vec<SessionName> {
    SessionSlot::all()
        .map(|slot| SessionName::derive_slot(repo, branch, slot))
        .filter(|name| live.iter().any(|s| &s.name == name))
        .collect()
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

    fn repo() -> RootRepoName {
        RootRepoName::parse("acme").unwrap()
    }

    fn branch() -> BranchName {
        BranchName::parse_local("OMEGA-1-topic").unwrap()
    }

    fn slot(n: u8) -> SessionSlot {
        SessionSlot::new(n).unwrap()
    }

    fn live(name: &SessionName, attached: bool) -> LiveSession {
        LiveSession {
            name: name.clone(),
            attached,
            path: PathBuf::new(),
        }
    }

    #[test]
    fn the_first_slot_keeps_the_name_sessions_have_always_had() {
        // Sessions outlive the binary. A first slot that renamed itself would
        // leave every running agent unreachable after an upgrade.
        assert_eq!(
            SessionName::derive_slot(&repo(), &branch(), SessionSlot::FIRST),
            SessionName::derive(&repo(), &branch())
        );
    }

    #[test]
    fn later_slots_append_their_number() {
        let first = SessionName::derive(&repo(), &branch());
        let third = SessionName::derive_slot(&repo(), &branch(), slot(3));
        assert_eq!(third.as_str(), format!("{}-3", first.as_str()));
        assert_eq!(SessionName::parse(third.as_str()).unwrap(), third);
    }

    #[test]
    fn slot_zero_and_slots_past_the_limit_do_not_exist() {
        assert_eq!(SessionSlot::new(0), None);
        assert!(SessionSlot::new(SessionSlot::MAX).is_some());
        assert_eq!(SessionSlot::new(SessionSlot::MAX + 1), None);
    }

    #[test]
    fn an_empty_worktree_creates_its_first_session() {
        assert_eq!(
            choose_session(&repo(), &branch(), &[]),
            SessionChoice::Create(SessionName::derive(&repo(), &branch()))
        );
    }

    #[test]
    fn a_detached_session_is_reattached_rather_than_joined_by_another() {
        // Zed restarting, or a closed agent terminal: the agent is still
        // there and nobody is looking at it.
        let first = SessionName::derive(&repo(), &branch());
        assert_eq!(
            choose_session(&repo(), &branch(), &[live(&first, false)]),
            SessionChoice::Reattach(first)
        );
    }

    #[test]
    fn a_session_somebody_is_attached_to_gets_a_sibling() {
        // A second agent thread in the same worktree wants its own agent, not
        // a mirror of the first one.
        let first = SessionName::derive(&repo(), &branch());
        assert_eq!(
            choose_session(&repo(), &branch(), &[live(&first, true)]),
            SessionChoice::Create(SessionName::derive_slot(&repo(), &branch(), slot(2)))
        );
    }

    #[test]
    fn a_detached_later_slot_wins_over_filling_a_gap() {
        // Slot 1 was killed while slot 2 kept running. Reattaching to the
        // live agent beats building a fresh one in the hole.
        let second = SessionName::derive_slot(&repo(), &branch(), slot(2));
        let third = SessionName::derive_slot(&repo(), &branch(), slot(3));
        assert_eq!(
            choose_session(
                &repo(),
                &branch(),
                &[live(&second, false), live(&third, true)]
            ),
            SessionChoice::Reattach(second)
        );
    }

    #[test]
    fn a_new_session_fills_the_lowest_free_slot() {
        let second = SessionName::derive_slot(&repo(), &branch(), slot(2));
        assert_eq!(
            choose_session(&repo(), &branch(), &[live(&second, true)]),
            SessionChoice::Create(SessionName::derive(&repo(), &branch()))
        );
    }

    #[test]
    fn another_worktrees_sessions_are_not_ours_to_reattach() {
        let other = SessionName::derive(&repo(), &BranchName::parse_local("other").unwrap());
        assert_eq!(
            choose_session(&repo(), &branch(), &[live(&other, false)]),
            SessionChoice::Create(SessionName::derive(&repo(), &branch()))
        );
    }

    #[test]
    fn every_slot_taken_and_attached_is_full() {
        let all: Vec<_> = SessionSlot::all()
            .map(|s| live(&SessionName::derive_slot(&repo(), &branch(), s), true))
            .collect();
        assert_eq!(
            choose_session(&repo(), &branch(), &all),
            SessionChoice::Full
        );
    }

    #[test]
    fn a_worktrees_sessions_are_listed_in_slot_order() {
        let first = SessionName::derive(&repo(), &branch());
        let second = SessionName::derive_slot(&repo(), &branch(), slot(2));
        let other = SessionName::derive(&repo(), &BranchName::parse_local("other").unwrap());
        assert_eq!(
            worktree_sessions(
                &repo(),
                &branch(),
                &[
                    live(&second, true),
                    live(&other, false),
                    live(&first, false)
                ]
            ),
            vec![first, second]
        );
    }

    #[test]
    fn parse_rejects_the_bare_prefix() {
        assert!(matches!(
            SessionName::parse("zax-"),
            Err(SessionNameError::NotAZaxSession(_))
        ));
    }
}
