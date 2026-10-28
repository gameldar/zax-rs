//! `branch-status`: what stands between this worktree's branch and merging.

use std::time::SystemTime;

use ratatui::crossterm::event::{KeyEvent, MouseEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use zax_domain::keymap::Keymap;
use zax_domain::{BranchName, BranchView, KeyCommand, PullRequestStatus};

use crate::action::PaneAction;
use crate::cache::Cached;
use crate::chrome::{error_line, header_line};
use crate::fmt::{display_width, truncate};
use crate::keys::{split_footer, KeyInput};
use crate::pr_fields::{
    check_glyph, check_style, checks_label, mergeable_label, mergeable_style, review_label,
    review_style, threads_label, threads_style,
};

const TITLE: &str = "This branch";

pub struct BranchStatusPane {
    cache: Cached<BranchView>,
    focused: bool,
    keys: KeyInput,
}

impl BranchStatusPane {
    pub fn new(keymap: Keymap) -> Self {
        Self {
            cache: Cached::new(),
            focused: false,
            keys: KeyInput::new(keymap),
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn apply(&mut self, result: Result<BranchView, String>, at: SystemTime) {
        match result {
            Ok(view) => self.cache.record_ok(view, at),
            Err(message) => self.cache.record_err(message, at),
        }
    }

    fn status(&self) -> Option<&PullRequestStatus> {
        self.cache.value().and_then(BranchView::status)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, now: SystemTime) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let (area, footer) = split_footer(area);

        let mut lines = vec![header_line(
            TITLE,
            &self.cache.status(now),
            self.focused,
            area.width,
        )];

        match self.cache.value() {
            None => {
                if let Some(error) = self.cache.error() {
                    lines.push(error_line(error));
                }
            }
            // A detached HEAD is on no branch, so there is nothing to say about
            // a pull request either.
            Some(BranchView::Detached) => lines.push(Line::styled(
                "detached HEAD",
                Style::default().fg(Color::DarkGray),
            )),
            Some(BranchView::OnBranch { branch, status }) => {
                lines.push(branch_line(branch, area.width));
                match status {
                    None => lines.push(Line::styled(
                        "No open pull request for this branch",
                        Style::default().fg(Color::DarkGray),
                    )),
                    Some(status) => lines.extend(pull_request_lines(status, area.width)),
                }
            }
        }

        frame.render_widget(Paragraph::new(Text::from(lines)), area);
        self.keys.render_which_key(frame, area);
        self.keys.render_footer(frame, footer);
    }

    /// One branch, no rows: there is nothing for the mouse to pick.
    pub fn on_mouse(&mut self, _mouse: MouseEvent) {}

    pub fn on_key(&mut self, key: KeyEvent) -> PaneAction {
        match self.keys.press(key) {
            Some(command) => self.run(command),
            None => PaneAction::None,
        }
    }

    fn run(&mut self, command: KeyCommand) -> PaneAction {
        match command {
            KeyCommand::Refresh => PaneAction::Refresh,
            KeyCommand::Quit => PaneAction::Quit,
            KeyCommand::Open => self
                .status()
                .map(|s| PaneAction::OpenUrl(s.pr.url.clone()))
                .unwrap_or_default(),
            KeyCommand::CopyUrl => self
                .status()
                .map(|s| PaneAction::CopyUrl(s.pr.url.clone()))
                .unwrap_or_default(),
            // Not in this pane's context; the keymap refuses to bind them.
            _ => PaneAction::None,
        }
    }
}

/// The branch the worktree is on. A foreign branch is somebody else's name, so
/// it goes through `truncate` like every other borrowed string.
fn branch_line(branch: &BranchName, width: u16) -> Line<'static> {
    Line::styled(
        truncate(branch.as_str(), usize::from(width)),
        Style::default().add_modifier(Modifier::BOLD),
    )
}

fn pull_request_lines(status: &PullRequestStatus, width: u16) -> Vec<Line<'static>> {
    // Through `truncate`, like every other string this crate renders. It bounds
    // the text to the pane and flattens control characters, and a pull request
    // title is somebody else's text arriving from a forge. The field rows below
    // are built from this crate's own constants and need no such care.
    let reference = truncate(&status.pr.short_ref(), usize::from(width));
    let title_budget = usize::from(width).saturating_sub(display_width(&reference) + 1);
    vec![
        Line::from(vec![
            Span::styled(reference, Style::default().fg(Color::Cyan)),
            Span::raw(" "),
            Span::raw(truncate(&status.pr.title, title_budget)),
        ]),
        field(
            "Checks",
            check_glyph(status.pr.checks),
            checks_label(status.pr.checks),
            check_style(status.pr.checks),
        ),
        field(
            "Review",
            "",
            review_label(status.review),
            review_style(status.review),
        ),
        field(
            "Threads",
            "",
            threads_label(status.unresolved_threads),
            threads_style(status.unresolved_threads),
        ),
        field(
            "Merge",
            "",
            mergeable_label(status.mergeable),
            mergeable_style(status.mergeable),
        ),
    ]
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use zax_domain::{
        BranchName, BranchView, CheckState, Mergeable, PullRequest, PullRequestStatus, RepoSlug,
        ReviewState,
    };

    use super::*;
    use crate::action::PaneAction;
    use crate::keys::testing::press;
    use zax_domain::keymap::{Keymaps, PaneContext};

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn keymap() -> Keymap {
        Keymaps::defaults()
            .for_context(PaneContext::BranchStatus)
            .clone()
    }

    /// Type a key sequence such as `SPC w o`; the action of the last key.
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

    fn status() -> PullRequestStatus {
        PullRequestStatus {
            pr: PullRequest {
                repo: RepoSlug::parse("acme/omega").unwrap(),
                number: 1963,
                title: "Export reports".to_owned(),
                url: "https://github.com/acme/omega/pull/1963".to_owned(),
                is_draft: false,
                created_at: t(100),
                checks: CheckState::Pending,
                author: None,
            },
            review: ReviewState::ChangesRequested,
            unresolved_threads: 2,
            mergeable: Mergeable::Conflicting,
            head: None,
        }
    }

    fn branch() -> BranchName {
        BranchName::parse_local("OMEGA-1963-export-csv-reports").unwrap()
    }

    fn view(status: Option<PullRequestStatus>) -> BranchView {
        BranchView::on_branch(branch(), status)
    }

    fn loaded() -> BranchStatusPane {
        let mut pane = BranchStatusPane::new(keymap());
        pane.apply(Ok(view(Some(status()))), t(100));
        pane
    }

    #[test]
    fn the_pane_names_the_branch_it_is_reporting_on() {
        let text = buffer_text(&loaded(), t(100));
        assert!(
            text.contains("OMEGA-1963-export-csv-reports"),
            "got:\n{text}"
        );
    }

    #[test]
    fn a_branch_with_no_pull_request_is_still_named() {
        // Which branch the worktree is on is the one thing this pane can always
        // answer, so a missing pull request must not blank it.
        let mut pane = BranchStatusPane::new(keymap());
        pane.apply(Ok(view(None)), t(100));
        let text = buffer_text(&pane, t(100));
        assert!(
            text.contains("OMEGA-1963-export-csv-reports"),
            "got:\n{text}"
        );
    }

    #[test]
    fn a_detached_head_says_so_rather_than_naming_a_branch() {
        let mut pane = BranchStatusPane::new(keymap());
        pane.apply(Ok(BranchView::Detached), t(100));
        let text = buffer_text(&pane, t(100));
        assert!(text.contains("detached HEAD"), "got:\n{text}");
        // There is no branch, so a line about "this branch" would be a claim
        // about nothing.
        assert!(!text.contains("No open pull request"), "got:\n{text}");
    }

    #[test]
    fn a_long_branch_name_is_bounded_to_the_pane_width() {
        // A pull request's head branch is not ours to name: it can be long and
        // it can carry slashes. Measured in columns through `unicode-width`
        // directly rather than through this crate's own `display_width`, so the
        // assertion cannot agree with a broken measurement.
        let long = BranchName::parse_foreign(&format!("dependabot/cargo/{}", "serde-".repeat(20)))
            .unwrap();

        for width in 0..=60u16 {
            let line = branch_line(&long, width);
            let rendered: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            assert!(
                unicode_width::UnicodeWidthStr::width(rendered.as_str()) <= usize::from(width),
                "width {width} overflowed: {rendered:?}"
            );
        }
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
        let mut pane = BranchStatusPane::new(keymap());
        pane.apply(Ok(view(None)), t(100));
        assert!(buffer_text(&pane, t(100)).contains("No open pull request"));
    }

    #[test]
    fn no_unresolved_threads_is_reported_positively() {
        let mut pane = BranchStatusPane::new(keymap());
        pane.apply(
            Ok(view(Some(PullRequestStatus {
                unresolved_threads: 0,
                ..status()
            }))),
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
        let mut pane = BranchStatusPane::new(keymap());
        pane.apply(Err("gh is not installed or not on PATH".to_owned()), t(100));
        assert!(buffer_text(&pane, t(100)).contains("not installed"));
    }

    #[test]
    fn enter_opens_the_pull_request_and_spc_y_copies_its_url() {
        let mut pane = loaded();
        let url = "https://github.com/acme/omega/pull/1963".to_owned();
        assert_eq!(press(&mut pane, "RET"), PaneAction::OpenUrl(url.clone()));
        assert_eq!(
            press(&mut pane, "SPC o b"),
            PaneAction::OpenUrl(url.clone())
        );
        assert_eq!(press(&mut pane, "SPC y"), PaneAction::CopyUrl(url));
    }

    #[test]
    fn row_actions_do_nothing_when_there_is_no_pull_request() {
        let mut pane = BranchStatusPane::new(keymap());
        pane.apply(Ok(view(None)), t(100));
        assert_eq!(press(&mut pane, "RET"), PaneAction::None);
    }

    #[test]
    fn spc_r_refreshes_and_spc_q_quits() {
        let mut pane = loaded();
        assert_eq!(press(&mut pane, "SPC r"), PaneAction::Refresh);
        assert_eq!(press(&mut pane, "SPC q"), PaneAction::Quit);
    }

    #[test]
    fn bare_q_and_esc_no_longer_quit() {
        let mut pane = loaded();
        assert_eq!(press(&mut pane, "q"), PaneAction::None);
        assert_eq!(press(&mut pane, "<Esc>"), PaneAction::None);
    }

    #[test]
    fn a_hostile_title_stays_inside_the_pane() {
        // A title arrives from a forge, so it may be long and may carry control
        // characters. The review pane has always bounded it; this one rendered
        // it raw.
        let mut pane = BranchStatusPane::new(keymap());
        let mut broken = status();
        broken.pr.title = format!("first\nsecond\tthird\r{}", "very long ".repeat(40));
        pane.apply(Ok(view(Some(broken))), t(100));

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
