//! The shape of a session: what runs in the agent pane, and what surrounds it.

use std::time::Duration;

use thiserror::Error;

/// A program and its arguments. Never empty, so a pane with no command cannot
/// exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command(Vec<String>);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CommandError {
    #[error("command is empty")]
    Empty,
    #[error("command has an empty program name")]
    EmptyProgram,
}

impl Command {
    pub fn new(argv: Vec<String>) -> Result<Self, CommandError> {
        let program = argv.first().ok_or(CommandError::Empty)?;
        if program.trim().is_empty() {
            return Err(CommandError::EmptyProgram);
        }
        Ok(Self(argv))
    }

    /// A shell script, as config hooks are written.
    pub fn shell(script: &str) -> Result<Self, CommandError> {
        if script.trim().is_empty() {
            return Err(CommandError::EmptyProgram);
        }
        Ok(Self(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            script.to_owned(),
        ]))
    }

    pub fn program(&self) -> &str {
        // Non-empty by construction; `first()` keeps the indexing lint happy.
        self.0.first().map(String::as_str).unwrap_or_default()
    }

    pub fn argv(&self) -> &[String] {
        &self.0
    }
}

/// A pane's size, as a percentage of the pane it is split from - which is what
/// `tmux split-window -l` takes, and is not the same as a share of the window.
///
/// Pane 1 splits the agent sideways, so its size is the context column's width.
/// Every later pane splits its predecessor, so its size is a share of that
/// pane's height and the predecessor keeps the rest. Sizes therefore compound
/// down the stack and do not add up to anything: a column of three even rows
/// is written 67% then 50%, not 33% apiece, and the first pane's own size is
/// the column's width rather than a height at all. `default_panes` in
/// `zax-config` is the worked example.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PaneSize(u8);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PaneSizeError {
    #[error("pane size {0}% must be between 1% and 99%")]
    OutOfRange(u8),
}

impl PaneSize {
    pub fn percent(value: u8) -> Result<Self, PaneSizeError> {
        if (1..=99).contains(&value) {
            Ok(Self(value))
        } else {
            Err(PaneSizeError::OutOfRange(value))
        }
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

/// A pane implemented in Rust and shipped with zax.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinPane {
    PrsForReview,
    BranchStatus,
    MyPrs,
    Tickets,
    Todo,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuiltinPaneError {
    #[error(
        "`{0}` is not a built-in pane; the built-ins are prs-for-review, branch-status, my-prs, tickets and todo"
    )]
    Unknown(String),
}

impl BuiltinPane {
    /// Every built-in there is. Help text, validation messages and the CLI's
    /// own subcommand test read the list from here, so a built-in added to
    /// the enum cannot be left out of one of them unnoticed.
    pub const ALL: [Self; 5] = [
        Self::PrsForReview,
        Self::BranchStatus,
        Self::MyPrs,
        Self::Tickets,
        Self::Todo,
    ];

    pub fn parse(raw: &str) -> Result<Self, BuiltinPaneError> {
        match raw {
            "prs-for-review" => Ok(Self::PrsForReview),
            "branch-status" => Ok(Self::BranchStatus),
            "my-prs" => Ok(Self::MyPrs),
            "tickets" => Ok(Self::Tickets),
            "todo" => Ok(Self::Todo),
            other => Err(BuiltinPaneError::Unknown(other.to_owned())),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::PrsForReview => "prs-for-review",
            Self::BranchStatus => "branch-status",
            Self::MyPrs => "my-prs",
            Self::Tickets => "tickets",
            Self::Todo => "todo",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneKind {
    Builtin(BuiltinPane),
    /// A command whose stdout zax wraps in a styled header and refresh loop.
    Exec {
        title: String,
        every: Duration,
        command: Command,
    },
    /// A command that owns the pane outright. zax gets out of the way.
    Raw {
        command: Command,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneSpec {
    pub kind: PaneKind,
    pub size: Option<PaneSize>,
}

/// The agent command plus the context panes around it. Pane 0 is always the
/// agent, so it is not in `panes`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayoutSpec {
    agent: Command,
    panes: Vec<PaneSpec>,
}

impl LayoutSpec {
    /// Infallible: a `PaneSize` is already a percentage of the pane it splits,
    /// and panes do not share a budget, so there is no cross-pane rule left to
    /// check.
    pub fn new(agent: Command, panes: Vec<PaneSpec>) -> Self {
        Self { agent, panes }
    }

    pub fn agent(&self) -> &Command {
        &self.agent
    }

    pub fn panes(&self) -> &[PaneSpec] {
        &self.panes
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DurationError {
    #[error("`{0}` is not a duration; write it as 30s, 5m, 2h or 14d")]
    Malformed(String),
    #[error("duration must be greater than zero")]
    Zero,
}

/// Parse `30s`, `5m`, `2h`, or a bare number meaning seconds.
pub fn parse_duration(raw: &str) -> Result<Duration, DurationError> {
    let malformed = || DurationError::Malformed(raw.to_owned());

    let (digits, multiplier) = match raw.as_bytes().last() {
        Some(b's') => (raw.get(..raw.len() - 1).ok_or_else(malformed)?, 1u64),
        Some(b'm') => (raw.get(..raw.len() - 1).ok_or_else(malformed)?, 60),
        Some(b'h') => (raw.get(..raw.len() - 1).ok_or_else(malformed)?, 3600),
        Some(b'd') => (raw.get(..raw.len() - 1).ok_or_else(malformed)?, 86_400),
        Some(b'0'..=b'9') => (raw, 1),
        _ => return Err(malformed()),
    };

    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(malformed());
    }
    let count: u64 = digits.parse().map_err(|_| malformed())?;
    if count == 0 {
        return Err(DurationError::Zero);
    }
    let secs = count.checked_mul(multiplier).ok_or_else(malformed)?;
    Ok(Duration::from_secs(secs))
}

/// Render a duration in the same grammar `parse_duration` accepts.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs > 0 && secs.is_multiple_of(3600) {
        format!("{}h", secs / 3600)
    } else if secs > 0 && secs.is_multiple_of(60) {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    #[test]
    fn a_command_needs_at_least_a_program() {
        assert_eq!(Command::new(vec![]), Err(CommandError::Empty));
        assert_eq!(
            Command::new(vec![String::new()]),
            Err(CommandError::EmptyProgram)
        );
        let c = Command::new(vec!["claude".to_owned(), "--resume".to_owned()]).unwrap();
        assert_eq!(c.program(), "claude");
        assert_eq!(c.argv(), ["claude", "--resume"]);
    }

    #[test]
    fn a_shell_command_wraps_the_script() {
        let c = Command::shell("direnv allow && bundle install").unwrap();
        assert_eq!(c.argv(), ["sh", "-c", "direnv allow && bundle install"]);
    }

    #[test]
    fn an_empty_shell_script_is_rejected() {
        assert_eq!(Command::shell("   "), Err(CommandError::EmptyProgram));
    }

    #[test]
    fn a_pane_size_is_a_percentage_strictly_between_zero_and_one_hundred() {
        assert_eq!(PaneSize::percent(30).unwrap().get(), 30);
        assert!(PaneSize::percent(0).is_err());
        assert!(PaneSize::percent(100).is_err());
        assert!(PaneSize::percent(99).is_ok());
    }

    #[test]
    fn builtin_pane_names_round_trip() {
        assert_eq!(
            BuiltinPane::parse("prs-for-review").unwrap(),
            BuiltinPane::PrsForReview
        );
        assert_eq!(
            BuiltinPane::parse("branch-status").unwrap(),
            BuiltinPane::BranchStatus
        );
        assert_eq!(BuiltinPane::PrsForReview.as_str(), "prs-for-review");
        assert_eq!(BuiltinPane::BranchStatus.as_str(), "branch-status");
        let err = BuiltinPane::parse("worktrees").unwrap_err();
        assert!(err.to_string().contains("prs-for-review"), "got: {err}");
    }

    #[test]
    fn pane_sizes_do_not_share_one_budget_because_each_splits_its_predecessor() {
        // A size is a share of the pane being split, not of the window, so
        // sizes past 100% together are the normal way to describe an even
        // stack rather than a layout that crowds out the agent.
        let agent = Command::new(vec!["claude".to_owned()]).unwrap();
        let panes = vec![
            PaneSpec {
                kind: PaneKind::Builtin(BuiltinPane::PrsForReview),
                size: Some(PaneSize::percent(30).unwrap()),
            },
            PaneSpec {
                kind: PaneKind::Builtin(BuiltinPane::BranchStatus),
                size: Some(PaneSize::percent(55).unwrap()),
            },
            PaneSpec {
                kind: PaneKind::Builtin(BuiltinPane::MyPrs),
                size: Some(PaneSize::percent(64).unwrap()),
            },
        ];
        let layout = LayoutSpec::new(agent, panes);
        assert_eq!(layout.panes().len(), 3);
    }

    #[test]
    fn a_layout_with_no_context_panes_is_valid() {
        let agent = Command::new(vec!["claude".to_owned()]).unwrap();
        let layout = LayoutSpec::new(agent, vec![]);
        assert_eq!(layout.agent().program(), "claude");
        assert!(layout.panes().is_empty());
    }

    #[test]
    fn durations_parse_from_the_suffixes_the_config_uses() {
        assert_eq!(parse_duration("60s").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("5m").unwrap(), Duration::from_secs(300));
        assert_eq!(parse_duration("2h").unwrap(), Duration::from_secs(7200));
        assert_eq!(parse_duration("90").unwrap(), Duration::from_secs(90));
    }

    #[test]
    fn durations_parse_the_day_suffix_a_pr_age_threshold_is_written_in() {
        assert_eq!(parse_duration("1d").unwrap(), Duration::from_secs(86_400));
        assert_eq!(
            parse_duration("14d").unwrap(),
            Duration::from_secs(1_209_600)
        );
    }

    #[test]
    fn durations_reject_nonsense_and_zero() {
        assert!(parse_duration("").is_err());
        assert!(parse_duration("s").is_err());
        assert!(parse_duration("0s").is_err());
        assert!(parse_duration("-5s").is_err());
        assert!(parse_duration("5 s").is_err());
        assert!(parse_duration("5w").is_err());
    }

    #[test]
    fn durations_format_back_into_something_parse_duration_accepts() {
        for text in ["60s", "5m", "2h"] {
            let parsed = parse_duration(text).unwrap();
            let rendered = format_duration(parsed);
            assert_eq!(parse_duration(&rendered).unwrap(), parsed, "for {text}");
        }
        assert_eq!(format_duration(Duration::from_secs(90)), "90s");
    }

    #[test]
    fn my_prs_is_a_built_in_pane() {
        assert_eq!(BuiltinPane::parse("my-prs").unwrap(), BuiltinPane::MyPrs);
        assert_eq!(BuiltinPane::MyPrs.as_str(), "my-prs");
    }

    #[test]
    fn todo_is_a_built_in_pane() {
        assert_eq!(BuiltinPane::parse("todo").unwrap(), BuiltinPane::Todo);
        assert_eq!(BuiltinPane::Todo.as_str(), "todo");
    }

    #[test]
    fn tickets_is_a_built_in_pane() {
        assert_eq!(BuiltinPane::parse("tickets").unwrap(), BuiltinPane::Tickets);
        assert_eq!(BuiltinPane::Tickets.as_str(), "tickets");
    }

    #[test]
    fn an_unknown_built_in_names_every_built_in_there_is() {
        // Driven from ALL rather than a list written out here, so adding a
        // built-in and forgetting the message is a failing test.
        let message = BuiltinPane::parse("my-pr").unwrap_err().to_string();
        for known in BuiltinPane::ALL {
            assert!(
                message.contains(known.as_str()),
                "`{}` is missing from: {message}",
                known.as_str()
            );
        }
    }

    #[test]
    fn all_lists_every_built_in_and_each_one_round_trips() {
        assert_eq!(BuiltinPane::ALL.len(), 5);
        for builtin in BuiltinPane::ALL {
            assert_eq!(BuiltinPane::parse(builtin.as_str()).unwrap(), builtin);
        }
    }
}
