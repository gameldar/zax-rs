//! Text shaping for panes that are only a few columns wide.

use std::time::Duration;

use unicode_width::UnicodeWidthStr;

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

/// Flatten anything that is not printable on a single line into spaces.
///
/// Two reasons, and the second is the one that bites. A pane header is one
/// line, while an error from `gh` or `kubectl` is frequently several, so a
/// newline in a header is a broken layout however it is measured. And
/// `unicode-width` scores a control character one column when it measures a
/// whole string but refuses to score it at all on its own, so a truncation loop
/// that walks characters and a fast path that measures the string disagree
/// about exactly those characters. Replacing them with spaces removes the
/// disagreement at its source rather than teaching both sides the same
/// workaround.
pub fn one_line(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
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
    // Before anything is measured, so the loop below and the check above cannot
    // disagree about a character neither of them should be rendering.
    let text = one_line(text);
    if display_width(&text) <= width {
        return text;
    }

    // One column goes to the ellipsis, which is itself one column wide.
    let budget = width.saturating_sub(1);

    // Grown one character at a time and measured WITH THE SAME FUNCTION that
    // decided the text did not fit. Summing per-character widths instead was
    // wrong twice over, for two unrelated character classes, because no
    // per-character total reproduces a string measurement: a control character
    // scores nothing alone and one column in a string, and a base character
    // followed by a variation selector scores one alone and two together, which
    // is how `❤️` overflowed. Both were the same defect - two functions
    // disagreeing - and measuring one way makes that disagreement impossible
    // rather than merely absent for the classes anyone has thought of.
    //
    // This measures the accumulated string once per character. The strings are
    // a pane row wide, so the cost is irrelevant and correctness is not.
    let mut out = String::new();
    for c in text.chars() {
        out.push(c);
        if display_width(&out) > budget {
            out.pop();
            break;
        }
    }

    // Cutting between a base character and its variation selector leaves the
    // base in its text presentation, one column narrower. That is a change of
    // glyph, not of width, and the alternative is grapheme segmentation and
    // another dependency to avoid trimming one emoji slightly differently.
    for _ in display_width(&out)..budget {
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
    fn an_emoji_with_a_variation_selector_cannot_overflow() {
        // `❤️` is U+2764 followed by U+FE0F. The base scores one column on its
        // own and the pair scores two, so any per-character total is wrong
        // about it. Hearts and thumbs-up in pull request titles are ordinary.
        let text = "AB\u{2764}\u{FE0F}CD";
        for width in 1..=display_width(text) {
            assert_eq!(
                display_width(&truncate(text, width)),
                width,
                "width {width} produced {:?}",
                truncate(text, width)
            );
        }
        // And a row of them, where the error used to compound.
        let hearts = "\u{2764}\u{FE0F}".repeat(10);
        for width in 1..=20 {
            assert_eq!(
                display_width(&truncate(&hearts, width)),
                width,
                "width {width}"
            );
        }
    }

    #[test]
    fn a_control_character_cannot_push_a_row_past_its_budget() {
        // `unicode-width` charges a control character one column inside a
        // string and refuses to score it alone, so measuring the whole string
        // and walking it character by character used to disagree. A status line
        // carries raw stderr from `gh` or `kubectl`, which is often several
        // lines, so this was an ordinary input rather than a hostile one.
        let messy = "title\nwith-control-\n-chars-embedded";
        for width in 1..=30 {
            assert_eq!(
                display_width(&truncate(messy, width)),
                width,
                "width {width} produced {:?}",
                truncate(messy, width)
            );
        }
    }

    #[test]
    fn control_characters_become_spaces_so_a_header_stays_one_line() {
        assert_eq!(one_line("a\nb\tc\rd"), "a b c d");
        assert_eq!(one_line("plain text"), "plain text");
        assert!(!truncate("a\nb", 10).contains('\n'));
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
