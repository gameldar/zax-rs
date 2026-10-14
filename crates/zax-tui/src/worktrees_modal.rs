//! The worktrees popup: one ticket's worktrees, for cleaning up.
//!
//! It runs in its own process inside a tmux popup, so it fills the window
//! rather than the pane that opened it, and nothing here draws a box of its
//! own. It never touches a worktree: the host applies the action it returns
//! and hands back the worktrees that are left.

use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use zax_domain::{TicketKey, Worktree};

use crate::chrome::error_line;
use crate::fmt::truncate;

/// What a keystroke asks the host to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum WorktreesAction {
    #[default]
    None,
    /// Remove this worktree, unforced. Git refuses one with uncommitted
    /// changes, and the branch survives either way.
    Remove(PathBuf),
    Close,
}

pub struct WorktreesModal {
    key: TicketKey,
    worktrees: Vec<Worktree>,
    selected: usize,
    /// `d` was pressed on the selected worktree and `y` has not answered.
    confirming: bool,
    notice: Option<String>,
}

impl WorktreesModal {
    pub fn new(key: TicketKey, worktrees: Vec<Worktree>) -> Self {
        Self {
            key,
            worktrees,
            selected: 0,
            confirming: false,
            notice: None,
        }
    }

    /// The worktrees that are left after a removal.
    pub fn set_worktrees(&mut self, worktrees: Vec<Worktree>) {
        self.selected = self.selected.min(worktrees.len().saturating_sub(1));
        self.worktrees = worktrees;
        self.confirming = false;
    }

    pub fn is_empty(&self) -> bool {
        self.worktrees.is_empty()
    }

    /// A one-line message about the last keystroke, cleared by the next one.
    pub fn notice(&mut self, message: String) {
        self.notice = Some(message);
    }

    pub fn on_key(&mut self, key: KeyEvent) -> WorktreesAction {
        self.notice = None;
        if self.confirming {
            self.confirming = false;
            if key.code == KeyCode::Char('y') {
                if let Some(w) = self.worktrees.get(self.selected) {
                    return WorktreesAction::Remove(w.path.clone());
                }
            }
            return WorktreesAction::None;
        }
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return WorktreesAction::Close,
            KeyCode::Char('j') | KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.worktrees.len().saturating_sub(1));
            }
            KeyCode::Char('k') | KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('d') if !self.worktrees.is_empty() => self.confirming = true,
            _ => {}
        }
        WorktreesAction::None
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let width = usize::from(area.width);
        let mut lines = vec![Line::styled(
            truncate(
                &format!("Worktrees ({}) {}", self.worktrees.len(), self.key),
                width,
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )];
        // Header, hint and the message line are chrome.
        let visible = usize::from(area.height).saturating_sub(3);
        let first = self
            .selected
            .saturating_sub(visible.saturating_sub(1))
            .min(self.worktrees.len());
        let last = (first + visible).min(self.worktrees.len());
        for (i, w) in self.worktrees.iter().enumerate().take(last).skip(first) {
            let marker = if i == self.selected { "> " } else { "  " };
            let style = if i == self.selected {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let text = format!("{marker}{}:{}  {}", w.repo, w.branch, w.path.display());
            lines.push(Line::styled(truncate(&text, width), style));
        }
        let (hint, style) = match (self.confirming, self.worktrees.get(self.selected)) {
            (true, Some(w)) => (
                format!("remove {}? y/n", w.branch),
                Style::default().fg(Color::Red),
            ),
            _ => (
                "j/k move  d remove  esc close".to_owned(),
                Style::default().fg(Color::DarkGray),
            ),
        };
        lines.push(Line::styled(truncate(&hint, width), style));
        if let Some(n) = &self.notice {
            lines.push(error_line(n));
        }
        frame.render_widget(Paragraph::new(Text::from(lines)), area);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::KeyModifiers;
    use ratatui::Terminal;
    use zax_domain::{BranchName, RootRepoName};

    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn worktree(branch: &str) -> Worktree {
        Worktree {
            repo: RootRepoName::parse("omega").unwrap(),
            branch: BranchName::parse_foreign(branch).unwrap(),
            path: PathBuf::from(format!("/w/omega/{branch}")),
        }
    }

    fn modal() -> WorktreesModal {
        WorktreesModal::new(
            TicketKey::parse("OMEGA-1").unwrap(),
            vec![worktree("OMEGA-1-a"), worktree("OMEGA-1-b")],
        )
    }

    fn draw(modal: &WorktreesModal) -> String {
        let mut terminal = Terminal::new(TestBackend::new(60, 8)).unwrap();
        terminal.draw(|f| modal.render(f, f.area())).unwrap();
        let buffer = terminal.backend().buffer();
        (0..8)
            .map(|y| {
                (0..60)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn it_lists_the_worktrees_and_d_then_y_removes_the_selected() {
        let mut modal = modal();
        let screen = draw(&modal);
        assert!(screen.contains("Worktrees (2) OMEGA-1"), "{screen}");
        assert!(screen.contains("omega:OMEGA-1-a"), "{screen}");
        assert!(screen.contains("omega:OMEGA-1-b"), "{screen}");
        modal.on_key(key(KeyCode::Char('j')));
        modal.on_key(key(KeyCode::Char('d')));
        assert!(draw(&modal).contains("remove OMEGA-1-b? y/n"));
        assert_eq!(
            modal.on_key(key(KeyCode::Char('y'))),
            WorktreesAction::Remove(PathBuf::from("/w/omega/OMEGA-1-b"))
        );
    }

    #[test]
    fn anything_but_y_cancels_a_removal() {
        let mut modal = modal();
        modal.on_key(key(KeyCode::Char('d')));
        assert_eq!(modal.on_key(key(KeyCode::Char('n'))), WorktreesAction::None);
        assert_eq!(modal.on_key(key(KeyCode::Char('y'))), WorktreesAction::None);
    }

    #[test]
    fn esc_and_q_close_it() {
        assert_eq!(modal().on_key(key(KeyCode::Esc)), WorktreesAction::Close);
        assert_eq!(
            modal().on_key(key(KeyCode::Char('q'))),
            WorktreesAction::Close
        );
    }

    #[test]
    fn the_selection_stays_on_the_list_when_it_shrinks() {
        let mut modal = modal();
        modal.on_key(key(KeyCode::Char('j')));
        modal.set_worktrees(vec![worktree("OMEGA-1-a")]);
        modal.on_key(key(KeyCode::Char('d')));
        assert_eq!(
            modal.on_key(key(KeyCode::Char('y'))),
            WorktreesAction::Remove(PathBuf::from("/w/omega/OMEGA-1-a"))
        );
    }

    #[test]
    fn a_notice_shows_until_the_next_key() {
        let mut modal = modal();
        modal.notice("git: refused".to_owned());
        assert!(draw(&modal).contains("git: refused"));
        modal.on_key(key(KeyCode::Char('j')));
        assert!(!draw(&modal).contains("git: refused"));
    }
}
