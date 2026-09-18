//! `branch-status`: what stands between this worktree's branch and merging.

use std::time::SystemTime;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use zax_domain::{BranchStatus, CheckState, Mergeable, ReviewState};

use crate::action::PaneAction;
use crate::cache::Cached;
use crate::chrome::{error_line, header_line};
use crate::fmt::{display_width, truncate};
use crate::prs_pane::{check_glyph, check_style};

const TITLE: &str = "This branch";

pub struct BranchStatusPane {
    cache: Cached<Option<BranchStatus>>,
    focused: bool,
}

impl Default for BranchStatusPane {
    fn default() -> Self {
        Self::new()
    }
}

impl BranchStatusPane {
    pub fn new() -> Self {
        Self {
            cache: Cached::new(),
            focused: false,
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn apply(&mut self, result: Result<Option<BranchStatus>, String>, at: SystemTime) {
        match result {
            Ok(status) => self.cache.record_ok(status, at),
            Err(message) => self.cache.record_err(message, at),
        }
    }

    fn status(&self) -> Option<&BranchStatus> {
        self.cache.value().and_then(Option::as_ref)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, now: SystemTime) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let mut lines = vec![header_line(
            TITLE,
            &self.cache.status(now),
            self.focused,
            area.width,
        )];

        match (self.cache.value(), self.status()) {
            (None, _) => {
                if let Some(error) = self.cache.error() {
                    lines.push(error_line(error));
                }
            }
            (Some(_), None) => lines.push(Line::styled(
                "No open pull request for this branch",
                Style::default().fg(Color::DarkGray),
            )),
            (Some(_), Some(status)) => {
                // Through `truncate`, like every other string this crate
                // renders. It bounds the text to the pane and flattens control
                // characters, and a pull request title is somebody else's text
                // arriving from a forge. The field rows below are built from
                // this crate's own constants and need no such care.
                let reference = truncate(&status.pr.short_ref(), usize::from(area.width));
                let title_budget =
                    usize::from(area.width).saturating_sub(display_width(&reference) + 1);
                lines.push(Line::from(vec![
                    Span::styled(reference, Style::default().fg(Color::Cyan)),
                    Span::raw(" "),
                    Span::raw(truncate(&status.pr.title, title_budget)),
                ]));
                lines.push(field(
                    "Checks",
                    check_glyph(status.pr.checks),
                    checks_label(status.pr.checks),
                    check_style(status.pr.checks),
                ));
                lines.push(field(
                    "Review",
                    "",
                    review_label(status.review),
                    review_style(status.review),
                ));
                lines.push(field(
                    "Threads",
                    "",
                    threads_label(status.unresolved_threads),
                    threads_style(status.unresolved_threads),
                ));
                lines.push(field(
                    "Merge",
                    "",
                    mergeable_label(status.mergeable),
                    mergeable_style(status.mergeable),
                ));
            }
        }

        frame.render_widget(Paragraph::new(Text::from(lines)), area);
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PaneAction {
        match key.code {
            KeyCode::Char('r') => PaneAction::Refresh,
            KeyCode::Char('q') | KeyCode::Esc => PaneAction::Quit,
            KeyCode::Enter => self
                .status()
                .map(|s| PaneAction::OpenUrl(s.pr.url.clone()))
                .unwrap_or_default(),
            KeyCode::Char('y') => self
                .status()
                .map(|s| PaneAction::CopyUrl(s.pr.url.clone()))
                .unwrap_or_default(),
            _ => PaneAction::None,
        }
    }
}

fn field(label: &str, glyph: &str, value: String, style: Style) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{label:<8}"),
        Style::default().fg(Color::DarkGray),
    )];
    if !glyph.is_empty() {
        spans.push(Span::styled(glyph.to_owned(), style));
        spans.push(Span::raw(" "));
    }
    spans.push(Span::styled(value, style));
    Line::from(spans)
}

fn checks_label(state: CheckState) -> String {
    match state {
        CheckState::Passing => "passing",
        CheckState::Failing => "failing",
        CheckState::Pending => "pending",
        CheckState::None => "none reported",
    }
    .to_owned()
}

fn review_label(state: ReviewState) -> String {
    match state {
        ReviewState::Approved => "approved",
        ReviewState::ChangesRequested => "changes requested",
        ReviewState::ReviewRequired => "review required",
        ReviewState::None => "no review requested",
    }
    .to_owned()
}

fn review_style(state: ReviewState) -> Style {
    let colour = match state {
        ReviewState::Approved => Color::Green,
        ReviewState::ChangesRequested => Color::Red,
        ReviewState::ReviewRequired => Color::Yellow,
        ReviewState::None => Color::DarkGray,
    };
    Style::default().fg(colour)
}

fn threads_label(unresolved: u32) -> String {
    if unresolved == 0 {
        "all resolved".to_owned()
    } else {
        format!("{unresolved} unresolved")
    }
}

fn threads_style(unresolved: u32) -> Style {
    Style::default().fg(if unresolved == 0 {
        Color::Green
    } else {
        Color::Yellow
    })
}

fn mergeable_label(state: Mergeable) -> String {
    match state {
        Mergeable::Mergeable => "mergeable",
        Mergeable::Conflicting => "conflicting",
        Mergeable::Unknown => "unknown",
    }
    .to_owned()
}

fn mergeable_style(state: Mergeable) -> Style {
    let colour = match state {
        Mergeable::Mergeable => Color::Green,
        Mergeable::Conflicting => Color::Red,
        Mergeable::Unknown => Color::DarkGray,
    };
    Style::default().fg(colour)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use zax_domain::{BranchStatus, CheckState, Mergeable, PullRequest, RepoSlug, ReviewState};

    use super::*;
    use crate::action::PaneAction;

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn buffer_text(pane: &BranchStatusPane, now: SystemTime) -> String {
        let mut terminal = Terminal::new(TestBackend::new(44, 8)).unwrap();
        terminal
            .draw(|frame| pane.render(frame, frame.area(), now))
            .unwrap();
        let buffer: Buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn status() -> BranchStatus {
        BranchStatus {
            pr: PullRequest {
                repo: RepoSlug::parse("acme/omega").unwrap(),
                number: 1963,
                title: "Export reports".to_owned(),
                url: "https://github.com/acme/omega/pull/1963".to_owned(),
                is_draft: false,
                created_at: t(100),
                checks: CheckState::Pending,
            },
            review: ReviewState::ChangesRequested,
            unresolved_threads: 2,
            mergeable: Mergeable::Conflicting,
        }
    }

    fn loaded() -> BranchStatusPane {
        let mut pane = BranchStatusPane::new();
        pane.apply(Ok(Some(status())), t(100));
        pane
    }

    #[test]
    fn the_pane_shows_checks_review_threads_and_mergeability() {
        let text = buffer_text(&loaded(), t(100));
        assert!(text.contains("omega#1963"), "got:\n{text}");
        assert!(text.contains("Checks"), "got:\n{text}");
        assert!(text.contains("Review"), "got:\n{text}");
        assert!(text.contains("changes requested"), "got:\n{text}");
        assert!(text.contains("2 unresolved"), "got:\n{text}");
        assert!(text.contains("conflicting"), "got:\n{text}");
    }

    #[test]
    fn a_branch_with_no_pull_request_says_so() {
        let mut pane = BranchStatusPane::new();
        pane.apply(Ok(None), t(100));
        assert!(buffer_text(&pane, t(100)).contains("No open pull request"));
    }

    #[test]
    fn no_unresolved_threads_is_reported_positively() {
        let mut pane = BranchStatusPane::new();
        pane.apply(
            Ok(Some(BranchStatus {
                unresolved_threads: 0,
                ..status()
            })),
            t(100),
        );
        assert!(buffer_text(&pane, t(100)).contains("all resolved"));
    }

    #[test]
    fn a_failure_keeps_the_last_good_status_and_marks_the_header_stale() {
        let mut pane = loaded();
        pane.apply(Err("gh: HTTP 502".to_owned()), t(160));
        let text = buffer_text(&pane, t(160));
        assert!(text.contains("stale"), "got:\n{text}");
        assert!(text.contains("omega#1963"), "got:\n{text}");
    }

    #[test]
    fn a_failure_with_nothing_cached_shows_an_actionable_error() {
        let mut pane = BranchStatusPane::new();
        pane.apply(Err("gh is not installed or not on PATH".to_owned()), t(100));
        assert!(buffer_text(&pane, t(100)).contains("not installed"));
    }

    #[test]
    fn enter_opens_the_pull_request_and_y_copies_its_url() {
        let mut pane = loaded();
        assert_eq!(
            pane.on_key(key(KeyCode::Enter)),
            PaneAction::OpenUrl("https://github.com/acme/omega/pull/1963".to_owned())
        );
        assert_eq!(
            pane.on_key(key(KeyCode::Char('y'))),
            PaneAction::CopyUrl("https://github.com/acme/omega/pull/1963".to_owned())
        );
    }

    #[test]
    fn row_actions_do_nothing_when_there_is_no_pull_request() {
        let mut pane = BranchStatusPane::new();
        pane.apply(Ok(None), t(100));
        assert_eq!(pane.on_key(key(KeyCode::Enter)), PaneAction::None);
    }

    #[test]
    fn r_refreshes_and_q_quits() {
        let mut pane = loaded();
        assert_eq!(pane.on_key(key(KeyCode::Char('r'))), PaneAction::Refresh);
        assert_eq!(pane.on_key(key(KeyCode::Char('q'))), PaneAction::Quit);
    }

    #[test]
    fn a_hostile_title_stays_inside_the_pane() {
        // A title arrives from a forge, so it may be long and may carry control
        // characters. The review pane has always bounded it; this one rendered
        // it raw.
        let mut pane = BranchStatusPane::new();
        let mut broken = status();
        broken.pr.title = format!("first\nsecond\tthird\r{}", "very long ".repeat(40));
        pane.apply(Ok(Some(broken)), t(100));

        for width in 1..=60u16 {
            let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
            terminal
                .draw(|frame| pane.render(frame, frame.area(), t(100)))
                .unwrap();
            let buffer: Buffer = terminal.backend().buffer().clone();
            // Read cells rather than the line: this asserts nothing escaped
            // onto the screen. Column arithmetic is the review pane's test.
            for y in 0..buffer.area.height {
                for x in 0..buffer.area.width {
                    let symbol = buffer[(x, y)].symbol();
                    assert!(
                        !symbol.contains('\n') && !symbol.contains('\t') && !symbol.contains('\r'),
                        "raw control character at {x},{y} for width {width}"
                    );
                }
            }
        }
    }

    #[test]
    fn a_zero_sized_area_renders_without_panicking() {
        let mut terminal = Terminal::new(TestBackend::new(0, 0)).unwrap();
        let pane = loaded();
        terminal
            .draw(|frame| pane.render(frame, frame.area(), t(100)))
            .unwrap();
    }
}
