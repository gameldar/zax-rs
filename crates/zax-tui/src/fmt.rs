//! Text shaping for panes that are only a few columns wide.

use std::time::Duration;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const MINUTE: u64 = 60;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;
const WEEK: u64 = 7 * DAY;

/// A compact age: `now`, `5m`, `2h`, `3d`, `6w`.
pub fn format_age(age: Duration) -> String {
    let secs = age.as_secs();
    if secs < MINUTE {
        "now".to_owned()
    } else if secs < HOUR {
        format!("{}m", secs / MINUTE)
    } else if secs < DAY {
        format!("{}h", secs / HOUR)
    } else if secs < WEEK {
        format!("{}d", secs / DAY)
    } else {
        format!("{}w", secs / WEEK)
    }
}

/// How many terminal columns this text occupies.
///
/// Not the same as its character count, which is the distinction that matters
/// here. A CJK ideograph and most emoji occupy two columns, so budgeting by
/// characters lets a title overflow its pane by up to its own length again.
/// Emoji in a pull request title is common enough that this is the ordinary
/// case rather than an exotic one.
pub fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Truncate to `width` terminal COLUMNS, ellipsis included, never splitting a
/// character.
///
/// The result is exactly `width` columns when truncation happens. A double
/// width character can leave a single column unfillable, in which case a space
/// takes it, so a row cannot shift by one depending on its content.
pub fn truncate(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if display_width(text) <= width {
        return text.to_owned();
    }

    // One column goes to the ellipsis, which is itself one column wide.
    let budget = width.saturating_sub(1);
    let mut out = String::new();
    let mut used = 0;
    for c in text.chars() {
        let advance = UnicodeWidthChar::width(c).unwrap_or(0);
        if used + advance > budget {
            break;
        }
        out.push(c);
        used += advance;
    }
    // A two-column character that did not fit can leave one column spare.
    for _ in used..budget {
        out.push(' ');
    }
    out.push('\u{2026}');
    out
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_very_recent_age_reads_as_now() {
        assert_eq!(format_age(Duration::from_secs(0)), "now");
        assert_eq!(format_age(Duration::from_secs(59)), "now");
    }

    #[test]
    fn ages_step_up_through_minutes_hours_days_and_weeks() {
        assert_eq!(format_age(Duration::from_secs(60)), "1m");
        assert_eq!(format_age(Duration::from_secs(3_599)), "59m");
        assert_eq!(format_age(Duration::from_secs(3_600)), "1h");
        assert_eq!(format_age(Duration::from_secs(86_399)), "23h");
        assert_eq!(format_age(Duration::from_secs(86_400)), "1d");
        assert_eq!(format_age(Duration::from_secs(604_799)), "6d");
        assert_eq!(format_age(Duration::from_secs(604_800)), "1w");
        assert_eq!(format_age(Duration::from_secs(52 * 604_800)), "52w");
    }

    #[test]
    fn truncation_leaves_short_text_alone() {
        assert_eq!(truncate("short", 10), "short");
        assert_eq!(truncate("exactly-10", 10), "exactly-10");
    }

    #[test]
    fn truncation_adds_an_ellipsis_within_the_budget() {
        assert_eq!(truncate("abcdefghij", 5), "abcd\u{2026}");
        assert_eq!(display_width(&truncate("abcdefghij", 5)), 5);
    }

    #[test]
    fn a_wide_character_costs_two_columns_not_one() {
        // The distinction the whole budget rests on. A pane asked for 24
        // columns that hands back 24 CHARACTERS of CJK draws 48 columns wide.
        assert_eq!(display_width("abc"), 3);
        assert_eq!(display_width("日本語"), 6);
        assert_eq!(display_width("\u{1f600}"), 2);
        assert_eq!("日本語".chars().count(), 3, "three characters, six columns");
    }

    #[test]
    fn truncation_of_wide_text_fits_the_column_budget_exactly() {
        // `日本語のタイトル` is 8 characters and 16 columns.
        let long = "日本語のタイトル";
        for width in 1..=16 {
            let out = truncate(long, width);
            assert_eq!(display_width(&out), width, "width {width} produced {out:?}");
        }
        // A two-column character that will not fit leaves one column, which a
        // space takes so the row cannot shift by one.
        assert_eq!(truncate(long, 4), "日 \u{2026}");
    }

    #[test]
    fn truncation_never_splits_a_multi_byte_character() {
        // A naive byte slice here would panic. These are all single-column
        // characters, so columns and characters agree.
        let text = "naïve café résumé";
        let out = truncate(text, 6);
        assert_eq!(display_width(&out), 6);
        assert!(text.starts_with(out.trim_end_matches('\u{2026}')));
    }

    #[test]
    fn truncation_to_a_useless_width_degrades_rather_than_panicking() {
        assert_eq!(truncate("abcdef", 1), "\u{2026}");
        assert_eq!(truncate("abcdef", 0), "");
    }
}
