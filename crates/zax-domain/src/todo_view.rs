//! What the todo pane needs from the domain: reading a due date as typed,
//! reading the prompt, and the order the rows are drawn in.

use thiserror::Error;

use crate::date::{Date, Weekday};
use crate::todo::{Due, TodoItem};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DueError {
    #[error("`due:{0}` is not a date; try today, tom, fri, +3d or 2026-10-20")]
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PromptError {
    #[error(transparent)]
    Due(#[from] DueError),
    #[error("a todo takes one due: at most")]
    TwoDue,
    #[error("a todo needs some text")]
    Empty,
}

/// The one place a relative date is resolved: `today`, `tom`/`tomorrow`,
/// a weekday (the next one, never today), `+Nd`, `+Nw`, or `YYYY-MM-DD`.
pub fn parse_due_spec(spec: &str, today: Date) -> Result<Date, DueError> {
    let unknown = || DueError::Unknown(spec.to_owned());
    let lower = spec.to_ascii_lowercase();
    match lower.as_str() {
        "today" => return Ok(today),
        "tom" | "tomorrow" => return Ok(today.add_days(1)),
        _ => {}
    }
    if let Some(day) = Weekday::parse(&lower) {
        let ahead = (day.index() - today.weekday().index()).rem_euclid(7);
        return Ok(today.add_days(if ahead == 0 { 7 } else { ahead }));
    }
    if let Some(offset) = lower.strip_prefix('+') {
        let count = |digits: &str| {
            if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
                digits.parse::<u32>().ok().map(i64::from)
            } else {
                None
            }
        };
        if let Some(n) = offset.strip_suffix('d').and_then(count) {
            return Ok(today.add_days(n));
        }
        if let Some(n) = offset.strip_suffix('w').and_then(count) {
            return Ok(today.add_days(n * 7));
        }
        return Err(unknown());
    }
    Date::parse_iso(spec).map_err(|_| unknown())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptEntry {
    pub text: String,
    pub due: Option<Date>,
}

/// `call bank due:mon` into its text and resolved due date. Words are
/// joined by single spaces.
pub fn parse_prompt(input: &str, today: Date) -> Result<PromptEntry, PromptError> {
    let mut due = None;
    let mut words = Vec::new();
    for word in input.split_whitespace() {
        match word.strip_prefix("due:") {
            Some(spec) if !spec.starts_with(':') => {
                if due.is_some() {
                    return Err(PromptError::TwoDue);
                }
                due = Some(parse_due_spec(spec, today)?);
            }
            _ => words.push(word),
        }
    }
    if words.is_empty() {
        return Err(PromptError::Empty);
    }
    Ok(PromptEntry {
        text: words.join(" "),
        due,
    })
}

/// What `e` puts in the prompt. An unparsed due is written back as it was,
/// so submitting it unchanged reports it rather than quietly dropping it.
pub fn prompt_text(item: &TodoItem) -> String {
    match &item.due {
        None => item.text.clone(),
        Some(Due::Date(d)) => format!("{} due:{d}", item.text),
        Some(Due::Unparsed(v)) => format!("{} due:{v}", item.text),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DueState {
    /// Days late.
    Overdue(i64),
    Today,
    /// Days to go.
    Upcoming(i64),
    Unparsed,
    NoDue,
}

pub fn due_state(item: &TodoItem, today: Date) -> DueState {
    match &item.due {
        None => DueState::NoDue,
        Some(Due::Unparsed(_)) => DueState::Unparsed,
        Some(Due::Date(d)) => match today.days_until(*d) {
            n if n < 0 => DueState::Overdue(-n),
            0 => DueState::Today,
            n => DueState::Upcoming(n),
        },
    }
}

pub fn is_late(item: &TodoItem, today: Date) -> bool {
    !item.done && matches!(due_state(item, today), DueState::Overdue(_))
}

/// The rows in drawing order: overdue (oldest first), due today, upcoming
/// by date, undated and unparsed in file order, then done. A done item
/// completed before today, or with no completion date, is hidden unless
/// `show_all_done`.
pub fn todo_rows(items: &[TodoItem], today: Date, show_all_done: bool) -> Vec<TodoItem> {
    let mut rows: Vec<TodoItem> = items
        .iter()
        .filter(|i| !i.done || show_all_done || i.completion.is_some_and(|c| c >= today))
        .cloned()
        .collect();
    // Stable, so ties keep file order.
    rows.sort_by_key(|i| sort_key(i, today));
    rows
}

fn sort_key(item: &TodoItem, today: Date) -> (u8, Option<Date>) {
    if item.done {
        return (4, None);
    }
    match &item.due {
        Some(Due::Date(d)) if *d < today => (0, Some(*d)),
        Some(Due::Date(d)) if *d == today => (1, None),
        Some(Due::Date(d)) => (2, Some(*d)),
        _ => (3, None),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn d(raw: &str) -> Date {
        Date::parse_iso(raw).unwrap()
    }
    /// A Wednesday.
    fn today() -> Date {
        d("2026-10-07")
    }
    fn item(line: usize, raw: &str) -> TodoItem {
        TodoItem::parse(line, raw).unwrap()
    }

    #[test]
    fn named_days_resolve_against_today() {
        assert_eq!(parse_due_spec("today", today()), Ok(d("2026-10-07")));
        assert_eq!(parse_due_spec("TODAY", today()), Ok(d("2026-10-07")));
        assert_eq!(parse_due_spec("tom", today()), Ok(d("2026-10-08")));
        assert_eq!(parse_due_spec("tomorrow", today()), Ok(d("2026-10-08")));
    }

    #[test]
    fn a_weekday_is_the_next_one_and_never_today() {
        assert_eq!(parse_due_spec("fri", today()), Ok(d("2026-10-09")));
        assert_eq!(parse_due_spec("mon", today()), Ok(d("2026-10-12")));
        assert_eq!(parse_due_spec("wed", today()), Ok(d("2026-10-14")));
        assert_eq!(parse_due_spec("Thursday", today()), Ok(d("2026-10-08")));
    }

    #[test]
    fn a_weekday_crosses_month_and_year_ends() {
        // Friday 30 October, Wednesday 30 December.
        assert_eq!(parse_due_spec("mon", d("2026-10-30")), Ok(d("2026-11-02")));
        assert_eq!(parse_due_spec("fri", d("2026-12-30")), Ok(d("2027-01-01")));
    }

    #[test]
    fn offsets_count_days_or_weeks() {
        assert_eq!(parse_due_spec("+3d", today()), Ok(d("2026-10-10")));
        assert_eq!(parse_due_spec("+2w", today()), Ok(d("2026-10-21")));
        assert_eq!(parse_due_spec("+0d", today()), Ok(d("2026-10-07")));
        assert_eq!(parse_due_spec("+90d", d("2026-12-01")), Ok(d("2027-03-01")));
    }

    #[test]
    fn an_iso_date_is_taken_as_written() {
        assert_eq!(parse_due_spec("2026-12-25", today()), Ok(d("2026-12-25")));
    }

    #[test]
    fn anything_else_is_refused_with_examples() {
        for bad in [
            "",
            "someday",
            "+3",
            "+d",
            "+3m",
            "+-3d",
            "2026-02-30",
            "fr",
            "+３d",
        ] {
            let err = parse_due_spec(bad, today()).unwrap_err();
            assert!(err.to_string().contains("try today"), "{bad:?}: {err}");
        }
    }

    #[test]
    fn a_prompt_splits_the_due_token_from_the_text() {
        assert_eq!(
            parse_prompt("call bank due:mon", today()),
            Ok(PromptEntry {
                text: "call bank".into(),
                due: Some(d("2026-10-12"))
            })
        );
        assert_eq!(
            parse_prompt("  due:tom   call   bank ", today()),
            Ok(PromptEntry {
                text: "call bank".into(),
                due: Some(d("2026-10-08"))
            })
        );
        assert_eq!(
            parse_prompt("just text", today()),
            Ok(PromptEntry {
                text: "just text".into(),
                due: None
            })
        );
    }

    #[test]
    fn a_prompt_keeps_fields_and_words_that_only_look_like_due() {
        assert_eq!(
            parse_prompt("a [priority:: high] [due:: x] overdue:no", today())
                .unwrap()
                .text,
            "a [priority:: high] [due:: x] overdue:no"
        );
    }

    #[test]
    fn a_bad_prompt_says_what_is_wrong() {
        assert_eq!(parse_prompt("   ", today()), Err(PromptError::Empty));
        assert_eq!(parse_prompt("due:fri", today()), Err(PromptError::Empty));
        assert_eq!(
            parse_prompt("a due:fri due:mon", today()),
            Err(PromptError::TwoDue)
        );
        assert!(matches!(
            parse_prompt("a due:someday", today()),
            Err(PromptError::Due(_))
        ));
        assert!(matches!(
            parse_prompt("a due:", today()),
            Err(PromptError::Due(_))
        ));
    }

    #[test]
    fn the_prompt_prefill_round_trips_through_the_prompt() {
        let it = item(0, "- [ ] renew [x:: 1] [due:: 2026-10-10]");
        assert_eq!(prompt_text(&it), "renew [x:: 1] due:2026-10-10");
        assert_eq!(
            parse_prompt(&prompt_text(&it), today()),
            Ok(PromptEntry {
                text: "renew [x:: 1]".into(),
                due: Some(d("2026-10-10"))
            })
        );
        assert_eq!(prompt_text(&item(0, "- [ ] a")), "a");
    }

    #[test]
    fn an_unparsed_due_is_prefilled_so_submitting_it_unchanged_is_refused() {
        let it = item(0, "- [ ] a [due:: someday]");
        assert_eq!(prompt_text(&it), "a due:someday");
        assert!(parse_prompt(&prompt_text(&it), today()).is_err());
    }

    #[test]
    fn due_state_measures_from_today() {
        assert_eq!(
            due_state(&item(0, "- [ ] a [due:: 2026-10-05]"), today()),
            DueState::Overdue(2)
        );
        assert_eq!(
            due_state(&item(0, "- [ ] a [due:: 2026-10-07]"), today()),
            DueState::Today
        );
        assert_eq!(
            due_state(&item(0, "- [ ] a [due:: 2026-10-10]"), today()),
            DueState::Upcoming(3)
        );
        assert_eq!(
            due_state(&item(0, "- [ ] a [due:: x]"), today()),
            DueState::Unparsed
        );
        assert_eq!(due_state(&item(0, "- [ ] a"), today()), DueState::NoDue);
        assert!(is_late(&item(0, "- [ ] a [due:: 2026-10-05]"), today()));
        assert!(!is_late(&item(0, "- [x] a [due:: 2026-10-05]"), today()));
    }

    #[test]
    fn rows_run_overdue_today_upcoming_undated_then_done() {
        let items = vec![
            item(0, "- [ ] undated one"),
            item(1, "- [x] done today [completion:: 2026-10-07]"),
            item(2, "- [ ] later [due:: 2026-10-20]"),
            item(3, "- [ ] today [due:: 2026-10-07]"),
            item(4, "- [ ] late recent [due:: 2026-10-06]"),
            item(5, "- [ ] bad [due:: someday]"),
            item(6, "- [ ] soon [due:: 2026-10-09]"),
            item(7, "- [ ] late oldest [due:: 2026-10-01]"),
            item(8, "- [ ] undated two"),
        ];
        let order: Vec<String> = todo_rows(&items, today(), false)
            .into_iter()
            .map(|i| i.text)
            .collect();
        assert_eq!(
            order,
            [
                "late oldest",
                "late recent",
                "today",
                "soon",
                "later",
                "undated one",
                "bad",
                "undated two",
                "done today",
            ]
        );
    }

    #[test]
    fn done_items_from_before_today_hide_unless_asked_for() {
        let items = vec![
            item(0, "- [x] yesterday [completion:: 2026-10-06]"),
            item(1, "- [x] by hand"),
            item(2, "- [x] today [completion:: 2026-10-07]"),
            item(3, "- [ ] open"),
        ];
        let shown: Vec<String> = todo_rows(&items, today(), false)
            .into_iter()
            .map(|i| i.text)
            .collect();
        assert_eq!(shown, ["open", "today"]);
        let all: Vec<String> = todo_rows(&items, today(), true)
            .into_iter()
            .map(|i| i.text)
            .collect();
        assert_eq!(all, ["open", "yesterday", "by hand", "today"]);
    }
}
