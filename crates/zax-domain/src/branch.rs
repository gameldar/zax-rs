//! Branch names, and the project's naming rule expressed as a constructor.
//!
//! Two constructors, one type. [`BranchName::parse_local`] enforces the
//! project's rule for branches we create. [`BranchName::parse_foreign`] enforces
//! only git's own ref syntax, because a pull request's head branch is not ours
//! to name. A value remembers which constructor produced it.

use std::fmt;

use thiserror::Error;

use crate::ticket::TicketKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchOrigin {
    /// Created by us, and therefore held to the project's naming rule.
    Local,
    /// Came from a pull request. Only git's ref syntax applies.
    Foreign,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchName {
    raw: String,
    origin: BranchOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BranchNameError {
    #[error("branch name is empty")]
    Empty,
    #[error("`{0}` is not a valid git ref name")]
    NotARef(String),
    #[error(
        "`{0}` contains a `/`: name new branches `<TICKET>-<short-desc>` with no \
         category prefix and no slash"
    )]
    SlashNotAllowed(String),
    #[error("`{found}` starts with a lowercase ticket key; write it as `{expected}`")]
    LowercaseTicketKey { found: String, expected: String },
    #[error(
        "`{0}`: after any ticket key a branch needs a short description of \
         lowercase hyphen-separated words, such as OMEGA-1963-export-csv-reports"
    )]
    BadShortDescription(String),
}

impl BranchName {
    /// Parse a branch we are about to create. Enforces the project's rule.
    pub fn parse_local(raw: &str) -> Result<Self, BranchNameError> {
        if raw.is_empty() {
            return Err(BranchNameError::Empty);
        }
        validate_ref_syntax(raw)?;
        if raw.contains('/') {
            return Err(BranchNameError::SlashNotAllowed(raw.to_owned()));
        }

        match split_ticket_prefix(raw) {
            Some((project, number, rest)) => {
                if !project.chars().all(|c| c.is_ascii_uppercase()) {
                    let expected = format!("{}-{number}{rest}", project.to_ascii_uppercase());
                    return Err(BranchNameError::LowercaseTicketKey {
                        found: raw.to_owned(),
                        expected,
                    });
                }
                // `rest` is either empty or starts with the hyphen after the key.
                let desc = rest.strip_prefix('-').unwrap_or(rest);
                if desc.is_empty() || !is_short_description(desc) {
                    return Err(BranchNameError::BadShortDescription(raw.to_owned()));
                }
            }
            None => {
                if !is_short_description(raw) {
                    return Err(BranchNameError::BadShortDescription(raw.to_owned()));
                }
            }
        }

        Ok(Self {
            raw: raw.to_owned(),
            origin: BranchOrigin::Local,
        })
    }

    /// Parse a branch that came from a pull request. Git ref syntax only.
    pub fn parse_foreign(raw: &str) -> Result<Self, BranchNameError> {
        if raw.is_empty() {
            return Err(BranchNameError::Empty);
        }
        validate_ref_syntax(raw)?;
        Ok(Self {
            raw: raw.to_owned(),
            origin: BranchOrigin::Foreign,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    pub fn origin(&self) -> BranchOrigin {
        self.origin
    }

    /// The ticket key this branch names, if it leads with one.
    pub fn ticket(&self) -> Option<TicketKey> {
        let (project, number, _) = split_ticket_prefix(&self.raw)?;
        TicketKey::parse(&format!("{project}-{number}")).ok()
    }

    /// Path components. A local branch always yields exactly one; a foreign
    /// branch yields one per `/`, which is how a slashed branch becomes real
    /// directories under the worktree root.
    pub fn components(&self) -> std::str::Split<'_, char> {
        self.raw.split('/')
    }
}

impl fmt::Display for BranchName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

/// Split a leading `<alpha>-<digits>` run, returning `(project, number, rest)`
/// where `rest` is either empty or begins with the following `-`.
///
/// Any `<alpha>-<digits>` prefix is treated as a ticket key, including a
/// lowercase one, so that `parse_local` can tell the caller to uppercase it
/// rather than silently accepting a broken key. The cost is that a topic branch
/// like `retry-3-times` is read as a malformed ticket key; reword it.
fn split_ticket_prefix(raw: &str) -> Option<(&str, &str, &str)> {
    let alpha_len = raw.find(|c: char| !c.is_ascii_alphabetic())?;
    if alpha_len == 0 {
        return None;
    }
    let (project, after_project) = raw.split_at(alpha_len);
    let after_hyphen = after_project.strip_prefix('-')?;

    let digit_len = after_hyphen
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(after_hyphen.len());
    if digit_len == 0 {
        return None;
    }
    let (number, rest) = after_hyphen.split_at(digit_len);
    // A digit run that runs straight into more alphanumerics is not a key.
    if !rest.is_empty() && !rest.starts_with('-') {
        return None;
    }
    Some((project, number, rest))
}

/// Lowercase hyphen-separated words: `[a-z0-9]+(-[a-z0-9]+)*`.
fn is_short_description(s: &str) -> bool {
    !s.is_empty()
        && s.split('-').all(|word| {
            !word.is_empty()
                && word
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        })
}

/// The subset of `git check-ref-format --branch` that matters here.
fn validate_ref_syntax(raw: &str) -> Result<(), BranchNameError> {
    let bad = || BranchNameError::NotARef(raw.to_owned());

    if raw == "@" || raw.contains("..") || raw.contains("@{") {
        return Err(bad());
    }
    if raw.starts_with('/') || raw.ends_with('/') || raw.contains("//") {
        return Err(bad());
    }
    if raw.ends_with('.') || raw.ends_with(".lock") {
        return Err(bad());
    }
    for c in raw.chars() {
        if c.is_ascii_control() || c == '\u{7f}' {
            return Err(bad());
        }
        if matches!(c, ' ' | '~' | '^' | ':' | '?' | '*' | '[' | '\\') {
            return Err(bad());
        }
    }
    for component in raw.split('/') {
        if component.is_empty() || component.starts_with('.') || component.ends_with(".lock") {
            return Err(bad());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn accepts_a_ticketed_branch() {
        let b = BranchName::parse_local("OMEGA-1963-export-csv-reports").unwrap();
        assert_eq!(b.as_str(), "OMEGA-1963-export-csv-reports");
        assert_eq!(b.origin(), BranchOrigin::Local);
        assert_eq!(b.ticket().unwrap().as_str(), "OMEGA-1963");
    }

    #[test]
    fn accepts_a_bare_topic_with_no_ticket() {
        let b = BranchName::parse_local("branded-primitive-helper").unwrap();
        assert_eq!(b.ticket(), None);
    }

    #[test]
    fn accepts_digits_inside_the_short_description() {
        assert!(BranchName::parse_local("OMEGA-1963-migrate-v2-schema").is_ok());
    }

    #[test]
    fn rejects_a_category_prefix() {
        assert_eq!(
            BranchName::parse_local("feat/OMEGA-1963-stamp"),
            Err(BranchNameError::SlashNotAllowed(
                "feat/OMEGA-1963-stamp".to_owned()
            ))
        );
    }

    #[test]
    fn rejects_a_lowercase_ticket_key_and_says_what_to_write() {
        let err = BranchName::parse_local("omega-1963-stamp").unwrap_err();
        assert_eq!(
            err,
            BranchNameError::LowercaseTicketKey {
                found: "omega-1963-stamp".to_owned(),
                expected: "OMEGA-1963-stamp".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_an_uppercase_short_description() {
        assert!(matches!(
            BranchName::parse_local("OMEGA-1963-Stamp"),
            Err(BranchNameError::BadShortDescription(_))
        ));
    }

    #[test]
    fn rejects_a_bare_ticket_key_with_no_description() {
        assert!(matches!(
            BranchName::parse_local("OMEGA-1963"),
            Err(BranchNameError::BadShortDescription(_))
        ));
    }

    #[test]
    fn rejects_an_empty_branch() {
        assert_eq!(BranchName::parse_local(""), Err(BranchNameError::Empty));
        assert_eq!(BranchName::parse_foreign(""), Err(BranchNameError::Empty));
    }

    #[test]
    fn rejects_whitespace() {
        assert!(matches!(
            BranchName::parse_local("has space"),
            Err(BranchNameError::NotARef(_))
        ));
    }

    #[test]
    fn rejects_double_underscores_style_separators() {
        assert!(matches!(
            BranchName::parse_local("north_1963_stamp"),
            Err(BranchNameError::BadShortDescription(_))
        ));
    }

    #[test]
    fn foreign_branches_may_carry_slashes() {
        let b = BranchName::parse_foreign("dependabot/cargo/serde-1.2.3").unwrap();
        assert_eq!(b.origin(), BranchOrigin::Foreign);
        assert_eq!(
            b.components().collect::<Vec<_>>(),
            vec!["dependabot", "cargo", "serde-1.2.3"]
        );
    }

    #[test]
    fn local_branches_are_a_single_component() {
        let b = BranchName::parse_local("OMEGA-1963-stamp").unwrap();
        assert_eq!(b.components().count(), 1);
    }

    #[test]
    fn foreign_branches_still_have_to_be_git_refs() {
        for bad in [
            "bad..ref",
            "trailing/",
            "/leading",
            "double//slash",
            "tilde~1",
            "caret^1",
            "colon:name",
            "question?",
            "star*",
            "bracket[1]",
            "back\\slash",
            "at@{brace}",
            "@",
            "ends.lock",
            "trailing.",
            ".hidden/component",
        ] {
            assert!(
                matches!(
                    BranchName::parse_foreign(bad),
                    Err(BranchNameError::NotARef(_))
                ),
                "expected `{bad}` to be rejected"
            );
        }
    }

    #[test]
    fn foreign_ticket_extraction_still_works() {
        let b = BranchName::parse_foreign("OMEGA-1963-from-a-fork").unwrap();
        assert_eq!(b.ticket().unwrap().as_str(), "OMEGA-1963");
    }
}
