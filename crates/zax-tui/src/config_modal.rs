//! The config modal: a state machine over a `ConfigView`.
//!
//! It holds a screen, a cursor, the layer edits land in and any half-typed text.
//! It never holds a file, a document or an opinion about whether a value is
//! valid - the host applies the action it returns to the draft and hands back a
//! refreshed view, exactly as the pane loops turn a `PaneAction` into an effect.
//!
//! The popup is framed by tmux, so nothing here draws a box of its own.

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use zax_domain::settings::{
    ConfigView, LayerSlot, Origin, PaneEdit, PaneField, PaneKindTag, Pending, Saved, SettingKey,
    SettingRow, SettingShape, SettingValue,
};
use zax_domain::BuiltinPane;

use crate::chrome::{error_line, header_line, GLYPH_ERROR, GLYPH_NONE, GLYPH_PASS};
use crate::fmt::{display_width, truncate};

/// What a keystroke asks the host to do to the draft.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum ConfigAction {
    #[default]
    None,
    Set {
        slot: LayerSlot,
        key: SettingKey,
        value: SettingValue,
    },
    Clear {
        slot: LayerSlot,
        key: SettingKey,
    },
    SetPanes {
        slot: LayerSlot,
        panes: Vec<PaneEdit>,
    },
    Save,
    Reload {
        slot: LayerSlot,
    },
    Close,
}

/// A selectable line on the main screen. The pane list is a row like any other,
/// because "why is the layout what it is" is the same question as for a scalar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Setting(SettingKey),
    Panes,
}

impl Row {
    /// Grouped as the TOML is, with the layout where a reader expects it.
    const ALL: [Row; 10] = [
        Row::Setting(SettingKey::AgentCommand),
        Row::Panes,
        Row::Setting(SettingKey::WorktreeRoot),
        Row::Setting(SettingKey::WorktreeCopy),
        Row::Setting(SettingKey::WorktreeHooks),
        Row::Setting(SettingKey::TmuxPrefix),
        Row::Setting(SettingKey::PrsInclude),
        Row::Setting(SettingKey::PrsExclude),
        Row::Setting(SettingKey::PrsMaxAge),
        Row::Setting(SettingKey::RepoKeyName),
    ];

    fn group(self) -> &'static str {
        match self {
            // `name` is a top-level key rather than a table entry, and it is
            // about this repository's bookkeeping, so that is the heading it
            // gets rather than a blank one.
            Row::Setting(key) => key.group().unwrap_or("repo"),
            Row::Panes => "layout",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Row::Setting(key) => key.label(),
            Row::Panes => "panes",
        }
    }
}

/// Where a list being edited belongs. A pane's `command` is a list too, and
/// editing it as one is what keeps an argument containing a space intact.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ListTarget {
    Setting(SettingKey),
    PaneCommand { pane: usize },
}

/// A list being edited: where it belongs, its items, and which one the cursor is
/// on. One value rather than three parameters threaded through every handler.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ListEdit {
    target: ListTarget,
    items: Vec<String>,
    cursor: usize,
}

enum Screen {
    Settings,
    Scalar {
        key: SettingKey,
        buffer: String,
    },
    List(ListEdit),
    ListItem {
        edit: ListEdit,
        buffer: String,
        appending: bool,
    },
    Panes {
        cursor: usize,
    },
    PaneKind {
        /// `None` while adding a pane, `Some` while changing one's kind.
        pane: Option<usize>,
        cursor: usize,
    },
    PaneDetail {
        pane: usize,
        cursor: usize,
    },
    PaneField {
        pane: usize,
        field: PaneField,
        cursor: usize,
        buffer: String,
    },
    Confirm {
        lines: Vec<String>,
        action: ConfigAction,
        on_yes: Box<Screen>,
        on_no: Box<Screen>,
    },
    Help {
        back: Box<Screen>,
    },
    /// What a save did, or why it did nothing.
    Report {
        saved: Option<Saved>,
        error: Option<String>,
    },
}

pub struct ConfigModal {
    /// Changes the wording of the restart notices: inside a session they speak
    /// of this session, standalone of the next `zax session start`.
    inside_session: bool,
    target: LayerSlot,
    cursor: usize,
    screen: Screen,
    /// A refusal or a one-off remark, shown until the next keystroke.
    notice: Option<String>,
}

impl ConfigModal {
    pub fn new(inside_session: bool) -> Self {
        Self {
            inside_session,
            // Your layer: the one edit that is always yours to make.
            target: LayerSlot::Personal,
            cursor: 0,
            screen: Screen::Settings,
            notice: None,
        }
    }

    /// The layer edits land in. The header names it; `tab` cycles it.
    pub fn target(&self) -> LayerSlot {
        self.target
    }

    pub fn handle_key(&mut self, key: KeyEvent, view: &ConfigView) -> ConfigAction {
        self.notice = None;
        // Taken out so each handler can own its screen and hand back the next
        // one, rather than borrowing `self` twice.
        let screen = std::mem::replace(&mut self.screen, Screen::Settings);
        let (action, screen) = self.dispatch(key, view, screen);
        self.screen = screen;
        action
    }

    fn dispatch(
        &mut self,
        key: KeyEvent,
        view: &ConfigView,
        screen: Screen,
    ) -> (ConfigAction, Screen) {
        // A confirmation and a report answer for themselves: a `tab` here would
        // change the layer the pending action writes to after it was described.
        match screen {
            Screen::Confirm {
                lines,
                action,
                on_yes,
                on_no,
            } => {
                return match key.code {
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => (action, *on_yes),
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        (ConfigAction::None, *on_no)
                    }
                    _ => (
                        ConfigAction::None,
                        Screen::Confirm {
                            lines,
                            action,
                            on_yes,
                            on_no,
                        },
                    ),
                }
            }
            Screen::Report { .. } => return (ConfigAction::None, Screen::Settings),
            Screen::Help { back } => {
                return match key.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('?') => {
                        (ConfigAction::None, *back)
                    }
                    _ => (ConfigAction::None, Screen::Help { back }),
                }
            }
            _ => {}
        }

        // A screen with a text buffer takes every printable key as text, `?`
        // and `tab` included.
        match screen {
            Screen::Scalar {
                key: setting,
                buffer,
            } => return self.on_scalar_key(key, setting, buffer),
            Screen::ListItem {
                edit,
                buffer,
                appending,
            } => return self.on_list_item_key(key, view, edit, buffer, appending),
            Screen::PaneField {
                pane,
                field,
                cursor,
                buffer,
            } => return self.on_pane_field_key(key, view, pane, field, cursor, buffer),
            _ => {}
        }

        match key.code {
            KeyCode::Tab => {
                self.target = self.target.next();
                return (ConfigAction::None, screen);
            }
            KeyCode::BackTab => {
                self.target = self.target.previous();
                return (ConfigAction::None, screen);
            }
            KeyCode::Char('?') => {
                return (
                    ConfigAction::None,
                    Screen::Help {
                        back: Box::new(screen),
                    },
                )
            }
            _ => {}
        }

        match screen {
            Screen::Settings => self.on_settings_key(key, view),
            Screen::List(edit) => self.on_list_key(key, view, edit),
            Screen::Panes { cursor } => self.on_panes_key(key, view, cursor),
            Screen::PaneKind { pane, cursor } => self.on_pane_kind_key(key, view, pane, cursor),
            Screen::PaneDetail { pane, cursor } => self.on_pane_detail_key(key, view, pane, cursor),
            // Handled above.
            other => (ConfigAction::None, other),
        }
    }

    fn on_settings_key(&mut self, key: KeyEvent, view: &ConfigView) -> (ConfigAction, Screen) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.cursor = (self.cursor + 1).min(Row::ALL.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                return (ConfigAction::None, self.open_selected(view))
            }
            KeyCode::Char('x') => return (self.clear_selected(), Screen::Settings),
            KeyCode::Char('s') => return (ConfigAction::Save, Screen::Settings),
            KeyCode::Char('r') => {
                return (ConfigAction::Reload { slot: self.target }, Screen::Settings)
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                return (self.close(view), self.screen_or_confirm(view))
            }
            _ => {}
        }
        (ConfigAction::None, Screen::Settings)
    }

    /// `esc` with unsaved work asks first: the draft is staged, so closing is
    /// the one keystroke that can throw an edit away.
    fn close(&mut self, view: &ConfigView) -> ConfigAction {
        if view.any_dirty() {
            ConfigAction::None
        } else {
            ConfigAction::Close
        }
    }

    fn screen_or_confirm(&self, view: &ConfigView) -> Screen {
        if !view.any_dirty() {
            return Screen::Settings;
        }
        Screen::Confirm {
            lines: vec![format!(
                "unsaved changes in {} - close and lose them?",
                dirty_layers(view)
            )],
            action: ConfigAction::Close,
            on_yes: Box::new(Screen::Settings),
            on_no: Box::new(Screen::Settings),
        }
    }

    fn selected_row(&self) -> Row {
        Row::ALL.get(self.cursor).copied().unwrap_or(Row::Panes)
    }

    fn open_selected(&mut self, view: &ConfigView) -> Screen {
        let row = self.selected_row();
        let Row::Setting(key) = row else {
            return Screen::Panes { cursor: 0 };
        };
        if !key.allows(self.target) {
            self.notice = Some(refusal(key));
            return Screen::Settings;
        }
        match key.shape() {
            SettingShape::Scalar => Screen::Scalar {
                key,
                buffer: self.starting_text(view, key),
            },
            SettingShape::List => Screen::List(ListEdit {
                target: ListTarget::Setting(key),
                items: self.starting_items(view, key),
                cursor: 0,
            }),
        }
    }

    fn clear_selected(&mut self) -> ConfigAction {
        let Row::Setting(key) = self.selected_row() else {
            // Clearing a layout is clearing a list; the panes screen is where
            // that happens, with the copy rule in front of it.
            self.notice = Some("the layout is edited on the panes screen".to_owned());
            return ConfigAction::None;
        };
        if !key.allows(self.target) {
            self.notice = Some(refusal(key));
            return ConfigAction::None;
        }
        ConfigAction::Clear {
            slot: self.target,
            key,
        }
    }

    /// What an edit starts from: the target layer's own value, or the value in
    /// force, so changing `C-a` to `C-q` is two keystrokes.
    fn starting_text(&self, view: &ConfigView, key: SettingKey) -> String {
        let Some(row) = view.row(key) else {
            return String::new();
        };
        match row.at(self.target).or(row.effective.as_ref()) {
            Some(SettingValue::Scalar(text)) => text.clone(),
            Some(SettingValue::List(items)) => items.join(" "),
            None => String::new(),
        }
    }

    fn starting_items(&self, view: &ConfigView, key: SettingKey) -> Vec<String> {
        let Some(row) = view.row(key) else {
            return Vec::new();
        };
        match row.at(self.target).or(row.effective.as_ref()) {
            Some(SettingValue::List(items)) => items.clone(),
            Some(SettingValue::Scalar(text)) => vec![text.clone()],
            None => Vec::new(),
        }
    }

    fn on_scalar_key(
        &mut self,
        key: KeyEvent,
        setting: SettingKey,
        mut buffer: String,
    ) -> (ConfigAction, Screen) {
        match key.code {
            KeyCode::Char(c) => buffer.push(c),
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Enter => {
                return (
                    ConfigAction::Set {
                        slot: self.target,
                        key: setting,
                        value: SettingValue::Scalar(buffer),
                    },
                    Screen::Settings,
                )
            }
            KeyCode::Esc => return (ConfigAction::None, Screen::Settings),
            _ => {}
        }
        (
            ConfigAction::None,
            Screen::Scalar {
                key: setting,
                buffer,
            },
        )
    }

    fn on_list_key(
        &mut self,
        key: KeyEvent,
        view: &ConfigView,
        mut edit: ListEdit,
    ) -> (ConfigAction, Screen) {
        let last = edit.items.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => edit.cursor = (edit.cursor + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => edit.cursor = edit.cursor.saturating_sub(1),
            KeyCode::Char('a') => {
                return (
                    ConfigAction::None,
                    Screen::ListItem {
                        edit,
                        buffer: String::new(),
                        appending: true,
                    },
                )
            }
            KeyCode::Enter => {
                let buffer = edit.items.get(edit.cursor).cloned().unwrap_or_default();
                return (
                    ConfigAction::None,
                    Screen::ListItem {
                        edit,
                        buffer,
                        appending: false,
                    },
                );
            }
            KeyCode::Char('d') => {
                if edit.cursor < edit.items.len() {
                    edit.items.remove(edit.cursor);
                    edit.cursor = edit.cursor.min(edit.items.len().saturating_sub(1));
                    return self.commit_list(view, edit);
                }
            }
            // A different act from unsetting the key: `hooks = []` clears an
            // inherited list, an absent `hooks` inherits it.
            KeyCode::Char('D') => {
                edit.items.clear();
                edit.cursor = 0;
                return self.commit_list(view, edit);
            }
            KeyCode::Char('J') => {
                if edit.cursor + 1 < edit.items.len() {
                    edit.items.swap(edit.cursor, edit.cursor + 1);
                    edit.cursor += 1;
                    return self.commit_list(view, edit);
                }
            }
            KeyCode::Char('K') => {
                if edit.cursor > 0 && edit.cursor < edit.items.len() {
                    edit.items.swap(edit.cursor, edit.cursor - 1);
                    edit.cursor -= 1;
                    return self.commit_list(view, edit);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                return (ConfigAction::None, back_from(&edit.target))
            }
            _ => {}
        }
        (ConfigAction::None, Screen::List(edit))
    }

    fn on_list_item_key(
        &mut self,
        key: KeyEvent,
        view: &ConfigView,
        mut edit: ListEdit,
        mut buffer: String,
        appending: bool,
    ) -> (ConfigAction, Screen) {
        match key.code {
            KeyCode::Char(c) => buffer.push(c),
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Enter => {
                if appending {
                    edit.items.push(buffer);
                    edit.cursor = edit.items.len().saturating_sub(1);
                } else {
                    match edit.items.get_mut(edit.cursor) {
                        Some(item) => *item = buffer,
                        None => edit.items.push(buffer),
                    }
                }
                return self.commit_list(view, edit);
            }
            KeyCode::Esc => return (ConfigAction::None, Screen::List(edit)),
            _ => {}
        }
        (
            ConfigAction::None,
            Screen::ListItem {
                edit,
                buffer,
                appending,
            },
        )
    }

    /// A list is written whole, whichever list it is.
    fn commit_list(&mut self, view: &ConfigView, edit: ListEdit) -> (ConfigAction, Screen) {
        match edit.target {
            ListTarget::Setting(key) => (
                ConfigAction::Set {
                    slot: self.target,
                    key,
                    value: SettingValue::List(edit.items.clone()),
                },
                Screen::List(edit),
            ),
            ListTarget::PaneCommand { pane } => {
                let items = edit.items.clone();
                let panes = panes_with(view, pane, |edit| edit.command = Some(items));
                self.commit_panes(
                    view,
                    panes,
                    Screen::List(edit),
                    Screen::PaneDetail { pane, cursor: 0 },
                )
            }
        }
    }

    fn on_panes_key(
        &mut self,
        key: KeyEvent,
        view: &ConfigView,
        mut cursor: usize,
    ) -> (ConfigAction, Screen) {
        let panes = &view.panes.panes;
        let last = panes.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => cursor = (cursor + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                if cursor < panes.len() {
                    return (
                        ConfigAction::None,
                        Screen::PaneDetail {
                            pane: cursor,
                            cursor: 0,
                        },
                    );
                }
            }
            KeyCode::Char('a') => {
                return (
                    ConfigAction::None,
                    Screen::PaneKind {
                        pane: None,
                        cursor: 0,
                    },
                )
            }
            KeyCode::Char('d') => {
                if cursor < panes.len() {
                    let mut next = panes.clone();
                    next.remove(cursor);
                    let after = Screen::Panes {
                        cursor: cursor.min(next.len().saturating_sub(1)),
                    };
                    return self.commit_panes(view, next, after, Screen::Panes { cursor });
                }
            }
            KeyCode::Char('J') => {
                if cursor + 1 < panes.len() {
                    let mut next = panes.clone();
                    next.swap(cursor, cursor + 1);
                    let after = Screen::Panes { cursor: cursor + 1 };
                    return self.commit_panes(view, next, after, Screen::Panes { cursor });
                }
            }
            KeyCode::Char('K') => {
                if cursor > 0 && cursor < panes.len() {
                    let mut next = panes.clone();
                    next.swap(cursor, cursor - 1);
                    let after = Screen::Panes { cursor: cursor - 1 };
                    return self.commit_panes(view, next, after, Screen::Panes { cursor });
                }
            }
            KeyCode::Esc | KeyCode::Char('q') => return (ConfigAction::None, Screen::Settings),
            _ => {}
        }
        (ConfigAction::None, Screen::Panes { cursor })
    }

    fn on_pane_kind_key(
        &mut self,
        key: KeyEvent,
        view: &ConfigView,
        pane: Option<usize>,
        mut cursor: usize,
    ) -> (ConfigAction, Screen) {
        let last = PaneKindTag::ALL.len().saturating_sub(1);
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => cursor = (cursor + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Enter => {
                let Some(kind) = PaneKindTag::ALL.get(cursor).copied() else {
                    return (ConfigAction::None, Screen::PaneKind { pane, cursor });
                };
                return match pane {
                    None => {
                        let mut next = view.panes.panes.clone();
                        next.push(PaneEdit::new(kind));
                        let index = next.len().saturating_sub(1);
                        self.commit_panes(
                            view,
                            next,
                            Screen::PaneDetail {
                                pane: index,
                                cursor: 0,
                            },
                            Screen::Panes { cursor: index },
                        )
                    }
                    Some(index) => self.change_kind(view, index, kind),
                };
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                return (
                    ConfigAction::None,
                    match pane {
                        Some(pane) => Screen::PaneDetail { pane, cursor: 0 },
                        None => Screen::Panes { cursor: 0 },
                    },
                )
            }
            _ => {}
        }
        (ConfigAction::None, Screen::PaneKind { pane, cursor })
    }

    /// Switching kind is the one edit that destroys fields, so it says which
    /// ones first.
    fn change_kind(
        &mut self,
        view: &ConfigView,
        index: usize,
        kind: PaneKindTag,
    ) -> (ConfigAction, Screen) {
        let Some(current) = view.panes.panes.get(index) else {
            return (ConfigAction::None, Screen::Panes { cursor: 0 });
        };
        let lost = current.fields_lost_changing_to(kind);
        let next = panes_with(view, index, |edit| *edit = edit.change_kind(kind));
        let after = Screen::PaneDetail {
            pane: index,
            cursor: 0,
        };
        let back = Screen::PaneDetail {
            pane: index,
            cursor: 0,
        };
        if lost.is_empty() {
            return self.commit_panes(view, next, after, back);
        }
        let dropped: Vec<&str> = lost.iter().map(|field| field.label()).collect();
        let mut lines = vec![format!(
            "a {} pane has no {}, so changing kind drops {}",
            kind.as_str(),
            dropped.join(" and "),
            if dropped.len() == 1 {
                "that field"
            } else {
                "those fields"
            }
        )];
        if let Some(copy) = self.copy_notice(view) {
            lines.push(copy);
        }
        match self.pane_target_refusal(view) {
            Some(refusal) => {
                self.notice = Some(refusal);
                (ConfigAction::None, back)
            }
            None => (
                ConfigAction::None,
                Screen::Confirm {
                    lines,
                    action: ConfigAction::SetPanes {
                        slot: self.target,
                        panes: next,
                    },
                    on_yes: Box::new(after),
                    on_no: Box::new(back),
                },
            ),
        }
    }

    fn on_pane_detail_key(
        &mut self,
        key: KeyEvent,
        view: &ConfigView,
        pane: usize,
        mut cursor: usize,
    ) -> (ConfigAction, Screen) {
        let Some(edit) = view.panes.panes.get(pane) else {
            return (ConfigAction::None, Screen::Panes { cursor: 0 });
        };
        let fields = edit.kind.fields();
        // Row 0 is the kind; the rest are its fields.
        let last = fields.len();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => cursor = (cursor + 1).min(last),
            KeyCode::Char('k') | KeyCode::Up => cursor = cursor.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => {
                if cursor == 0 {
                    return (
                        ConfigAction::None,
                        Screen::PaneKind {
                            pane: Some(pane),
                            cursor: kind_index(edit.kind),
                        },
                    );
                }
                let Some(field) = fields.get(cursor.saturating_sub(1)).copied() else {
                    return (ConfigAction::None, Screen::PaneDetail { pane, cursor });
                };
                // A command is a list, and editing it as one is what keeps an
                // argument containing a space intact.
                if field == PaneField::Command {
                    return (
                        ConfigAction::None,
                        Screen::List(ListEdit {
                            target: ListTarget::PaneCommand { pane },
                            items: edit.command.clone().unwrap_or_default(),
                            cursor: 0,
                        }),
                    );
                }
                return (
                    ConfigAction::None,
                    Screen::PaneField {
                        pane,
                        field,
                        cursor,
                        buffer: edit.field(field).unwrap_or_default(),
                    },
                );
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                return (ConfigAction::None, Screen::Panes { cursor: pane })
            }
            _ => {}
        }
        (ConfigAction::None, Screen::PaneDetail { pane, cursor })
    }

    fn on_pane_field_key(
        &mut self,
        key: KeyEvent,
        view: &ConfigView,
        pane: usize,
        field: PaneField,
        cursor: usize,
        mut buffer: String,
    ) -> (ConfigAction, Screen) {
        match key.code {
            KeyCode::Char(c) => buffer.push(c),
            KeyCode::Backspace => {
                buffer.pop();
            }
            KeyCode::Enter => {
                let value = buffer.clone();
                let panes = panes_with(view, pane, |edit| {
                    let text = (!value.is_empty()).then(|| value.clone());
                    match field {
                        PaneField::Name => edit.name = text,
                        PaneField::Title => edit.title = text,
                        PaneField::Every => edit.every = text,
                        PaneField::Size => edit.size = text,
                        PaneField::Command => {
                            edit.command = (!value.is_empty())
                                .then(|| value.split_whitespace().map(str::to_owned).collect());
                        }
                    }
                });
                let after = Screen::PaneDetail { pane, cursor };
                let back = Screen::PaneDetail { pane, cursor };
                return self.commit_panes(view, panes, after, back);
            }
            KeyCode::Esc => return (ConfigAction::None, Screen::PaneDetail { pane, cursor }),
            _ => {}
        }
        (
            ConfigAction::None,
            Screen::PaneField {
                pane,
                field,
                cursor,
                buffer,
            },
        )
    }

    /// Emit a pane list, with the two things that have to be said first.
    ///
    /// A layer below the one that owns the layout cannot be written to at all -
    /// the edit would vanish from the screen as it was made - and a layout this
    /// layer does not own yet is copied wholesale, because arrays replace.
    fn commit_panes(
        &mut self,
        view: &ConfigView,
        panes: Vec<PaneEdit>,
        on_yes: Screen,
        on_no: Screen,
    ) -> (ConfigAction, Screen) {
        if let Some(refusal) = self.pane_target_refusal(view) {
            self.notice = Some(refusal);
            return (ConfigAction::None, on_no);
        }
        match self.copy_notice(view) {
            None => (
                ConfigAction::SetPanes {
                    slot: self.target,
                    panes,
                },
                on_yes,
            ),
            Some(notice) => (
                ConfigAction::None,
                Screen::Confirm {
                    lines: vec![notice],
                    action: ConfigAction::SetPanes {
                        slot: self.target,
                        panes,
                    },
                    on_yes: Box::new(on_yes),
                    on_no: Box::new(on_no),
                },
            ),
        }
    }

    /// Why this layer cannot hold the layout at all, if it cannot.
    fn pane_target_refusal(&self, view: &ConfigView) -> Option<String> {
        match view.panes.origin {
            Origin::Layer(owner) if owner > self.target => Some(format!(
                "the {} layer defines the layout and wins over {}; \
                 switch the layer with tab",
                owner.label(),
                self.target.label()
            )),
            _ => None,
        }
    }

    /// What editing an inherited layout is about to do, if anything.
    fn copy_notice(&self, view: &ConfigView) -> Option<String> {
        if view.panes.origin == Origin::Layer(self.target) {
            return None;
        }
        let source = match view.panes.origin {
            Origin::Default => "zax's built-in default".to_owned(),
            Origin::Layer(slot) => format!("the {} layer", slot.label()),
        };
        Some(format!(
            "the layout comes from {source}; editing copies all {} panes into your {} layer",
            view.panes.panes.len(),
            self.target.label()
        ))
    }

    /// What a save did, so the modal can report what is still pending.
    pub fn report_saved(&mut self, saved: Saved) {
        self.screen = Screen::Report {
            saved: Some(saved),
            error: None,
        };
    }

    /// A save that wrote nothing, in the words the host has for it.
    pub fn report_error(&mut self, message: String) {
        self.screen = Screen::Report {
            saved: None,
            error: Some(message),
        };
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, view: &ConfigView) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let width = usize::from(area.width);
        let (body, focus, hint) = match &self.screen {
            Screen::Settings => self.render_settings(view, width),
            Screen::Scalar { key, buffer } => (
                vec![
                    blank(),
                    field_line(key.label(), buffer, true),
                    help_line(key.help()),
                ],
                1,
                "enter save   esc cancel",
            ),
            Screen::List(edit) => self.render_list(view, edit, width),
            Screen::ListItem {
                edit,
                buffer,
                appending,
            } => (
                vec![
                    blank(),
                    field_line(if *appending { "new item" } else { "item" }, buffer, true),
                    help_line(&format!(
                        "{} of {}",
                        edit.cursor.saturating_add(1),
                        edit.items.len().max(1)
                    )),
                ],
                1,
                "enter save   esc cancel",
            ),
            Screen::Panes { cursor } => self.render_panes(view, *cursor, width),
            Screen::PaneKind { cursor, .. } => (
                self.render_kinds(*cursor),
                *cursor + 1,
                "enter choose   esc back",
            ),
            Screen::PaneDetail { pane, cursor } => {
                self.render_pane_detail(view, *pane, *cursor, width)
            }
            Screen::PaneField {
                pane,
                field,
                buffer,
                ..
            } => (
                vec![
                    blank(),
                    field_line(field.label(), buffer, true),
                    help_line(&pane_field_help(view, *pane, *field)),
                ],
                1,
                "enter save   esc cancel",
            ),
            Screen::Confirm { lines, .. } => (
                std::iter::once(blank())
                    .chain(
                        lines
                            .iter()
                            .flat_map(|line| wrap(line, width.saturating_sub(3)))
                            .map(|text| Line::from(format!("  {text}"))),
                    )
                    .collect(),
                0,
                "y yes   n no",
            ),
            Screen::Help { .. } => (help_screen(), 0, "esc back"),
            Screen::Report { saved, error } => (
                self.render_report(saved.as_ref(), error.as_deref(), width),
                0,
                "esc back",
            ),
        };

        let title = self.title();
        let mut lines = vec![header_line(
            &title,
            &format!("writing to: {} \u{27e8}tab\u{27e9}", self.target.label()),
            true,
            area.width,
        )];
        lines.extend(body);

        let footer = self.footer(view, hint, area.width);
        draw(frame, area, lines, footer, focus + 1);
    }

    fn title(&self) -> String {
        match &self.screen {
            Screen::Settings => "zax config".to_owned(),
            Screen::Scalar { key, .. } => format!("zax config \u{203a} {}", key.full_name()),
            Screen::List(ListEdit { target, .. })
            | Screen::ListItem {
                edit: ListEdit { target, .. },
                ..
            } => match target {
                ListTarget::Setting(key) => format!("zax config \u{203a} {}", key.full_name()),
                ListTarget::PaneCommand { pane } => {
                    format!(
                        "zax config \u{203a} panes \u{203a} {} \u{203a} command",
                        pane + 1
                    )
                }
            },
            Screen::Panes { .. } => "zax config \u{203a} panes".to_owned(),
            Screen::PaneKind { pane, .. } => match pane {
                Some(pane) => format!(
                    "zax config \u{203a} panes \u{203a} {} \u{203a} kind",
                    pane + 1
                ),
                None => "zax config \u{203a} panes \u{203a} add".to_owned(),
            },
            Screen::PaneDetail { pane, .. } | Screen::PaneField { pane, .. } => {
                format!("zax config \u{203a} panes \u{203a} {}", pane + 1)
            }
            Screen::Confirm { .. } => "zax config \u{203a} confirm".to_owned(),
            Screen::Help { .. } => "zax config \u{203a} keys".to_owned(),
            Screen::Report { .. } => "zax config \u{203a} saved".to_owned(),
        }
    }

    fn render_settings(
        &self,
        view: &ConfigView,
        width: usize,
    ) -> (Vec<Line<'static>>, usize, &'static str) {
        let show_origin = width >= ORIGIN_COLUMN_AT;
        let mut lines = vec![blank()];
        let mut focus = 0;
        let mut group: Option<&str> = None;

        for (index, row) in Row::ALL.iter().enumerate() {
            if group != Some(row.group()) {
                group = Some(row.group());
                lines.push(Line::styled(
                    format!(" {}", row.group()),
                    Style::default().fg(Color::Cyan),
                ));
            }
            let selected = index == self.cursor;
            if selected {
                focus = lines.len();
            }
            match row {
                Row::Setting(key) => {
                    let found = view.row(*key);
                    let value = found.map(SettingRow::display_effective).unwrap_or_default();
                    let origin = found.map(|r| r.origin.label()).unwrap_or("default");
                    let changed = found.is_some_and(|r| r.changed);
                    lines.push(row_line(
                        row.label(),
                        &value,
                        origin,
                        changed,
                        selected,
                        width,
                        show_origin,
                    ));
                    if selected {
                        lines.extend(self.expansion(found, *key, width));
                    }
                }
                Row::Panes => {
                    let count = view.panes.panes.len();
                    lines.push(row_line(
                        row.label(),
                        &format!("{count} pane{}", if count == 1 { "" } else { "s" }),
                        view.panes.origin.label(),
                        view.panes.changed,
                        selected,
                        width,
                        show_origin,
                    ));
                    if selected {
                        lines.push(help_line(
                            "the panes around the agent; one layer owns the whole list",
                        ));
                    }
                }
            }
        }
        (
            lines,
            focus,
            "enter edit   x clear   tab layer   s save   ? help",
        )
    }

    /// Provenance detail with no extra screen and no extra key.
    fn expansion(
        &self,
        row: Option<&SettingRow>,
        key: SettingKey,
        width: usize,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();
        for slot in LayerSlot::ALL {
            let value = row
                .map(|row| row.display_at(slot))
                .unwrap_or_else(|| "(unset)".to_owned());
            lines.push(detail_line(slot.label(), &value, width));
        }
        lines.push(detail_line("default", key.default_display(), width));
        lines.push(help_line(key.help()));
        lines
    }

    fn render_list(
        &self,
        view: &ConfigView,
        edit: &ListEdit,
        width: usize,
    ) -> (Vec<Line<'static>>, usize, &'static str) {
        let mut lines = vec![blank()];
        let mut focus = 0;
        if edit.items.is_empty() {
            lines.push(help_line("(empty)"));
        }
        for (index, item) in edit.items.iter().enumerate() {
            if index == edit.cursor {
                focus = lines.len();
            }
            lines.push(numbered_line(index, item, index == edit.cursor, width));
        }
        if let ListTarget::Setting(key) = edit.target {
            lines.push(blank());
            lines.push(help_line(key.help()));
        }
        if let Err(error) = &view.status {
            lines.push(blank());
            lines.extend(wrapped_error(error, width));
        }
        (
            lines,
            focus,
            "enter edit   a add   d delete   D empty   J/K move   esc back",
        )
    }

    fn render_panes(
        &self,
        view: &ConfigView,
        cursor: usize,
        width: usize,
    ) -> (Vec<Line<'static>>, usize, &'static str) {
        let mut lines = vec![blank()];
        let mut focus = 0;
        if view.panes.panes.is_empty() {
            lines.push(help_line("(no panes; the agent has the window to itself)"));
        }
        for (index, pane) in view.panes.panes.iter().enumerate() {
            if index == cursor {
                focus = lines.len();
            }
            lines.push(pane_line(index, pane, index == cursor, width));
        }
        if let Some(notice) = self.copy_notice(view) {
            lines.push(blank());
            lines.extend(wrapped_warning(&notice, width));
        }
        if let Err(error) = &view.status {
            lines.push(blank());
            lines.extend(wrapped_error(error, width));
        }
        (
            lines,
            focus,
            "enter edit   a add   d delete   J/K move   esc back",
        )
    }

    fn render_kinds(&self, cursor: usize) -> Vec<Line<'static>> {
        let mut lines = vec![blank()];
        for (index, kind) in PaneKindTag::ALL.into_iter().enumerate() {
            lines.push(Line::from(format!(
                " {} {:<9} {}",
                if index == cursor { "\u{203a}" } else { " " },
                kind.as_str(),
                kind_help(kind)
            )));
        }
        lines
    }

    fn render_pane_detail(
        &self,
        view: &ConfigView,
        pane: usize,
        cursor: usize,
        width: usize,
    ) -> (Vec<Line<'static>>, usize, &'static str) {
        let Some(edit) = view.panes.panes.get(pane) else {
            return (vec![help_line("(no such pane)")], 0, "esc back");
        };
        let mut lines = vec![blank()];
        let mut focus = 0;
        let mut push = |index: usize, label: &str, value: String| {
            if index == cursor {
                focus = lines.len();
            }
            lines.push(field_row(label, &value, index == cursor, width));
        };
        push(0, "kind", edit.kind.as_str().to_owned());
        for (offset, field) in edit.kind.fields().iter().enumerate() {
            push(
                offset + 1,
                field.label(),
                edit.field(*field).unwrap_or_else(|| "(unset)".to_owned()),
            );
        }
        if let Err(error) = &view.status {
            lines.push(blank());
            lines.extend(wrapped_error(error, width));
        }
        (lines, focus, "enter edit   esc back")
    }

    /// Only for what this editing session changed, and worded for where it runs.
    fn render_report(
        &self,
        saved: Option<&Saved>,
        error: Option<&str>,
        width: usize,
    ) -> Vec<Line<'static>> {
        let mut lines = vec![blank()];
        if let Some(error) = error {
            lines.extend(wrapped_error(error, width));
            lines.push(blank());
            lines.push(help_line(
                "r reloads that layer; your edits stay in the buffer",
            ));
            return lines;
        }
        let Some(saved) = saved else { return lines };

        let written: Vec<&str> = saved.written.iter().map(|slot| slot.label()).collect();
        lines.push(Line::from(vec![
            Span::styled(format!(" {GLYPH_PASS} "), Style::default().fg(Color::Green)),
            Span::raw(if written.is_empty() {
                "nothing to save".to_owned()
            } else {
                format!("saved {}", written.join(" and "))
            }),
        ]));
        for pending in &saved.pending {
            let text = self.pending_notice(*pending);
            for (index, line) in wrap(&text, width.saturating_sub(4)).into_iter().enumerate() {
                lines.push(if index == 0 {
                    Line::from(format!(" \u{25cf} {line}"))
                } else {
                    Line::from(format!("   {line}"))
                });
            }
        }
        lines
    }

    fn pending_notice(&self, pending: Pending) -> String {
        match pending {
            Pending::Setting(SettingKey::TmuxPrefix) => {
                "tmux.prefix cannot change in a running server - stop every zax session, \
                 or `tmux -L zax kill-server`, then start again"
                    .to_owned()
            }
            Pending::Setting(SettingKey::AgentCommand) => {
                if self.inside_session {
                    "agent.command changed - this session keeps the agent it started with"
                        .to_owned()
                } else {
                    "agent.command changed - the next `zax session start` will use it".to_owned()
                }
            }
            Pending::Setting(SettingKey::RepoKeyName) => {
                "name changed - your personal layer is a different file now, and this \
                 editor still has the old one open"
                    .to_owned()
            }
            Pending::Setting(key) => format!("{} changed", key.full_name()),
            Pending::Panes => {
                if self.inside_session {
                    "the layout changed - a new session will use it; this one keeps the \
                     panes it was built with"
                        .to_owned()
                } else {
                    "the layout changed - the next `zax session start` will use it".to_owned()
                }
            }
        }
    }

    /// Per-layer dirty state and the save hint - replaced by whatever is more
    /// urgent: a refusal just now, or a draft that will not start a session.
    ///
    /// The main screen is where the three layers are in view, so that is where
    /// their dirty state belongs; every other screen gives the line to its own
    /// keys and keeps only the fact that something is unsaved.
    fn footer(&self, view: &ConfigView, hint: &str, width: u16) -> Line<'static> {
        if let Some(notice) = &self.notice {
            return error_line(&truncate(notice, usize::from(width).saturating_sub(2)));
        }
        if let Err(error) = &view.status {
            return error_line(&truncate(error, usize::from(width).saturating_sub(2)));
        }
        let marks = LayerSlot::ALL
            .map(|slot| {
                let dirty = view.layer(slot).is_some_and(|layer| layer.dirty);
                format!(
                    "{} {}",
                    slot.initial(),
                    if dirty { "*" } else { GLYPH_NONE }
                )
            })
            .join("   ");
        let right = if view.any_dirty() {
            format!("unsaved in {} - s to save", dirty_layers(view))
        } else {
            hint.to_owned()
        };
        header_line(&marks, &right, false, width)
    }
}

/// Below this width the origin column is dropped: it is the first thing to go,
/// because a value nobody can read is worse than a value whose layer is not
/// named on the row that is already expanded to say so.
const ORIGIN_COLUMN_AT: usize = 56;
const LABEL_WIDTH: usize = 18;

fn refusal(key: SettingKey) -> String {
    let slots: Vec<&str> = key.allowed_slots().iter().map(|s| s.label()).collect();
    format!(
        "`{}` is only read from the {} layer; switch the layer with tab",
        key.full_name(),
        slots.join(" or ")
    )
}

fn dirty_layers(view: &ConfigView) -> String {
    view.layers
        .iter()
        .filter(|layer| layer.dirty)
        .map(|layer| layer.slot.label())
        .collect::<Vec<_>>()
        .join(" and ")
}

fn back_from(target: &ListTarget) -> Screen {
    match target {
        ListTarget::Setting(_) => Screen::Settings,
        ListTarget::PaneCommand { pane } => Screen::PaneDetail {
            pane: *pane,
            cursor: 0,
        },
    }
}

/// One pane of a list, with `edit` applied to it.
fn panes_with(view: &ConfigView, index: usize, edit: impl FnOnce(&mut PaneEdit)) -> Vec<PaneEdit> {
    let mut panes = view.panes.panes.clone();
    if let Some(pane) = panes.get_mut(index) {
        edit(pane);
    }
    panes
}

fn kind_index(kind: PaneKindTag) -> usize {
    PaneKindTag::ALL
        .iter()
        .position(|candidate| *candidate == kind)
        .unwrap_or(0)
}

fn kind_help(kind: PaneKindTag) -> String {
    match kind {
        PaneKindTag::Builtin => format!("a pane zax ships: {}", builtin_names()),
        PaneKindTag::Exec => "a command zax re-runs and wraps in a header".to_owned(),
        PaneKindTag::Raw => "a command that owns the pane outright".to_owned(),
    }
}

/// The built-ins, read from the domain rather than written out again. Three
/// places used to spell this list, and they drifted the moment a fourth
/// built-in was added.
fn builtin_names() -> String {
    BuiltinPane::ALL
        .iter()
        .map(|b| b.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

fn pane_field_help(view: &ConfigView, pane: usize, field: PaneField) -> String {
    let kind = view
        .panes
        .panes
        .get(pane)
        .map(|edit| edit.kind.as_str())
        .unwrap_or("pane");
    match field {
        PaneField::Name => format!("a built-in pane: {}", builtin_names()),
        PaneField::Title => "the header this pane carries".to_owned(),
        PaneField::Every => "how often to re-run it: 30s, 5m, 2h".to_owned(),
        PaneField::Command => "the program and its arguments".to_owned(),
        PaneField::Size => format!("this {kind} pane's share of the window, such as 30%"),
    }
}

fn blank() -> Line<'static> {
    Line::from(String::new())
}

fn help_line(text: &str) -> Line<'static> {
    Line::styled(format!("     {text}"), Style::default().fg(Color::DarkGray))
}

fn field_line(label: &str, buffer: &str, focused: bool) -> Line<'static> {
    let style = if focused {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::raw(format!("   {label:<10} ")),
        Span::styled(format!("{buffer}\u{2588}"), style),
    ])
}

fn field_row(label: &str, value: &str, selected: bool, width: usize) -> Line<'static> {
    let marker = if selected { "\u{203a}" } else { " " };
    let text = format!(" {marker} {label:<10} {value}");
    Line::styled(truncate(&text, width), row_style(selected))
}

fn numbered_line(index: usize, item: &str, selected: bool, width: usize) -> Line<'static> {
    let marker = if selected { "\u{203a}" } else { " " };
    let text = format!(" {marker} {:<3}{item}", index + 1);
    Line::styled(truncate(&text, width), row_style(selected))
}

fn pane_line(index: usize, pane: &PaneEdit, selected: bool, width: usize) -> Line<'static> {
    let marker = if selected { "\u{203a}" } else { " " };
    let detail = match pane.kind {
        PaneKindTag::Builtin => pane.name.clone().unwrap_or_else(|| "(no name)".to_owned()),
        PaneKindTag::Exec => {
            let title = pane
                .title
                .clone()
                .unwrap_or_else(|| "(no title)".to_owned());
            let command = pane.field(PaneField::Command).unwrap_or_default();
            let every = pane
                .every
                .as_ref()
                .map(|every| format!("  every {every}"))
                .unwrap_or_default();
            format!("{title} - {command}{every}")
        }
        PaneKindTag::Raw => pane
            .field(PaneField::Command)
            .unwrap_or_else(|| "(no command)".to_owned()),
    };
    let left = format!(
        " {marker} {:<2} {:<8} {detail}",
        index + 1,
        pane.kind.as_str()
    );
    let size = pane.size.clone().unwrap_or_default();
    Line::styled(right_align(&left, &size, width), row_style(selected))
}

fn detail_line(label: &str, value: &str, width: usize) -> Line<'static> {
    Line::styled(
        truncate(&format!("       {label:<10} {value}"), width),
        Style::default().fg(Color::DarkGray),
    )
}

fn row_style(selected: bool) -> Style {
    if selected {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    }
}

/// `  label              value                      * origin`
fn row_line(
    label: &str,
    value: &str,
    origin: &str,
    changed: bool,
    selected: bool,
    width: usize,
    show_origin: bool,
) -> Line<'static> {
    let marker = if selected { "\u{203a}" } else { " " };
    let left = format!(" {marker} {label:<LABEL_WIDTH$} {value}");
    let right = if show_origin {
        format!("{} {origin}", if changed { "*" } else { " " })
    } else if changed {
        "*".to_owned()
    } else {
        String::new()
    };
    Line::styled(right_align(&left, &right, width), row_style(selected))
}

/// `left` padded so `right` ends at the last column, measured in terminal
/// columns rather than characters, and `right` keeps its space when the two
/// cannot both fit.
fn right_align(left: &str, right: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    let right = truncate(right, width);
    let right_width = display_width(&right);
    let left_budget = width.saturating_sub(right_width.saturating_add(1));
    let left = truncate(left, left_budget);
    let gap = width
        .saturating_sub(display_width(&left))
        .saturating_sub(right_width);
    format!("{left}{}{right}", " ".repeat(gap))
}

fn wrapped_error(message: &str, width: usize) -> Vec<Line<'static>> {
    wrap(message, width.saturating_sub(3))
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                error_line(&line)
            } else {
                Line::styled(format!("  {line}"), Style::default().fg(Color::Yellow))
            }
        })
        .collect()
}

fn wrapped_warning(message: &str, width: usize) -> Vec<Line<'static>> {
    wrap(message, width.saturating_sub(4))
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let text = if index == 0 {
                format!(" {GLYPH_ERROR} {line}")
            } else {
                format!("   {line}")
            };
            Line::styled(text, Style::default().fg(Color::Yellow))
        })
        .collect()
}

/// Break text on spaces to fit `width` terminal columns. A word longer than the
/// width is truncated rather than allowed to overflow the popup.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate = if current.is_empty() {
            word.to_owned()
        } else {
            format!("{current} {word}")
        };
        if display_width(&candidate) <= width {
            current = candidate;
        } else {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            if display_width(word) > width {
                lines.push(truncate(word, width));
            } else {
                current = word.to_owned();
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn help_screen() -> Vec<Line<'static>> {
    let keys = [
        ("j / k", "move the selection"),
        ("enter", "edit the selected setting in the target layer"),
        ("x", "clear it there - back to inherited or default"),
        ("tab", "cycle the layer edits land in"),
        ("s", "save every layer this editor changed"),
        ("r", "re-read the target layer from disk"),
        ("esc / q", "back, or close"),
        ("?", "this list"),
    ];
    let mut lines = vec![blank()];
    for (key, what) in keys {
        lines.push(Line::from(format!("   {key:<9} {what}")));
    }
    lines.push(blank());
    lines.push(help_line(
        "on a list: a add   d delete   D empty   J/K move",
    ));
    lines.push(help_line(
        "edits are staged until `s`; nothing is written before that",
    ));
    lines
}

/// The body, scrolled so the selected line is visible, with the footer pinned to
/// the last row.
fn draw(
    frame: &mut Frame,
    area: Rect,
    lines: Vec<Line<'static>>,
    footer: Line<'static>,
    focus: usize,
) {
    let height = area.height;
    if height == 0 {
        return;
    }
    if height == 1 {
        frame.render_widget(Paragraph::new(Text::from(lines)), area);
        return;
    }
    let body_height = height.saturating_sub(1);
    let scroll = u16::try_from(focus.saturating_sub(usize::from(body_height).saturating_sub(1)))
        .unwrap_or(u16::MAX);
    let body = Rect {
        height: body_height,
        ..area
    };
    let footer_area = Rect {
        y: area.y.saturating_add(body_height),
        height: 1,
        ..area
    };
    frame.render_widget(Paragraph::new(Text::from(lines)).scroll((scroll, 0)), body);
    frame.render_widget(Paragraph::new(Text::from(vec![footer])), footer_area);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::PathBuf;

    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::KeyModifiers;
    use ratatui::Terminal;
    use zax_domain::settings::{LayerInfo, Origin, PanesView, Pending, SettingRow};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(text: &str) -> Vec<KeyEvent> {
        text.chars().map(|c| key(KeyCode::Char(c))).collect()
    }

    fn scalar(text: &str) -> SettingValue {
        SettingValue::Scalar(text.to_owned())
    }

    fn list(items: &[&str]) -> SettingValue {
        SettingValue::List(items.iter().map(|s| (*s).to_owned()).collect())
    }

    /// A view with nothing configured anywhere: every row on its default.
    fn empty_view() -> ConfigView {
        ConfigView {
            settings: SettingKey::ALL
                .into_iter()
                .map(|key| SettingRow::new(key, [None, None, None]))
                .collect(),
            panes: PanesView {
                panes: vec![
                    builtin("prs-for-review", "30%"),
                    builtin("branch-status", "25%"),
                ],
                origin: Origin::Default,
                changed: false,
            },
            layers: [
                layer(LayerSlot::Global, false),
                layer(LayerSlot::Repo, false),
                layer(LayerSlot::Personal, false),
            ],
            status: Ok(()),
        }
    }

    fn layer(slot: LayerSlot, dirty: bool) -> LayerInfo {
        LayerInfo {
            slot,
            path: PathBuf::from(format!("/tmp/{}.toml", slot.label())),
            exists: true,
            dirty,
        }
    }

    fn builtin(name: &str, size: &str) -> PaneEdit {
        PaneEdit {
            kind: PaneKindTag::Builtin,
            name: Some(name.to_owned()),
            title: None,
            every: None,
            command: None,
            size: Some(size.to_owned()),
        }
    }

    fn with_row(
        mut view: ConfigView,
        key: SettingKey,
        per_layer: [Option<SettingValue>; 3],
    ) -> ConfigView {
        for row in &mut view.settings {
            if row.key == key {
                *row = SettingRow::new(key, per_layer.clone());
            }
        }
        view
    }

    /// Feed a sequence of keys, returning the action the last one produced.
    fn press(modal: &mut ConfigModal, view: &ConfigView, keys: &[KeyEvent]) -> ConfigAction {
        let mut action = ConfigAction::None;
        for k in keys {
            action = modal.handle_key(*k, view);
        }
        action
    }

    fn buffer_of(modal: &ConfigModal, view: &ConfigView, width: u16, height: u16) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| modal.render(frame, frame.area(), view))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn screen(modal: &ConfigModal, view: &ConfigView) -> String {
        rendered(modal, view, 78, 24)
    }

    fn rendered(modal: &ConfigModal, view: &ConfigView, width: u16, height: u16) -> String {
        let buffer = buffer_of(modal, view, width, height);
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Move the cursor onto a row by its label, so a test does not depend on how
    /// many rows happen to come before it.
    fn select(modal: &mut ConfigModal, view: &ConfigView, label: &str) {
        for _ in 0..20 {
            if selected_line(modal, view).contains(label) {
                return;
            }
            modal.handle_key(key(KeyCode::Char('j')), view);
        }
        panic!("no row labelled {label}");
    }

    fn selected_line(modal: &ConfigModal, view: &ConfigView) -> String {
        screen(modal, view)
            .lines()
            .find(|line| line.trim_start().starts_with('\u{203a}'))
            .unwrap_or_default()
            .to_owned()
    }

    #[test]
    fn the_header_names_the_layer_edits_land_in() {
        let modal = ConfigModal::new(true);
        let text = screen(&modal, &empty_view());
        assert!(text.contains("zax config"), "got:\n{text}");
        assert!(text.contains("writing to: personal"), "got:\n{text}");
    }

    #[test]
    fn tab_cycles_the_target_layer_and_the_next_edit_lands_there() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        assert_eq!(modal.target(), LayerSlot::Personal);

        modal.handle_key(key(KeyCode::Tab), &view);
        assert_eq!(modal.target(), LayerSlot::Global);
        assert!(screen(&modal, &view).contains("writing to: global"));

        select(&mut modal, &view, "prefix");
        let action = press(
            &mut modal,
            &view,
            &[
                vec![key(KeyCode::Enter)],
                typed("C-q"),
                vec![key(KeyCode::Enter)],
            ]
            .concat(),
        );
        assert_eq!(
            action,
            ConfigAction::Set {
                slot: LayerSlot::Global,
                key: SettingKey::TmuxPrefix,
                value: scalar("C-q"),
            }
        );
    }

    #[test]
    fn shift_tab_cycles_the_target_layer_backwards() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        modal.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT), &view);
        assert_eq!(modal.target(), LayerSlot::Repo);
    }

    #[test]
    fn a_scalar_edit_starts_from_the_value_the_target_layer_already_has() {
        // So changing `C-a` to `C-q` is two keystrokes, not a retyped value.
        let view = with_row(
            empty_view(),
            SettingKey::TmuxPrefix,
            [None, None, Some(scalar("C-a"))],
        );
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "prefix");
        modal.handle_key(key(KeyCode::Enter), &view);

        let text = screen(&modal, &view);
        assert!(text.contains("C-a"), "got:\n{text}");

        let action = press(
            &mut modal,
            &view,
            &[
                vec![key(KeyCode::Backspace)],
                typed("q"),
                vec![key(KeyCode::Enter)],
            ]
            .concat(),
        );
        assert_eq!(
            action,
            ConfigAction::Set {
                slot: LayerSlot::Personal,
                key: SettingKey::TmuxPrefix,
                value: scalar("C-q"),
            }
        );
    }

    #[test]
    fn escape_leaves_an_edit_without_emitting_anything() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "prefix");

        let action = press(
            &mut modal,
            &view,
            &[
                vec![key(KeyCode::Enter)],
                typed("C-q"),
                vec![key(KeyCode::Esc)],
            ]
            .concat(),
        );

        assert_eq!(action, ConfigAction::None);
        // And it is the settings screen again, not a closed modal.
        assert!(screen(&modal, &view).contains("writing to:"));
    }

    #[test]
    fn x_clears_the_setting_in_the_target_layer() {
        let view = with_row(
            empty_view(),
            SettingKey::TmuxPrefix,
            [None, None, Some(scalar("C-q"))],
        );
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "prefix");

        assert_eq!(
            modal.handle_key(key(KeyCode::Char('x')), &view),
            ConfigAction::Clear {
                slot: LayerSlot::Personal,
                key: SettingKey::TmuxPrefix,
            }
        );
    }

    #[test]
    fn s_saves() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        assert_eq!(
            modal.handle_key(key(KeyCode::Char('s')), &view),
            ConfigAction::Save
        );
    }

    #[test]
    fn a_row_the_target_layer_may_not_define_says_so_rather_than_writing() {
        // `name` is read from the repository layer alone: written anywhere else
        // it parses and is then ignored.
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "name");

        assert_eq!(
            modal.handle_key(key(KeyCode::Enter), &view),
            ConfigAction::None
        );
        let text = screen(&modal, &view);
        assert!(text.contains("repo"), "got:\n{text}");
        assert!(
            text.to_lowercase().contains("only"),
            "the refusal has to say why; got:\n{text}"
        );
    }

    #[test]
    fn the_same_row_is_editable_once_the_target_is_the_layer_that_owns_it() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "name");
        modal.handle_key(key(KeyCode::Tab), &view);
        modal.handle_key(key(KeyCode::Tab), &view);
        assert_eq!(modal.target(), LayerSlot::Repo);

        let action = press(
            &mut modal,
            &view,
            &[
                vec![key(KeyCode::Enter)],
                typed("other-key"),
                vec![key(KeyCode::Enter)],
            ]
            .concat(),
        );
        assert_eq!(
            action,
            ConfigAction::Set {
                slot: LayerSlot::Repo,
                key: SettingKey::RepoKeyName,
                value: scalar("other-key"),
            }
        );
    }

    #[test]
    fn the_selected_row_expands_to_every_layer_the_default_and_its_help() {
        let view = with_row(
            empty_view(),
            SettingKey::TmuxPrefix,
            [Some(scalar("C-a")), None, Some(scalar("C-q"))],
        );
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "prefix");

        let text = screen(&modal, &view);
        assert!(text.contains("global"), "got:\n{text}");
        assert!(text.contains("C-a"), "got:\n{text}");
        assert!(text.contains("(unset)"), "got:\n{text}");
        assert!(text.contains("default"), "got:\n{text}");
        assert!(
            text.contains("C-Space"),
            "the built-in default; got:\n{text}"
        );
        assert!(text.contains("tmux prefix key"), "the help; got:\n{text}");
    }

    #[test]
    fn the_origin_column_is_the_first_thing_dropped_when_the_popup_is_narrow() {
        let view = with_row(
            empty_view(),
            SettingKey::TmuxPrefix,
            [None, None, Some(scalar("C-q"))],
        );
        let modal = ConfigModal::new(true);

        let wide = rendered(&modal, &view, 78, 20);
        assert!(wide.contains("personal"), "got:\n{wide}");

        let narrow = rendered(&modal, &view, 34, 20);
        // The value survives; the answer to "which layer" is what gives way.
        assert!(narrow.contains("C-q"), "got:\n{narrow}");
        assert!(
            !narrow
                .lines()
                .any(|line| line.contains("C-q") && line.contains("personal")),
            "got:\n{narrow}"
        );
    }

    #[test]
    fn the_footer_carries_per_layer_dirty_state_and_the_save_hint() {
        let mut view = empty_view();
        view.layers[LayerSlot::Personal.index()].dirty = true;
        let modal = ConfigModal::new(true);

        let text = screen(&modal, &view);
        let footer = text.lines().last().unwrap_or_default();
        assert!(footer.contains('G'), "got: {footer:?}");
        assert!(footer.contains('P'), "got: {footer:?}");
        assert!(text.contains("s to save"), "got:\n{text}");
        assert!(text.contains("unsaved in personal"), "got:\n{text}");
    }

    #[test]
    fn a_validation_error_replaces_the_footer() {
        let mut view = empty_view();
        view.status = Err("invalid `prs.max_age`: `fortnight` is not a duration".to_owned());
        let modal = ConfigModal::new(true);

        let text = screen(&modal, &view);
        assert!(text.contains("fortnight"), "got:\n{text}");
        assert!(!text.contains("s to save"), "got:\n{text}");
    }

    #[test]
    fn escape_with_unsaved_work_confirms_rather_than_closing() {
        let mut view = empty_view();
        view.layers[LayerSlot::Personal.index()].dirty = true;
        let mut modal = ConfigModal::new(true);

        assert_eq!(
            modal.handle_key(key(KeyCode::Esc), &view),
            ConfigAction::None
        );
        let text = screen(&modal, &view);
        assert!(text.to_lowercase().contains("unsaved"), "got:\n{text}");

        assert_eq!(
            modal.handle_key(key(KeyCode::Char('y')), &view),
            ConfigAction::Close
        );
    }

    #[test]
    fn escape_with_nothing_unsaved_closes() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        assert_eq!(
            modal.handle_key(key(KeyCode::Esc), &view),
            ConfigAction::Close
        );
    }

    #[test]
    fn declining_the_close_confirmation_returns_to_the_settings_screen() {
        let mut view = empty_view();
        view.layers[LayerSlot::Repo.index()].dirty = true;
        let mut modal = ConfigModal::new(true);

        modal.handle_key(key(KeyCode::Esc), &view);
        assert_eq!(
            modal.handle_key(key(KeyCode::Char('n')), &view),
            ConfigAction::None
        );
        assert!(screen(&modal, &view).contains("writing to:"));
    }

    #[test]
    fn a_list_setting_opens_a_list_editor_rather_than_a_text_field() {
        let view = with_row(
            empty_view(),
            SettingKey::WorktreeCopy,
            [None, None, Some(list(&[".env", ".envrc"]))],
        );
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "copy");
        modal.handle_key(key(KeyCode::Enter), &view);

        let text = screen(&modal, &view);
        assert!(text.contains(".env"), "got:\n{text}");
        assert!(text.contains(".envrc"), "got:\n{text}");
        assert!(text.contains("a add"), "got:\n{text}");
    }

    #[test]
    fn appending_to_a_list_emits_the_whole_list() {
        let view = with_row(
            empty_view(),
            SettingKey::WorktreeCopy,
            [None, None, Some(list(&[".env"]))],
        );
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "copy");

        let action = press(
            &mut modal,
            &view,
            &[
                vec![key(KeyCode::Enter), key(KeyCode::Char('a'))],
                typed(".envrc"),
                vec![key(KeyCode::Enter)],
            ]
            .concat(),
        );

        assert_eq!(
            action,
            ConfigAction::Set {
                slot: LayerSlot::Personal,
                key: SettingKey::WorktreeCopy,
                value: list(&[".env", ".envrc"]),
            }
        );
    }

    #[test]
    fn deleting_an_item_emits_the_list_without_it() {
        let view = with_row(
            empty_view(),
            SettingKey::WorktreeCopy,
            [None, None, Some(list(&[".env", ".envrc"]))],
        );
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "copy");

        let action = press(
            &mut modal,
            &view,
            &[key(KeyCode::Enter), key(KeyCode::Char('d'))],
        );

        assert_eq!(
            action,
            ConfigAction::Set {
                slot: LayerSlot::Personal,
                key: SettingKey::WorktreeCopy,
                value: list(&[".envrc"]),
            }
        );
    }

    #[test]
    fn items_can_be_reordered_because_hooks_run_in_order() {
        let view = with_row(
            empty_view(),
            SettingKey::WorktreeHooks,
            [None, None, Some(list(&["direnv allow", "pnpm install"]))],
        );
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "hooks");

        let action = press(
            &mut modal,
            &view,
            &[key(KeyCode::Enter), key(KeyCode::Char('J'))],
        );

        assert_eq!(
            action,
            ConfigAction::Set {
                slot: LayerSlot::Personal,
                key: SettingKey::WorktreeHooks,
                value: list(&["pnpm install", "direnv allow"]),
            }
        );
    }

    #[test]
    fn writing_an_explicit_empty_list_is_a_different_key_from_unsetting_it() {
        // `hooks = []` clears an inherited list; an absent `hooks` inherits it.
        // Both have to be reachable, and they cannot be the same keystroke.
        let view = with_row(
            empty_view(),
            SettingKey::WorktreeHooks,
            [Some(list(&["direnv allow"])), None, None],
        );
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "hooks");

        let cleared = press(
            &mut modal,
            &view,
            &[key(KeyCode::Enter), key(KeyCode::Char('D'))],
        );
        assert_eq!(
            cleared,
            ConfigAction::Set {
                slot: LayerSlot::Personal,
                key: SettingKey::WorktreeHooks,
                value: list(&[]),
            }
        );
    }

    #[test]
    fn an_edited_list_item_replaces_that_item_only() {
        let view = with_row(
            empty_view(),
            SettingKey::PrsInclude,
            [None, None, Some(list(&["acme/*", "gameldar/zax"]))],
        );
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "include");

        let action = press(
            &mut modal,
            &view,
            &[
                vec![
                    key(KeyCode::Enter),
                    key(KeyCode::Char('j')),
                    key(KeyCode::Enter),
                ],
                vec![key(KeyCode::Backspace); 3],
                typed("yet"),
                vec![key(KeyCode::Enter)],
            ]
            .concat(),
        );

        assert_eq!(
            action,
            ConfigAction::Set {
                slot: LayerSlot::Personal,
                key: SettingKey::PrsInclude,
                value: list(&["acme/*", "gameldar/yet"]),
            }
        );
    }

    #[test]
    fn the_panes_row_opens_the_panes_screen() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "panes");
        assert_eq!(
            modal.handle_key(key(KeyCode::Enter), &view),
            ConfigAction::None
        );

        let text = screen(&modal, &view);
        assert!(text.contains("panes"), "got:\n{text}");
        assert!(text.contains("prs-for-review"), "got:\n{text}");
        assert!(text.contains("branch-status"), "got:\n{text}");
        assert!(text.contains("a add"), "got:\n{text}");
    }

    #[test]
    fn the_panes_row_counts_the_panes_and_names_their_origin() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "panes");
        let line = selected_line(&modal, &view);
        assert!(line.contains("2 panes"), "got: {line:?}");
        assert!(line.contains("default"), "got: {line:?}");
    }

    /// The panes screen, with the cursor already on it.
    fn panes_screen(view: &ConfigView) -> ConfigModal {
        let mut modal = ConfigModal::new(true);
        select(&mut modal, view, "panes");
        modal.handle_key(key(KeyCode::Enter), view);
        modal
    }

    fn repo_panes_view() -> ConfigView {
        let mut view = empty_view();
        view.panes = PanesView {
            panes: vec![
                builtin("prs-for-review", "30%"),
                builtin("branch-status", "25%"),
                PaneEdit {
                    kind: PaneKindTag::Raw,
                    name: None,
                    title: None,
                    every: None,
                    command: Some(vec!["lazygit".to_owned()]),
                    size: None,
                },
            ],
            origin: Origin::Layer(LayerSlot::Repo),
            changed: false,
        };
        view
    }

    #[test]
    fn editing_an_inherited_pane_list_confirms_the_copy_before_emitting_it() {
        // Arrays replace wholesale, so editing one pane of a list the repository
        // ships copies the whole list into your layer. That is a thing to be
        // told before it happens, not after.
        let view = repo_panes_view();
        let mut modal = panes_screen(&view);

        let action = modal.handle_key(key(KeyCode::Char('d')), &view);
        assert_eq!(action, ConfigAction::None);
        let text = screen(&modal, &view);
        assert!(text.contains("repo"), "got:\n{text}");
        assert!(text.contains('3'), "it says how many panes; got:\n{text}");
        assert!(text.contains("personal"), "got:\n{text}");

        let confirmed = modal.handle_key(key(KeyCode::Char('y')), &view);
        assert_eq!(
            confirmed,
            ConfigAction::SetPanes {
                slot: LayerSlot::Personal,
                panes: view.panes.panes[1..].to_vec(),
            }
        );
    }

    #[test]
    fn a_pane_list_already_in_the_target_layer_needs_no_confirmation() {
        let mut view = repo_panes_view();
        view.panes.origin = Origin::Layer(LayerSlot::Personal);
        let mut modal = panes_screen(&view);

        assert_eq!(
            modal.handle_key(key(KeyCode::Char('d')), &view),
            ConfigAction::SetPanes {
                slot: LayerSlot::Personal,
                panes: view.panes.panes[1..].to_vec(),
            }
        );
    }

    #[test]
    fn a_target_layer_the_pane_list_would_be_invisible_in_is_refused() {
        // Writing panes to `global` while the repository ships a layout produces
        // a layout no session will ever use, and an edit that vanishes from the
        // screen the moment it is made.
        let view = repo_panes_view();
        let mut modal = panes_screen(&view);
        modal.handle_key(key(KeyCode::Tab), &view);
        assert_eq!(modal.target(), LayerSlot::Global);

        assert_eq!(
            modal.handle_key(key(KeyCode::Char('d')), &view),
            ConfigAction::None
        );
        let text = screen(&modal, &view);
        assert!(text.contains("repo"), "got:\n{text}");
    }

    #[test]
    fn panes_can_be_reordered_and_the_whole_list_is_emitted() {
        let mut view = repo_panes_view();
        view.panes.origin = Origin::Layer(LayerSlot::Personal);
        let mut modal = panes_screen(&view);

        let action = modal.handle_key(key(KeyCode::Char('J')), &view);

        let mut expected = view.panes.panes.clone();
        expected.swap(0, 1);
        assert_eq!(
            action,
            ConfigAction::SetPanes {
                slot: LayerSlot::Personal,
                panes: expected,
            }
        );
    }

    #[test]
    fn adding_a_pane_picks_a_kind_first() {
        let mut view = repo_panes_view();
        view.panes.origin = Origin::Layer(LayerSlot::Personal);
        let mut modal = panes_screen(&view);

        modal.handle_key(key(KeyCode::Char('a')), &view);
        let text = screen(&modal, &view);
        assert!(text.contains("builtin"), "got:\n{text}");
        assert!(text.contains("exec"), "got:\n{text}");
        assert!(text.contains("raw"), "got:\n{text}");

        // `exec`, the second kind.
        let action = press(
            &mut modal,
            &view,
            &[key(KeyCode::Char('j')), key(KeyCode::Enter)],
        );
        match action {
            ConfigAction::SetPanes { slot, panes } => {
                assert_eq!(slot, LayerSlot::Personal);
                assert_eq!(panes.len(), 4);
                assert_eq!(panes[3], PaneEdit::new(PaneKindTag::Exec));
            }
            other => panic!("expected SetPanes, got {other:?}"),
        }
    }

    #[test]
    fn a_new_pane_opens_its_own_fields_so_it_can_be_filled_in() {
        let mut view = repo_panes_view();
        view.panes.origin = Origin::Layer(LayerSlot::Personal);
        let mut modal = panes_screen(&view);

        press(
            &mut modal,
            &view,
            &[
                key(KeyCode::Char('a')),
                key(KeyCode::Char('j')),
                key(KeyCode::Enter),
            ],
        );

        // The host has applied the SetPanes; the view it hands back has the pane.
        let mut after = view.clone();
        after.panes.panes.push(PaneEdit::new(PaneKindTag::Exec));
        let text = screen(&modal, &after);
        assert!(text.contains("title"), "got:\n{text}");
        assert!(text.contains("every"), "got:\n{text}");
        assert!(text.contains("command"), "got:\n{text}");
        assert!(
            !text.contains("name"),
            "an exec pane has no name; got:\n{text}"
        );
    }

    #[test]
    fn a_panes_field_is_edited_in_place_and_emits_the_whole_list() {
        let mut view = repo_panes_view();
        view.panes.origin = Origin::Layer(LayerSlot::Personal);
        let mut modal = panes_screen(&view);

        // Into the first pane, onto its `name`, and retype it.
        let action = press(
            &mut modal,
            &view,
            &[
                vec![
                    key(KeyCode::Enter),
                    key(KeyCode::Char('j')),
                    key(KeyCode::Enter),
                ],
                vec![key(KeyCode::Backspace); 14],
                typed("branch-status"),
                vec![key(KeyCode::Enter)],
            ]
            .concat(),
        );

        let mut expected = view.panes.panes.clone();
        expected[0].name = Some("branch-status".to_owned());
        assert_eq!(
            action,
            ConfigAction::SetPanes {
                slot: LayerSlot::Personal,
                panes: expected,
            }
        );
    }

    #[test]
    fn a_panes_command_is_edited_as_a_list_so_an_argument_may_contain_a_space() {
        let mut view = empty_view();
        view.panes = PanesView {
            panes: vec![PaneEdit {
                kind: PaneKindTag::Raw,
                name: None,
                title: None,
                every: None,
                command: Some(vec!["sh".to_owned(), "-c".to_owned()]),
                size: None,
            }],
            origin: Origin::Layer(LayerSlot::Personal),
            changed: false,
        };
        let mut modal = panes_screen(&view);

        let action = press(
            &mut modal,
            &view,
            &[
                vec![
                    key(KeyCode::Enter),
                    key(KeyCode::Char('j')),
                    key(KeyCode::Enter),
                    key(KeyCode::Char('a')),
                ],
                typed("echo one two"),
                vec![key(KeyCode::Enter)],
            ]
            .concat(),
        );

        assert_eq!(
            action,
            ConfigAction::SetPanes {
                slot: LayerSlot::Personal,
                panes: vec![PaneEdit {
                    kind: PaneKindTag::Raw,
                    name: None,
                    title: None,
                    every: None,
                    command: Some(vec![
                        "sh".to_owned(),
                        "-c".to_owned(),
                        "echo one two".to_owned()
                    ]),
                    size: None,
                }],
            }
        );
    }

    #[test]
    fn changing_a_panes_kind_confirms_before_dropping_the_fields_it_must_not_carry() {
        let mut view = empty_view();
        view.panes = PanesView {
            panes: vec![PaneEdit {
                kind: PaneKindTag::Exec,
                name: None,
                title: Some("Deploys".to_owned()),
                every: Some("60s".to_owned()),
                command: Some(vec!["kubectl".to_owned()]),
                size: None,
            }],
            origin: Origin::Layer(LayerSlot::Personal),
            changed: false,
        };
        let mut modal = panes_screen(&view);

        // Into the pane, onto the kind row, pick `raw`.
        let pending = press(
            &mut modal,
            &view,
            &[
                key(KeyCode::Enter),
                key(KeyCode::Enter),
                key(KeyCode::Char('j')),
                key(KeyCode::Char('j')),
                key(KeyCode::Enter),
            ],
        );
        assert_eq!(pending, ConfigAction::None);
        let text = screen(&modal, &view);
        assert!(text.contains("title"), "got:\n{text}");
        assert!(text.contains("every"), "got:\n{text}");

        let confirmed = modal.handle_key(key(KeyCode::Char('y')), &view);
        assert_eq!(
            confirmed,
            ConfigAction::SetPanes {
                slot: LayerSlot::Personal,
                panes: vec![PaneEdit {
                    kind: PaneKindTag::Raw,
                    name: None,
                    title: None,
                    every: None,
                    command: Some(vec!["kubectl".to_owned()]),
                    size: None,
                }],
            }
        );
    }

    #[test]
    fn a_pane_field_error_is_whatever_the_parser_said_about_it() {
        // The modal holds no opinion of its own about pane validity.
        let mut view = repo_panes_view();
        view.panes.origin = Origin::Layer(LayerSlot::Personal);
        view.status = Err("a `exec` pane needs a `command`".to_owned());
        let mut modal = panes_screen(&view);
        modal.handle_key(key(KeyCode::Enter), &view);

        let text = screen(&modal, &view);
        assert!(text.contains("needs a `command`"), "got:\n{text}");
    }

    #[test]
    fn escape_walks_back_out_one_screen_at_a_time() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        select(&mut modal, &view, "panes");
        modal.handle_key(key(KeyCode::Enter), &view);
        modal.handle_key(key(KeyCode::Enter), &view);
        assert!(screen(&modal, &view).contains("kind"));

        modal.handle_key(key(KeyCode::Esc), &view);
        assert!(
            screen(&modal, &view).contains("a add"),
            "back on the panes screen"
        );

        modal.handle_key(key(KeyCode::Esc), &view);
        assert!(
            screen(&modal, &view).contains("writing to:"),
            "back on the settings screen"
        );

        assert_eq!(
            modal.handle_key(key(KeyCode::Esc), &view),
            ConfigAction::Close
        );
    }

    #[test]
    fn a_help_overlay_lists_the_keys_and_escape_leaves_it() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);

        assert_eq!(
            modal.handle_key(key(KeyCode::Char('?')), &view),
            ConfigAction::None
        );
        let text = screen(&modal, &view);
        assert!(text.contains("tab"), "got:\n{text}");
        assert!(text.contains("save"), "got:\n{text}");

        modal.handle_key(key(KeyCode::Esc), &view);
        assert!(screen(&modal, &view).contains("writing to:"));
    }

    #[test]
    fn a_save_reports_what_the_running_session_cannot_pick_up() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        modal.report_saved(Saved {
            written: vec![LayerSlot::Personal],
            pending: vec![Pending::Setting(SettingKey::TmuxPrefix), Pending::Panes],
        });

        let text = screen(&modal, &view);
        assert!(text.contains("saved personal"), "got:\n{text}");
        assert!(text.contains("tmux.prefix"), "got:\n{text}");
        assert!(text.contains("kill-server"), "got:\n{text}");
        // Inside a session, the notice speaks of this session.
        assert!(text.contains("this one keeps"), "got:\n{text}");
    }

    #[test]
    fn standalone_the_same_report_speaks_of_the_next_session_instead() {
        let view = empty_view();
        let mut modal = ConfigModal::new(false);
        modal.report_saved(Saved {
            written: vec![LayerSlot::Global],
            pending: vec![Pending::Panes],
        });

        let text = screen(&modal, &view);
        assert!(text.contains("saved global"), "got:\n{text}");
        assert!(text.contains("next"), "got:\n{text}");
        assert!(!text.contains("this one keeps"), "got:\n{text}");
    }

    #[test]
    fn a_save_with_nothing_pending_reports_only_what_it_wrote() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        modal.report_saved(Saved {
            written: vec![LayerSlot::Personal],
            pending: Vec::new(),
        });

        let text = screen(&modal, &view);
        assert!(text.contains("saved personal"), "got:\n{text}");
        assert!(!text.contains("kill-server"), "got:\n{text}");
    }

    #[test]
    fn a_save_that_wrote_nothing_shows_the_reason_and_offers_a_reload() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        modal.report_error("/tmp/repo.toml changed on disk since this editor opened it".to_owned());

        let text = screen(&modal, &view);
        assert!(text.contains("changed on disk"), "got:\n{text}");
    }

    #[test]
    fn r_reloads_the_target_layer_so_a_changed_file_can_be_taken_on_board() {
        let view = empty_view();
        let mut modal = ConfigModal::new(true);
        assert_eq!(
            modal.handle_key(key(KeyCode::Char('r')), &view),
            ConfigAction::Reload {
                slot: LayerSlot::Personal
            }
        );
    }

    #[test]
    fn every_width_the_panes_are_tested_at_renders_without_panicking() {
        // The popup is 80% of a terminal nobody promised would be wide.
        let view = with_row(
            repo_panes_view(),
            SettingKey::AgentCommand,
            [
                None,
                None,
                Some(list(&["claude", "--mcp-config", "~/.mcp.json"])),
            ],
        );
        let mut modal = ConfigModal::new(true);
        for width in [20_u16, 24, 34, 48, 60, 78, 120] {
            let _ = rendered(&modal, &view, width, 20);
        }
        select(&mut modal, &view, "panes");
        modal.handle_key(key(KeyCode::Enter), &view);
        for width in [20_u16, 24, 34, 48, 60, 78, 120] {
            let _ = rendered(&modal, &view, width, 20);
        }
        for height in [1_u16, 2, 3, 6] {
            let _ = rendered(&modal, &view, 40, height);
        }
    }

    #[test]
    fn the_builtin_help_names_every_built_in_there_is() {
        // The modal is the documented way to edit a layout. Help text that
        // omits a built-in tells the reader it does not exist.
        let name_help = pane_field_help(&empty_view(), 0, PaneField::Name);
        let kind = kind_help(PaneKindTag::Builtin);
        for builtin in BuiltinPane::ALL {
            assert!(
                name_help.contains(builtin.as_str()),
                "`{}` missing from the name help: {name_help}",
                builtin.as_str()
            );
            assert!(
                kind.contains(builtin.as_str()),
                "`{}` missing from the kind help: {kind}",
                builtin.as_str()
            );
        }
    }
}
