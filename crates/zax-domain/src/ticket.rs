//! Issue keys as the tracker writes them: an uppercase project key, a hyphen,
//! and a number.

use std::fmt;

use thiserror::Error;

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

        let project_ok =
            !project.is_empty() && project.chars().all(|c| c.is_ascii_uppercase());
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
}
