//! `todo`: a manual todo list, kept in a markdown file the broker owns.

use std::path::PathBuf;
use std::time::SystemTime;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use zax_domain::{
    due_state, is_late, parse_prompt, prompt_text, todo_rows, Date, Due, DueState, LineRef,
    TodoEdit, TodoItem,
};

use crate::action::PaneAction;
use crate::cache::Cached;
use crate::chrome::{error_line, header_line};
use crate::fmt::{display_width, truncate};

const TITLE: &str = "Todo";
const SEPARATOR: &str = " · ";
const BOX_OPEN: &str = "\u{2610}";
const BOX_DONE: &str = "\u{2611}";
const PROMPT: &str = "> ";

enum PromptMode {
    Add,
    Edit(LineRef),
}

/// A one-line editor. The input is chars so the cursor is an index.
struct Prompt {
    mode: PromptMode,
    input: Vec<char>,
    cursor: usize,
}

impl Prompt {
    fn new(mode: PromptMode, text: &str) -> Self {
        let input: Vec<char> = text.chars().collect();
        let cursor = input.len();
        Self {
            mode,
            input,
            cursor,
        }
    }

    fn insert(&mut self, c: char) {
        self.input.insert(self.cursor, c);
        self.cursor += 1;
    }

    fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.input.remove(self.cursor);
        }
    }

    /// `ctrl-w`: the spaces before the cursor, then the word before them.
    fn delete_word(&mut self) {
        while self.cursor > 0 && self.input.get(self.cursor - 1) == Some(&' ') {
            self.backspace();
        }
        while self.cursor > 0 && self.input.get(self.cursor - 1).is_some_and(|c| *c != ' ') {
            self.backspace();
        }
    }

    fn clear(&mut self) {
        self.input.clear();
        self.cursor = 0;
    }

    fn text(&self) -> String {
        self.input.iter().collect()
    }
}

pub struct TodoPane {
    file: String,
    list: Cached<Vec<TodoItem>>,
    rows: Vec<TodoItem>,
    selected: usize,
    focused: bool,
    today: Date,
    show_all_done: bool,
    prompt: Option<Prompt>,
    confirm_delete: bool,
    notice: Option<String>,
}

impl TodoPane {
    pub fn new(file: String, today: Date) -> Self {
        Self {
            file,
            list: Cached::new(),
            rows: Vec::new(),
            selected: 0,
            focused: false,
            today,
            show_all_done: false,
            prompt: None,
            confirm_delete: false,
            notice: None,
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// The host reads the local date and passes it in, once a minute, so
    /// labels and hiding roll over at midnight.
    pub fn set_today(&mut self, today: Date) {
        if today != self.today {
            self.today = today;
            self.regroup();
        }
    }

    /// A broker snapshot, already decoded. `items` and `error` together
    /// mean stale.
    pub fn apply(
        &mut self,
        items: Option<Vec<TodoItem>>,
        fetched_at: Option<SystemTime>,
        error: Option<String>,
    ) {
        if let (Some(items), Some(at)) = (items, fetched_at) {
            self.list.record_ok(items, at);
        }
        if let Some(error) = error {
            self.list.record_err(error, SystemTime::now());
        }
        self.regroup();
    }

    /// A one-line message about the last keystroke, cleared by the next one.
    pub fn notice(&mut self, message: String) {
        self.notice = Some(message);
    }

    pub fn rows(&self) -> &[TodoItem] {
        &self.rows
    }

    pub fn prompt_open(&self) -> bool {
        self.prompt.is_some()
    }

    /// Rebuild the rows and keep the selection on the same item: a hand edit
    /// or another pane's add must not move the cursor onto a different todo.
    fn regroup(&mut self) {
        let keep = self.rows.get(self.selected).map(|i| i.raw.clone());
        self.rows = self
            .list
            .value()
            .map(|items| todo_rows(items, self.today, self.show_all_done))
            .unwrap_or_default();
        let found = keep.and_then(|raw| self.rows.iter().position(|i| i.raw == raw));
        self.selected = found
            .unwrap_or(self.selected)
            .min(self.rows.len().saturating_sub(1));
    }

    fn selected_item(&self) -> Option<&TodoItem> {
        self.rows.get(self.selected)
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PaneAction {
        self.notice = None;
        if self.prompt.is_some() {
            return self.prompt_key(key);
        }
        if std::mem::take(&mut self.confirm_delete) && key.code == KeyCode::Char('d') {
            return self
                .selected_item()
                .map(|i| PaneAction::Todo(TodoEdit::Delete { at: i.at() }))
                .unwrap_or_default();
        }
        let len = self.rows.len();
        match key.code {
            KeyCode::Char('r') => PaneAction::Refresh,
            KeyCode::Char('q') | KeyCode::Esc => PaneAction::Quit,
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected = (self.selected + 1).min(len.saturating_sub(1));
                PaneAction::None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                PaneAction::None
            }
            KeyCode::Char('a') => {
                self.prompt = Some(Prompt::new(PromptMode::Add, ""));
                PaneAction::None
            }
            KeyCode::Char('e') | KeyCode::Enter => {
                if let Some(item) = self.selected_item() {
                    self.prompt =
                        Some(Prompt::new(PromptMode::Edit(item.at()), &prompt_text(item)));
                }
                PaneAction::None
            }
            KeyCode::Char('x') | KeyCode::Char(' ') => self
                .selected_item()
                .map(|i| {
                    PaneAction::Todo(TodoEdit::Toggle {
                        at: i.at(),
                        today: self.today,
                    })
                })
                .unwrap_or_default(),
            KeyCode::Char('+') => self
                .selected_item()
                .map(|i| {
                    PaneAction::Todo(TodoEdit::Snooze {
                        at: i.at(),
                        today: self.today,
                    })
                })
                .unwrap_or_default(),
            KeyCode::Char('d') => {
                if self.selected_item().is_some() {
                    self.confirm_delete = true;
                    self.notice = Some("d again to delete".to_owned());
                }
                PaneAction::None
            }
            KeyCode::Char('o') => PaneAction::OpenFile(PathBuf::from(&self.file)),
            KeyCode::Char('.') => {
                self.show_all_done = !self.show_all_done;
                self.regroup();
                PaneAction::None
            }
            _ => PaneAction::None,
        }
    }

    fn prompt_key(&mut self, key: KeyEvent) -> PaneAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Enter => return self.submit(),
            KeyCode::Esc => {
                self.prompt = None;
                return PaneAction::None;
            }
            _ => {}
        }
        let Some(prompt) = self.prompt.as_mut() else {
            return PaneAction::None;
        };
        match key.code {
            KeyCode::Char('u') if ctrl => prompt.clear(),
            KeyCode::Char('w') if ctrl => prompt.delete_word(),
            KeyCode::Char(c) if !ctrl => prompt.insert(c),
            KeyCode::Backspace => prompt.backspace(),
            KeyCode::Left => prompt.cursor = prompt.cursor.saturating_sub(1),
            KeyCode::Right => prompt.cursor = (prompt.cursor + 1).min(prompt.input.len()),
            KeyCode::Home => prompt.cursor = 0,
            KeyCode::End => prompt.cursor = prompt.input.len(),
            _ => {}
        }
        PaneAction::None
    }

    /// A prompt that does not parse stays open with the reason; nothing is
    /// half-saved.
    fn submit(&mut self) -> PaneAction {
        let Some(prompt) = self.prompt.take() else {
            return PaneAction::None;
        };
        match parse_prompt(&prompt.text(), self.today) {
            Err(e) => {
                self.notice = Some(e.to_string());
                self.prompt = Some(prompt);
                PaneAction::None
            }
            Ok(entry) => PaneAction::Todo(match prompt.mode {
                PromptMode::Add => TodoEdit::Add {
                    text: entry.text,
                    due: entry.due,
                },
                PromptMode::Edit(at) => TodoEdit::Set {
                    at,
                    text: entry.text,
                    due: entry.due,
                },
            }),
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, now: SystemTime) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let width = usize::from(area.width);
        let mut lines = vec![self.header(area.width, now)];
        let messages = self.messages();
        // Header, detail line, prompt and messages are chrome.
        let chrome = 2 + usize::from(self.prompt.is_some()) + messages.len();
        match self.list.value() {
            None => {}
            Some(_) if self.rows.is_empty() => lines.push(Line::styled(
                truncate(&format!("a to add{SEPARATOR}file: {}", self.file), width),
                Style::default().fg(Color::DarkGray),
            )),
            Some(_) => {
                let visible = usize::from(area.height).saturating_sub(chrome);
                let first = self
                    .selected
                    .saturating_sub(visible.saturating_sub(1))
                    .min(self.rows.len());
                let last = (first + visible).min(self.rows.len());
                for index in first..last {
                    if let Some(item) = self.rows.get(index) {
                        lines.push(self.row(item, index == self.selected, width));
                    }
                }
                if first < last {
                    if let Some(item) = self.selected_item() {
                        lines.push(self.detail_line(item, width));
                    }
                }
            }
        }
        if let Some(prompt) = &self.prompt {
            lines.push(prompt_line(prompt, width));
        }
        for message in messages {
            lines.push(error_line(&message));
        }
        frame.render_widget(Paragraph::new(Text::from(lines)), area);
    }

    /// `Todo (3)` or `Todo (3 · 1 late)`, counting open items.
    fn header(&self, width: u16, now: SystemTime) -> Line<'static> {
        let title = match self.list.value() {
            None => TITLE.to_owned(),
            Some(items) => {
                let open = items.iter().filter(|i| !i.done).count();
                let late = items.iter().filter(|i| is_late(i, self.today)).count();
                if late > 0 {
                    format!("{TITLE} ({open}{SEPARATOR}{late} late)")
                } else {
                    format!("{TITLE} ({open})")
                }
            }
        };
        header_line(&title, &self.list.status(now), self.focused, width)
    }

    fn messages(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.list.value().is_none() {
            if let Some(e) = self.list.error() {
                out.push(e.to_owned());
            }
        }
        if let Some(n) = &self.notice {
            out.push(n.clone());
        }
        out
    }

    /// `<box> <text> <padding> <due>`, the due column right-aligned and kept
    /// whole, the text truncated to fit.
    fn row(&self, item: &TodoItem, selected: bool, width: usize) -> Line<'static> {
        let (glyph, base) = if item.done {
            (BOX_DONE, Style::default().fg(Color::DarkGray))
        } else {
            (BOX_OPEN, Style::default())
        };
        let style = if selected && self.focused {
            base.add_modifier(Modifier::REVERSED)
        } else {
            base
        };
        let label = if item.done {
            None
        } else {
            due_label(item, self.today)
        };
        let label_width = label
            .as_ref()
            .map_or(0, |(text, _)| display_width(text) + 1);
        let fixed = display_width(glyph) + 1 + label_width;
        let text = truncate(&item.text, width.saturating_sub(fixed));
        let gap = width.saturating_sub(fixed + display_width(&text));
        let mut spans = vec![
            Span::styled(glyph.to_owned(), style),
            Span::styled(" ", style),
            Span::styled(text, style),
            Span::styled(" ".repeat(gap), style),
        ];
        if let Some((text, label_style)) = label {
            spans.push(Span::styled(" ", style));
            spans.push(Span::styled(text, label_style.patch(style)));
        }
        Line::from(spans)
    }

    /// `<text> · due Mon 5 Oct · 2 days late`, or how it was done.
    fn detail_line(&self, item: &TodoItem, width: usize) -> Line<'static> {
        let mut parts = vec![item.text.clone()];
        if item.done {
            parts.push(match item.completion {
                Some(c) => format!("done {}", long_date(c)),
                None => "done".to_owned(),
            });
        } else {
            match (&item.due, due_state(item, self.today)) {
                (Some(Due::Unparsed(v)), _) => parts.push(format!("due \"{v}\" is not a date")),
                (Some(Due::Date(d)), DueState::Overdue(n)) => {
                    parts.push(format!("due {}", long_date(*d)));
                    parts.push(format!("{n} day{} late", plural(n)));
                }
                (Some(Due::Date(_)), DueState::Today) => parts.push("due today".to_owned()),
                (Some(Due::Date(d)), DueState::Upcoming(n)) => {
                    parts.push(format!("due {}", long_date(*d)));
                    parts.push(format!("in {n} day{}", plural(n)));
                }
                _ => {}
            }
        }
        Line::styled(
            truncate(&parts.join(SEPARATOR), width),
            Style::default().fg(Color::DarkGray),
        )
    }
}

fn plural(n: i64) -> &'static str {
    if n == 1 {
        ""
    } else {
        "s"
    }
}

/// `Mon 5 Oct`.
fn long_date(date: Date) -> String {
    format!(
        "{} {} {}",
        date.weekday().short(),
        date.day(),
        date.month_short()
    )
}

/// The due column: `-2d`, `today`, `tom`, a weekday within six days, then
/// `Oct 20`, or an ISO date in another year. `?` for a due that is not a date.
fn due_label(item: &TodoItem, today: Date) -> Option<(String, Style)> {
    let red = Style::default().fg(Color::Red);
    let date = match &item.due {
        Some(Due::Date(d)) => Some(*d),
        _ => None,
    };
    match (due_state(item, today), date) {
        (DueState::NoDue, _) => None,
        (DueState::Unparsed, _) => Some(("?".to_owned(), red)),
        (DueState::Overdue(n), _) => Some((format!("-{n}d"), red)),
        (DueState::Today, _) => Some(("today".to_owned(), Style::default().fg(Color::Yellow))),
        (DueState::Upcoming(1), _) => Some(("tom".to_owned(), Style::default())),
        (DueState::Upcoming(n), Some(d)) if n <= 6 => {
            Some((d.weekday().short().to_owned(), Style::default()))
        }
        (DueState::Upcoming(_), Some(d)) if d.year() == today.year() => {
            Some((format!("{} {}", d.month_short(), d.day()), Style::default()))
        }
        (DueState::Upcoming(_), Some(d)) => Some((d.to_string(), Style::default())),
        (DueState::Upcoming(_), None) => None,
    }
}

/// `> text` with the cursor drawn reversed. When the text is wider than the
/// pane, the start scrolls off so the cursor stays in view.
fn prompt_line(prompt: &Prompt, width: usize) -> Line<'static> {
    let budget = width.saturating_sub(display_width(PROMPT) + 1);
    let mut start = 0;
    let before = |start: usize| -> String {
        prompt
            .input
            .get(start..prompt.cursor)
            .unwrap_or(&[])
            .iter()
            .collect()
    };
    while start < prompt.cursor && display_width(&before(start)) > budget {
        start += 1;
    }
    let shown = before(start);
    let at = prompt
        .input
        .get(prompt.cursor)
        .map_or_else(|| " ".to_owned(), char::to_string);
    let after: String = prompt
        .input
        .get(prompt.cursor + 1..)
        .unwrap_or(&[])
        .iter()
        .collect();
    let room = budget.saturating_sub(display_width(&shown));
    Line::from(vec![
        Span::styled(PROMPT, Style::default().fg(Color::Cyan)),
        Span::raw(shown),
        Span::styled(at, Style::default().add_modifier(Modifier::REVERSED)),
        Span::raw(truncate(&after, room)),
    ])
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::{Duration, UNIX_EPOCH};

    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use zax_domain::TodoDocument;

    use super::*;

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }
    fn d(raw: &str) -> Date {
        Date::parse_iso(raw).unwrap()
    }
    /// A Wednesday.
    fn today() -> Date {
        d("2026-10-07")
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }
    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }
    fn typed(pane: &mut TodoPane, text: &str) {
        for c in text.chars() {
            pane.on_key(key(KeyCode::Char(c)));
        }
    }

    const FILE: &str = "# Todo\n\
        - [ ] renew TLS cert [due:: 2026-10-05]\n\
        - [ ] review RFC [due:: 2026-10-09]\n\
        - [ ] tidy dotfiles\n\
        - [x] book flights [completion:: 2026-10-07]\n\
        - [x] old [completion:: 2026-10-01]\n";

    fn pane_with(text: &str) -> TodoPane {
        let mut pane = TodoPane::new("/h/todo.md".into(), today());
        pane.set_focused(true);
        pane.apply(Some(TodoDocument::parse(text).items()), Some(t(100)), None);
        pane
    }

    fn draw(pane: &TodoPane, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| pane.render(f, f.area(), t(102))).unwrap();
        let buffer: &Buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| {
                (0..width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn the_rows_are_ordered_and_old_done_items_are_hidden() {
        let pane = pane_with(FILE);
        let texts: Vec<&str> = pane.rows().iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "renew TLS cert",
                "review RFC",
                "tidy dotfiles",
                "book flights"
            ]
        );
    }

    #[test]
    fn dot_shows_and_hides_every_done_item() {
        let mut pane = pane_with(FILE);
        pane.on_key(key(KeyCode::Char('.')));
        assert_eq!(pane.rows().len(), 5);
        pane.on_key(key(KeyCode::Char('.')));
        assert_eq!(pane.rows().len(), 4);
    }

    #[test]
    fn at_40_columns_the_header_counts_and_the_due_column_reads() {
        let lines = draw(&pane_with(FILE), 40, 8);
        assert!(lines[0].contains("Todo (3 · 1 late)"), "{lines:#?}");
        assert!(
            lines[1].starts_with("☐ renew TLS cert") && lines[1].ends_with("-2d"),
            "{lines:#?}"
        );
        assert!(lines[2].ends_with("Fri"), "{lines:#?}");
        assert!(lines[3].trim_end().ends_with("tidy dotfiles"), "{lines:#?}");
        assert!(lines[4].starts_with("☑ book flights"), "{lines:#?}");
        assert!(
            lines[5].starts_with("renew TLS cert · due Mon 5 Oct"),
            "{lines:#?}"
        );
        let wide = draw(&pane_with(FILE), 60, 8);
        assert!(
            wide[5].contains("renew TLS cert · due Mon 5 Oct · 2 days late"),
            "{wide:#?}"
        );
    }

    #[test]
    fn at_24_columns_the_due_column_keeps_its_place() {
        let pane = pane_with("- [ ] a very long todo that will not fit [due:: 2026-10-20]\n");
        let lines = draw(&pane, 24, 4);
        for line in &lines {
            assert!(
                unicode_width::UnicodeWidthStr::width(line.as_str()) <= 24,
                "{line:?}"
            );
        }
        assert!(lines[1].ends_with("Oct 20"), "{lines:#?}");
        assert!(lines[1].starts_with("☐ a very"), "{lines:#?}");
    }

    #[test]
    fn due_labels_cover_every_state() {
        let pane = pane_with(
            "- [ ] a [due:: 2026-10-07]\n- [ ] b [due:: 2026-10-08]\n- [ ] c [due:: someday]\n- [ ] e [due:: 2027-01-05]\n",
        );
        let lines = draw(&pane, 30, 7);
        assert!(lines[1].ends_with("today"), "{lines:#?}");
        assert!(lines[2].ends_with("tom"), "{lines:#?}");
        assert!(lines[3].ends_with("2027-01-05"), "{lines:#?}");
        assert!(lines[4].ends_with('?'), "{lines:#?}");
    }

    #[test]
    fn an_unparsed_due_is_quoted_on_the_detail_line() {
        let mut pane = pane_with("- [ ] c [due:: someday]\n");
        pane.on_key(key(KeyCode::Down));
        let lines = draw(&pane, 50, 4);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("due \"someday\" is not a date")),
            "{lines:#?}"
        );
    }

    #[test]
    fn an_empty_list_says_how_to_add_and_where_the_file_is() {
        let lines = draw(&pane_with(""), 40, 4);
        assert!(
            lines[1].contains("a to add · file: /h/todo.md"),
            "{lines:#?}"
        );
    }

    #[test]
    fn a_failed_first_fetch_shows_the_error() {
        let mut pane = TodoPane::new("/h/todo.md".into(), today());
        pane.apply(None, None, Some("/h/todo.md is not UTF-8 text".into()));
        let lines = draw(&pane, 40, 4);
        assert!(
            lines.iter().any(|l| l.contains("is not UTF-8 text")),
            "{lines:#?}"
        );
    }

    #[test]
    fn x_toggles_the_selected_row_as_of_today() {
        let mut pane = pane_with(FILE);
        assert_eq!(
            pane.on_key(key(KeyCode::Char('x'))),
            PaneAction::Todo(TodoEdit::Toggle {
                at: LineRef {
                    line: 1,
                    raw: "- [ ] renew TLS cert [due:: 2026-10-05]".into()
                },
                today: today(),
            })
        );
        assert_eq!(
            pane.on_key(key(KeyCode::Char(' '))),
            pane.on_key(key(KeyCode::Char('x')))
        );
    }

    #[test]
    fn plus_snoozes_the_selected_row() {
        let mut pane = pane_with(FILE);
        pane.on_key(key(KeyCode::Char('j')));
        assert!(matches!(
            pane.on_key(key(KeyCode::Char('+'))),
            PaneAction::Todo(TodoEdit::Snooze {
                at: LineRef { line: 2, .. },
                ..
            })
        ));
    }

    #[test]
    fn d_asks_once_and_d_again_deletes() {
        let mut pane = pane_with(FILE);
        assert_eq!(pane.on_key(key(KeyCode::Char('d'))), PaneAction::None);
        assert!(draw(&pane, 40, 8)
            .iter()
            .any(|l| l.contains("d again to delete")));
        assert!(matches!(
            pane.on_key(key(KeyCode::Char('d'))),
            PaneAction::Todo(TodoEdit::Delete {
                at: LineRef { line: 1, .. }
            })
        ));
    }

    #[test]
    fn any_other_key_after_d_cancels_and_still_acts() {
        let mut pane = pane_with(FILE);
        pane.on_key(key(KeyCode::Char('d')));
        assert_eq!(pane.on_key(key(KeyCode::Char('j'))), PaneAction::None);
        // Moved down, and no delete is pending.
        assert_eq!(pane.on_key(key(KeyCode::Char('d'))), PaneAction::None);
        assert!(matches!(
            pane.on_key(key(KeyCode::Char('d'))),
            PaneAction::Todo(TodoEdit::Delete {
                at: LineRef { line: 2, .. }
            })
        ));
    }

    #[test]
    fn a_adds_through_the_prompt() {
        let mut pane = pane_with(FILE);
        pane.on_key(key(KeyCode::Char('a')));
        assert!(pane.prompt_open());
        typed(&mut pane, "call bank due:mon");
        assert!(draw(&pane, 40, 9)
            .iter()
            .any(|l| l.starts_with("> call bank due:mon")));
        assert_eq!(
            pane.on_key(key(KeyCode::Enter)),
            PaneAction::Todo(TodoEdit::Add {
                text: "call bank".into(),
                due: Some(d("2026-10-12"))
            })
        );
        assert!(!pane.prompt_open());
    }

    #[test]
    fn the_prompt_swallows_the_pane_keys() {
        let mut pane = pane_with(FILE);
        pane.on_key(key(KeyCode::Char('a')));
        for c in ['q', 'x', 'd', 'j', 'r'] {
            assert_eq!(pane.on_key(key(KeyCode::Char(c))), PaneAction::None);
        }
        assert!(pane.prompt_open());
    }

    #[test]
    fn a_bad_prompt_stays_open_with_the_reason() {
        let mut pane = pane_with(FILE);
        pane.on_key(key(KeyCode::Char('a')));
        typed(&mut pane, "x due:someday");
        assert_eq!(pane.on_key(key(KeyCode::Enter)), PaneAction::None);
        assert!(pane.prompt_open());
        assert!(draw(&pane, 60, 9)
            .iter()
            .any(|l| l.contains("is not a date")));
    }

    #[test]
    fn esc_closes_the_prompt_without_quitting() {
        let mut pane = pane_with(FILE);
        pane.on_key(key(KeyCode::Char('a')));
        assert_eq!(pane.on_key(key(KeyCode::Esc)), PaneAction::None);
        assert!(!pane.prompt_open());
        assert_eq!(pane.on_key(key(KeyCode::Esc)), PaneAction::Quit);
    }

    #[test]
    fn e_edits_the_selected_row_prefilled() {
        let mut pane = pane_with(FILE);
        pane.on_key(key(KeyCode::Char('e')));
        pane.on_key(ctrl('w'));
        typed(&mut pane, "due:tom");
        assert_eq!(
            pane.on_key(key(KeyCode::Enter)),
            PaneAction::Todo(TodoEdit::Set {
                at: LineRef {
                    line: 1,
                    raw: "- [ ] renew TLS cert [due:: 2026-10-05]".into()
                },
                text: "renew TLS cert".into(),
                due: Some(d("2026-10-08")),
            })
        );
    }

    #[test]
    fn the_prompt_edits_at_its_cursor() {
        let mut pane = pane_with("");
        pane.on_key(key(KeyCode::Char('a')));
        typed(&mut pane, "ac");
        pane.on_key(key(KeyCode::Left));
        typed(&mut pane, "b");
        pane.on_key(key(KeyCode::End));
        typed(&mut pane, " d");
        pane.on_key(key(KeyCode::Backspace));
        pane.on_key(key(KeyCode::Backspace));
        assert_eq!(
            pane.on_key(key(KeyCode::Enter)),
            PaneAction::Todo(TodoEdit::Add {
                text: "abc".into(),
                due: None
            })
        );
        pane.on_key(key(KeyCode::Char('a')));
        typed(&mut pane, "junk");
        pane.on_key(ctrl('u'));
        typed(&mut pane, "z");
        assert_eq!(
            pane.on_key(key(KeyCode::Enter)),
            PaneAction::Todo(TodoEdit::Add {
                text: "z".into(),
                due: None
            })
        );
    }

    #[test]
    fn the_selection_follows_its_item_across_a_snapshot() {
        let mut pane = pane_with(FILE);
        pane.on_key(key(KeyCode::Char('j')));
        assert_eq!(pane.rows()[1].text, "review RFC");
        // Another pane adds an overdue item above it.
        let moved = FILE.replace("# Todo\n", "# Todo\n- [ ] urgent [due:: 2026-10-01]\n");
        pane.apply(
            Some(TodoDocument::parse(&moved).items()),
            Some(t(110)),
            None,
        );
        assert!(matches!(
            pane.on_key(key(KeyCode::Char('x'))),
            PaneAction::Todo(TodoEdit::Toggle {
                at: LineRef { line: 3, .. },
                ..
            })
        ));
    }

    #[test]
    fn a_new_day_regroups_the_rows() {
        let mut pane = pane_with(FILE);
        pane.set_today(d("2026-10-08"));
        // book flights was done yesterday now, so it hides.
        assert_eq!(pane.rows().len(), 3);
    }

    #[test]
    fn o_opens_the_file() {
        let mut pane = pane_with(FILE);
        assert_eq!(
            pane.on_key(key(KeyCode::Char('o'))),
            PaneAction::OpenFile(PathBuf::from("/h/todo.md"))
        );
    }

    #[test]
    fn keys_on_an_empty_list_do_nothing_but_add() {
        let mut pane = pane_with("");
        for c in ['x', ' ', '+', 'e', 'd'] {
            assert_eq!(pane.on_key(key(KeyCode::Char(c))), PaneAction::None, "{c}");
        }
        assert_eq!(pane.on_key(key(KeyCode::Char('r'))), PaneAction::Refresh);
    }
}
