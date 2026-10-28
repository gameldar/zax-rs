//! `tickets`: the tickets assigned to you, each with the work behind it.

use std::time::SystemTime;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use zax_domain::keymap::Keymap;
use zax_domain::{
    branch_slug, join, Attention, BranchName, KeyCommand, RepoChoices, Ticket, TicketKey,
    TicketRank, TicketRow, Transition, WorkIndex,
};

use crate::action::PaneAction;
use crate::cache::Cached;
use crate::chrome::{error_line, header_line};
use crate::fmt::{display_width, truncate};
use crate::keys::{split_footer, KeyInput};
use crate::my_prs_pane::{attention_glyph, attention_style};
use crate::pr_fields::{check_glyph, check_style};
use crate::repo_picker::{RepoPicker, RepoPickerAction};

const TITLE: &str = "Tickets";
const SEPARATOR: &str = " · ";
const MIN_TITLE: usize = 8;

const LEAD_LIVE: &str = "\u{25cf}";
const LEAD_IN_PROGRESS: &str = "\u{25d0}";
const LEAD_TO_DO: &str = "\u{25cb}";
const LEAD_DONE: &str = "\u{2714}";
const GLYPH_WORKTREE: &str = "\u{25b2}";

struct TicketsValue {
    provider: String,
    transitions: bool,
    tickets: Vec<Ticket>,
}

struct Picker {
    key: TicketKey,
    transitions: Vec<Transition>,
}

pub struct TicketsPane {
    configured: bool,
    tickets: Cached<TicketsValue>,
    work: Cached<WorkIndex>,
    rows: Vec<TicketRow>,
    selected: usize,
    focused: bool,
    picker: Option<Picker>,
    /// `projects.root` is configured, so `SPC w o` asks which checkout a new
    /// worktree goes in instead of starting it in whatever repository the
    /// session happened to start in.
    repo_choice: bool,
    repo_picker: Option<RepoPicker>,
    notice: Option<String>,
    keys: KeyInput,
}

impl TicketsPane {
    pub fn new(keymap: Keymap) -> Self {
        Self {
            configured: true,
            tickets: Cached::new(),
            work: Cached::new(),
            rows: Vec::new(),
            selected: 0,
            focused: false,
            picker: None,
            repo_choice: false,
            repo_picker: None,
            notice: None,
            keys: KeyInput::new(keymap),
        }
    }

    /// No `[tickets] provider`: the pane explains itself and never talks to
    /// the broker.
    pub fn unconfigured(keymap: Keymap) -> Self {
        Self {
            configured: false,
            ..Self::new(keymap)
        }
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.focused = focused;
    }

    /// A broker snapshot of the `tickets` topic, already decoded. `value`
    /// and `error` together mean stale.
    pub fn apply_tickets(
        &mut self,
        value: Option<(String, bool, Vec<Ticket>)>,
        fetched_at: Option<SystemTime>,
        error: Option<String>,
    ) {
        if let (Some((provider, transitions, tickets)), Some(at)) = (value, fetched_at) {
            self.tickets.record_ok(
                TicketsValue {
                    provider,
                    transitions,
                    tickets,
                },
                at,
            );
        }
        if let Some(error) = error {
            self.tickets.record_err(error, SystemTime::now());
        }
        self.rejoin();
    }

    pub fn apply_work(
        &mut self,
        value: Option<WorkIndex>,
        fetched_at: Option<SystemTime>,
        error: Option<String>,
    ) {
        if let (Some(work), Some(at)) = (value, fetched_at) {
            self.work.record_ok(work, at);
        }
        if let Some(error) = error {
            self.work.record_err(error, SystemTime::now());
        }
        self.rejoin();
    }

    fn rejoin(&mut self) {
        let empty = WorkIndex::default();
        let work = self.work.value().unwrap_or(&empty);
        self.rows = self
            .tickets
            .value()
            .map(|t| join(t.tickets.clone(), work))
            .unwrap_or_default();
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    pub fn show_picker(&mut self, key: TicketKey, transitions: Vec<Transition>) {
        if transitions.is_empty() {
            self.notice = Some(format!("{key} has no transitions to offer"));
            return;
        }
        self.keys.cancel();
        self.picker = Some(Picker { key, transitions });
    }

    /// Whether `projects.root` is configured: with it on, `SPC w o` asks which
    /// checkout to start work in rather than guessing from where the session
    /// started.
    pub fn set_repo_choice(&mut self, enabled: bool) {
        self.repo_choice = enabled;
    }

    /// The host already has the checkouts to offer (`ResolveCheckout::list`);
    /// showing them is all that is left to do here.
    pub fn show_repo_picker(&mut self, branch: BranchName, choices: RepoChoices) {
        self.keys.cancel();
        self.repo_picker = Some(RepoPicker::new(branch, choices));
    }

    /// A one-line message about the last keystroke, cleared by the next one.
    pub fn notice(&mut self, message: String) {
        self.notice = Some(message);
    }

    pub fn rows(&self) -> &[TicketRow] {
        &self.rows
    }

    fn selected_row(&self) -> Option<&TicketRow> {
        self.rows.get(self.selected)
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, now: SystemTime) {
        if area.height == 0 || area.width == 0 {
            return;
        }
        // Unlike the transition picker, the choice of repository can be a
        // long list, so it takes the whole pane rather than squeezing onto
        // the header line.
        if let Some(picker) = &self.repo_picker {
            picker.render(frame, area);
            return;
        }
        let (area, footer) = split_footer(area);
        let mut lines = vec![self.top_line(area.width, now)];

        if !self.configured {
            lines.push(Line::styled(
                "No ticket provider configured; set [tickets] provider",
                Style::default().fg(Color::DarkGray),
            ));
            frame.render_widget(Paragraph::new(Text::from(lines)), area);
            self.keys.render_which_key(frame, area);
            self.keys.render_footer(frame, footer);
            return;
        }

        // Header, detail line and the message line are chrome.
        let errors = self.messages();
        let chrome = 2 + errors.len();
        match self.tickets.value() {
            None => {}
            Some(_) if self.rows.is_empty() => lines.push(Line::styled(
                "Nothing assigned to you",
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
                    if let Some(row) = self.rows.get(index) {
                        lines.push(self.row(row, index == self.selected, area.width));
                    }
                }
                if first < last {
                    if let Some(row) = self.selected_row() {
                        lines.push(detail_line(row, area.width));
                    }
                }
            }
        }
        for message in errors {
            lines.push(error_line(&message));
        }
        frame.render_widget(Paragraph::new(Text::from(lines)), area);
        self.keys.render_which_key(frame, area);
        self.keys.render_footer(frame, footer);
    }

    /// The header, or the transition picker while it is open.
    fn top_line(&self, width: u16, now: SystemTime) -> Line<'static> {
        if let Some(picker) = &self.picker {
            let choices: Vec<String> = picker
                .transitions
                .iter()
                .take(9)
                .enumerate()
                .map(|(i, t)| format!("{} {}", i + 1, t.name))
                .collect();
            let text = format!("\u{2192} {}", choices.join("  "));
            return Line::styled(
                truncate(&text, usize::from(width)),
                Style::default().fg(Color::Cyan),
            );
        }
        let title = match self.tickets.value() {
            Some(v) => format!("{TITLE} ({}) {}", self.rows.len(), v.provider),
            None => TITLE.to_owned(),
        };
        let status = if self.configured {
            self.tickets.status(now)
        } else {
            String::new()
        };
        header_line(&title, &status, self.focused, width)
    }

    /// Errors worth a line of their own: an unfetched topic, a stale work
    /// source (tickets' staleness is in the header), and the last notice.
    fn messages(&self) -> Vec<String> {
        let mut out = Vec::new();
        if self.tickets.value().is_none() {
            if let Some(e) = self.tickets.error() {
                out.push(e.to_owned());
            }
        }
        if let Some(e) = self.work.error() {
            out.push(format!("work: {e}"));
        }
        if let Some(n) = &self.notice {
            out.push(n.clone());
        }
        out
    }

    /// `<lead> <KEY> <work glyphs> <title>`.
    fn row(&self, row: &TicketRow, selected: bool, width: u16) -> Line<'static> {
        let width = usize::from(width);
        let (lead, lead_style) = lead(row);
        let key = row.ticket.key.to_string();

        let mut glyphs: Vec<Span<'static>> = Vec::new();
        if !row.work.worktrees.is_empty() {
            glyphs.push(Span::styled(
                GLYPH_WORKTREE,
                Style::default().fg(Color::Cyan),
            ));
        }
        if let Some(pr) = row.work.most_urgent_pr() {
            glyphs.push(Span::styled(
                check_glyph(pr.status.pr.checks),
                check_style(pr.status.pr.checks),
            ));
            if !matches!(
                pr.attention,
                Attention::ChecksFailing | Attention::Waiting | Attention::Draft
            ) {
                glyphs.push(Span::styled(
                    attention_glyph(pr.attention),
                    attention_style(pr.attention),
                ));
            }
        }
        let glyph_width: usize = glyphs.iter().map(|s| display_width(&s.content)).sum();

        let base = display_width(lead) + 1 + display_width(&key) + 1;
        let with_glyphs = base + glyph_width + usize::from(glyph_width > 0);
        let show_glyphs = glyph_width > 0 && width.saturating_sub(with_glyphs) >= MIN_TITLE;
        let fixed = if show_glyphs { with_glyphs } else { base };
        let title = truncate(&row.ticket.summary, width.saturating_sub(fixed));

        let style = if selected && self.focused {
            Style::default().add_modifier(Modifier::REVERSED)
        } else {
            Style::default()
        };
        let mut spans = vec![
            Span::styled(lead.to_owned(), lead_style.patch(style)),
            Span::styled(" ", style),
            Span::styled(key, style.fg(Color::Cyan)),
            Span::styled(" ", style),
        ];
        if show_glyphs {
            spans.extend(glyphs.into_iter().map(|s| s.patch_style(style)));
            spans.push(Span::styled(" ", style));
        }
        spans.push(Span::styled(title, style));
        Line::from(spans)
    }

    pub fn on_key(&mut self, key: KeyEvent) -> PaneAction {
        self.notice = None;
        if let Some(mut picker) = self.repo_picker.take() {
            return match picker.on_key(key) {
                RepoPickerAction::Choose(path) => PaneAction::StartWork {
                    branch: picker.branch().clone(),
                    repo: Some(path),
                },
                RepoPickerAction::Cancel => PaneAction::None,
                RepoPickerAction::None => {
                    self.repo_picker = Some(picker);
                    PaneAction::None
                }
            };
        }
        if let Some(picker) = self.picker.take() {
            return match key.code {
                KeyCode::Char(c @ '1'..='9') => {
                    let index = c as usize - '1' as usize;
                    match picker.transitions.get(index) {
                        Some(t) => PaneAction::Transition {
                            key: picker.key,
                            id: t.id.clone(),
                        },
                        None => {
                            self.picker = Some(picker);
                            PaneAction::None
                        }
                    }
                }
                KeyCode::Esc => PaneAction::None,
                _ => {
                    self.picker = Some(picker);
                    PaneAction::None
                }
            };
        }
        match self.keys.press(key) {
            Some(command) => self.run(command),
            None => PaneAction::None,
        }
    }

    fn run(&mut self, command: KeyCommand) -> PaneAction {
        let len = self.rows.len();
        match command {
            KeyCommand::Refresh => PaneAction::Refresh,
            KeyCommand::Quit => PaneAction::Quit,
            KeyCommand::MoveDown => {
                self.selected = (self.selected + 1).min(len.saturating_sub(1));
                PaneAction::None
            }
            KeyCommand::MoveUp => {
                self.selected = self.selected.saturating_sub(1);
                PaneAction::None
            }
            KeyCommand::Open => self
                .selected_row()
                .map(|r| PaneAction::OpenUrl(r.ticket.url.clone()))
                .unwrap_or_default(),
            KeyCommand::CopyUrl => self
                .selected_row()
                .map(|r| PaneAction::CopyUrl(r.ticket.url.clone()))
                .unwrap_or_default(),
            KeyCommand::OpenPr => match self
                .selected_row()
                .map(|r| r.work.most_urgent_pr().map(|p| p.status.pr.url.clone()))
            {
                Some(Some(url)) => PaneAction::OpenUrl(url),
                Some(None) => {
                    self.notice = Some("no pull request".to_owned());
                    PaneAction::None
                }
                None => PaneAction::None,
            },
            KeyCommand::WorktreeOpen => self.start_or_open(),
            KeyCommand::WorktreeList => match self.selected_row() {
                Some(r) if r.work.worktrees.is_empty() => {
                    self.notice = Some(format!("{} has no worktrees", r.ticket.key));
                    PaneAction::None
                }
                Some(r) => PaneAction::ListWorktrees {
                    key: r.ticket.key.clone(),
                },
                None => PaneAction::None,
            },
            KeyCommand::Transition => {
                let can = self.tickets.value().is_some_and(|v| v.transitions);
                match self.selected_row() {
                    Some(r) if can => PaneAction::ListTransitions {
                        key: r.ticket.key.clone(),
                    },
                    _ => PaneAction::None,
                }
            }
            // Not in this pane's context; the keymap refuses to bind them.
            _ => PaneAction::None,
        }
    }

    fn start_or_open(&mut self) -> PaneAction {
        let Some(row) = self.selected_row() else {
            return PaneAction::None;
        };
        if let Some(first) = row.work.worktrees.first() {
            return PaneAction::OpenWorktree {
                path: first.path.clone(),
                others: row.work.worktrees.len() - 1,
            };
        }
        match branch_slug(&row.ticket.key, &row.ticket.summary) {
            Ok(branch) if self.repo_choice => PaneAction::ChooseRepo { branch },
            Ok(branch) => PaneAction::StartWork { branch, repo: None },
            Err(e) => {
                self.notice = Some(e.to_string());
                PaneAction::None
            }
        }
    }
}

fn lead(row: &TicketRow) -> (&'static str, Style) {
    match row.rank() {
        TicketRank::Live => {
            let style = Style::default().fg(Color::Green);
            (
                LEAD_LIVE,
                if row.work.attached {
                    style.add_modifier(Modifier::BOLD)
                } else {
                    style
                },
            )
        }
        TicketRank::InProgress => (LEAD_IN_PROGRESS, Style::default().fg(Color::Yellow)),
        TicketRank::ToDo => (LEAD_TO_DO, Style::default().fg(Color::Gray)),
        TicketRank::Done => (LEAD_DONE, Style::default().fg(Color::DarkGray)),
    }
}

fn attention_label(attention: Attention) -> String {
    match attention {
        Attention::Conflicting => "conflicting".to_owned(),
        Attention::ChangesRequested => "changes requested".to_owned(),
        Attention::ChecksFailing => "checks failing".to_owned(),
        Attention::Unresolved(n) => format!("{n} unresolved"),
        Attention::ReadyToMerge => "ready to merge".to_owned(),
        Attention::Waiting => "waiting on review".to_owned(),
        Attention::Draft => "draft".to_owned(),
    }
}

/// `<status> · <priority> · <repo>:<branch>, <repo> · <n> PR: <attention>`.
fn detail_line(row: &TicketRow, width: u16) -> Line<'static> {
    let mut parts = vec![row.ticket.status.name.clone()];
    if let Some(p) = &row.ticket.priority {
        parts.push(p.clone());
    }
    parts.push(match row.work.worktrees.split_first() {
        None => "no worktree".to_owned(),
        Some((first, rest)) => {
            let mut text = format!("{}:{}", first.repo, first.branch);
            for w in rest {
                text.push_str(&format!(", {}", w.repo));
            }
            text
        }
    });
    parts.push(match row.work.most_urgent_pr() {
        None => "no PR".to_owned(),
        Some(pr) => {
            let n = row.work.prs.len();
            format!(
                "{n} PR{}: {}",
                if n == 1 { "" } else { "s" },
                attention_label(pr.attention)
            )
        }
    });
    Line::styled(
        truncate(&parts.join(SEPARATOR), usize::from(width)),
        Style::default().fg(Color::DarkGray),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use ratatui::backend::TestBackend;
    use ratatui::buffer::Buffer;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use zax_domain::*;

    use super::*;
    use crate::keys::testing::press;
    use zax_domain::keymap::{Keymaps, PaneContext};

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn keymap() -> Keymap {
        Keymaps::defaults()
            .for_context(PaneContext::Tickets)
            .clone()
    }

    /// Type a key sequence such as `SPC w o`; the action of the last key.
    fn ticket(k: &str, category: StatusCategory, summary: &str) -> Ticket {
        Ticket {
            key: TicketKey::parse(k).unwrap(),
            summary: summary.into(),
            status: TicketStatus {
                category,
                name: "In Review".into(),
            },
            priority: Some("High".into()),
            kind: None,
            url: format!("https://x/browse/{k}"),
        }
    }

    fn worktree(branch: &str) -> Worktree {
        Worktree {
            repo: RootRepoName::parse("omega").unwrap(),
            branch: BranchName::parse_foreign(branch).unwrap(),
            path: PathBuf::from(format!("/w/omega/{branch}")),
        }
    }

    fn pane_with(tickets: Vec<Ticket>, work: WorkIndex) -> TicketsPane {
        let mut pane = TicketsPane::new(keymap());
        pane.set_focused(true);
        pane.apply_tickets(Some(("Jira".into(), true, tickets)), Some(t(100)), None);
        pane.apply_work(Some(work), Some(t(100)), None);
        pane
    }

    fn draw(pane: &TicketsPane, width: u16, height: u16) -> Vec<String> {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|f| pane.render(f, f.area(), t(130))).unwrap();
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
    fn tickets_render_before_work_arrives() {
        let mut pane = TicketsPane::new(keymap());
        pane.apply_tickets(
            Some((
                "Jira".into(),
                true,
                vec![ticket("OMEGA-1", StatusCategory::ToDo, "Do it")],
            )),
            Some(t(100)),
            None,
        );
        pane.apply_work(None, None, Some("gh is not authenticated".into()));
        let lines = draw(&pane, 40, 6);
        assert!(lines.iter().any(|l| l.contains("OMEGA-1")), "{lines:#?}");
        assert!(
            lines.iter().any(|l| l.contains("gh is not authenticated")),
            "{lines:#?}"
        );
    }

    #[test]
    fn a_row_at_24_columns_keeps_eight_columns_of_title() {
        let work = WorkIndex {
            worktrees: vec![worktree("OMEGA-2713-a")],
            ..WorkIndex::default()
        };
        let pane = pane_with(
            vec![ticket(
                "OMEGA-2713",
                StatusCategory::InProgress,
                "Make activity history",
            )],
            work,
        );
        let lines = draw(&pane, 24, 5);
        let row = &lines[1];
        assert!(row.starts_with("◐ OMEGA-2713"), "{row}");
        assert!(row.contains("Make act"), "{row}");
        assert!(display_width(row) <= 24, "{row}");
    }

    #[test]
    fn work_glyphs_give_way_before_the_title_does() {
        // A 12-column key leaves 9 for the title alone, but only 7 once `▲ `
        // is drawn, so the glyph goes and the title keeps its 8.
        let work = WorkIndex {
            worktrees: vec![worktree("OMEGAWI-2713-a")],
            ..WorkIndex::default()
        };
        let pane = pane_with(
            vec![ticket("OMEGAWI-2713", StatusCategory::ToDo, "Abcdefghijk")],
            work,
        );
        let row = draw(&pane, 24, 5)[1].clone();
        assert!(!row.contains('▲'), "{row}");
        assert!(row.contains("Abcdefgh"), "{row}");
    }

    #[test]
    fn the_lead_glyph_follows_the_rank() {
        let work = WorkIndex {
            worktrees: vec![worktree("OMEGA-1-a")],
            sessions: vec![LiveSession {
                name: SessionName::parse("zax-omega-a-00000000").unwrap(),
                attached: true,
                path: PathBuf::from("/w/omega/OMEGA-1-a"),
            }],
            ..WorkIndex::default()
        };
        let pane = pane_with(
            vec![
                ticket("OMEGA-1", StatusCategory::ToDo, "live"),
                ticket("OMEGA-2", StatusCategory::ToDo, "todo"),
            ],
            work,
        );
        let lines = draw(&pane, 40, 6);
        assert!(lines[1].starts_with("● OMEGA-1"), "{lines:#?}");
        assert!(lines[2].starts_with("○ OMEGA-2"), "{lines:#?}");
    }

    #[test]
    fn the_detail_line_spells_out_the_selected_row() {
        let work = WorkIndex {
            worktrees: vec![worktree("OMEGA-1-a")],
            ..WorkIndex::default()
        };
        let pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::InProgress, "x")],
            work,
        );
        let lines = draw(&pane, 80, 5);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("In Review · High · omega:OMEGA-1-a · no PR")),
            "{lines:#?}"
        );
    }

    #[test]
    fn enter_opens_and_spc_y_copies_the_ticket() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            WorkIndex::default(),
        );
        assert_eq!(
            press(&mut pane, "RET"),
            PaneAction::OpenUrl("https://x/browse/OMEGA-1".into())
        );
        assert_eq!(
            press(&mut pane, "SPC y"),
            PaneAction::CopyUrl("https://x/browse/OMEGA-1".into())
        );
    }

    #[test]
    fn spc_w_o_starts_work_when_there_is_none_and_opens_it_when_there_is() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "Make it go")],
            WorkIndex::default(),
        );
        assert_eq!(
            press(&mut pane, "SPC w o"),
            PaneAction::StartWork {
                branch: BranchName::parse_local("OMEGA-1-make-it-go").unwrap(),
                repo: None,
            }
        );
        let work = WorkIndex {
            worktrees: vec![worktree("OMEGA-1-a"), worktree("OMEGA-1-b")],
            ..WorkIndex::default()
        };
        let mut pane = pane_with(vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")], work);
        assert_eq!(
            press(&mut pane, "SPC w o"),
            PaneAction::OpenWorktree {
                path: PathBuf::from("/w/omega/OMEGA-1-a"),
                others: 1
            }
        );
    }

    #[test]
    fn spc_w_o_asks_for_a_repo_when_projects_are_configured() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "Make it go")],
            WorkIndex::default(),
        );
        pane.set_repo_choice(true);
        assert_eq!(
            press(&mut pane, "SPC w o"),
            PaneAction::ChooseRepo {
                branch: BranchName::parse_local("OMEGA-1-make-it-go").unwrap()
            }
        );
    }

    #[test]
    fn spc_w_o_starts_work_directly_without_projects() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "Make it go")],
            WorkIndex::default(),
        );
        pane.set_repo_choice(false);
        assert_eq!(
            press(&mut pane, "SPC w o"),
            PaneAction::StartWork {
                branch: BranchName::parse_local("OMEGA-1-make-it-go").unwrap(),
                repo: None,
            }
        );
    }

    #[test]
    fn an_existing_worktree_opens_without_the_picker() {
        let work = WorkIndex {
            worktrees: vec![worktree("OMEGA-1-a")],
            ..WorkIndex::default()
        };
        let mut pane = pane_with(vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")], work);
        pane.set_repo_choice(true);
        assert_eq!(
            press(&mut pane, "SPC w o"),
            PaneAction::OpenWorktree {
                path: PathBuf::from("/w/omega/OMEGA-1-a"),
                others: 0
            }
        );
    }

    fn repo_choices(preselected: Option<usize>) -> RepoChoices {
        RepoChoices {
            checkouts: vec![accept_checkout(CheckoutProbe::Main {
                top: PathBuf::from("/h/src/omega"),
                origin_url: None,
            })
            .unwrap()],
            preselected,
        }
    }

    #[test]
    fn choosing_a_repo_starts_work_there() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "Make it go")],
            WorkIndex::default(),
        );
        let branch = BranchName::parse_local("OMEGA-1-go").unwrap();
        pane.show_repo_picker(branch.clone(), repo_choices(None));
        assert_eq!(
            press(&mut pane, "RET"),
            PaneAction::StartWork {
                branch,
                repo: Some(PathBuf::from("/h/src/omega")),
            }
        );
    }

    #[test]
    fn cancelling_the_picker_aborts() {
        let mut pane = pane_with(
            vec![
                ticket("OMEGA-1", StatusCategory::ToDo, "x"),
                ticket("OMEGA-2", StatusCategory::ToDo, "y"),
            ],
            WorkIndex::default(),
        );
        let branch = BranchName::parse_local("OMEGA-1-go").unwrap();
        pane.show_repo_picker(branch, repo_choices(None));
        assert_eq!(pane.on_key(key(KeyCode::Esc)), PaneAction::None);
        // The picker is gone, not merely hidden: the next `j` moves the
        // ticket list rather than being swallowed by the picker again.
        assert_eq!(press(&mut pane, "j"), PaneAction::None);
        assert_eq!(pane.selected, 1);
    }

    #[test]
    fn spc_w_l_asks_for_the_worktrees_popup_or_says_there_are_none() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            WorkIndex::default(),
        );
        assert_eq!(press(&mut pane, "SPC w l"), PaneAction::None);
        assert!(draw(&pane, 60, 6)
            .join("\n")
            .contains("OMEGA-1 has no worktrees"));
        let work = WorkIndex {
            worktrees: vec![worktree("OMEGA-1-a")],
            ..WorkIndex::default()
        };
        let mut pane = pane_with(vec![ticket("OMEGA-1", StatusCategory::Done, "x")], work);
        assert_eq!(
            press(&mut pane, "SPC w l"),
            PaneAction::ListWorktrees {
                key: TicketKey::parse("OMEGA-1").unwrap()
            }
        );
    }

    #[test]
    fn spc_o_p_without_a_pull_request_says_so() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            WorkIndex::default(),
        );
        assert_eq!(press(&mut pane, "SPC o p"), PaneAction::None);
        assert!(draw(&pane, 40, 6)
            .iter()
            .any(|l| l.contains("no pull request")));
    }

    #[test]
    fn spc_s_asks_for_transitions_and_a_digit_picks_one() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            WorkIndex::default(),
        );
        let k = TicketKey::parse("OMEGA-1").unwrap();
        assert_eq!(
            press(&mut pane, "SPC s"),
            PaneAction::ListTransitions { key: k.clone() }
        );
        pane.show_picker(
            k.clone(),
            vec![
                Transition {
                    id: "In Progress".into(),
                    name: "In Progress".into(),
                },
                Transition {
                    id: "Done".into(),
                    name: "Done".into(),
                },
            ],
        );
        assert!(draw(&pane, 60, 5)[0].contains("→ 1 In Progress  2 Done"));
        assert_eq!(
            pane.on_key(key(KeyCode::Char('2'))),
            PaneAction::Transition {
                key: k,
                id: "Done".into()
            }
        );
        assert!(
            !draw(&pane, 60, 5)[0].contains('→'),
            "the picker closes once used"
        );
    }

    #[test]
    fn a_prefix_typed_before_the_picker_opened_does_not_wait_behind_it() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            WorkIndex::default(),
        );
        let k = TicketKey::parse("OMEGA-1").unwrap();
        assert_eq!(press(&mut pane, "SPC"), PaneAction::None);
        pane.show_picker(
            k.clone(),
            vec![
                Transition {
                    id: "a".into(),
                    name: "a".into(),
                },
                Transition {
                    id: "b".into(),
                    name: "b".into(),
                },
            ],
        );
        assert_eq!(
            pane.on_key(key(KeyCode::Char('2'))),
            PaneAction::Transition {
                key: k,
                id: "b".into()
            }
        );
        assert_eq!(press(&mut pane, "q"), PaneAction::None);
    }

    #[test]
    fn esc_closes_the_picker_rather_than_the_pane() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            WorkIndex::default(),
        );
        pane.show_picker(
            TicketKey::parse("OMEGA-1").unwrap(),
            vec![Transition {
                id: "a".into(),
                name: "a".into(),
            }],
        );
        assert_eq!(pane.on_key(key(KeyCode::Esc)), PaneAction::None);
        assert_eq!(press(&mut pane, "SPC q"), PaneAction::Quit);
    }

    #[test]
    fn spc_s_does_nothing_when_the_provider_cannot_transition() {
        let mut pane = TicketsPane::new(keymap());
        pane.apply_tickets(
            Some((
                "Jira".into(),
                false,
                vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            )),
            Some(t(1)),
            None,
        );
        assert_eq!(press(&mut pane, "SPC s"), PaneAction::None);
    }

    #[test]
    fn spc_r_refreshes_and_spc_q_quits_but_q_and_esc_do_not() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            WorkIndex::default(),
        );
        assert_eq!(press(&mut pane, "SPC r"), PaneAction::Refresh);
        assert_eq!(press(&mut pane, "q"), PaneAction::None);
        assert_eq!(press(&mut pane, "<Esc>"), PaneAction::None);
        assert_eq!(press(&mut pane, "SPC q"), PaneAction::Quit);
    }

    #[test]
    fn an_unconfigured_pane_still_quits_with_spc_q() {
        let mut pane = TicketsPane::unconfigured(keymap());
        assert_eq!(press(&mut pane, "SPC q"), PaneAction::Quit);
    }

    #[test]
    fn a_pane_notice_stays_in_the_body_and_the_status_bar_fills_the_footer() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            WorkIndex::default(),
        );
        press(&mut pane, "SPC o p");
        let lines = draw(&pane, 80, 6);
        assert!(lines[..5].iter().any(|l| l.contains("no pull request")));
        assert!(lines[5].contains("SPC"), "{lines:#?}");
    }

    #[test]
    fn an_unconfigured_pane_says_how_to_configure_it() {
        let lines = draw(&TicketsPane::unconfigured(keymap()), 40, 4);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("No ticket provider configured")),
            "{lines:#?}"
        );
    }

    #[test]
    fn a_stale_snapshot_keeps_its_rows_and_marks_the_header() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            WorkIndex::default(),
        );
        pane.apply_tickets(
            Some((
                "Jira".into(),
                true,
                vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            )),
            Some(t(100)),
            Some("acli: 502".into()),
        );
        let lines = draw(&pane, 60, 5);
        assert!(lines[0].contains("stale"), "{lines:#?}");
        assert!(lines[1].contains("OMEGA-1"));
    }
}
