//! Which pull requests the review-requests pane shows.

use std::fmt;
use std::time::{Duration, SystemTime};

use thiserror::Error;

use crate::forge::{PullRequest, RepoSlug};

/// One entry of the pane's `include` or `exclude` list: a repository, or every
/// repository of one owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPattern {
    owner: String,
    name: Name,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Name {
    Exact(String),
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RepoPatternError {
    #[error("`{0}` is not a repository filter; write it as owner/name or owner/*")]
    Malformed(String),
    #[error("repository filter `{0}` contains a control character")]
    ControlCharacter(String),
}

impl RepoPattern {
    /// `owner/name` or `owner/*`. A `*` anywhere else is refused rather than
    /// taken literally: `acme/nor*` reads as a prefix match, and accepting it
    /// as the name of a repository would silently match nothing forever.
    pub fn parse(raw: &str) -> Result<Self, RepoPatternError> {
        let malformed = || RepoPatternError::Malformed(raw.to_owned());
        if raw.chars().any(char::is_control) {
            return Err(RepoPatternError::ControlCharacter(raw.to_owned()));
        }
        let (owner, name) = raw.split_once('/').ok_or_else(malformed)?;
        if owner.is_empty() || name.is_empty() || name.contains('/') {
            return Err(malformed());
        }
        if owner.contains('*') {
            return Err(malformed());
        }
        let name = match name {
            "*" => Name::Any,
            other if other.contains('*') => return Err(malformed()),
            other => Name::Exact(other.to_owned()),
        };
        Ok(Self {
            owner: owner.to_owned(),
            name,
        })
    }

    /// The whole repository, when the pattern names one. A forge that can
    /// narrow its own search needs a name to send; a wildcard has none.
    pub fn exact_slug(&self) -> Option<String> {
        match &self.name {
            Name::Any => None,
            Name::Exact(name) => Some(format!("{}/{name}", self.owner)),
        }
    }

    pub fn matches(&self, repo: &RepoSlug) -> bool {
        if repo.owner() != self.owner {
            return false;
        }
        match &self.name {
            Name::Any => true,
            Name::Exact(name) => repo.name() == name,
        }
    }
}

/// The pane's `[prs]` configuration, already parsed.
///
/// An empty filter is the unconfigured pane, and admits everything. Note that
/// `include` being empty means "every repository", not "no repository": the
/// lists narrow a queue that is otherwise whole.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PrFilter {
    pub include: Vec<RepoPattern>,
    pub exclude: Vec<RepoPattern>,
    pub max_age: Option<Duration>,
}

impl PrFilter {
    pub fn admits(&self, pr: &PullRequest, now: SystemTime) -> bool {
        if self.exclude.iter().any(|p| p.matches(&pr.repo)) {
            return false;
        }
        if !self.include.is_empty() && !self.include.iter().any(|p| p.matches(&pr.repo)) {
            return false;
        }
        // `age` clamps a future timestamp to zero, so a skewed clock cannot
        // empty the pane.
        match self.max_age {
            Some(max) => pr.age(now) <= max,
            None => true,
        }
    }

    pub fn apply(&self, prs: Vec<PullRequest>, now: SystemTime) -> Vec<PullRequest> {
        prs.into_iter().filter(|pr| self.admits(pr, now)).collect()
    }
}

impl fmt::Display for RepoPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.name {
            Name::Any => write!(f, "{}/*", self.owner),
            Name::Exact(name) => write!(f, "{}/{name}", self.owner),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;
    use crate::forge::RepoSlug;

    fn slug(raw: &str) -> RepoSlug {
        RepoSlug::parse(raw).unwrap()
    }

    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use crate::forge::{CheckState, PullRequest};

    const NOW_SECS: u64 = 1_700_000_000;

    fn now() -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(NOW_SECS)
    }

    fn pr_from(repo: &str, age: Duration) -> PullRequest {
        PullRequest {
            repo: slug(repo),
            number: 1,
            title: "a title".to_owned(),
            url: format!("https://github.com/{repo}/pull/1"),
            is_draft: false,
            created_at: now() - age,
            checks: CheckState::Passing,
        }
    }

    fn patterns(raw: &[&str]) -> Vec<RepoPattern> {
        raw.iter().map(|p| RepoPattern::parse(p).unwrap()).collect()
    }

    fn days(n: u64) -> Duration {
        Duration::from_secs(n * 86_400)
    }

    #[test]
    fn an_unfiltered_pane_admits_everything() {
        let filter = PrFilter::default();
        assert!(filter.admits(&pr_from("acme/omega", days(400)), now()));
    }

    #[test]
    fn an_include_list_admits_only_the_repositories_on_it() {
        let filter = PrFilter {
            include: patterns(&["acme/*", "gameldar/zax"]),
            ..PrFilter::default()
        };
        assert!(filter.admits(&pr_from("acme/omega", days(1)), now()));
        assert!(filter.admits(&pr_from("gameldar/zax", days(1)), now()));
        assert!(!filter.admits(&pr_from("gameldar/other", days(1)), now()));
    }

    #[test]
    fn an_exclude_list_drops_the_repositories_on_it() {
        let filter = PrFilter {
            exclude: patterns(&["acme/legacy-api"]),
            ..PrFilter::default()
        };
        assert!(!filter.admits(&pr_from("acme/legacy-api", days(1)), now()));
        assert!(filter.admits(&pr_from("acme/omega", days(1)), now()));
    }

    #[test]
    fn an_exclusion_beats_an_inclusion() {
        // `include = ["acme/*"]` with `exclude = ["acme/legacy-api"]` is
        // the whole point of having both lists.
        let filter = PrFilter {
            include: patterns(&["acme/*"]),
            exclude: patterns(&["acme/legacy-api"]),
            max_age: None,
        };
        assert!(!filter.admits(&pr_from("acme/legacy-api", days(1)), now()));
        assert!(filter.admits(&pr_from("acme/omega", days(1)), now()));
    }

    #[test]
    fn max_age_drops_a_pull_request_older_than_the_threshold() {
        let filter = PrFilter {
            max_age: Some(days(14)),
            ..PrFilter::default()
        };
        assert!(filter.admits(&pr_from("acme/omega", days(13)), now()));
        assert!(!filter.admits(&pr_from("acme/omega", days(15)), now()));
    }

    #[test]
    fn a_pull_request_exactly_at_the_threshold_is_still_admitted() {
        let filter = PrFilter {
            max_age: Some(days(14)),
            ..PrFilter::default()
        };
        assert!(filter.admits(&pr_from("acme/omega", days(14)), now()));
    }

    #[test]
    fn a_pull_request_created_in_the_future_is_not_aged_out() {
        // A skewed clock must not empty the pane.
        let filter = PrFilter {
            max_age: Some(days(1)),
            ..PrFilter::default()
        };
        let mut pr = pr_from("acme/omega", Duration::ZERO);
        pr.created_at = now() + days(2);
        assert!(filter.admits(&pr, now()));
    }

    #[test]
    fn filtering_keeps_the_order_the_forge_returned() {
        let filter = PrFilter {
            exclude: patterns(&["acme/legacy-api"]),
            ..PrFilter::default()
        };
        let prs = vec![
            pr_from("acme/omega", days(1)),
            pr_from("acme/legacy-api", days(1)),
            pr_from("gameldar/zax", days(1)),
        ];

        let kept: Vec<String> = filter
            .apply(prs, now())
            .into_iter()
            .map(|pr| pr.repo.to_string())
            .collect();

        assert_eq!(kept, ["acme/omega", "gameldar/zax"]);
    }

    #[test]
    fn an_exact_pattern_matches_only_that_repository() {
        let pattern = RepoPattern::parse("acme/omega").unwrap();
        assert!(pattern.matches(&slug("acme/omega")));
        assert!(!pattern.matches(&slug("acme/omega-legacy")));
        assert!(!pattern.matches(&slug("other/omega")));
    }

    #[test]
    fn an_owner_wildcard_matches_every_repository_of_that_owner() {
        let pattern = RepoPattern::parse("acme/*").unwrap();
        assert!(pattern.matches(&slug("acme/omega")));
        assert!(pattern.matches(&slug("acme/south")));
        assert!(!pattern.matches(&slug("acmeco/omega")));
    }

    #[test]
    fn a_star_is_a_wildcard_only_as_the_whole_name() {
        // `acme/nor*` reads as a prefix match and is not one; refusing it is
        // better than silently looking for a repository called `nor*`.
        assert!(RepoPattern::parse("acme/nor*").is_err());
        assert!(RepoPattern::parse("*/omega").is_err());
    }

    #[test]
    fn a_pattern_needs_both_an_owner_and_a_name() {
        for bad in ["", "omega", "acme/", "/omega", "acme/omega/extra"] {
            assert!(RepoPattern::parse(bad).is_err(), "`{bad}` is not a pattern");
        }
    }

    #[test]
    fn a_pattern_refuses_a_control_character() {
        assert!(RepoPattern::parse("acme/no\0rth").is_err());
    }

    #[test]
    fn only_an_exact_pattern_names_a_whole_repository() {
        assert_eq!(
            RepoPattern::parse("acme/omega").unwrap().exact_slug(),
            Some("acme/omega".to_owned())
        );
        assert_eq!(RepoPattern::parse("acme/*").unwrap().exact_slug(), None);
    }

    #[test]
    fn a_pattern_displays_as_it_was_written() {
        assert_eq!(
            RepoPattern::parse("acme/*").unwrap().to_string(),
            "acme/*"
        );
    }
}
