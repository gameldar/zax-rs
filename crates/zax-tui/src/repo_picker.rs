//! Which checkout a ticket's worktree goes in, when a ticket belongs to no
//! repository of its own.
//!
//! It runs inside the tickets pane, not a popup, so nothing here draws a box
//! of its own. It holds the branch the worktree is for, the choices offered
//! and a type-to-filter query; the host applies the action it returns.

use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use zax_domain::{BranchName, MainCheckout, RepoChoices};

use crate::fmt::truncate;

/// What a keystroke on the picker asks the host to do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RepoPickerAction {
    #[default]
    None,
    Choose(PathBuf),
    Cancel,
}

pub struct RepoPicker {
    branch: BranchName,
    checkouts: Vec<MainCheckout>,
    filter: String,
    /// An index into the *filtered* view, recomputed from `checkouts` and
    /// `filter` on every keystroke. Clamped every time either changes, so it
    /// is never used against a view it does not fit.
    selected: usize,
}

impl RepoPicker {
    pub fn new(branch: BranchName, choices: RepoChoices) -> Self {
        let checkouts = choices.checkouts;
        // `preselected` indexes the unfiltered list, which at construction
        // is exactly the filtered view - nothing has been typed yet.
        let selected = choices
            .preselected
            .filter(|i| *i < checkouts.len())
            .unwrap_or(0);
        Self {
            branch,
            checkouts,
            filter: String::new(),
            selected,
        }
    }

    pub fn branch(&self) -> &BranchName {
        &self.branch
    }

    fn filtered(&self) -> impl Iterator<Item = &MainCheckout> + '_ {
        let needle = self.filter.to_lowercase();
        self.checkouts
            .iter()
            .filter(move |checkout| checkout.name().to_lowercase().contains(&needle))
    }

    /// Keeps `selected` inside the filtered view after it changed size.
    fn clamp(&mut self) {
        let last = self.filtered().count().saturating_sub(1);
        self.selected = self.selected.min(last);
    }

    pub fn on_key(&mut self, key: KeyEvent) -> RepoPickerAction {
        match key.code {
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.clamp();
                RepoPickerAction::None
            }
            KeyCode::Backspace => {
                self.filter.pop();
                self.clamp();
                RepoPickerAction::None
            }
            KeyCode::Down => {
                let last = self.filtered().count().saturating_sub(1);
                self.selected = (self.selected + 1).min(last);
                RepoPickerAction::None
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                RepoPickerAction::None
            }
            KeyCode::Enter => self
                .filtered()
                .nth(self.selected)
                .map(|checkout| RepoPickerAction::Choose(checkout.path().to_owned()))
                .unwrap_or_default(),
            KeyCode::Esc => RepoPickerAction::Cancel,
            _ => RepoPickerAction::None,
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        let width = usize::from(area.width);
        // One filtered view for the whole frame: the header counts it and the
        // rows are taken from it.
        let filtered: Vec<&MainCheckout> = self.filtered().collect();
        let position = if filtered.is_empty() {
            0
        } else {
            self.selected + 1
        };
        let header = format!(
            "repo for {} ({position}/{}): {}",
            self.branch,
            filtered.len(),
            self.filter
        );
        let mut lines = vec![Line::styled(
            truncate(&header, width),
            Style::default().fg(Color::Cyan),
        )];
        // The header is the only chrome, so every other row is a checkout.
        // The window follows `selected`, which is the whole point: the pane
        // is short, a `projects.root` holds dozens of checkouts, and `Enter`
        // acts on the selection whether or not it was ever drawn.
        let visible = usize::from(area.height).saturating_sub(1);
        let first = self
            .selected
            .saturating_sub(visible.saturating_sub(1))
            .min(filtered.len());
        let last = (first + visible).min(filtered.len());
        for (index, checkout) in filtered.iter().enumerate().take(last).skip(first) {
            let text = format!("{}  {}", checkout.name(), checkout.path().display());
            let style = if index == self.selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            lines.push(Line::styled(truncate(&text, width), style));
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
    use zax_domain::accept_checkout;
    use zax_domain::CheckoutProbe;

    use super::*;

    /// A pane five rows tall: the header plus four checkouts, which is the
    /// shape a tickets pane at a third of a terminal actually has.
    const HEIGHT: u16 = 5;
    const WIDTH: u16 = 60;

    fn draw(picker: &RepoPicker) -> String {
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).unwrap();
        terminal.draw(|f| picker.render(f, f.area())).unwrap();
        let buffer = terminal.backend().buffer();
        (0..HEIGHT)
            .map(|y| {
                (0..WIDTH)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn checkout(path: &str) -> MainCheckout {
        accept_checkout(CheckoutProbe::Main {
            top: PathBuf::from(path),
            origin_url: None,
        })
        .unwrap()
    }

    fn choices(paths: &[&str], preselected: Option<usize>) -> RepoChoices {
        RepoChoices {
            checkouts: paths.iter().map(|p| checkout(p)).collect(),
            preselected,
        }
    }

    /// Ten checkouts: more than the pane can show at once.
    fn many() -> RepoChoices {
        let paths: Vec<String> = (0..10).map(|n| format!("/h/src/r{n:02}")).collect();
        let borrowed: Vec<&str> = paths.iter().map(String::as_str).collect();
        choices(&borrowed, None)
    }

    fn branch() -> BranchName {
        BranchName::parse_local("OMEGA-1-go").unwrap()
    }

    fn type_str(picker: &mut RepoPicker, text: &str) {
        for c in text.chars() {
            picker.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn typing_filters_the_list() {
        let mut picker = RepoPicker::new(
            branch(),
            choices(
                &["/h/src/alpha", "/h/src/omega", "/h/src/zed-ai-extensions"],
                None,
            ),
        );
        type_str(&mut picker, "ze");
        assert_eq!(
            picker.on_key(key(KeyCode::Enter)),
            RepoPickerAction::Choose(PathBuf::from("/h/src/zed-ai-extensions"))
        );
    }

    #[test]
    fn the_preselected_checkout_is_selected_first() {
        let mut picker = RepoPicker::new(
            branch(),
            choices(&["/h/src/alpha", "/h/src/omega"], Some(1)),
        );
        assert_eq!(
            picker.on_key(key(KeyCode::Enter)),
            RepoPickerAction::Choose(PathBuf::from("/h/src/omega"))
        );
    }

    #[test]
    fn esc_cancels() {
        let mut picker = RepoPicker::new(branch(), choices(&["/h/src/alpha"], None));
        assert_eq!(picker.on_key(key(KeyCode::Esc)), RepoPickerAction::Cancel);
    }

    #[test]
    fn enter_with_nothing_matching_does_nothing() {
        let mut picker =
            RepoPicker::new(branch(), choices(&["/h/src/alpha", "/h/src/omega"], None));
        type_str(&mut picker, "qqq");
        assert_eq!(picker.on_key(key(KeyCode::Enter)), RepoPickerAction::None);
    }

    #[test]
    fn the_header_counts_where_the_selection_is_in_the_filtered_list() {
        let mut picker = RepoPicker::new(branch(), many());
        let screen = draw(&picker);
        assert!(screen.contains("repo for OMEGA-1-go (1/10):"), "{screen}");

        picker.on_key(key(KeyCode::Down));
        type_str(&mut picker, "r0");
        let screen = draw(&picker);
        assert!(
            screen.contains("repo for OMEGA-1-go (2/10): r0"),
            "{screen}"
        );
    }

    #[test]
    fn a_preselected_checkout_below_the_viewport_is_drawn_anyway() {
        // `Enter` acts on the preselection immediately, so a selection the
        // pane is too short to show would create a worktree in a checkout the
        // user never saw.
        let choices = RepoChoices {
            preselected: Some(9),
            ..many()
        };
        let picker = RepoPicker::new(branch(), choices);

        let screen = draw(&picker);
        assert!(screen.contains("r09"), "{screen}");
        assert!(!screen.contains("r00"), "{screen}");
    }

    #[test]
    fn arrowing_below_the_visible_rows_scrolls_the_selection_into_view() {
        let mut picker = RepoPicker::new(branch(), many());
        for _ in 0..5 {
            picker.on_key(key(KeyCode::Down));
        }

        let screen = draw(&picker);
        assert!(screen.contains("r05"), "{screen}");
        assert!(!screen.contains("r00"), "{screen}");

        for _ in 0..5 {
            picker.on_key(key(KeyCode::Up));
        }
        let screen = draw(&picker);
        assert!(screen.contains("r00"), "{screen}");
        assert!(!screen.contains("r05"), "{screen}");
    }

    #[test]
    fn arrows_move_within_the_filtered_list() {
        // "om" matches omega and omen but not alpha, so down from the first
        // match has to land on omen, the second entry of the filtered view,
        // not on alpha or past the end of the full list.
        let mut picker = RepoPicker::new(
            branch(),
            choices(&["/h/src/alpha", "/h/src/omega", "/h/src/omen"], None),
        );
        type_str(&mut picker, "om");
        picker.on_key(key(KeyCode::Down));
        assert_eq!(
            picker.on_key(key(KeyCode::Enter)),
            RepoPickerAction::Choose(PathBuf::from("/h/src/omen"))
        );
        picker.on_key(key(KeyCode::Up));
        assert_eq!(
            picker.on_key(key(KeyCode::Enter)),
            RepoPickerAction::Choose(PathBuf::from("/h/src/omega"))
        );
    }
}
