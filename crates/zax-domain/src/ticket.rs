//! Issue keys as the tracker writes them: an uppercase project key, a hyphen,
//! and a number.

use std::fmt;

use thiserror::Error;

use crate::branch::{BranchName, BranchNameError};

/// A ticket key such as `OMEGA-1963`.
///
/// There is no construction path other than [`TicketKey::parse`], so a value of
/// this type is always exactly `[A-Z]+-[0-9]+`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TicketKey(String);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TicketKeyError {
    #[error("ticket key is empty")]
    Empty,
    #[error(
        "`{0}` is not a ticket key: expected an uppercase project key, a hyphen \
         and a number, like OMEGA-1963"
    )]
    Malformed(String),
}

impl TicketKey {
    pub fn parse(raw: &str) -> Result<Self, TicketKeyError> {
        if raw.is_empty() {
            return Err(TicketKeyError::Empty);
        }
        let malformed = || TicketKeyError::Malformed(raw.to_owned());
        let (project, number) = raw.split_once('-').ok_or_else(malformed)?;

        let project_ok = !project.is_empty() && project.chars().all(|c| c.is_ascii_uppercase());
        let number_ok = !number.is_empty() && number.chars().all(|c| c.is_ascii_digit());

        if project_ok && number_ok {
            Ok(Self(raw.to_owned()))
        } else {
            Err(malformed())
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TicketKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Where a ticket is in its tracker's workflow, in terms every tracker shares.
/// Drives sort order and colour; the tracker's own wording is in
/// [`TicketStatus::name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StatusCategory {
    ToDo,
    InProgress,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketStatus {
    pub category: StatusCategory,
    /// What the tracker calls it, such as "In Review".
    pub name: String,
}

/// A ticket assigned to you, as a provider reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ticket {
    pub key: TicketKey,
    pub summary: String,
    pub status: TicketStatus,
    /// Free text: trackers disagree on priority scales.
    pub priority: Option<String>,
    /// Story, Bug and so on.
    pub kind: Option<String>,
    /// Where a browser shows the ticket.
    pub url: String,
}

/// A move a provider offers for a ticket. `id` is opaque to zax.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    pub id: String,
    pub name: String,
}

/// The branch `w` creates for a ticket: its key and the first four words of
/// its summary, lowercased, every run of other characters one `-`.
///
/// A summary with no ASCII word in it (empty, emoji, CJK) gives `<KEY>-work`.
/// Both shapes always satisfy `parse_local`; the `Result` is the parser's,
/// passed through rather than unwrapped.
pub fn branch_slug(key: &TicketKey, summary: &str) -> Result<BranchName, BranchNameError> {
    let lower = summary.to_ascii_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .take(4)
        .collect();
    if words.is_empty() {
        return BranchName::parse_local(&format!("{key}-work"));
    }
    BranchName::parse_local(&format!("{key}-{}", words.join("-")))
        .or_else(|_| BranchName::parse_local(&format!("{key}-work")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn accepts_an_uppercase_key_and_number() {
        let key = TicketKey::parse("OMEGA-1963").unwrap();
        assert_eq!(key.as_str(), "OMEGA-1963");
        assert_eq!(key.to_string(), "OMEGA-1963");
    }

    #[test]
    fn rejects_an_empty_string() {
        assert_eq!(TicketKey::parse(""), Err(TicketKeyError::Empty));
    }

    #[test]
    fn rejects_a_lowercase_project_key() {
        assert_eq!(
            TicketKey::parse("omega-1963"),
            Err(TicketKeyError::Malformed("omega-1963".to_owned()))
        );
    }

    #[test]
    fn rejects_a_missing_hyphen() {
        assert!(TicketKey::parse("NORTH1963").is_err());
    }

    #[test]
    fn rejects_a_missing_number() {
        assert!(TicketKey::parse("OMEGA-").is_err());
    }

    #[test]
    fn rejects_a_missing_project_key() {
        assert!(TicketKey::parse("-1963").is_err());
    }

    #[test]
    fn rejects_a_non_numeric_suffix() {
        assert!(TicketKey::parse("OMEGA-19a").is_err());
    }

    #[test]
    fn rejects_trailing_text_after_the_number() {
        assert!(TicketKey::parse("OMEGA-1963-stamp").is_err());
    }

    #[test]
    fn the_error_message_shows_the_expected_shape() {
        let err = TicketKey::parse("nope").unwrap_err();
        assert!(err.to_string().contains("OMEGA-1963"), "got: {err}");
    }

    fn key(raw: &str) -> TicketKey {
        TicketKey::parse(raw).unwrap()
    }

    #[test]
    fn a_slug_is_the_key_and_the_first_four_words_of_the_summary() {
        let slug =
            branch_slug(&key("OMEGA-2713"), "US123 Make export history available").unwrap();
        assert_eq!(slug.as_str(), "OMEGA-2713-us123-make-export-history");
    }

    #[test]
    fn punctuation_runs_collapse_to_one_hyphen() {
        let slug = branch_slug(&key("OMEGA-1"), "Fix: the -- broken/thing!!").unwrap();
        assert_eq!(slug.as_str(), "OMEGA-1-fix-the-broken-thing");
    }

    #[test]
    fn a_summary_with_no_ascii_words_falls_back_to_work() {
        assert_eq!(
            branch_slug(&key("OMEGA-1"), "").unwrap().as_str(),
            "OMEGA-1-work"
        );
        assert_eq!(
            branch_slug(&key("OMEGA-1"), "🚀 日本語").unwrap().as_str(),
            "OMEGA-1-work"
        );
    }

    #[test]
    fn a_slug_is_a_valid_local_branch_that_names_its_ticket() {
        let slug = branch_slug(&key("OMEGA-9"), "Retry 3 times").unwrap();
        assert_eq!(slug.ticket(), Some(key("OMEGA-9")));
    }
}
