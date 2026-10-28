//! The selection and scroll window every list pane shares: line and page
//! movement, a click that picks a row, and the gutter that says more rows lie
//! above or below.

use std::cell::Cell;
use std::ops::Range;

use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};
use ratatui::Frame;

pub const GLYPH_MORE_ABOVE: &str = "\u{25b2}";
pub const GLYPH_MORE_BELOW: &str = "\u{25bc}";

/// A selection over rows that may outnumber the pane's lines.
///
/// The window is stored rather than derived from the selection, so moving up
/// does not drag the window with it and a click does not shift the row under
/// the pointer. Drawing is what knows the pane's height, so `layout` records
/// it in cells for page movement and clicks to read afterwards.
///
/// Keeping the selection on screen is not cosmetic. `RET`, `SPC y` and `SPC w`
/// all act on it, and `SPC w` builds a worktree, so a cursor the user cannot
/// see is a keystroke acting on a row they did not pick. `layout` scrolls to
/// the selection at every draw, so no movement can leave it off screen.
#[derive(Debug, Default)]
pub struct ListScroll {
    selected: usize,
    offset: Cell<usize>,
    /// How many rows the last draw had room for; a page.
    page: Cell<usize>,
    /// Where the last draw put the rows, gutter included.
    drawn: Cell<Rect>,
    /// Whether the last draw left rows out above and below.
    more: Cell<(bool, bool)>,
}

/// The rows a draw shows and the columns each row may fill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Viewport {
    pub rows: Range<usize>,
    /// The pane's width, less the gutter column when the rows overflow.
    pub width: u16,
}

impl ListScroll {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// Put the cursor on `index`, pulled back into `0..len`.
    pub fn select(&mut self, index: usize, len: usize) {
        self.selected = index.min(len.saturating_sub(1));
    }

    /// Pull the selection back into range after the rows changed under it.
    pub fn clamp(&mut self, len: usize) {
        self.select(self.selected, len);
    }

    pub fn down(&mut self, len: usize) {
        self.select(self.selected + 1, len);
    }

    pub fn up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// Move a page down. The window moves with the cursor, so the selection
    /// keeps its line on screen and a page of new rows comes into view.
    pub fn page_down(&mut self, len: usize) {
        let page = self.page();
        self.offset.set(self.offset.get() + page);
        self.select(self.selected + page, len);
    }

    pub fn page_up(&mut self) {
        let page = self.page();
        self.offset.set(self.offset.get().saturating_sub(page));
        self.selected = self.selected.saturating_sub(page);
    }

    /// A pane's height before the first draw is unknown; a page is one row
    /// until then, so the key still does something sensible.
    fn page(&self) -> usize {
        self.page.get().max(1)
    }

    /// Lay out `len` rows in `visible` lines starting at `(x, y)`, scrolling
    /// only as far as the selection needs. Records the result for page
    /// movement, clicks and `render_gutter`.
    pub fn layout(&self, len: usize, visible: usize, x: u16, y: u16, width: u16) -> Viewport {
        let mut first = self.offset.get();
        if self.selected < first {
            first = self.selected;
        } else if visible > 0 && self.selected >= first + visible {
            first = self.selected + 1 - visible;
        }
        // A list that shrank, or a page past the end, must not leave blank
        // lines under the last row while rows above are hidden.
        first = first.min(len.saturating_sub(visible));
        self.offset.set(first);
        self.page.set(visible);

        let rows = first..(first + visible).min(len);
        let more = (first > 0, rows.end < len);
        self.more.set(more);
        let overflow = more.0 || more.1;
        self.drawn.set(Rect {
            x,
            y,
            width,
            height: u16::try_from(rows.len()).unwrap_or(u16::MAX),
        });
        Viewport {
            rows,
            width: if overflow && width > 1 {
                width - 1
            } else {
                width
            },
        }
    }

    /// Draw the last layout's gutter: an arrow at the top when rows are
    /// hidden above, and at the bottom when rows are hidden below.
    pub fn render_gutter(&self, frame: &mut Frame) {
        let (above, below) = self.more.get();
        render_gutter(frame, self.drawn.get(), above, below);
    }

    /// Act on a mouse event: a left click selects the row under it, the wheel
    /// moves the selection a row. Returns whether the selection changed hands.
    pub fn on_mouse(&mut self, mouse: MouseEvent, len: usize) -> bool {
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => match self.row_at(mouse.column, mouse.row) {
                Some(index) if index < len => {
                    self.selected = index;
                    true
                }
                _ => false,
            },
            MouseEventKind::ScrollDown => {
                self.down(len);
                true
            }
            MouseEventKind::ScrollUp => {
                self.up();
                true
            }
            _ => false,
        }
    }

    /// The row index drawn at a screen cell in the last draw.
    fn row_at(&self, column: u16, row: u16) -> Option<usize> {
        let drawn = self.drawn.get();
        let inside = column >= drawn.x
            && column < drawn.x.saturating_add(drawn.width)
            && row >= drawn.y
            && row < drawn.y.saturating_add(drawn.height);
        inside.then(|| self.offset.get() + usize::from(row - drawn.y))
    }
}

/// The gutter is the last column of `rows`. Panes that scroll something other
/// than a selection (exec's output) call this directly.
pub fn render_gutter(frame: &mut Frame, rows: Rect, above: bool, below: bool) {
    if rows.width == 0 || rows.height == 0 {
        return;
    }
    let x = rows.x + rows.width - 1;
    let style = Style::default().fg(Color::DarkGray);
    let buffer = frame.buffer_mut();
    if above {
        buffer.set_string(x, rows.y, GLYPH_MORE_ABOVE, style);
    }
    if below {
        buffer.set_string(x, rows.y + rows.height - 1, GLYPH_MORE_BELOW, style);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyModifiers;
    use ratatui::Terminal;

    use super::*;

    fn rows(scroll: &ListScroll, len: usize, visible: usize) -> Range<usize> {
        scroll.layout(len, visible, 0, 1, 20).rows
    }

    fn click(column: u16, row: u16) -> MouseEvent {
        MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column,
            row,
            modifiers: KeyModifiers::NONE,
        }
    }

    fn wheel(kind: MouseEventKind) -> MouseEvent {
        MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        }
    }

    #[test]
    fn the_window_scrolls_only_as_far_as_the_selection_needs() {
        let mut scroll = ListScroll::new();
        assert_eq!(rows(&scroll, 10, 4), 0..4);
        for _ in 0..5 {
            scroll.down(10);
        }
        assert_eq!(rows(&scroll, 10, 4), 2..6);
    }

    #[test]
    fn moving_up_inside_the_window_leaves_it_where_it_is() {
        // Deriving the window from the selection dragged it up a row with
        // every `k`, so the cursor stuck to the bottom line.
        let mut scroll = ListScroll::new();
        scroll.select(5, 10);
        assert_eq!(rows(&scroll, 10, 4), 2..6);
        scroll.up();
        assert_eq!(rows(&scroll, 10, 4), 2..6);
        scroll.select(1, 10);
        assert_eq!(rows(&scroll, 10, 4), 1..5);
    }

    #[test]
    fn a_page_moves_the_cursor_and_the_window_by_the_drawn_height() {
        let mut scroll = ListScroll::new();
        rows(&scroll, 20, 5);
        scroll.page_down(20);
        assert_eq!(scroll.selected(), 5);
        assert_eq!(rows(&scroll, 20, 5), 5..10);
        scroll.page_down(20);
        scroll.page_down(20);
        scroll.page_down(20);
        assert_eq!(scroll.selected(), 19, "the last page stops on the last row");
        assert_eq!(rows(&scroll, 20, 5), 15..20, "no blank lines past the end");
        scroll.page_up();
        assert_eq!(scroll.selected(), 14);
        assert_eq!(rows(&scroll, 20, 5), 10..15);
        for _ in 0..5 {
            scroll.page_up();
        }
        assert_eq!(scroll.selected(), 0);
        assert_eq!(rows(&scroll, 20, 5), 0..5);
    }

    #[test]
    fn a_page_before_the_first_draw_is_one_row() {
        let mut scroll = ListScroll::new();
        scroll.page_down(10);
        assert_eq!(scroll.selected(), 1);
    }

    #[test]
    fn a_shrunken_list_pulls_the_window_back_so_no_rows_hide_above_blanks() {
        let mut scroll = ListScroll::new();
        scroll.select(9, 10);
        assert_eq!(rows(&scroll, 10, 4), 6..10);
        scroll.clamp(5);
        assert_eq!(rows(&scroll, 5, 4), 1..5);
    }

    #[test]
    fn overflowing_rows_give_up_a_column_to_the_gutter() {
        let scroll = ListScroll::new();
        assert_eq!(scroll.layout(3, 4, 0, 1, 20).width, 20);
        assert_eq!(scroll.layout(10, 4, 0, 1, 20).width, 19);
    }

    #[test]
    fn a_click_selects_the_row_under_it_counting_from_the_window() {
        let mut scroll = ListScroll::new();
        scroll.select(7, 10);
        // Rows start on line 1, under a header; window 4..8.
        assert_eq!(rows(&scroll, 10, 4), 4..8);
        assert!(scroll.on_mouse(click(3, 2), 10));
        assert_eq!(scroll.selected(), 5);
        assert_eq!(rows(&scroll, 10, 4), 4..8, "a click does not scroll");
    }

    #[test]
    fn a_click_off_the_rows_changes_nothing() {
        let mut scroll = ListScroll::new();
        rows(&scroll, 2, 4);
        for (column, row) in [(0, 0), (0, 3), (25, 1)] {
            assert!(!scroll.on_mouse(click(column, row), 2), "({column}, {row})");
        }
        assert_eq!(scroll.selected(), 0);
    }

    #[test]
    fn the_wheel_moves_the_selection_a_row() {
        let mut scroll = ListScroll::new();
        scroll.on_mouse(wheel(MouseEventKind::ScrollDown), 3);
        scroll.on_mouse(wheel(MouseEventKind::ScrollDown), 3);
        scroll.on_mouse(wheel(MouseEventKind::ScrollDown), 3);
        assert_eq!(scroll.selected(), 2);
        scroll.on_mouse(wheel(MouseEventKind::ScrollUp), 3);
        assert_eq!(scroll.selected(), 1);
    }

    fn gutter(scroll: &ListScroll) -> (String, String) {
        let mut terminal = Terminal::new(TestBackend::new(20, 6)).unwrap();
        terminal.draw(|frame| scroll.render_gutter(frame)).unwrap();
        let buffer = terminal.backend().buffer().clone();
        (
            buffer[(19, 1)].symbol().to_owned(),
            buffer[(19, 4)].symbol().to_owned(),
        )
    }

    #[test]
    fn the_gutter_marks_rows_hidden_above_and_below() {
        let mut scroll = ListScroll::new();
        rows(&scroll, 10, 4);
        assert_eq!(gutter(&scroll), (" ".into(), GLYPH_MORE_BELOW.into()));
        scroll.select(5, 10);
        rows(&scroll, 10, 4);
        assert_eq!(
            gutter(&scroll),
            (GLYPH_MORE_ABOVE.into(), GLYPH_MORE_BELOW.into())
        );
        scroll.select(9, 10);
        rows(&scroll, 10, 4);
        assert_eq!(gutter(&scroll), (GLYPH_MORE_ABOVE.into(), " ".into()));
    }

    #[test]
    fn a_list_that_fits_draws_no_gutter() {
        let scroll = ListScroll::new();
        rows(&scroll, 4, 4);
        assert_eq!(gutter(&scroll), (" ".into(), " ".into()));
    }
}
