//! `tickets`: the tickets assigned to you, each with the work behind it.

use std::time::SystemTime;

use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use zax_domain::{
    branch_slug, join, Attention, Ticket, TicketKey, TicketRank, TicketRow, Transition, WorkIndex,
};

use crate::action::PaneAction;
use crate::cache::Cached;
use crate::chrome::{error_line, header_line};
use crate::fmt::{display_width, truncate};
use crate::my_prs_pane::{attention_glyph, attention_style};
use crate::pr_fields::{check_glyph, check_style};

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
    notice: Option<String>,
}

impl Default for TicketsPane {
    fn default() -> Self {
        Self::new()
    }
}

impl TicketsPane {
    pub fn new() -> Self {
        Self {
            configured: true,
            tickets: Cached::new(),
            work: Cached::new(),
            rows: Vec::new(),
            selected: 0,
            focused: false,
            picker: None,
            notice: None,
        }
    }

    /// No `[tickets] provider`: the pane explains itself and never talks to
    /// the broker.
    pub fn unconfigured() -> Self {
        Self {
            configured: false,
            ..Self::new()
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
        self.picker = Some(Picker { key, transitions });
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
        let mut lines = vec![self.top_line(area.width, now)];

        if !self.configured {
            lines.push(Line::styled(
                "No ticket provider configured; set [tickets] provider",
                Style::default().fg(Color::DarkGray),
            ));
            frame.render_widget(Paragraph::new(Text::from(lines)), area);
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
            KeyCode::Enter => self
                .selected_row()
                .map(|r| PaneAction::OpenUrl(r.ticket.url.clone()))
                .unwrap_or_default(),
            KeyCode::Char('y') => self
                .selected_row()
                .map(|r| PaneAction::CopyUrl(r.ticket.url.clone()))
                .unwrap_or_default(),
            KeyCode::Char('p') => match self
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
            KeyCode::Char('w') => self.start_or_open(),
            KeyCode::Char('s') => {
                let can = self.tickets.value().is_some_and(|v| v.transitions);
                match self.selected_row() {
                    Some(r) if can => PaneAction::ListTransitions {
                        key: r.ticket.key.clone(),
                    },
                    _ => PaneAction::None,
                }
            }
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
            Ok(branch) => PaneAction::StartWork { branch },
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

    fn t(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }
    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

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
        let mut pane = TicketsPane::new();
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
        let mut pane = TicketsPane::new();
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
    fn enter_opens_and_y_copies_the_ticket() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            WorkIndex::default(),
        );
        assert_eq!(
            pane.on_key(key(KeyCode::Enter)),
            PaneAction::OpenUrl("https://x/browse/OMEGA-1".into())
        );
        assert_eq!(
            pane.on_key(key(KeyCode::Char('y'))),
            PaneAction::CopyUrl("https://x/browse/OMEGA-1".into())
        );
    }

    #[test]
    fn w_starts_work_when_there_is_none_and_opens_it_when_there_is() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "Make it go")],
            WorkIndex::default(),
        );
        assert_eq!(
            pane.on_key(key(KeyCode::Char('w'))),
            PaneAction::StartWork {
                branch: BranchName::parse_local("OMEGA-1-make-it-go").unwrap()
            }
        );
        let work = WorkIndex {
            worktrees: vec![worktree("OMEGA-1-a"), worktree("OMEGA-1-b")],
            ..WorkIndex::default()
        };
        let mut pane = pane_with(vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")], work);
        assert_eq!(
            pane.on_key(key(KeyCode::Char('w'))),
            PaneAction::OpenWorktree {
                path: PathBuf::from("/w/omega/OMEGA-1-a"),
                others: 1
            }
        );
    }

    #[test]
    fn p_without_a_pull_request_says_so() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            WorkIndex::default(),
        );
        assert_eq!(pane.on_key(key(KeyCode::Char('p'))), PaneAction::None);
        assert!(draw(&pane, 40, 6)
            .iter()
            .any(|l| l.contains("no pull request")));
    }

    #[test]
    fn s_asks_for_transitions_and_a_digit_picks_one() {
        let mut pane = pane_with(
            vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            WorkIndex::default(),
        );
        let k = TicketKey::parse("OMEGA-1").unwrap();
        assert_eq!(
            pane.on_key(key(KeyCode::Char('s'))),
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
        assert_eq!(pane.on_key(key(KeyCode::Esc)), PaneAction::Quit);
    }

    #[test]
    fn s_does_nothing_when_the_provider_cannot_transition() {
        let mut pane = TicketsPane::new();
        pane.apply_tickets(
            Some((
                "Jira".into(),
                false,
                vec![ticket("OMEGA-1", StatusCategory::ToDo, "x")],
            )),
            Some(t(1)),
            None,
        );
        assert_eq!(pane.on_key(key(KeyCode::Char('s'))), PaneAction::None);
    }

    #[test]
    fn an_unconfigured_pane_says_how_to_configure_it() {
        let lines = draw(&TicketsPane::unconfigured(), 40, 4);
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
