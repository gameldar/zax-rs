//! `prs-for-review`: the pull requests waiting on you, requested or to revisit,
//! one compact row each.

use std::num::NonZeroU32;
use std::time::SystemTime;

use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use zax_domain::keymap::Keymap;
use zax_domain::{KeyCommand, ReviewQueueItem, ReviewerState, Revisit};

use crate::action::{PaneAction, ReviewMode};
use crate::cache::Cached;
use crate::chrome::{error_line, header_line, GLYPH_UPDATED};
use crate::fmt::{display_width, format_age, truncate};
use crate::keys::{split_footer, KeyInput};
use crate::pr_fields::{check_glyph, check_style};

const TITLE: &str = "Review requests";

pub struct PrsForReviewPane {
    cache: Cached<Vec<ReviewQueueItem>>,
    selected: usize,
    focused: bool,
    keys: KeyInput,
}

impl PrsForReviewPane {
    pub fn new(keymap: Keymap) -> Self {
        Self {
            cache: Cached::new(),
            selected: 0,
            focused: false,
            keys: KeyInput::new(keymap),
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn apply(&mut self, result: Result<Vec<ReviewQueueItem>, String>, at: SystemTime) {
        match result {
            Ok(prs) => {
                // The queue can shrink under the cursor between refreshes.
                self.selected = self.selected.min(prs.len().saturating_sub(1));
                self.cache.record_ok(prs, at);
            }
            Err(message) => self.cache.record_err(message, at),
        }
    }

    pub fn rows(&self) -> &[ReviewQueueItem] {
        self.cache.value().map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn selected(&self) -> Option<&ReviewQueueItem> {
        self.rows().get(self.selected)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, now: SystemTime) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let (area, footer) = split_footer(area);

        let title = match self.cache.value() {
            Some(prs) => format!("{TITLE} ({})", prs.len()),
            None => TITLE.to_owned(),
        };
        let mut lines = vec![header_line(
            &title,
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
            Some(prs) if prs.is_empty() => {
                lines.push(Line::styled(
                    "Nothing waiting on you",
                    Style::default().fg(Color::DarkGray),
                ));
            }
            Some(prs) => {
                let window = self.visible_rows(prs.len(), area.height);
                for (index, item) in prs.iter().enumerate().skip(window.start).take(window.len()) {
                    lines.push(self.row(item, index == self.selected, area.width, now));
                }
            }
        }

        frame.render_widget(Paragraph::new(Text::from(lines)), area);
        self.keys.render_which_key(frame, area);
        self.keys.render_footer(frame, footer);
    }

    /// Which rows a pane of this height shows.
    ///
    /// The window follows the selection rather than being stored and nudged:
    /// derived from `selected` at every draw, it cannot fall out of step with
    /// it, and there is no second place for `j` to forget to update. It scrolls
    /// only as far as it must, so the selection becomes the last visible row
    /// rather than jumping to the middle.
    ///
    /// Keeping the selection on screen is not cosmetic. `RET`, `SPC y` and `SPC w`
    /// all act on it, and `SPC w` builds a worktree, so a cursor the user cannot
    /// see is a keystroke acting on a pull request they did not pick.
    fn visible_rows(&self, len: usize, height: u16) -> std::ops::Range<usize> {
        // One line of the pane is the header.
        let visible = usize::from(height).saturating_sub(1);
        let first = self.selected.saturating_sub(visible.saturating_sub(1));
        let first = first.min(len);
        first..(first + visible).min(len)
    }

    fn row(
        &self,
        item: &ReviewQueueItem,
        selected: bool,
        width: u16,
        now: SystemTime,
    ) -> Line<'static> {
        let pr = &item.pr;
        let age = format_age(pr.age(now));
        let check = check_glyph(pr.checks).to_owned();
        let state = state_field(item.state);
        let draft = if pr.is_draft { " draft" } else { "" };

        // What a row cannot do without: the check glyph, the state field, the
        // age, the draft flag and the three separators between the spans below.
        let essential = display_width(&check)
            + display_width(&state)
            + display_width(draft)
            + display_width(&age)
            + 3;

        // The reference shrinks too, not just the title. A repository name may
        // be wide and long, and a context pane is routinely narrow - the
        // default layout gives this one 30% of the window, which is 24 columns
        // on an 80-column terminal. Shrinking only the title left the row wider
        // than the pane no matter how little title was left.
        //
        // The number is what identifies the pull request, so it survives and
        // the name gives way around it.
        let number = format!("#{}", pr.number);
        let reference_budget = usize::from(width).saturating_sub(essential);
        let name_budget = reference_budget.saturating_sub(display_width(&number));
        let reference = format!("{}{}", truncate(pr.repo.name(), name_budget), number);

        // The author is context as well, and the first thing to give way: it
        // takes only what the reference left of the budget.
        let author_budget = reference_budget.saturating_sub(display_width(&reference));
        let author = pr
            .author
            .as_deref()
            .map(|login| author_suffix(login, author_budget))
            .unwrap_or_default();

        // Whatever the reference did not need goes to the title. Everything is
        // measured in COLUMNS: the slug parser deliberately does not restrict a
        // repository name's charset, so counting characters would hand the
        // title a budget several columns too large and push the row past the
        // pane. The glyph is measured rather than assumed to be one column, so
        // a future two-column glyph cannot quietly break the arithmetic.
        let fixed = essential + display_width(&reference) + display_width(&author);
        let title_budget = usize::from(width).saturating_sub(fixed);
        let title = truncate(&pr.title, title_budget);

        let base = if selected && self.focused {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };

        Line::from(vec![
            Span::styled(check, check_style(pr.checks).patch(base)),
            Span::styled(state, state_style(item.state).patch(base)),
            Span::styled(" ", base),
            Span::styled(reference, base.fg(Color::Cyan)),
            Span::styled(author, base.fg(Color::DarkGray)),
            Span::styled(" ", base),
            Span::styled(title, base),
            Span::styled(draft.to_owned(), base.fg(Color::DarkGray)),
            Span::styled(" ", base),
            Span::styled(age, base.fg(Color::DarkGray)),
        ])
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PaneAction {
        match self.keys.press(key) {
            Some(command) => self.run(command),
            None => PaneAction::None,
        }
    }

    fn run(&mut self, command: KeyCommand) -> PaneAction {
        let len = self.rows().len();
        match command {
            KeyCommand::Refresh => PaneAction::Refresh,
            KeyCommand::Quit => PaneAction::Quit,
            KeyCommand::MoveDown => {
                self.selected = (self.selected + 1).min(len.saturating_sub(1));
                PaneAction::None
            }
            KeyCommand::MoveUp => {
                self.selected = self.selected.saturating_sub(1);
                PaneAction::None
            }
            KeyCommand::Open => self
                .selected()
                .map(|item| PaneAction::OpenUrl(item.pr.url.clone()))
                .unwrap_or_default(),
            KeyCommand::CopyUrl => self
                .selected()
                .map(|item| PaneAction::CopyUrl(item.pr.url.clone()))
                .unwrap_or_default(),
            KeyCommand::WorktreeOpen => self.worktree(ReviewMode::Open),
            KeyCommand::WorktreeReview => self.worktree(ReviewMode::OpenAndReview),
            // Not in this pane's context; the keymap refuses to bind them.
            _ => PaneAction::None,
        }
    }

    fn worktree(&self, mode: ReviewMode) -> PaneAction {
        self.selected()
            .map(|item| PaneAction::ReviewWorktree {
                repo: item.pr.repo.clone(),
                number: item.pr.number,
                mode,
            })
            .unwrap_or_default()
    }
}

/// The two-column field after the check glyph: blank for a request, else the
/// updated glyph and your open-thread count.
fn state_field(state: ReviewerState) -> String {
    match state {
        ReviewerState::Requested => "  ".to_owned(),
        ReviewerState::Revisit(Revisit::OpenThreads(count)) => format!(" {}", thread_count(count)),
        ReviewerState::Revisit(Revisit::Updated) => format!("{GLYPH_UPDATED} "),
        ReviewerState::Revisit(Revisit::UpdatedWithOpenThreads(count)) => {
            format!("{GLYPH_UPDATED}{}", thread_count(count))
        }
    }
}

// One column, whatever the count: the row's budget is built on it.
fn thread_count(count: NonZeroU32) -> String {
    if count.get() > 9 {
        "+".to_owned()
    } else {
        count.get().to_string()
    }
}

fn state_style(state: ReviewerState) -> Style {
    match state {
        ReviewerState::Requested => Style::default(),
        ReviewerState::Revisit(Revisit::OpenThreads(_)) => Style::default().fg(Color::DarkGray),
        ReviewerState::Revisit(Revisit::Updated | Revisit::UpdatedWithOpenThreads(_)) => {
            Style::default().fg(Color::Yellow)
        }
    }
}

/// ` (login)` within `budget` columns, or nothing when not even one character
/// of the login would fit between the parentheses.
fn author_suffix(login: &str, budget: usize) -> String {
    // A space and two parentheses.
    const FRAME: usize = 3;
    if budget <= FRAME {
        return String::new();
    }
    let login = truncate(login, budget - FRAME);
    if login.is_empty() {
        return String::new();
    }
    format!(" ({login})")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::Terminal;
    use std::num::NonZeroU32;

    use zax_domain::{CheckState, PullRequest, RepoSlug, ReviewQueueItem, ReviewerState, Revisit};

    use super::*;
    use crate::action::PaneAction;
    use crate::chrome::{GLYPH_PASS, GLYPH_UPDATED};
    use crate::keys::testing::press;
    use zax_domain::keymap::{Keymaps, PaneContext};

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn keymap() -> Keymap {
        Keymaps::defaults()
            .for_context(PaneContext::PrsForReview)
            .clone()
    }

    /// Type a key sequence such as `SPC w o`; the action of the last key.
    fn row(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn buffer_rows(buffer: &Buffer) -> String {
        (0..buffer.area.height)
            .map(|y| row(buffer, y))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn draw(pane: &PrsForReviewPane, now: SystemTime) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(48, 6)).unwrap();
        terminal
            .draw(|frame| pane.render(frame, frame.area(), now))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn pr(repo: &str, number: u32, draft: bool, checks: CheckState) -> PullRequest {
        PullRequest {
            repo: RepoSlug::parse(repo).unwrap(),
            number,
            title: "Export reports as CSV".to_owned(),
            url: format!("https://github.com/{repo}/pull/{number}"),
            is_draft: draft,
            created_at: t(100),
            checks,
            author: Some("gameldar".to_owned()),
        }
    }

    fn requested(pr: PullRequest) -> ReviewQueueItem {
        ReviewQueueItem {
            pr,
            state: ReviewerState::Requested,
        }
    }

    fn revisit(pr: PullRequest, revisit: Revisit) -> ReviewQueueItem {
        ReviewQueueItem {
            pr,
            state: ReviewerState::Revisit(revisit),
        }
    }

    fn threads(count: u32) -> NonZeroU32 {
        NonZeroU32::new(count).unwrap()
    }

    /// The row `pane` builds for `item` at a comfortable width, as plain text.
    fn rendered(item: &ReviewQueueItem) -> String {
        let pane = PrsForReviewPane::new(keymap());
        pane.row(item, false, 80, t(100))
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    fn omega() -> PullRequest {
        pr("acme/omega", 1963, false, CheckState::Passing)
    }

    #[test]
    fn a_requested_row_has_a_blank_two_column_state_field() {
        let text = rendered(&requested(omega()));
        assert!(
            text.starts_with(&format!("{GLYPH_PASS}   omega#1963")),
            "got: {text:?}"
        );
    }

    #[test]
    fn an_open_threads_revisit_shows_the_count() {
        let text = rendered(&revisit(omega(), Revisit::OpenThreads(threads(3))));
        assert!(
            text.starts_with(&format!("{GLYPH_PASS} 3 omega#1963")),
            "got: {text:?}"
        );
    }

    #[test]
    fn an_updated_revisit_shows_the_updated_glyph() {
        let text = rendered(&revisit(omega(), Revisit::Updated));
        assert!(
            text.starts_with(&format!("{GLYPH_PASS}{GLYPH_UPDATED}  omega#1963")),
            "got: {text:?}"
        );
    }

    #[test]
    fn an_updated_revisit_with_open_threads_shows_glyph_and_count() {
        let text = rendered(&revisit(
            omega(),
            Revisit::UpdatedWithOpenThreads(threads(2)),
        ));
        assert!(
            text.starts_with(&format!("{GLYPH_PASS}{GLYPH_UPDATED}2 omega#1963")),
            "got: {text:?}"
        );
    }

    #[test]
    fn a_count_above_nine_renders_plus() {
        let open = rendered(&revisit(omega(), Revisit::OpenThreads(threads(10))));
        assert!(
            open.starts_with(&format!("{GLYPH_PASS} + omega#1963")),
            "got: {open:?}"
        );
        let both = rendered(&revisit(
            omega(),
            Revisit::UpdatedWithOpenThreads(threads(40)),
        ));
        assert!(
            both.starts_with(&format!("{GLYPH_PASS}{GLYPH_UPDATED}+ omega#1963")),
            "got: {both:?}"
        );
    }

    #[test]
    fn the_state_field_is_two_columns_for_every_state() {
        let states = [
            ReviewerState::Requested,
            ReviewerState::Revisit(Revisit::OpenThreads(threads(1))),
            ReviewerState::Revisit(Revisit::OpenThreads(threads(10))),
            ReviewerState::Revisit(Revisit::Updated),
            ReviewerState::Revisit(Revisit::UpdatedWithOpenThreads(threads(1))),
            ReviewerState::Revisit(Revisit::UpdatedWithOpenThreads(threads(10))),
        ];
        for state in states {
            let field = state_field(state);
            assert_eq!(
                unicode_width::UnicodeWidthStr::width(field.as_str()),
                2,
                "{state:?} gave {field:?}"
            );
        }
    }

    #[test]
    fn a_revisit_draft_still_shows_the_draft_flag() {
        let draft = pr("acme/south", 42, true, CheckState::Failing);
        let text = rendered(&revisit(draft, Revisit::Updated));
        assert!(text.contains(" draft"), "got: {text:?}");
        assert!(text.contains(GLYPH_UPDATED), "got: {text:?}");
    }

    fn loaded() -> PrsForReviewPane {
        let mut pane = PrsForReviewPane::new(keymap());
        pane.apply(
            Ok(vec![
                requested(pr("acme/omega", 1963, false, CheckState::Passing)),
                requested(pr("acme/south", 42, true, CheckState::Failing)),
            ]),
            t(100),
        );
        pane
    }

    #[test]
    fn the_header_names_the_pane_and_counts_the_queue() {
        let buffer = draw(&loaded(), t(100));
        let header = row(&buffer, 0);
        assert!(header.contains("Review requests"), "got: {header:?}");
        assert!(header.contains('2'), "got: {header:?}");
    }

    #[test]
    fn a_narrow_pane_keeps_the_number_and_gives_up_the_repository_name() {
        // The number identifies the pull request; the name is context. When
        // there is not room for both, the name is what goes.
        let mut pane = PrsForReviewPane::new(keymap());
        pane.apply(
            Ok(vec![requested(pr(
                "acme/a-rather-long-repository-name",
                1963,
                false,
                CheckState::Passing,
            ))]),
            t(100),
        );
        let row = pane.rows().first().expect("one row").clone();
        let built = pane.row(&row, false, 24, t(100));
        let rendered: String = built.spans.iter().map(|s| s.content.as_ref()).collect();

        assert!(
            rendered.contains("#1963"),
            "the number must survive: {rendered:?}"
        );
        assert!(
            crate::fmt::display_width(&rendered) <= 24,
            "{} columns: {rendered:?}",
            crate::fmt::display_width(&rendered)
        );
    }

    #[test]
    fn a_row_carries_repo_and_number_title_age_and_check_state() {
        let buffer = draw(&loaded(), t(100 + 3 * 86_400));
        let first = row(&buffer, 1);
        assert!(first.contains("omega#1963"), "got: {first:?}");
        assert!(first.contains("Stamp"), "got: {first:?}");
        assert!(first.contains("3d"), "got: {first:?}");
        assert!(first.contains(crate::chrome::GLYPH_PASS), "got: {first:?}");
    }

    #[test]
    fn a_row_names_the_author_after_the_reference() {
        let buffer = draw(&loaded(), t(100));
        let first = row(&buffer, 1);
        assert!(
            first.contains("omega#1963 (gameldar) Stamp"),
            "got: {first:?}"
        );
    }

    #[test]
    fn a_pull_request_without_an_author_shows_no_parentheses() {
        let mut orphan = requested(pr("acme/omega", 1963, false, CheckState::Passing));
        orphan.pr.author = None;
        let mut pane = PrsForReviewPane::new(keymap());
        pane.apply(Ok(vec![orphan]), t(100));
        let first = row(&draw(&pane, t(100)), 1);
        assert!(first.contains("omega#1963 Stamp"), "got: {first:?}");
    }

    #[test]
    fn the_author_gives_way_before_the_repository_name() {
        let mut pane = PrsForReviewPane::new(keymap());
        pane.apply(
            Ok(vec![requested(pr(
                "acme/omega",
                1963,
                false,
                CheckState::Passing,
            ))]),
            t(100),
        );
        let pr = pane.rows().first().expect("one row").clone();
        // Glyph, state field, three separators and `now` leave exactly `omega#1963`.
        let built = pane.row(&pr, false, 1 + 2 + 3 + 3 + 10, t(100));
        let rendered: String = built.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(rendered.contains("omega#1963"), "got: {rendered:?}");
        assert!(!rendered.contains('('), "got: {rendered:?}");
    }

    #[test]
    fn a_draft_is_flagged_and_its_failing_checks_shown() {
        let buffer = draw(&loaded(), t(100));
        let second = row(&buffer, 2);
        assert!(second.contains("draft"), "got: {second:?}");
        assert!(
            second.contains(crate::chrome::GLYPH_FAIL),
            "got: {second:?}"
        );
    }

    #[test]
    fn the_window_of_rows_follows_the_selection_down_the_queue() {
        // Hand-computed: a 6-row pane spends one row on the header, so five
        // rows of ten are visible, and the window scrolls only far enough to
        // keep the selection as the last of them.
        let mut pane = PrsForReviewPane::new(keymap());
        pane.apply(
            Ok((0..10)
                .map(|n| requested(pr("acme/omega", n, false, CheckState::Passing)))
                .collect()),
            t(100),
        );

        assert_eq!(pane.visible_rows(10, 6), 0..5);
        for expected in [0..5, 0..5, 0..5, 0..5, 1..6, 2..7, 3..8, 4..9, 5..10] {
            press(&mut pane, "j");
            assert_eq!(pane.visible_rows(10, 6), expected);
        }

        // A queue shorter than the pane fills what it has and no more.
        let mut short = PrsForReviewPane::new(keymap());
        short.apply(
            Ok(vec![requested(pr(
                "acme/omega",
                1,
                false,
                CheckState::Passing,
            ))]),
            t(100),
        );
        assert_eq!(short.visible_rows(1, 6), 0..1);
        // A pane with no room for rows at all shows none.
        assert_eq!(short.visible_rows(1, 1), 0..0);
        assert_eq!(short.visible_rows(1, 0), 0..0);
    }

    #[test]
    fn a_selection_pushed_past_the_viewport_is_still_on_screen() {
        // Nine `j` presses in a six-row pane. The pull request numbers are the
        // fixture's own, so what must be on screen is known without asking the
        // pane anything.
        let mut pane = PrsForReviewPane::new(keymap());
        pane.apply(
            Ok((100..110)
                .map(|n| requested(pr("acme/omega", n, false, CheckState::Passing)))
                .collect()),
            t(100),
        );
        pane.set_focused(true);
        for _ in 0..9 {
            press(&mut pane, "j");
        }

        let mut terminal = Terminal::new(TestBackend::new(48, 6)).unwrap();
        terminal
            .draw(|frame| pane.render(frame, frame.area(), t(100)))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let screen: String = (0..buffer.area.height)
            .map(|y| row(&buffer, y))
            .collect::<Vec<_>>()
            .join("\n");

        assert!(screen.contains("#109"), "got:\n{screen}");
        assert!(!screen.contains("#100"), "got:\n{screen}");

        // And the highlight is on it, not on a row that scrolled away.
        let highlighted: Vec<String> = (0..buffer.area.height)
            .filter(|y| buffer[(0, *y)].modifier.contains(Modifier::REVERSED))
            .map(|y| row(&buffer, y))
            .collect();
        assert_eq!(highlighted.len(), 1, "got:\n{screen}");
        assert!(highlighted[0].contains("#109"), "got: {:?}", highlighted[0]);
    }

    #[test]
    fn an_unfocused_pane_does_not_claim_to_hold_the_cursor() {
        // A reversed row reads as "your cursor is here". Only the focused pane
        // may say that, or every pane says it at once.
        let mut pane = loaded();
        pane.set_focused(false);
        let unfocused = draw(&pane, t(100));
        assert!(
            !reversed_cells(&unfocused),
            "nothing may be highlighted while the pane is not focused"
        );

        pane.set_focused(true);
        assert!(
            reversed_cells(&draw(&pane, t(100))),
            "the selected row is highlighted when the pane is focused"
        );
    }

    /// Does anything on screen carry the reversed attribute?
    fn reversed_cells(buffer: &Buffer) -> bool {
        (0..buffer.area.height).any(|y| {
            (0..buffer.area.width).any(|x| buffer[(x, y)].modifier.contains(Modifier::REVERSED))
        })
    }

    #[test]
    fn an_empty_queue_says_so_rather_than_showing_nothing() {
        let mut pane = PrsForReviewPane::new(keymap());
        pane.apply(Ok(vec![]), t(100));
        assert!(
            row(&draw(&pane, t(100)), 1).contains("Nothing"),
            "got: {:?}",
            row(&draw(&pane, t(100)), 1)
        );
    }

    #[test]
    fn a_failure_keeps_the_previous_queue_and_marks_the_header_stale() {
        let mut pane = loaded();
        pane.apply(Err("gh: HTTP 502".to_owned()), t(160));
        let buffer = draw(&pane, t(160));
        assert!(
            row(&buffer, 0).contains("stale"),
            "got: {:?}",
            row(&buffer, 0)
        );
        assert!(
            row(&buffer, 1).contains("omega#1963"),
            "the queue must not blank"
        );
    }

    #[test]
    fn selection_starts_at_the_first_row_and_clamps_at_both_ends() {
        let mut pane = loaded();
        assert_eq!(pane.selected().map(|p| p.pr.number), Some(1963));

        press(&mut pane, "k");
        assert_eq!(
            pane.selected().map(|p| p.pr.number),
            Some(1963),
            "clamped at the top"
        );

        press(&mut pane, "j");
        assert_eq!(pane.selected().map(|p| p.pr.number), Some(42));

        press(&mut pane, "j");
        assert_eq!(
            pane.selected().map(|p| p.pr.number),
            Some(42),
            "clamped at the bottom"
        );
    }

    #[test]
    fn enter_opens_the_selected_pull_request_and_spc_y_copies_its_url() {
        let mut pane = loaded();
        assert_eq!(
            press(&mut pane, "RET"),
            PaneAction::OpenUrl("https://github.com/acme/omega/pull/1963".to_owned())
        );
        assert_eq!(
            press(&mut pane, "SPC o b"),
            PaneAction::OpenUrl("https://github.com/acme/omega/pull/1963".to_owned())
        );
        assert_eq!(
            press(&mut pane, "SPC y"),
            PaneAction::CopyUrl("https://github.com/acme/omega/pull/1963".to_owned())
        );
    }

    #[test]
    fn spc_w_o_opens_the_worktree_and_spc_w_r_also_reviews_it() {
        let mut pane = loaded();
        press(&mut pane, "j");
        let south = RepoSlug::parse("acme/south").unwrap();
        assert_eq!(
            press(&mut pane, "SPC w o"),
            PaneAction::ReviewWorktree {
                repo: south.clone(),
                number: 42,
                mode: ReviewMode::Open,
            }
        );
        assert_eq!(
            press(&mut pane, "SPC w r"),
            PaneAction::ReviewWorktree {
                repo: south,
                number: 42,
                mode: ReviewMode::OpenAndReview,
            }
        );
    }

    #[test]
    fn row_actions_do_nothing_when_the_queue_is_empty() {
        let mut pane = PrsForReviewPane::new(keymap());
        pane.apply(Ok(vec![]), t(100));
        assert_eq!(press(&mut pane, "RET"), PaneAction::None);
        assert_eq!(press(&mut pane, "SPC w o"), PaneAction::None);
        assert_eq!(pane.selected(), None);
    }

    #[test]
    fn a_selection_beyond_a_shrunken_queue_is_pulled_back_into_range() {
        let mut pane = loaded();
        press(&mut pane, "j");
        pane.apply(
            Ok(vec![requested(pr(
                "acme/omega",
                1963,
                false,
                CheckState::Passing,
            ))]),
            t(200),
        );
        assert_eq!(pane.selected().map(|p| p.pr.number), Some(1963));
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
    fn a_half_typed_sequence_opens_the_which_key_popup_and_esc_closes_it() {
        let mut pane = loaded();
        press(&mut pane, "SPC w");
        let text = buffer_rows(&draw(&pane, t(100)));
        assert!(text.contains("open worktree and review"), "got:\n{text}");
        press(&mut pane, "<Esc>");
        let text = buffer_rows(&draw(&pane, t(100)));
        assert!(!text.contains("open worktree and review"), "got:\n{text}");
    }

    #[test]
    fn a_broken_sequence_shows_its_notice_in_the_footer() {
        let mut pane = loaded();
        press(&mut pane, "SPC w x");
        let buffer = draw(&pane, t(100));
        assert_eq!(row(&buffer, 5), "SPC w x is not bound");
    }

    #[test]
    fn the_footer_row_is_the_status_bar() {
        let pane = loaded();
        let footer = row(&draw(&pane, t(100)), 5);
        assert!(footer.starts_with("RET open"), "got: {footer}");
    }

    #[test]
    fn a_repository_with_wide_characters_does_not_overflow_the_row() {
        // The slug parser deliberately allows a non-ASCII repository name, so
        // `repo#number` can be far wider than its character count suggests.
        //
        // Measured on the `Line` the pane BUILDS, not on a rendered buffer.
        // ratatui stores a two-column glyph as one cell plus a continuation
        // cell holding a space, so joining raw cell symbols reports `日本語` as
        // eight columns rather than six. A test reading a buffer back that way
        // fails whether the budget is right or wrong, which is worse than
        // having no test: it looks like coverage and detects nothing. The row
        // the pane builds is also the thing the budget actually controls.
        let mut pane = PrsForReviewPane::new(keymap());
        pane.apply(
            Ok(vec![requested(PullRequest {
                repo: RepoSlug::parse("acme/日本語リポジトリ").unwrap(),
                number: 1963,
                title: "Export reports as CSV".to_owned(),
                url: "https://example.invalid/1963".to_owned(),
                is_draft: false,
                created_at: t(100),
                checks: CheckState::Passing,
                author: Some("gameldar".to_owned()),
            })]),
            t(100),
        );
        let pr = pane.rows().first().expect("one row").clone();

        // Terminal columns, measured through `unicode-width` DIRECTLY rather
        // than through this crate's `display_width`, which is part of what the
        // row's arithmetic is made of. An oracle sharing the measurement agrees
        // with the code however wrong both become.
        let columns = |line: &ratatui::text::Line<'_>| {
            unicode_width::UnicodeWidthStr::width(
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .as_str(),
            )
        };
        let w = |text: &str| unicode_width::UnicodeWidthStr::width(text);

        // The floor a row cannot shrink below, computed from the fixture and
        // the row's documented shape rather than by asking the row for it. An
        // earlier version measured `row(.., width = 0)` here, which grows with
        // any bug that widens the row, so the bound grew with it and the sweep
        // below could not fail: with both width budgets replaced by
        // `usize::MAX` the test still passed.
        //
        // The parts: the one-column check glyph, the two-column state field
        // (blank for a request), the three separators the row
        // always carries, the three-column age of a pull request created at the
        // fixture's own `now`, and `#1963`, which is never truncated because it
        // is what identifies the pull request. The repository name and the
        // title can both go to nothing.
        let floor = w(crate::chrome::GLYPH_PASS) + w("  ") + 3 + w("now") + w("#1963");
        assert_eq!(floor, 14, "the fixture's floor, spelled out");
        assert_eq!(
            columns(&pane.row(&pr, false, 0, t(100))),
            floor,
            "a row squeezed to nothing is exactly its irreducible parts"
        );

        // The invariant, at every width from nothing upward: a row fits its
        // pane, or it is already as small as it goes. Below the floor the
        // render layer clips, which the narrow-pane test covers.
        for width in 0..=60u16 {
            let built = pane.row(&pr, false, width, t(100));
            assert!(
                columns(&built) <= usize::from(width).max(floor),
                "width {width} produced a {}-column row (the floor is \
                 {floor}): {:?}",
                columns(&built),
                built
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            );
        }

        // And the floor really is the identifying part rather than something
        // that happens to be short: at width zero the number survives.
        let smallest: String = pane
            .row(&pr, false, 0, t(100))
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(smallest.contains("#1963"), "got: {smallest:?}");
    }

    #[test]
    fn a_very_narrow_pane_renders_without_panicking() {
        let mut terminal = Terminal::new(TestBackend::new(6, 3)).unwrap();
        let pane = loaded();
        terminal
            .draw(|frame| pane.render(frame, frame.area(), t(100)))
            .unwrap();
    }
}
