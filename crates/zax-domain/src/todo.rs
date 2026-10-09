//! The todo list: a markdown file of GitHub task-list lines carrying
//! Obsidian Tasks Dataview fields (`[due:: 2026-10-10]`).
//!
//! The document is kept as its lines, each with its own line ending, so a
//! file renders back byte for byte and an edit rewrites only the line it is
//! about. Headings, notes, blank lines and fields zax does not know survive
//! every edit.

use std::ops::Range;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::date::Date;

/// The two fields zax reads and writes. Any other field is the user's.
pub const DUE: &str = "due";
pub const COMPLETION: &str = "completion";

/// Where the todo list lives. Absolute, because the broker that reads it
/// runs from the home directory, not from the pane that named it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoFile(PathBuf);

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TodoFileError {
    #[error("the todo file `{0}` must be an absolute path")]
    NotAbsolute(String),
}

impl TodoFile {
    pub fn new(path: PathBuf) -> Result<Self, TodoFileError> {
        if path.is_absolute() {
            Ok(Self(path))
        } else {
            Err(TodoFileError::NotAbsolute(path.display().to_string()))
        }
    }

    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

/// A todo's due date. A value that is not a date is its own variant, so it
/// can never be mistaken for a todo with no due date.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Due {
    Date(Date),
    Unparsed(String),
}

/// Which line an edit is about: its index and its text as last read. An
/// edit whose line no longer reads this way is refused, never redirected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineRef {
    pub line: usize,
    pub raw: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoItem {
    /// 0-based index of the line in the file.
    pub line: usize,
    /// The line as read, without its line ending.
    pub raw: String,
    pub done: bool,
    /// The description without the `due` field and a parsed `completion`
    /// field. Every other field stays in it.
    pub text: String,
    pub due: Option<Due>,
    pub completion: Option<Date>,
}

impl TodoItem {
    /// `None` for a line that is not a task.
    pub fn parse(line: usize, raw: &str) -> Option<Self> {
        let task = split_task(raw)?;
        let due = find_field(task.body, DUE).map(|(_, value)| match Date::parse_iso(value) {
            Ok(date) => Due::Date(date),
            Err(_) => Due::Unparsed(value.to_owned()),
        });
        let completion =
            find_field(task.body, COMPLETION).and_then(|(_, v)| Date::parse_iso(v).ok());
        let mut text = without_field(task.body, DUE);
        if completion.is_some() {
            text = without_field(&text, COMPLETION);
        }
        Some(Self {
            line,
            raw: raw.to_owned(),
            done: task.done,
            text: text.trim().to_owned(),
            due,
            completion,
        })
    }

    pub fn at(&self) -> LineRef {
        LineRef {
            line: self.line,
            raw: self.raw.clone(),
        }
    }
}

/// A todo file as the broker sends it to a pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodoList {
    pub file: String,
    pub items: Vec<TodoItem>,
}

/// One change to the todo file, as a pane asks for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TodoEdit {
    Add {
        text: String,
        due: Option<Date>,
    },
    Toggle {
        at: LineRef,
        today: Date,
    },
    Set {
        at: LineRef,
        text: String,
        due: Option<Date>,
    },
    Delete {
        at: LineRef,
    },
    Snooze {
        at: LineRef,
        today: Date,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TodoError {
    #[error("the todo file changed - refreshed")]
    Stale,
    #[error("a todo needs some text")]
    EmptyText,
    #[error("a todo is a single line")]
    Multiline,
}

fn checked_text(text: &str) -> Result<String, TodoError> {
    let text = text.trim();
    if text.is_empty() {
        Err(TodoError::EmptyText)
    } else if text.contains(['\n', '\r']) {
        Err(TodoError::Multiline)
    } else {
        Ok(text.to_owned())
    }
}

/// A task line split into what an edit keeps and what it changes.
pub(crate) struct TaskLine<'a> {
    /// The indent and the list marker with its space: `"  - "`.
    pub(crate) prefix: &'a str,
    pub(crate) done: bool,
    /// Everything after `[ ] `.
    pub(crate) body: &'a str,
}

pub(crate) fn split_task(raw: &str) -> Option<TaskLine<'_>> {
    let indent = raw.len() - raw.trim_start_matches([' ', '\t']).len();
    let rest = raw.get(indent..)?;
    let after_marker = rest
        .strip_prefix("- ")
        .or_else(|| rest.strip_prefix("* "))?;
    let (done, after_box) = if let Some(r) = after_marker.strip_prefix("[ ]") {
        (false, r)
    } else {
        let r = after_marker
            .strip_prefix("[x]")
            .or_else(|| after_marker.strip_prefix("[X]"))?;
        (true, r)
    };
    let body = if after_box.is_empty() {
        ""
    } else {
        after_box.strip_prefix(' ')?
    };
    Some(TaskLine {
        prefix: raw.get(..indent + 2)?,
        done,
        body,
    })
}

pub(crate) fn render_task(prefix: &str, done: bool, body: &str) -> String {
    let mark = if done { 'x' } else { ' ' };
    if body.is_empty() {
        format!("{prefix}[{mark}]")
    } else {
        format!("{prefix}[{mark}] {body}")
    }
}

/// The byte range of the first `[key:: value]` in `body`, and its value,
/// trimmed.
pub(crate) fn find_field<'a>(body: &'a str, key: &str) -> Option<(Range<usize>, &'a str)> {
    let open = format!("[{key}::");
    let start = body.find(&open)?;
    let value_start = start + open.len();
    let close = value_start + body.get(value_start..)?.find(']')?;
    Some((start..close + 1, body.get(value_start..close)?.trim()))
}

/// `body` without its first `key` field, and without the space that
/// separated it from its neighbours.
pub(crate) fn without_field(body: &str, key: &str) -> String {
    let Some((range, _)) = find_field(body, key) else {
        return body.to_owned();
    };
    let before = body
        .get(..range.start)
        .unwrap_or_default()
        .trim_end_matches(' ');
    let after = body
        .get(range.end..)
        .unwrap_or_default()
        .trim_start_matches(' ');
    match (before.is_empty(), after.is_empty()) {
        (true, _) => after.to_owned(),
        (_, true) => before.to_owned(),
        _ => format!("{before} {after}"),
    }
}

/// `body` with its `key` field set to `value`: replaced where it is, or
/// appended when there is none.
pub(crate) fn with_field(body: &str, key: &str, value: &str) -> String {
    let field = format!("[{key}:: {value}]");
    match find_field(body, key) {
        Some((range, _)) => format!(
            "{}{field}{}",
            body.get(..range.start).unwrap_or_default(),
            body.get(range.end..).unwrap_or_default()
        ),
        None if body.trim().is_empty() => field,
        None => format!("{} {field}", body.trim_end_matches(' ')),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) content: String,
    /// `"\n"`, `"\r\n"`, or `""` for a last line with no newline.
    pub(crate) ending: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TodoDocument {
    pub(crate) lines: Vec<Line>,
}

impl TodoDocument {
    pub fn parse(text: &str) -> Self {
        let lines = text
            .split_inclusive('\n')
            .map(|piece| {
                if let Some(content) = piece.strip_suffix("\r\n") {
                    Line {
                        content: content.to_owned(),
                        ending: "\r\n",
                    }
                } else if let Some(content) = piece.strip_suffix('\n') {
                    Line {
                        content: content.to_owned(),
                        ending: "\n",
                    }
                } else {
                    Line {
                        content: piece.to_owned(),
                        ending: "",
                    }
                }
            })
            .collect();
        Self { lines }
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(&line.content);
            out.push_str(line.ending);
        }
        out
    }

    pub fn items(&self) -> Vec<TodoItem> {
        self.lines
            .iter()
            .enumerate()
            .filter_map(|(i, line)| TodoItem::parse(i, &line.content))
            .collect()
    }

    pub fn apply(&mut self, edit: &TodoEdit) -> Result<(), TodoError> {
        match edit {
            TodoEdit::Add { text, due } => {
                let text = checked_text(text)?;
                let body = match due {
                    Some(d) => with_field(&text, DUE, &d.to_string()),
                    None => text,
                };
                let at = self
                    .lines
                    .iter()
                    .rposition(|l| split_task(&l.content).is_some())
                    .map_or(self.lines.len(), |i| i + 1);
                self.insert_line(at, render_task("- ", false, &body));
                Ok(())
            }
            TodoEdit::Toggle { at, today } => self.rewrite(at, |task| {
                let body = without_field(task.body, COMPLETION);
                if task.done {
                    render_task(task.prefix, false, &body)
                } else {
                    render_task(
                        task.prefix,
                        true,
                        &with_field(&body, COMPLETION, &today.to_string()),
                    )
                }
            }),
            TodoEdit::Set { at, text, due } => {
                let text = checked_text(text)?;
                self.rewrite(at, |task| {
                    let mut body = text;
                    if let Some(d) = due {
                        body = with_field(&body, DUE, &d.to_string());
                    }
                    if find_field(&body, COMPLETION).is_none() {
                        if let Some((_, value)) = find_field(task.body, COMPLETION) {
                            body = with_field(&body, COMPLETION, value);
                        }
                    }
                    render_task(task.prefix, task.done, &body)
                })
            }
            TodoEdit::Snooze { at, today } => self.rewrite(at, |task| {
                let current = find_field(task.body, DUE).and_then(|(_, v)| Date::parse_iso(v).ok());
                let next = match current {
                    Some(d) if d >= *today => d.add_days(1),
                    _ => today.add_days(1),
                };
                render_task(
                    task.prefix,
                    task.done,
                    &with_field(task.body, DUE, &next.to_string()),
                )
            }),
            TodoEdit::Delete { at } => {
                self.task_at(at)?;
                let removed = self.lines.remove(at.line);
                if removed.ending.is_empty() {
                    if let Some(previous) =
                        at.line.checked_sub(1).and_then(|i| self.lines.get_mut(i))
                    {
                        previous.ending = "";
                    }
                }
                Ok(())
            }
        }
    }

    /// `\r\n` when any line uses it, else `\n`.
    fn ending(&self) -> &'static str {
        if self.lines.iter().any(|l| l.ending == "\r\n") {
            "\r\n"
        } else {
            "\n"
        }
    }

    /// The new line takes the ending of the line before it, which takes the
    /// document's own ending if it had none, so a file with no final newline
    /// still has none and no two lines are glued together.
    fn insert_line(&mut self, at: usize, content: String) {
        let document = self.ending();
        let ending = match at.checked_sub(1).and_then(|i| self.lines.get_mut(i)) {
            Some(previous) if previous.ending.is_empty() => {
                previous.ending = document;
                ""
            }
            Some(previous) => previous.ending,
            None => document,
        };
        self.lines
            .insert(at.min(self.lines.len()), Line { content, ending });
    }

    fn task_at(&self, at: &LineRef) -> Result<(), TodoError> {
        match self.lines.get(at.line) {
            Some(line) if line.content == at.raw && split_task(&line.content).is_some() => Ok(()),
            _ => Err(TodoError::Stale),
        }
    }

    fn rewrite(
        &mut self,
        at: &LineRef,
        edit: impl FnOnce(TaskLine<'_>) -> String,
    ) -> Result<(), TodoError> {
        self.task_at(at)?;
        let line = self.lines.get_mut(at.line).ok_or(TodoError::Stale)?;
        let new = {
            let task = split_task(&line.content).ok_or(TodoError::Stale)?;
            edit(task)
        };
        line.content = new;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::PathBuf;

    use super::*;

    fn date(raw: &str) -> Date {
        Date::parse_iso(raw).unwrap()
    }

    fn item(raw: &str) -> TodoItem {
        TodoItem::parse(0, raw).unwrap()
    }

    #[test]
    fn every_task_marker_is_a_task_and_nothing_else_is() {
        for raw in [
            "- [ ] a",
            "- [x] a",
            "- [X] a",
            "* [ ] a",
            "  - [ ] a",
            "\t* [x] a",
            "- [ ]",
        ] {
            assert!(TodoItem::parse(0, raw).is_some(), "{raw:?}");
        }
        for raw in [
            "-[ ] a",
            "- [] a",
            "- [y] a",
            "- [ ]a",
            "# - [ ] a",
            "1. [ ] a",
            "",
            "text",
        ] {
            assert!(TodoItem::parse(0, raw).is_none(), "{raw:?}");
        }
    }

    #[test]
    fn a_task_reads_its_state_text_and_fields() {
        let open = item("- [ ] renew TLS cert [due:: 2026-10-10]");
        assert!(!open.done);
        assert_eq!(open.text, "renew TLS cert");
        assert_eq!(open.due, Some(Due::Date(date("2026-10-10"))));
        assert_eq!(open.completion, None);

        let done = item("- [x] book flights [due:: 2026-10-06] [completion:: 2026-10-07]");
        assert!(done.done);
        assert_eq!(done.text, "book flights");
        assert_eq!(done.completion, Some(date("2026-10-07")));
    }

    #[test]
    fn a_due_that_is_not_a_date_is_kept_as_written() {
        let it = item("- [ ] think [due:: someday]");
        assert_eq!(it.due, Some(Due::Unparsed("someday".into())));
        assert_eq!(it.text, "think");
    }

    #[test]
    fn other_fields_stay_in_the_text() {
        let it = item("- [ ] ship [priority:: high] it [due:: 2026-10-10]");
        assert_eq!(it.text, "ship [priority:: high] it");
    }

    #[test]
    fn a_completion_that_is_not_a_date_stays_in_the_text() {
        let it = item("- [x] old [completion:: yesterday]");
        assert_eq!(it.completion, None);
        assert_eq!(it.text, "old [completion:: yesterday]");
    }

    #[test]
    fn the_first_field_of_a_key_is_the_one_read() {
        let it = item("- [ ] a [due:: 2026-10-10] [due:: 2026-11-11]");
        assert_eq!(it.due, Some(Due::Date(date("2026-10-10"))));
    }

    #[test]
    fn a_document_lists_its_tasks_with_their_line_numbers() {
        let doc = TodoDocument::parse("# Todo\n\n- [ ] a\nnote\n- [x] b\n");
        let items = doc.items();
        assert_eq!(items.len(), 2);
        assert_eq!((items[0].line, items[0].raw.as_str()), (2, "- [ ] a"));
        assert_eq!((items[1].line, items[1].raw.as_str()), (4, "- [x] b"));
        assert_eq!(
            items[1].at(),
            LineRef {
                line: 4,
                raw: "- [x] b".into()
            }
        );
    }

    #[test]
    fn a_crlf_line_reads_without_its_carriage_return() {
        let items = TodoDocument::parse("- [ ] a\r\n- [ ] b\r\n").items();
        assert_eq!(items[0].raw, "- [ ] a");
        assert_eq!(items[1].text, "b");
    }

    #[test]
    fn rendering_a_parsed_document_gives_back_the_same_bytes() {
        for text in [
            "",
            "\n",
            "- [ ] a",
            "- [ ] a\n",
            "- [ ] a\r\n- [x] b\r\n",
            "# H\n\n- [ ] a\n\n\n",
            "mixed\r\nendings\nhere",
            "\r",
            "\r\r\n",
            "日本語\n- [ ] 🎉 [due:: 2026-10-10]\n",
        ] {
            assert_eq!(TodoDocument::parse(text).render(), text, "{text:?}");
        }
    }

    #[test]
    fn rendering_round_trips_arbitrary_documents() {
        // A seeded generator rather than a property-testing crate: the
        // workspace takes no new dependency for one test.
        const FRAGMENTS: [&str; 16] = [
            "",
            " ",
            "\t",
            "\n",
            "\r\n",
            "\r",
            "- [ ] a",
            "- [x] b [due:: 2026-10-10]",
            "* [X] c",
            "# H",
            "[due:: x]",
            "- [",
            "-[ ] d",
            "日本",
            "]",
            "::",
        ];
        let mut seed: u64 = 0x5eed;
        for _ in 0..2000 {
            let mut text = String::new();
            seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
            let len = (seed >> 33) % 12;
            for _ in 0..len {
                seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
                text.push_str(FRAGMENTS[((seed >> 33) % 16) as usize]);
            }
            assert_eq!(TodoDocument::parse(&text).render(), text, "{text:?}");
        }
    }

    #[test]
    fn a_todo_file_must_be_absolute() {
        assert!(TodoFile::new(PathBuf::from("/home/k/todo.md")).is_ok());
        assert!(TodoFile::new(PathBuf::from("todo.md")).is_err());
        assert!(TodoFile::new(PathBuf::from("")).is_err());
    }

    fn applied(text: &str, edit: TodoEdit) -> String {
        let mut doc = TodoDocument::parse(text);
        doc.apply(&edit).unwrap();
        doc.render()
    }

    fn at(line: usize, raw: &str) -> LineRef {
        LineRef {
            line,
            raw: raw.into(),
        }
    }

    const TODAY: &str = "2026-10-07";

    #[test]
    fn adding_goes_after_the_last_task() {
        let out = applied(
            "# Todo\n\n- [ ] a\n\nnotes\n",
            TodoEdit::Add {
                text: "b".into(),
                due: Some(date("2026-10-12")),
            },
        );
        assert_eq!(
            out,
            "# Todo\n\n- [ ] a\n- [ ] b [due:: 2026-10-12]\n\nnotes\n"
        );
    }

    #[test]
    fn adding_to_a_file_with_no_tasks_appends() {
        assert_eq!(
            applied(
                "# Todo\n",
                TodoEdit::Add {
                    text: "a".into(),
                    due: None
                }
            ),
            "# Todo\n- [ ] a\n"
        );
        assert_eq!(
            applied(
                "",
                TodoEdit::Add {
                    text: "a".into(),
                    due: None
                }
            ),
            "- [ ] a\n"
        );
    }

    #[test]
    fn adding_after_a_last_line_without_a_newline_keeps_the_file_unterminated() {
        assert_eq!(
            applied(
                "- [ ] a",
                TodoEdit::Add {
                    text: "b".into(),
                    due: None
                }
            ),
            "- [ ] a\n- [ ] b"
        );
    }

    #[test]
    fn a_new_line_in_a_crlf_file_ends_in_crlf() {
        assert_eq!(
            applied(
                "- [ ] a\r\nnote",
                TodoEdit::Add {
                    text: "b".into(),
                    due: None
                }
            ),
            "- [ ] a\r\n- [ ] b\r\nnote"
        );
    }

    #[test]
    fn adding_trims_and_refuses_empty_or_multiline_text() {
        assert_eq!(
            applied(
                "",
                TodoEdit::Add {
                    text: "  a  ".into(),
                    due: None
                }
            ),
            "- [ ] a\n"
        );
        let mut doc = TodoDocument::default();
        assert_eq!(
            doc.apply(&TodoEdit::Add {
                text: "  ".into(),
                due: None
            }),
            Err(TodoError::EmptyText)
        );
        assert_eq!(
            doc.apply(&TodoEdit::Add {
                text: "a\nb".into(),
                due: None
            }),
            Err(TodoError::Multiline)
        );
    }

    #[test]
    fn toggling_an_open_todo_completes_it_today() {
        assert_eq!(
            applied(
                "- [ ] a [due:: 2026-10-06]\n",
                TodoEdit::Toggle {
                    at: at(0, "- [ ] a [due:: 2026-10-06]"),
                    today: date(TODAY)
                },
            ),
            "- [x] a [due:: 2026-10-06] [completion:: 2026-10-07]\n"
        );
    }

    #[test]
    fn toggling_a_done_todo_reopens_it_and_drops_the_completion() {
        let raw = "  * [X] a [completion:: 2026-10-01] [priority:: high]";
        assert_eq!(
            applied(
                raw,
                TodoEdit::Toggle {
                    at: at(0, raw),
                    today: date(TODAY)
                }
            ),
            "  * [ ] a [priority:: high]"
        );
    }

    #[test]
    fn setting_rebuilds_the_text_and_due_and_keeps_the_completion() {
        let raw = "- [x] old [due:: 2026-10-01] [completion:: 2026-10-02]";
        assert_eq!(
            applied(
                raw,
                TodoEdit::Set {
                    at: at(0, raw),
                    text: "new [priority:: low]".into(),
                    due: Some(date("2026-10-20")),
                }
            ),
            "- [x] new [priority:: low] [due:: 2026-10-20] [completion:: 2026-10-02]"
        );
    }

    #[test]
    fn setting_with_no_due_clears_it() {
        let raw = "- [ ] a [due:: someday]";
        assert_eq!(
            applied(
                raw,
                TodoEdit::Set {
                    at: at(0, raw),
                    text: "a".into(),
                    due: None
                }
            ),
            "- [ ] a"
        );
    }

    #[test]
    fn snoozing_moves_a_future_due_by_a_day_in_place() {
        let raw = "- [ ] a [due:: 2026-10-09] [priority:: high]";
        assert_eq!(
            applied(
                raw,
                TodoEdit::Snooze {
                    at: at(0, raw),
                    today: date(TODAY)
                }
            ),
            "- [ ] a [due:: 2026-10-10] [priority:: high]"
        );
        let raw = "- [ ] a [due:: 2026-10-07]";
        assert_eq!(
            applied(
                raw,
                TodoEdit::Snooze {
                    at: at(0, raw),
                    today: date(TODAY)
                }
            ),
            "- [ ] a [due:: 2026-10-08]"
        );
    }

    #[test]
    fn snoozing_an_overdue_undated_or_unparsed_todo_makes_it_due_tomorrow() {
        for raw in [
            "- [ ] a [due:: 2026-10-01]",
            "- [ ] a",
            "- [ ] a [due:: someday]",
        ] {
            assert_eq!(
                applied(
                    raw,
                    TodoEdit::Snooze {
                        at: at(0, raw),
                        today: date(TODAY)
                    }
                ),
                "- [ ] a [due:: 2026-10-08]",
                "{raw:?}"
            );
        }
    }

    #[test]
    fn deleting_removes_only_that_line() {
        assert_eq!(
            applied(
                "a\n- [ ] b\nc\n",
                TodoEdit::Delete {
                    at: at(1, "- [ ] b")
                }
            ),
            "a\nc\n"
        );
    }

    #[test]
    fn deleting_the_unterminated_last_line_leaves_the_new_last_line_unterminated() {
        assert_eq!(
            applied(
                "- [ ] a\n- [ ] b",
                TodoEdit::Delete {
                    at: at(1, "- [ ] b")
                }
            ),
            "- [ ] a"
        );
    }

    #[test]
    fn an_edit_naming_a_moved_line_is_stale() {
        let mut doc = TodoDocument::parse("- [ ] new\n- [ ] a\n");
        for edit in [
            TodoEdit::Toggle {
                at: at(0, "- [ ] a"),
                today: date(TODAY),
            },
            TodoEdit::Set {
                at: at(0, "- [ ] a"),
                text: "b".into(),
                due: None,
            },
            TodoEdit::Delete {
                at: at(0, "- [ ] a"),
            },
            TodoEdit::Snooze {
                at: at(0, "- [ ] a"),
                today: date(TODAY),
            },
            TodoEdit::Delete {
                at: at(9, "- [ ] a"),
            },
        ] {
            assert_eq!(doc.apply(&edit), Err(TodoError::Stale), "{edit:?}");
        }
        assert_eq!(doc.render(), "- [ ] new\n- [ ] a\n");
    }

    #[test]
    fn an_edit_naming_a_line_that_is_no_longer_a_task_is_stale() {
        let mut doc = TodoDocument::parse("note\n");
        assert_eq!(
            doc.apply(&TodoEdit::Delete { at: at(0, "note") }),
            Err(TodoError::Stale)
        );
    }

    #[test]
    fn a_duplicate_line_is_edited_by_its_own_line_number() {
        assert_eq!(
            applied(
                "- [ ] a\n- [ ] a\n",
                TodoEdit::Toggle {
                    at: at(1, "- [ ] a"),
                    today: date(TODAY)
                },
            ),
            "- [ ] a\n- [x] a [completion:: 2026-10-07]\n"
        );
    }

    #[test]
    fn an_edit_changes_no_other_line() {
        let text = "# H\r\n\n- [ ] a [x:: 1]\n  notes  \n- [x] b\n\n";
        let edits = [
            TodoEdit::Toggle {
                at: at(2, "- [ ] a [x:: 1]"),
                today: date(TODAY),
            },
            TodoEdit::Set {
                at: at(2, "- [ ] a [x:: 1]"),
                text: "z".into(),
                due: None,
            },
            TodoEdit::Snooze {
                at: at(4, "- [x] b"),
                today: date(TODAY),
            },
        ];
        let before: Vec<&str> = text.split_inclusive('\n').collect();
        for edit in edits {
            let out = applied(text, edit.clone());
            let after: Vec<&str> = out.split_inclusive('\n').collect();
            assert_eq!(before.len(), after.len(), "{edit:?}");
            let changed = before.iter().zip(&after).filter(|(b, a)| b != a).count();
            assert_eq!(changed, 1, "{edit:?}: {out:?}");
        }
    }

    #[test]
    fn a_stale_edit_says_so_in_the_words_the_pane_shows() {
        assert_eq!(
            TodoError::Stale.to_string(),
            "the todo file changed - refreshed"
        );
    }
}
