//! Leader-key input for the panes: turns terminal key events into domain
//! chords, tracks a half-typed sequence, and draws the which-key popup and the
//! status bar that teach the bindings.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use zax_domain::keymap::{Candidate, Key, KeyChord, Keymap, Lookup};
use zax_domain::{KeyCommand, KeySequence};

use crate::fmt::truncate;

/// The chord a terminal key event stands for, or `None` for keys the keymap
/// cannot spell (function keys, Home, and the like). Shift is not recorded:
/// it is already carried by the character itself. Press-versus-release
/// filtering stays with the caller.
#[must_use]
pub fn chord(key: &KeyEvent) -> Option<KeyChord> {
    let code = match key.code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Tab => Key::Tab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        _ => return None,
    };
    Some(KeyChord {
        code,
        ctrl: key.modifiers.contains(KeyModifiers::CONTROL),
        alt: key.modifiers.contains(KeyModifiers::ALT),
    })
}

/// A pane's key state: its keymap plus the chords typed so far toward a
/// multi-key sequence. There is no timeout; `Esc` cancels.
#[derive(Debug, Clone)]
pub struct KeyInput {
    keymap: Keymap,
    pending: Vec<KeyChord>,
    notice: Option<String>,
}

impl KeyInput {
    #[must_use]
    pub fn new(keymap: Keymap) -> Self {
        Self {
            keymap,
            pending: Vec::new(),
            notice: None,
        }
    }

    /// Feed one key. Returns the command a completed sequence runs.
    ///
    /// `Esc` with a pending prefix only cancels it. A broken multi-key
    /// sequence leaves a notice; a stray unbound key on its own is ignored.
    pub fn press(&mut self, key: KeyEvent) -> Option<KeyCommand> {
        self.notice = None;
        let chord = chord(&key)?;
        if chord.code == Key::Esc && !self.pending.is_empty() {
            self.pending.clear();
            return None;
        }
        self.pending.push(chord);
        match self.keymap.lookup(&self.pending) {
            Lookup::Command(command) => {
                self.pending.clear();
                Some(command)
            }
            Lookup::Prefix(_) => None,
            Lookup::Unbound => {
                if self.pending.len() > 1 {
                    self.notice = Some(format!("{} is not bound", spell(&self.pending)));
                }
                self.pending.clear();
                None
            }
        }
    }

    /// Drop a half-typed sequence and its popup. A picker or prompt that opens
    /// takes the keys from here on, so a prefix typed before it must not wait
    /// to be completed by whatever is pressed after it closes.
    pub fn cancel(&mut self) {
        self.pending.clear();
        self.notice = None;
    }

    /// The "not bound" message from the last key, if it broke a sequence.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The first sequence bound to `command`, for hints that name its key.
    #[must_use]
    pub fn sequence_for(&self, command: KeyCommand) -> Option<KeySequence> {
        self.keymap.sequence_for(command)
    }

    /// Whether a sequence is half-typed.
    #[must_use]
    pub fn pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Draw the which-key popup at the bottom of `body` while a prefix is
    /// pending. Draws nothing when idle.
    pub fn render_which_key(&self, frame: &mut Frame, body: Rect) {
        let Lookup::Prefix(candidates) = self.keymap.lookup(&self.pending) else {
            return;
        };
        if self.pending.is_empty() || body.height < 3 || body.width < 4 {
            return;
        }
        let capacity = usize::from(body.height - 2);
        // When the candidates do not fit, the last row says how many are left
        // out instead of cutting the list off silently.
        let (rows, more) = if candidates.len() > capacity {
            let shown = capacity - 1;
            (shown, Some(candidates.len() - shown))
        } else {
            (candidates.len(), None)
        };
        let height = u16::try_from(rows + usize::from(more.is_some())).unwrap_or(0) + 2;
        let area = Rect {
            x: body.x,
            y: body.y + body.height - height,
            width: body.width,
            height,
        };
        let key_width = candidates
            .iter()
            .map(|c| c.chord.to_string().chars().count())
            .max()
            .unwrap_or(0);
        let mut lines: Vec<Line> = candidates
            .iter()
            .take(rows)
            .map(|c| {
                let key = format!("{:<key_width$}", c.chord.to_string());
                Line::from(vec![
                    Span::styled(key, Style::default().add_modifier(Modifier::BOLD)),
                    Span::raw("  "),
                    Span::raw(c.label.clone()),
                ])
            })
            .collect();
        if let Some(more) = more {
            lines.push(Line::raw(format!("... +{more} more")));
        }
        let block = Block::default()
            .borders(Borders::ALL)
            .title(format!(" {} ", spell(&self.pending)));
        frame.render_widget(Clear, area);
        frame.render_widget(Paragraph::new(lines).block(block), area);
    }

    /// The status bar text: the plain keys, then what follows `SPC`.
    /// Movement is left out; it is taught by habit, not by the bar.
    #[must_use]
    pub fn status_line(&self) -> Line<'static> {
        let leader = KeyChord::leader();
        let mut items = Vec::new();
        for candidate in self.keymap.top_level() {
            if candidate.chord == leader {
                continue;
            }
            if let Some(item) = status_item(None, &candidate) {
                items.push(item);
            }
        }
        if let Lookup::Prefix(children) = self.keymap.lookup(&[leader]) {
            for candidate in &children {
                if let Some(item) = status_item(Some(leader), candidate) {
                    items.push(item);
                }
            }
        }
        Line::styled(items.join("  "), Style::default().fg(Color::DarkGray))
    }

    /// Draw the footer row: the "not bound" notice if the last key broke a
    /// sequence, otherwise the status bar. The notice wins because it answers
    /// the key just pressed and the bar is there all the time.
    pub fn render_footer(&self, frame: &mut Frame, row: Rect) {
        match self.notice() {
            Some(notice) => frame.render_widget(
                Paragraph::new(Line::styled(
                    truncate(notice, usize::from(row.width)),
                    Style::default().fg(Color::Yellow),
                )),
                row,
            ),
            None => self.render_status(frame, row),
        }
    }

    /// Draw the status bar on `row`, truncated to its width.
    fn render_status(&self, frame: &mut Frame, row: Rect) {
        let text: String = self
            .status_line()
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        let line = Line::styled(
            truncate(&text, usize::from(row.width)),
            Style::default().fg(Color::DarkGray),
        );
        frame.render_widget(Paragraph::new(line), row);
    }
}

/// Split a pane's area into the body and the one-row footer under it. An
/// area of a single row is all body: the header matters more than the bar.
#[must_use]
pub fn split_footer(area: Rect) -> (Rect, Rect) {
    if area.height < 2 {
        return (area, Rect { height: 0, ..area });
    }
    let body = Rect {
        height: area.height - 1,
        ..area
    };
    let footer = Rect {
        y: area.y + body.height,
        height: 1,
        ..area
    };
    (body, footer)
}

fn status_item(prefix: Option<KeyChord>, candidate: &Candidate) -> Option<String> {
    if matches!(
        candidate.command,
        Some(
            KeyCommand::MoveDown
                | KeyCommand::MoveUp
                | KeyCommand::PageDown
                | KeyCommand::PageUp
                | KeyCommand::Top
                | KeyCommand::Bottom
        )
    ) {
        return None;
    }
    Some(match prefix {
        Some(p) => format!("{p} {} {}", candidate.chord, candidate.label),
        None => format!("{} {}", candidate.chord, candidate.label),
    })
}

fn spell(chords: &[KeyChord]) -> String {
    chords
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use zax_domain::keymap::{Keymaps, PaneContext};

    use super::*;

    fn input() -> KeyInput {
        KeyInput::new(
            Keymaps::defaults()
                .for_context(PaneContext::PrsForReview)
                .clone(),
        )
    }

    fn press(input: &mut KeyInput, code: KeyCode) -> Option<KeyCommand> {
        input.press(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn screen(input: &KeyInput) -> String {
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal
            .draw(|f| input.render_which_key(f, f.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_leader_sequence_reports_progress_then_dispatches() {
        let mut input = input();
        assert_eq!(press(&mut input, KeyCode::Char(' ')), None);
        assert!(input.pending());
        assert_eq!(press(&mut input, KeyCode::Char('w')), None);
        assert!(input.pending());
        assert_eq!(
            press(&mut input, KeyCode::Char('r')),
            Some(KeyCommand::WorktreeReview)
        );
        assert!(!input.pending());
    }

    #[test]
    fn esc_cancels_a_pending_prefix_and_is_ignored_when_idle() {
        let mut input = input();
        press(&mut input, KeyCode::Char(' '));
        assert_eq!(press(&mut input, KeyCode::Esc), None);
        assert!(!input.pending());
        assert_eq!(input.notice(), None);
        assert_eq!(press(&mut input, KeyCode::Esc), None);
        assert_eq!(input.notice(), None);
    }

    #[test]
    fn a_broken_sequence_leaves_a_notice_until_the_next_key() {
        let mut input = input();
        press(&mut input, KeyCode::Char(' '));
        press(&mut input, KeyCode::Char('w'));
        assert_eq!(press(&mut input, KeyCode::Char('z')), None);
        assert_eq!(input.notice(), Some("SPC w z is not bound"));
        assert!(!input.pending());
        press(&mut input, KeyCode::Char('j'));
        assert_eq!(input.notice(), None);
    }

    #[test]
    fn a_stray_unbound_key_is_silent() {
        let mut input = input();
        assert_eq!(press(&mut input, KeyCode::Char('z')), None);
        assert_eq!(input.notice(), None);
        assert!(!input.pending());
    }

    #[test]
    fn a_plain_key_dispatches_at_once() {
        let mut input = input();
        assert_eq!(
            press(&mut input, KeyCode::Char('j')),
            Some(KeyCommand::MoveDown)
        );
    }

    #[test]
    fn keys_the_keymap_cannot_spell_are_ignored() {
        assert_eq!(
            chord(&KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            chord(&KeyEvent::new(KeyCode::Char('W'), KeyModifiers::SHIFT)),
            Some(KeyChord::char('W'))
        );
    }

    #[test]
    fn the_status_line_lists_the_prs_defaults() {
        let text: String = input()
            .status_line()
            .spans
            .iter()
            .map(|s| s.content.to_string())
            .collect();
        assert_eq!(
            text,
            "RET open in browser  SPC o +open  SPC q close pane  SPC r refresh  SPC w +worktree  SPC y copy url"
        );
    }

    #[test]
    fn cancel_drops_a_pending_prefix_and_its_popup() {
        let mut input = input();
        press(&mut input, KeyCode::Char(' '));
        input.cancel();
        assert!(!input.pending());
        assert!(!screen(&input).contains("SPC"));
        assert_eq!(press(&mut input, KeyCode::Char('q')), None);
        assert_eq!(input.notice(), None);
    }

    #[test]
    fn a_popup_that_does_not_fit_says_how_many_rows_it_left_out() {
        let mut input = input();
        press(&mut input, KeyCode::Char(' '));
        let mut terminal = Terminal::new(TestBackend::new(40, 3)).unwrap();
        terminal
            .draw(|f| input.render_which_key(f, f.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let middle: String = (0..40).map(|x| buffer[(x, 1)].symbol()).collect();
        assert!(middle.contains("... +5 more"), "{middle}");
        let mut terminal = Terminal::new(TestBackend::new(40, 5)).unwrap();
        terminal
            .draw(|f| input.render_which_key(f, f.area()))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        let last: String = (0..40).map(|x| buffer[(x, 3)].symbol()).collect();
        assert!(last.contains("... +3 more"), "{last}");
    }

    #[test]
    fn the_popup_shows_its_title_and_candidates_only_while_pending() {
        let mut input = input();
        assert!(!screen(&input).contains("SPC"));
        press(&mut input, KeyCode::Char(' '));
        press(&mut input, KeyCode::Char('w'));
        let shown = screen(&input);
        assert!(shown.contains(" SPC w "), "{shown}");
        assert!(shown.contains("o  open worktree"), "{shown}");
        press(&mut input, KeyCode::Esc);
        assert!(!screen(&input).contains("SPC"));
    }
}

/// Key-typing helpers shared by the pane tests.
#[cfg(test)]
pub(crate) mod testing {
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use crate::action::PaneAction;
    use crate::branch_pane::BranchStatusPane;
    use crate::exec_pane::ExecPane;
    use crate::my_prs_pane::MyPrsPane;
    use crate::prs_pane::PrsForReviewPane;
    use crate::tickets_pane::TicketsPane;
    use crate::todo_pane::TodoPane;

    /// A pane that takes key events, so one `press` serves every pane test.
    pub(crate) trait KeyPane {
        fn on_key(&mut self, key: KeyEvent) -> PaneAction;
    }

    macro_rules! key_pane {
        ($($pane:ty),*) => {$(
            impl KeyPane for $pane {
                fn on_key(&mut self, key: KeyEvent) -> PaneAction {
                    <$pane>::on_key(self, key)
                }
            }
        )*};
    }
    key_pane!(
        BranchStatusPane,
        ExecPane,
        MyPrsPane,
        PrsForReviewPane,
        TicketsPane,
        TodoPane
    );

    /// Type `"SPC w o"` into a pane and return what the last key did.
    pub(crate) fn press(pane: &mut impl KeyPane, sequence: &str) -> PaneAction {
        let mut action = PaneAction::None;
        for event in events(sequence) {
            action = pane.on_key(event);
        }
        action
    }

    /// `"SPC w o"` as the key events a user would type.
    pub(crate) fn events(sequence: &str) -> Vec<KeyEvent> {
        sequence
            .split_whitespace()
            .map(|token| {
                let code = match token {
                    "SPC" => KeyCode::Char(' '),
                    "RET" => KeyCode::Enter,
                    "<Esc>" => KeyCode::Esc,
                    "<Up>" => KeyCode::Up,
                    "<Down>" => KeyCode::Down,
                    "<PageUp>" => KeyCode::PageUp,
                    "<PageDown>" => KeyCode::PageDown,
                    "<Home>" => KeyCode::Home,
                    "<End>" => KeyCode::End,
                    one if one.chars().count() == 1 => {
                        KeyCode::Char(one.chars().next().unwrap_or(' '))
                    }
                    other => panic!("no such test key: {other}"),
                };
                KeyEvent::new(code, KeyModifiers::NONE)
            })
            .collect()
    }
}
