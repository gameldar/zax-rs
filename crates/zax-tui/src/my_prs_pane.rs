//! `my-prs`: every open pull request you wrote, worst first.

use std::time::SystemTime;

use ratatui::crossterm::event::{KeyEvent, MouseEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use zax_domain::keymap::Keymap;
use zax_domain::{Attention, KeyCommand, MyPullRequest, PullRequestStatus};

use crate::action::{PaneAction, ReviewMode};
use crate::cache::Cached;
use crate::chrome::{error_line, header_line, GLYPH_CHANGES, GLYPH_ERROR, GLYPH_NONE, GLYPH_PASS};
use crate::fmt::{display_width, format_age, truncate};
use crate::keys::{split_footer, KeyInput};
use crate::pr_fields::{
    check_glyph, check_style, checks_label, mergeable_label, mergeable_style, review_label,
    review_style, threads_label, threads_style,
};
use crate::scroll::{ListScroll, Viewport};

const TITLE: &str = "My pull requests";
const SEPARATOR: &str = " · ";

pub struct MyPrsPane {
    cache: Cached<Vec<MyPullRequest>>,
    scroll: ListScroll,
    focused: bool,
    keys: KeyInput,
}

impl MyPrsPane {
    pub fn new(keymap: Keymap) -> Self {
        Self {
            cache: Cached::new(),
            scroll: ListScroll::new(),
            focused: false,
            keys: KeyInput::new(keymap),
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    pub fn apply(&mut self, result: Result<Vec<MyPullRequest>, String>, at: SystemTime) {
        match result {
            Ok(mine) => {
                // The queue can shrink under the cursor between refreshes.
                self.scroll.clamp(mine.len());
                self.cache.record_ok(mine, at);
            }
            Err(message) => self.cache.record_err(message, at),
        }
    }

    pub fn rows(&self) -> &[MyPullRequest] {
        self.cache.value().map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn selected(&self) -> Option<&MyPullRequest> {
        self.rows().get(self.scroll.selected())
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, now: SystemTime) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let (area, footer) = split_footer(area);

        let title = match self.cache.value() {
            Some(mine) => format!("{TITLE} ({})", mine.len()),
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
            Some(mine) if mine.is_empty() => lines.push(Line::styled(
                "No open pull requests of your own",
                Style::default().fg(Color::DarkGray),
            )),
            Some(mine) => {
                let window = self.viewport(mine.len(), area);
                for (index, row) in mine
                    .iter()
                    .enumerate()
                    .skip(window.rows.start)
                    .take(window.rows.len())
                {
                    let selected = index == self.scroll.selected();
                    lines.push(self.row(row, selected, window.width, now));
                }
                // Only once a row is on screen: a detail line describing a row
                // the pane had no room to draw describes nothing.
                if !window.rows.is_empty() {
                    if let Some(selected) = self.selected() {
                        lines.push(detail_line(&selected.status, area.width));
                    }
                }
            }
        }

        frame.render_widget(Paragraph::new(Text::from(lines)), area);
        if self.cache.value().is_some_and(|mine| !mine.is_empty()) {
            self.scroll.render_gutter(frame);
        }
        self.keys.render_which_key(frame, area);
        self.keys.render_footer(frame, footer);
    }

    /// Which rows a pane of this size shows.
    ///
    /// Two lines of chrome rather than the review pane's one: the header, and
    /// the detail line under the rows.
    fn viewport(&self, len: usize, area: Rect) -> Viewport {
        let visible = usize::from(area.height).saturating_sub(2);
        self.scroll
            .layout(len, visible, area.x, area.y + 1, area.width)
    }

    /// `<ci><attention> repo#number title[ draft] age`.
    ///
    /// The two glyphs sit adjacent, as one status cluster. A separator between
    /// them would cost the row a column it does not have at the default pane
    /// width.
    fn row(
        &self,
        row: &MyPullRequest,
        selected: bool,
        width: u16,
        now: SystemTime,
    ) -> Line<'static> {
        let pr = &row.status.pr;
        let age = format_age(pr.age(now));
        let check = check_glyph(pr.checks).to_owned();
        let attention = attention_glyph(row.attention);
        let draft = if pr.is_draft { " draft" } else { "" };

        let essential = display_width(&check)
            + display_width(&attention)
            + display_width(draft)
            + display_width(&age)
            + 3;

        // The number identifies the pull request, so it survives and the
        // repository name gives way around it.
        let number = format!("#{}", pr.number);
        let reference_budget = usize::from(width).saturating_sub(essential);
        let name_budget = reference_budget.saturating_sub(display_width(&number));
        let reference = format!("{}{}", truncate(pr.repo.name(), name_budget), number);

        let fixed = essential + display_width(&reference);
        let title = truncate(&pr.title, usize::from(width).saturating_sub(fixed));

        let base = if selected && self.focused {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };

        Line::from(vec![
            Span::styled(check, check_style(pr.checks).patch(base)),
            Span::styled(attention, attention_style(row.attention).patch(base)),
            Span::styled(" ", base),
            Span::styled(reference, base.fg(Color::Cyan)),
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

    /// A click selects the row under it; the wheel moves the selection.
    pub fn on_mouse(&mut self, mouse: MouseEvent) {
        if self.scroll.on_mouse(mouse, self.rows().len()) {
            self.keys.cancel();
        }
    }

    fn run(&mut self, command: KeyCommand) -> PaneAction {
        let len = self.rows().len();
        match command {
            KeyCommand::Refresh => PaneAction::Refresh,
            KeyCommand::Quit => PaneAction::Quit,
            KeyCommand::MoveDown => {
                self.scroll.down(len);
                PaneAction::None
            }
            KeyCommand::MoveUp => {
                self.scroll.up();
                PaneAction::None
            }
            KeyCommand::PageDown => {
                self.scroll.page_down(len);
                PaneAction::None
            }
            KeyCommand::PageUp => {
                self.scroll.page_up();
                PaneAction::None
            }
            KeyCommand::Open => self
                .selected()
                .map(|m| PaneAction::OpenUrl(m.status.pr.url.clone()))
                .unwrap_or_default(),
            KeyCommand::CopyUrl => self
                .selected()
                .map(|m| PaneAction::CopyUrl(m.status.pr.url.clone()))
                .unwrap_or_default(),
            KeyCommand::WorktreeOpen => self.worktree(ReviewMode::Open),
            KeyCommand::WorktreeReview => self.worktree(ReviewMode::OpenAndReview),
            // Not in this pane's context; the keymap refuses to bind them.
            _ => PaneAction::None,
        }
    }

    fn worktree(&self, mode: ReviewMode) -> PaneAction {
        self.selected()
            .map(|m| PaneAction::ReviewWorktree {
                repo: m.status.pr.repo.clone(),
                number: m.status.pr.number,
                mode,
            })
            .unwrap_or_default()
    }
}

/// What the check glyph beside it cannot say. A red build is already red in
/// that column, and a draft carries its own suffix, so both are quiet here.
pub(crate) fn attention_glyph(attention: Attention) -> String {
    match attention {
        Attention::Conflicting => GLYPH_ERROR.to_owned(),
        Attention::ChangesRequested => GLYPH_CHANGES.to_owned(),
        // One column, whatever the count: the row's budget is built on it.
        Attention::Unresolved(count) if count.get() > 9 => "+".to_owned(),
        Attention::Unresolved(count) => count.get().to_string(),
        Attention::ReadyToMerge => GLYPH_PASS.to_owned(),
        Attention::ChecksFailing | Attention::Waiting | Attention::Draft => GLYPH_NONE.to_owned(),
    }
}

pub(crate) fn attention_style(attention: Attention) -> Style {
    let colour = match attention {
        Attention::Conflicting | Attention::ChangesRequested => Color::Red,
        Attention::Unresolved(_) => Color::Yellow,
        Attention::ReadyToMerge => Color::Green,
        Attention::ChecksFailing | Attention::Waiting | Attention::Draft => Color::DarkGray,
    };
    Style::default().fg(colour)
}

/// The selected row's four signals, in words. The glyphs rank and summarise;
/// this is where the pane says what it actually checked.
fn detail_line(status: &PullRequestStatus, width: u16) -> Line<'static> {
    let parts = [
        (
            checks_label(status.pr.checks),
            check_style(status.pr.checks),
        ),
        (review_label(status.review), review_style(status.review)),
        (
            threads_label(status.unresolved_threads),
            threads_style(status.unresolved_threads),
        ),
        (
            mergeable_label(status.mergeable),
            mergeable_style(status.mergeable),
        ),
    ];

    let mut spans = Vec::new();
    let mut remaining = usize::from(width);
    for (index, (label, style)) in parts.into_iter().enumerate() {
        if remaining == 0 {
            break;
        }
        if index > 0 {
            let separator = truncate(SEPARATOR, remaining);
            remaining -= display_width(&separator);
            spans.push(Span::styled(
                separator,
                Style::default().fg(Color::DarkGray),
            ));
            if remaining == 0 {
                break;
            }
        }
        let text = truncate(&label, remaining);
        remaining -= display_width(&text);
        spans.push(Span::styled(text, style));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::num::NonZeroU32;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::style::Modifier;
    use ratatui::Terminal;
    use zax_domain::{
        Attention, CheckState, Mergeable, MyPullRequest, PullRequest, PullRequestStatus, RepoSlug,
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
        Keymaps::defaults().for_context(PaneContext::MyPrs).clone()
    }

    /// Type a key sequence such as `SPC w o`; the action of the last key.
    fn line(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn mine(
        repo: &str,
        number: u32,
        checks: CheckState,
        review: ReviewState,
        threads: u32,
        mergeable: Mergeable,
        draft: bool,
    ) -> MyPullRequest {
        MyPullRequest::new(PullRequestStatus {
            pr: PullRequest {
                repo: RepoSlug::parse(repo).unwrap(),
                number,
                title: "Export reports as CSV".to_owned(),
                url: format!("https://github.com/{repo}/pull/{number}"),
                is_draft: draft,
                created_at: t(100),
                checks,
                author: None,
            },
            review,
            unresolved_threads: threads,
            mergeable,
            head: None,
        })
    }

    fn loaded() -> MyPrsPane {
        let mut pane = MyPrsPane::new(keymap());
        pane.apply(
            Ok(vec![
                mine(
                    "gameldar/zax",
                    12,
                    CheckState::Failing,
                    ReviewState::ChangesRequested,
                    1,
                    Mergeable::Mergeable,
                    false,
                ),
                mine(
                    "acme/omega",
                    8,
                    CheckState::Passing,
                    ReviewState::Approved,
                    0,
                    Mergeable::Mergeable,
                    false,
                ),
            ]),
            t(100),
        );
        pane.set_focused(true);
        pane
    }

    fn draw_at(pane: &MyPrsPane, now: SystemTime, width: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, 6)).unwrap();
        terminal
            .draw(|frame| pane.render(frame, frame.area(), now))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn draw(pane: &MyPrsPane, now: SystemTime) -> Buffer {
        draw_at(pane, now, 48)
    }

    /// Wide enough for the whole detail line, which is 54 columns for the
    /// fixture. A 48-column draw truncates it, and an assertion on the last
    /// signal would then fail for a reason that is not the thing under test.
    fn draw_wide(pane: &MyPrsPane, now: SystemTime) -> Buffer {
        draw_at(pane, now, 80)
    }

    #[test]
    fn the_header_names_the_pane_and_counts_the_queue() {
        let header = line(&draw(&loaded(), t(100)), 0);
        assert!(header.contains("My pull requests"), "got: {header:?}");
        assert!(header.contains('2'), "got: {header:?}");
    }

    #[test]
    fn a_row_carries_both_glyphs_the_reference_the_title_and_the_age() {
        let first = line(&draw(&loaded(), t(100 + 3 * 86_400)), 1);
        assert!(first.contains("zax#12"), "got: {first:?}");
        assert!(first.contains("Stamp"), "got: {first:?}");
        assert!(first.contains("3d"), "got: {first:?}");
        assert!(first.contains(crate::chrome::GLYPH_FAIL), "got: {first:?}");
        assert!(
            first.contains(crate::chrome::GLYPH_CHANGES),
            "got: {first:?}"
        );
    }

    #[test]
    fn the_attention_glyph_says_what_the_check_glyph_cannot() {
        assert_eq!(
            attention_glyph(Attention::Conflicting),
            crate::chrome::GLYPH_ERROR
        );
        assert_eq!(
            attention_glyph(Attention::ChangesRequested),
            crate::chrome::GLYPH_CHANGES
        );
        assert_eq!(
            attention_glyph(Attention::ReadyToMerge),
            crate::chrome::GLYPH_PASS
        );

        // A red build is already red in the check column beside it, and a
        // draft is marked by its own suffix.
        for quiet in [
            Attention::ChecksFailing,
            Attention::Waiting,
            Attention::Draft,
        ] {
            assert_eq!(attention_glyph(quiet), crate::chrome::GLYPH_NONE);
        }
    }

    #[test]
    fn an_unresolved_count_is_the_digit_and_a_plus_above_nine() {
        let n =
            |count: u32| attention_glyph(Attention::Unresolved(NonZeroU32::new(count).unwrap()));
        assert_eq!(n(1), "1");
        assert_eq!(n(9), "9");
        assert_eq!(n(10), "+");
        assert_eq!(n(400), "+");
    }

    #[test]
    fn every_attention_glyph_is_exactly_one_column() {
        // The row budget is built on it. A two-column glyph puts the row one
        // column past the pane at exactly the moment the pane matters.
        let all = [
            Attention::Conflicting,
            Attention::ChangesRequested,
            Attention::ChecksFailing,
            Attention::Unresolved(NonZeroU32::new(1).unwrap()),
            Attention::Unresolved(NonZeroU32::new(10).unwrap()),
            Attention::ReadyToMerge,
            Attention::Waiting,
            Attention::Draft,
        ];
        for attention in all {
            let glyph = attention_glyph(attention);
            assert_eq!(
                unicode_width::UnicodeWidthStr::width(glyph.as_str()),
                1,
                "{attention:?} rendered {glyph:?}"
            );
        }
    }

    #[test]
    fn the_detail_line_spells_out_all_four_signals_for_the_selection() {
        // Header, two rows, then the detail line.
        let detail = line(&draw_wide(&loaded(), t(100)), 3);
        assert!(detail.contains("failing"), "got: {detail:?}");
        assert!(detail.contains("changes requested"), "got: {detail:?}");
        assert!(detail.contains("1 unresolved"), "got: {detail:?}");
        assert!(detail.contains("mergeable"), "got: {detail:?}");
    }

    #[test]
    fn the_detail_line_follows_the_selection() {
        let mut pane = loaded();
        press(&mut pane, "j");
        let detail = line(&draw_wide(&pane, t(100)), 3);
        assert!(detail.contains("approved"), "got: {detail:?}");
        assert!(detail.contains("all resolved"), "got: {detail:?}");
    }

    #[test]
    fn the_window_reserves_the_header_and_the_detail_line() {
        // Hand-computed: a six-row pane spends one row on the header and one
        // on the detail line, so four rows of ten are visible.
        let mut pane = MyPrsPane::new(keymap());
        pane.apply(
            Ok((0..10)
                .map(|n| {
                    mine(
                        "acme/omega",
                        n,
                        CheckState::Passing,
                        ReviewState::ReviewRequired,
                        0,
                        Mergeable::Mergeable,
                        false,
                    )
                })
                .collect()),
            t(100),
        );

        assert_eq!(pane.viewport(10, Rect::new(0, 0, 48, 6)).rows, 0..4);
        for expected in [0..4, 0..4, 0..4, 1..5, 2..6] {
            press(&mut pane, "j");
            assert_eq!(pane.viewport(10, Rect::new(0, 0, 48, 6)).rows, expected);
        }

        // A pane with room for the chrome and nothing else shows no rows
        // rather than one it has no room to describe. The range is empty
        // wherever it starts, which is what is being asserted.
        for height in [0, 1, 2] {
            assert!(
                pane.viewport(10, Rect::new(0, 0, 48, height))
                    .rows
                    .is_empty(),
                "height {height} showed rows"
            );
        }
    }

    #[test]
    fn a_pane_with_no_room_for_rows_renders_without_panicking() {
        let pane = loaded();
        for (width, height) in [(48, 2), (48, 1), (6, 3), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| pane.render(frame, frame.area(), t(100)))
                .unwrap();
        }
    }

    #[test]
    fn an_empty_queue_says_so_rather_than_showing_nothing() {
        let mut pane = MyPrsPane::new(keymap());
        pane.apply(Ok(vec![]), t(100));
        assert!(
            line(&draw(&pane, t(100)), 1).contains("No open pull requests"),
            "got: {:?}",
            line(&draw(&pane, t(100)), 1)
        );
    }

    #[test]
    fn a_failure_keeps_the_previous_queue_and_marks_the_header_stale() {
        let mut pane = loaded();
        pane.apply(Err("gh: HTTP 502".to_owned()), t(160));
        let buffer = draw(&pane, t(160));
        assert!(
            line(&buffer, 0).contains("stale"),
            "got: {:?}",
            line(&buffer, 0)
        );
        assert!(
            line(&buffer, 1).contains("zax#12"),
            "the queue must not blank"
        );
    }

    #[test]
    fn an_unfocused_pane_does_not_claim_to_hold_the_cursor() {
        let mut pane = loaded();
        pane.set_focused(false);
        let buffer = draw(&pane, t(100));
        let reversed = (0..buffer.area.height).any(|y| {
            (0..buffer.area.width).any(|x| buffer[(x, y)].modifier.contains(Modifier::REVERSED))
        });
        assert!(!reversed, "nothing may be highlighted while unfocused");
    }

    #[test]
    fn the_row_keys_match_the_review_pane() {
        let mut pane = loaded();
        let url = "https://github.com/gameldar/zax/pull/12".to_owned();
        assert_eq!(press(&mut pane, "RET"), PaneAction::OpenUrl(url.clone()));
        assert_eq!(press(&mut pane, "SPC y"), PaneAction::CopyUrl(url));
        let repo = RepoSlug::parse("gameldar/zax").unwrap();
        assert_eq!(
            press(&mut pane, "SPC w o"),
            PaneAction::ReviewWorktree {
                repo: repo.clone(),
                number: 12,
                mode: ReviewMode::Open,
            }
        );
        assert_eq!(
            press(&mut pane, "SPC w r"),
            PaneAction::ReviewWorktree {
                repo,
                number: 12,
                mode: ReviewMode::OpenAndReview,
            }
        );
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
    fn row_actions_do_nothing_when_the_queue_is_empty() {
        let mut pane = MyPrsPane::new(keymap());
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
            Ok(vec![mine(
                "gameldar/zax",
                12,
                CheckState::Failing,
                ReviewState::ChangesRequested,
                1,
                Mergeable::Mergeable,
                false,
            )]),
            t(200),
        );
        assert_eq!(pane.selected().map(|m| m.status.pr.number), Some(12));
    }

    #[test]
    fn a_draft_is_flagged_on_its_row() {
        let mut pane = MyPrsPane::new(keymap());
        pane.apply(
            Ok(vec![mine(
                "gameldar/zax",
                14,
                CheckState::None,
                ReviewState::None,
                0,
                Mergeable::Unknown,
                true,
            )]),
            t(100),
        );
        assert!(line(&draw(&pane, t(100)), 1).contains("draft"));
    }

    #[test]
    fn a_repository_with_wide_characters_does_not_overflow_the_row() {
        // `RepoSlug` deliberately allows a non-ASCII name, so `repo#number`
        // can be far wider than its character count suggests. Measured on the
        // `Line` the pane BUILDS: ratatui stores a two-column glyph as a cell
        // plus a continuation cell holding a space, so reading a buffer back
        // reports `日本語` as eight columns rather than six, and a test doing
        // that fails whether the budget is right or wrong.
        let mut pane = MyPrsPane::new(keymap());
        pane.apply(
            Ok(vec![mine(
                "acme/日本語リポジトリ",
                1963,
                CheckState::Passing,
                ReviewState::ChangesRequested,
                0,
                Mergeable::Mergeable,
                false,
            )]),
            t(100),
        );
        let row = pane.rows().first().expect("one row").clone();

        // Measured through `unicode-width` DIRECTLY rather than through this
        // crate's `display_width`, which is part of what the row arithmetic
        // is made of. An oracle sharing the measurement agrees with the code
        // however wrong both become.
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

        // The floor, computed from the fixture and the row's documented shape
        // rather than by asking the row for it: two one-column glyphs sitting
        // adjacent, the three separators the row always carries, `#1963`,
        // which identifies the pull request and is never truncated, and the
        // three-column age of a pull request created at the fixture's `now`.
        let floor = w(crate::chrome::GLYPH_PASS)
            + w(crate::chrome::GLYPH_CHANGES)
            + 3
            + w("#1963")
            + w("now");
        assert_eq!(floor, 13, "the fixture's floor, spelled out");
        assert_eq!(
            columns(&pane.row(&row, false, 0, t(100))),
            floor,
            "a row squeezed to nothing is exactly its irreducible parts"
        );

        for width in 0..=60u16 {
            let built = pane.row(&row, false, width, t(100));
            assert!(
                columns(&built) <= usize::from(width).max(floor),
                "width {width} produced a {}-column row (the floor is {floor})",
                columns(&built)
            );
        }

        let smallest: String = pane
            .row(&row, false, 0, t(100))
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(smallest.contains("#1963"), "got: {smallest:?}");
    }

    #[test]
    fn the_detail_line_fits_a_narrow_pane() {
        let status = loaded().rows()[0].status.clone();
        for width in 0..=60u16 {
            let built = detail_line(&status, width);
            let columns = unicode_width::UnicodeWidthStr::width(
                built
                    .spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    .as_str(),
            );
            assert!(
                columns <= usize::from(width),
                "width {width} gave {columns}"
            );
        }
    }
}
