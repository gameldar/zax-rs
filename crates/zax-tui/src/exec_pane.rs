//! The `exec` pane: a styled header with title and last-updated time, stale and
//! error marking, and scrollback, all from a command's plain stdout.

use std::time::{Duration, SystemTime};

use ratatui::crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::text::{Line, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use zax_domain::keymap::Keymap;
use zax_domain::KeyCommand;

use crate::action::PaneAction;
use crate::cache::Cached;
use crate::chrome::{error_line, header_line};
use crate::keys::{split_footer, KeyInput};

pub struct ExecPane {
    title: String,
    every: Duration,
    cache: Cached<Vec<String>>,
    scroll: usize,
    focused: bool,
    keys: KeyInput,
}

impl ExecPane {
    pub fn new(title: String, every: Duration, keymap: Keymap) -> Self {
        Self {
            title,
            every,
            cache: Cached::new(),
            scroll: 0,
            focused: false,
            keys: KeyInput::new(keymap),
        }
    }

    pub fn every(&self) -> Duration {
        self.every
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// Fold a refresh result into the pane.
    pub fn apply(&mut self, result: Result<String, String>, at: SystemTime) {
        match result {
            Ok(stdout) => {
                let lines: Vec<String> = stdout.lines().map(str::to_owned).collect();
                self.scroll = self.scroll.min(lines.len().saturating_sub(1));
                self.cache.record_ok(lines, at);
            }
            Err(message) => self.cache.record_err(message, at),
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, now: SystemTime) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let (area, footer) = split_footer(area);

        let mut lines: Vec<Line<'static>> = vec![header_line(
            &self.title,
            &self.cache.status(now),
            self.focused,
            area.width,
        )];

        match self.cache.value() {
            Some(body) => {
                lines.extend(
                    body.iter()
                        .skip(self.scroll)
                        .take(usize::from(area.height).saturating_sub(1))
                        .map(|l| Line::raw(l.clone())),
                );
            }
            None => {
                if let Some(error) = self.cache.error() {
                    lines.push(error_line(error));
                }
            }
        }

        frame.render_widget(Paragraph::new(Text::from(lines)), area);
        self.keys.render_which_key(frame, area);
        self.keys.render_footer(frame, footer);
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PaneAction {
        match self.keys.press(key) {
            Some(command) => self.run(command),
            None => PaneAction::None,
        }
    }

    fn run(&mut self, command: KeyCommand) -> PaneAction {
        let body_len = self.cache.value().map(Vec::len).unwrap_or(0);
        match command {
            KeyCommand::Refresh => PaneAction::Refresh,
            KeyCommand::Quit => PaneAction::Quit,
            KeyCommand::MoveDown => {
                self.scroll = (self.scroll + 1).min(body_len.saturating_sub(1));
                PaneAction::None
            }
            KeyCommand::MoveUp => {
                self.scroll = self.scroll.saturating_sub(1);
                PaneAction::None
            }
            // Not in this pane's context; the keymap refuses to bind them.
            _ => PaneAction::None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::{Duration, UNIX_EPOCH};

    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;

    use super::*;
    use crate::action::PaneAction;
    use crate::keys::testing::press;
    use zax_domain::keymap::{Keymaps, PaneContext};

    fn t(secs: u64) -> std::time::SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn keymap() -> Keymap {
        Keymaps::defaults().for_context(PaneContext::Exec).clone()
    }

    /// Type a key sequence such as `SPC w o`; the action of the last key.
    fn row(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    fn draw(pane: &ExecPane, now: std::time::SystemTime) -> Buffer {
        let mut terminal = Terminal::new(TestBackend::new(40, 6)).unwrap();
        terminal
            .draw(|frame| pane.render(frame, frame.area(), now))
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn pane() -> ExecPane {
        ExecPane::new("Deploys".to_owned(), Duration::from_secs(60), keymap())
    }

    #[test]
    fn before_the_first_result_the_header_says_loading() {
        let buffer = draw(&pane(), t(100));
        assert!(row(&buffer, 0).contains("Deploys"));
        assert!(row(&buffer, 0).contains("loading"));
    }

    #[test]
    fn stdout_is_shown_line_by_line_under_the_header() {
        let mut pane = pane();
        pane.apply(Ok("pod-a   Running\npod-b   Running".to_owned()), t(100));
        let buffer = draw(&pane, t(100));
        assert!(row(&buffer, 0).contains("Deploys"));
        assert_eq!(row(&buffer, 1), "pod-a   Running");
        assert_eq!(row(&buffer, 2), "pod-b   Running");
    }

    #[test]
    fn a_failure_keeps_the_last_good_output_and_marks_the_header_stale() {
        let mut pane = pane();
        pane.apply(Ok("pod-a   Running".to_owned()), t(100));
        pane.apply(Err("kubectl: connection refused".to_owned()), t(160));

        let buffer = draw(&pane, t(160));
        assert!(
            row(&buffer, 0).contains("stale"),
            "got: {:?}",
            row(&buffer, 0)
        );
        assert_eq!(
            row(&buffer, 1),
            "pod-a   Running",
            "the last good output survives"
        );
    }

    #[test]
    fn a_failure_with_nothing_cached_shows_an_actionable_error_line() {
        let mut pane = pane();
        pane.apply(Err("kubectl is not installed".to_owned()), t(100));
        let buffer = draw(&pane, t(100));
        assert!(row(&buffer, 1).contains("kubectl is not installed"));
    }

    #[test]
    fn spc_r_asks_for_a_refresh_and_spc_q_quits() {
        let mut pane = pane();
        assert_eq!(press(&mut pane, "SPC r"), PaneAction::Refresh);
        assert_eq!(press(&mut pane, "SPC q"), PaneAction::Quit);
    }

    #[test]
    fn bare_q_and_esc_no_longer_quit() {
        let mut pane = pane();
        assert_eq!(press(&mut pane, "q"), PaneAction::None);
        assert_eq!(press(&mut pane, "<Esc>"), PaneAction::None);
    }

    #[test]
    fn scrolling_moves_through_the_output_and_stops_at_the_ends() {
        let mut pane = pane();
        pane.apply(
            Ok((1..=20)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n")),
            t(100),
        );

        assert_eq!(press(&mut pane, "k"), PaneAction::None);
        assert_eq!(row(&draw(&pane, t(100)), 1), "line 1", "already at the top");

        press(&mut pane, "j");
        press(&mut pane, "j");
        assert_eq!(row(&draw(&pane, t(100)), 1), "line 3");

        press(&mut pane, "k");
        assert_eq!(row(&draw(&pane, t(100)), 1), "line 2");
    }

    #[test]
    fn an_unknown_key_does_nothing() {
        let mut pane = pane();
        assert_eq!(pane.on_key(key(KeyCode::Char('z'))), PaneAction::None);
    }

    #[test]
    fn a_zero_height_area_renders_without_panicking() {
        let mut terminal = Terminal::new(TestBackend::new(40, 0)).unwrap();
        let pane = pane();
        terminal
            .draw(|frame| pane.render(frame, frame.area(), t(100)))
            .unwrap();
    }
}
