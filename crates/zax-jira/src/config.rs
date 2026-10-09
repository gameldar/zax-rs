//! `~/.config/zax/providers/jira.toml`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const DEFAULT_JQL: &str =
    "assignee = currentUser() AND (statusCategory != Done OR updated >= -14d) ORDER BY updated DESC";
const DEFAULT_TRANSITIONS: [&str; 3] = ["In Progress", "In Review", "Done"];

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    jql: Option<String>,
    transitions: Option<Vec<String>>,
    #[serde(default)]
    projects: BTreeMap<String, RawProject>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawProject {
    transitions: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JiraConfig {
    pub jql: String,
    transitions: Vec<String>,
    projects: BTreeMap<String, Vec<String>>,
}

impl JiraConfig {
    pub fn from_toml(text: &str) -> Result<Self, String> {
        let raw: Raw = toml::from_str(text).map_err(|e| one_line(text, &e))?;
        Ok(Self {
            jql: raw.jql.unwrap_or_else(|| DEFAULT_JQL.to_owned()),
            transitions: raw.transitions.unwrap_or_else(|| {
                DEFAULT_TRANSITIONS
                    .iter()
                    .map(|s| (*s).to_owned())
                    .collect()
            }),
            projects: raw
                .projects
                .into_iter()
                .filter_map(|(k, p)| p.transitions.map(|t| (k, t)))
                .collect(),
        })
    }

    /// An absent file is the defaults.
    pub fn load(path: &Path) -> Result<Self, String> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml(&text).map_err(|e| format!("{}: {e}", path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::from_toml(""),
            Err(e) => Err(format!("{}: {e}", path.display())),
        }
    }

    pub fn default_path() -> Option<PathBuf> {
        let base = match std::env::var_os("XDG_CONFIG_HOME") {
            Some(v) if !v.is_empty() => PathBuf::from(v),
            _ => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
        };
        Some(base.join("zax").join("providers").join("jira.toml"))
    }

    /// The project's own list when it has one, else the global one.
    pub fn transitions_for(&self, key: &str) -> &[String] {
        let project = key.split_once('-').map_or(key, |(p, _)| p);
        self.projects.get(project).unwrap_or(&self.transitions)
    }
}

/// toml's own rendering spans several lines with a source excerpt, which
/// an error line in a pane cannot show; the line number and the message
/// say the same in one line.
fn one_line(text: &str, e: &toml::de::Error) -> String {
    let message = e.message().trim().replace('\n', " ");
    match e.span() {
        Some(span) => {
            let line = text
                .get(..span.start)
                .map_or(1, |before| before.matches('\n').count() + 1);
            format!("line {line}: {message}")
        }
        None => message,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn an_absent_file_is_the_defaults() {
        let c = JiraConfig::from_toml("").unwrap();
        assert!(c.jql.contains("assignee = currentUser()"));
        assert_eq!(
            c.transitions_for("ACME-1"),
            ["In Progress", "In Review", "Done"]
        );
    }

    #[test]
    fn a_project_overrides_the_transitions() {
        let c = JiraConfig::from_toml(
            "transitions = [\"A\"]\n[projects.OMEGA]\ntransitions = [\"B\", \"C\"]\n",
        )
        .unwrap();
        assert_eq!(c.transitions_for("OMEGA-1"), ["B", "C"]);
        assert_eq!(c.transitions_for("ACME-1"), ["A"]);
    }

    #[test]
    fn a_parse_error_is_one_line_naming_the_line() {
        let err = JiraConfig::from_toml("jql = \"x\"\ntransitions = [\n").unwrap_err();
        assert!(!err.contains('\n'), "{err}");
        assert!(err.starts_with("line 2: "), "{err}");
    }

    #[test]
    fn unknown_keys_are_errors() {
        assert!(JiraConfig::from_toml("jqll = \"x\"").is_err());
    }
}
