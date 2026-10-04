//! The four signals a pull request carries, as labels and colours.
//!
//! Three panes render these. They live here rather than in whichever pane
//! happened to need them first.

use ratatui::style::{Color, Style};
use zax_domain::{CheckState, Mergeable, ReviewState};

use crate::chrome::{GLYPH_FAIL, GLYPH_NONE, GLYPH_PASS, GLYPH_PENDING};

pub(crate) fn check_glyph(state: CheckState) -> &'static str {
    match state {
        CheckState::Passing => GLYPH_PASS,
        CheckState::Failing => GLYPH_FAIL,
        CheckState::Pending => GLYPH_PENDING,
        CheckState::None => GLYPH_NONE,
    }
}

pub(crate) fn check_style(state: CheckState) -> Style {
    let colour = match state {
        CheckState::Passing => Color::Green,
        CheckState::Failing => Color::Red,
        CheckState::Pending => Color::Yellow,
        CheckState::None => Color::DarkGray,
    };
    Style::default().fg(colour)
}

pub(crate) fn checks_label(state: CheckState) -> String {
    match state {
        CheckState::Passing => "passing",
        CheckState::Failing => "failing",
        CheckState::Pending => "pending",
        CheckState::None => "none reported",
    }
    .to_owned()
}

pub(crate) fn review_label(state: ReviewState) -> String {
    match state {
        ReviewState::Approved => "approved",
        ReviewState::ChangesRequested => "changes requested",
        ReviewState::ReviewRequired => "review required",
        ReviewState::None => "no review requested",
    }
    .to_owned()
}

pub(crate) fn review_style(state: ReviewState) -> Style {
    let colour = match state {
        ReviewState::Approved => Color::Green,
        ReviewState::ChangesRequested => Color::Red,
        ReviewState::ReviewRequired => Color::Yellow,
        ReviewState::None => Color::DarkGray,
    };
    Style::default().fg(colour)
}

pub(crate) fn threads_label(unresolved: u32) -> String {
    if unresolved == 0 {
        "all resolved".to_owned()
    } else {
        format!("{unresolved} unresolved")
    }
}

pub(crate) fn threads_style(unresolved: u32) -> Style {
    Style::default().fg(if unresolved == 0 {
        Color::Green
    } else {
        Color::Yellow
    })
}

pub(crate) fn mergeable_label(state: Mergeable) -> String {
    match state {
        Mergeable::Mergeable => "mergeable",
        Mergeable::Conflicting => "conflicting",
        Mergeable::Unknown => "unknown",
    }
    .to_owned()
}

pub(crate) fn mergeable_style(state: Mergeable) -> Style {
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
    use ratatui::style::Color;
    use zax_domain::{CheckState, Mergeable, ReviewState};

    use super::*;

    #[test]
    fn every_signal_has_a_label_and_a_colour() {
        assert_eq!(checks_label(CheckState::Failing), "failing");
        assert_eq!(check_glyph(CheckState::Failing), crate::chrome::GLYPH_FAIL);
        assert_eq!(check_style(CheckState::Passing).fg, Some(Color::Green));

        assert_eq!(
            review_label(ReviewState::ChangesRequested),
            "changes requested"
        );
        assert_eq!(review_style(ReviewState::Approved).fg, Some(Color::Green));

        assert_eq!(threads_label(0), "all resolved");
        assert_eq!(threads_label(3), "3 unresolved");
        assert_eq!(threads_style(0).fg, Some(Color::Green));
        assert_eq!(threads_style(1).fg, Some(Color::Yellow));

        assert_eq!(mergeable_label(Mergeable::Conflicting), "conflicting");
        assert_eq!(
            mergeable_style(Mergeable::Unknown).fg,
            Some(Color::DarkGray)
        );
    }
}
