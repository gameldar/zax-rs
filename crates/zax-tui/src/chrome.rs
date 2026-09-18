//! The shared look: a title on the left, a freshness status on the right.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::fmt::{display_width, truncate};

pub const GLYPH_BAR: &str = "\u{258e}";
pub const GLYPH_ERROR: &str = "\u{26a0}";
pub const GLYPH_PASS: &str = "\u{2713}";
pub const GLYPH_FAIL: &str = "\u{2717}";
pub const GLYPH_PENDING: &str = "\u{25cf}";
pub const GLYPH_NONE: &str = "\u{00b7}";

/// A single header line exactly `width` characters wide.
pub fn header_line(title: &str, status: &str, focused: bool, width: u16) -> Line<'static> {
    let width = usize::from(width);
    if width == 0 {
        return Line::from(Vec::<Span<'static>>::new());
    }

    let title_style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };

    // `bar + space` costs two columns; anything left over is shared between the
    // title and the status, with the status taking priority because a stale
    // marker matters more than the rest of a title.
    let prefix = format!("{GLYPH_BAR} ");
    let prefix_width = display_width(&prefix).min(width);
    let remaining = width.saturating_sub(prefix_width);

    // Every measurement here is in terminal columns, not characters. Mixing the
    // two is what lets a pane overflow on a title containing emoji or CJK.
    let status_text = truncate(status, remaining);
    let status_width = display_width(&status_text);
    let title_budget = remaining.saturating_sub(status_width + 1);
    let title_text = truncate(title, title_budget);
    let gap = remaining
        .saturating_sub(display_width(&title_text))
        .saturating_sub(status_width);

    Line::from(vec![
        Span::styled(truncate(&prefix, prefix_width), title_style),
        Span::styled(title_text, title_style),
        Span::raw(" ".repeat(gap)),
        Span::styled(status_text, status_style(status)),
    ])
}

fn status_style(status: &str) -> Style {
    if status.starts_with("stale") {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// What a pane shows instead of data when it has none and cannot get any.
pub fn error_line(message: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(GLYPH_ERROR, Style::default().fg(Color::Yellow)),
        Span::raw(" "),
        Span::styled(message.to_owned(), Style::default().fg(Color::Yellow)),
    ])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Terminal columns, which is what a header's width budget is measured in.
    fn columns(line: &Line<'_>) -> usize {
        crate::fmt::display_width(&text(line))
    }

    #[test]
    fn a_header_shows_the_title_on_the_left_and_the_status_on_the_right() {
        let line = header_line("Deploys", "12s", false, 24);
        let rendered = text(&line);
        assert!(
            rendered.starts_with("\u{258e} Deploys"),
            "got: {rendered:?}"
        );
        assert!(rendered.ends_with("12s"), "got: {rendered:?}");
        assert_eq!(columns(&line), 24);
    }

    #[test]
    fn a_narrow_header_truncates_the_title_rather_than_overflowing() {
        let line = header_line("A very long pane title indeed", "3m", false, 16);
        assert_eq!(columns(&line), 16);
    }

    #[test]
    fn a_header_with_wide_characters_still_fits_its_columns() {
        // Counting characters here would draw a header up to twice as wide as
        // the pane, which is how this reaches a user: a title with emoji.
        for width in 0..=40_u16 {
            assert_eq!(
                columns(&header_line(
                    "\u{1f600} 日本語のタイトル",
                    "3m",
                    false,
                    width
                )),
                width as usize,
                "width {width}"
            );
        }
    }

    #[test]
    fn a_header_too_narrow_for_anything_still_produces_a_line() {
        for width in [0_u16, 1, 2, 3] {
            let line = header_line("Deploys", "12s", false, width);
            assert_eq!(columns(&line), width as usize);
        }
    }

    #[test]
    fn a_focused_header_is_styled_differently_from_an_unfocused_one() {
        let focused = header_line("Deploys", "12s", true, 24);
        let unfocused = header_line("Deploys", "12s", false, 24);
        assert_ne!(focused.spans[0].style, unfocused.spans[0].style);
    }

    #[test]
    fn an_error_line_is_actionable_rather_than_a_bare_message() {
        let line = error_line("gh is not installed or not on PATH");
        let rendered = text(&line);
        assert!(
            rendered.contains("gh is not installed"),
            "got: {rendered:?}"
        );
        assert!(rendered.starts_with(GLYPH_ERROR), "got: {rendered:?}");
    }
}
