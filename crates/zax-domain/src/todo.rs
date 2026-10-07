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

/// A task line split into what an edit keeps and what it changes.
pub(crate) struct TaskLine<'a> {
    /// The indent and the list marker with its space: `"  - "`.
    // The edits use it.
    #[expect(dead_code)]
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

// The edits use it.
#[expect(dead_code)]
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
// The edits use it.
#[expect(dead_code)]
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
}
