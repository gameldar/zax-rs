//! The work behind a ticket: its worktrees, the sessions running in them, and
//! its pull requests, in every repository.
//!
//! The join is a pure function so that it can be tested without a tracker, a
//! forge or a multiplexer, and so the source that collects the work can be
//! shared by panes that know nothing of tickets.

use std::path::PathBuf;

use crate::branch::BranchName;
use crate::forge::MyPullRequest;
use crate::repo::RootRepoName;
use crate::session::LiveSession;
use crate::ticket::{StatusCategory, Ticket, TicketKey};

/// A worktree under the configured root, in any repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub repo: RootRepoName,
    pub branch: BranchName,
    pub path: PathBuf,
}

/// Everything the `work` source collects. Not specific to tickets.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WorkIndex {
    pub worktrees: Vec<Worktree>,
    pub sessions: Vec<LiveSession>,
    pub prs: Vec<MyPullRequest>,
}

/// One ticket's share of the [`WorkIndex`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TicketWork {
    pub worktrees: Vec<Worktree>,
    /// Sessions running in any of those worktrees.
    pub live_sessions: u8,
    /// Whether anyone is looking at one of them.
    pub attached: bool,
    pub prs: Vec<MyPullRequest>,
}

impl TicketWork {
    pub fn is_empty(&self) -> bool {
        self.worktrees.is_empty() && self.prs.is_empty() && self.live_sessions == 0
    }

    /// The pull request most waiting on you, by the ranking `my-prs` sorts on.
    pub fn most_urgent_pr(&self) -> Option<&MyPullRequest> {
        self.prs.iter().min_by_key(|pr| pr.attention.rank())
    }
}

/// Where a row sorts, and which lead glyph it draws: one ordering for both,
/// so a row can never sort above one whose glyph looks more pressing.
///
/// No payload, so the derived `Ord` (declaration order) is exactly the
/// ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TicketRank {
    /// An agent session is running in one of its worktrees.
    Live,
    InProgress,
    ToDo,
    Done,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketRow {
    pub ticket: Ticket,
    pub work: TicketWork,
}

impl TicketRow {
    pub fn rank(&self) -> TicketRank {
        if self.work.live_sessions > 0 {
            return TicketRank::Live;
        }
        match self.ticket.status.category {
            StatusCategory::InProgress => TicketRank::InProgress,
            StatusCategory::ToDo => TicketRank::ToDo,
            StatusCategory::Done => TicketRank::Done,
        }
    }

    /// The tie-break inside a rank: the most urgent pull request's rank, and
    /// no pull request after any.
    fn pr_rank(&self) -> u8 {
        self.work
            .most_urgent_pr()
            .map_or(u8::MAX, |pr| pr.attention.rank())
    }
}

/// Pair each ticket with its work and sort the result.
///
/// A done ticket is kept only while work remains: it is there as a nudge to
/// clean up, not as a record.
pub fn join(tickets: Vec<Ticket>, work: &WorkIndex) -> Vec<TicketRow> {
    let mut rows: Vec<TicketRow> = tickets
        .into_iter()
        .map(|ticket| {
            let work = work_for(&ticket.key, work);
            TicketRow { ticket, work }
        })
        .filter(|row| row.ticket.status.category != StatusCategory::Done || !row.work.is_empty())
        .collect();
    rows.sort_by(|a, b| {
        a.rank()
            .cmp(&b.rank())
            .then_with(|| a.pr_rank().cmp(&b.pr_rank()))
            .then_with(|| a.ticket.key.cmp(&b.ticket.key))
    });
    rows
}

fn work_for(key: &TicketKey, work: &WorkIndex) -> TicketWork {
    let names_key = |branch: &BranchName| branch.ticket().as_ref() == Some(key);

    let worktrees: Vec<Worktree> = work
        .worktrees
        .iter()
        .filter(|w| names_key(&w.branch))
        .cloned()
        .collect();
    // `Path::starts_with` compares whole components, so `OMEGA-1-ab` is not
    // inside `OMEGA-1-a`, while a session started in a subdirectory is.
    let sessions: Vec<&LiveSession> = work
        .sessions
        .iter()
        .filter(|s| worktrees.iter().any(|w| s.path.starts_with(&w.path)))
        .collect();
    let prs = work
        .prs
        .iter()
        .filter(|pr| pr.status.head.as_ref().is_some_and(names_key))
        .cloned()
        .collect();

    TicketWork {
        live_sessions: u8::try_from(sessions.len()).unwrap_or(u8::MAX),
        attached: sessions.iter().any(|s| s.attached),
        worktrees,
        prs,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::time::UNIX_EPOCH;

    use super::*;
    use crate::{
        CheckState, Mergeable, PullRequest, PullRequestStatus, RepoSlug, ReviewState, SessionName,
        TicketKey, TicketStatus,
    };

    fn ticket(key: &str, category: StatusCategory) -> Ticket {
        Ticket {
            key: TicketKey::parse(key).unwrap(),
            summary: format!("{key} summary"),
            status: TicketStatus {
                category,
                name: format!("{category:?}"),
            },
            priority: None,
            kind: None,
            url: format!("https://example.atlassian.net/browse/{key}"),
        }
    }

    fn worktree(repo: &str, branch: &str) -> Worktree {
        Worktree {
            repo: RootRepoName::parse(repo).unwrap(),
            branch: BranchName::parse_foreign(branch).unwrap(),
            path: PathBuf::from(format!("/w/{repo}/{branch}")),
        }
    }

    fn session(path: &str, attached: bool) -> LiveSession {
        LiveSession {
            name: SessionName::parse("zax-x-y-00000000").unwrap(),
            attached,
            path: PathBuf::from(path),
        }
    }

    fn pr(head: &str, review: ReviewState) -> MyPullRequest {
        MyPullRequest::new(PullRequestStatus {
            pr: PullRequest {
                repo: RepoSlug::parse("acme/omega").unwrap(),
                number: 1,
                title: "t".to_owned(),
                url: "https://github.com/acme/omega/pull/1".to_owned(),
                is_draft: false,
                created_at: UNIX_EPOCH,
                checks: CheckState::Passing,
            },
            review,
            unresolved_threads: 0,
            mergeable: Mergeable::Mergeable,
            head: BranchName::parse_foreign(head).ok(),
        })
    }

    fn keys(rows: &[TicketRow]) -> Vec<&str> {
        rows.iter().map(|r| r.ticket.key.as_str()).collect()
    }

    #[test]
    fn worktrees_join_on_the_ticket_key_their_branch_leads_with() {
        let work = WorkIndex {
            worktrees: vec![
                worktree("omega", "OMEGA-1-a"),
                worktree("api", "OMEGA-1-b"),
                worktree("omega", "OMEGA-2-c"),
                worktree("omega", "topic"),
            ],
            ..WorkIndex::default()
        };
        let rows = join(vec![ticket("OMEGA-1", StatusCategory::ToDo)], &work);
        assert_eq!(rows[0].work.worktrees.len(), 2);
    }

    #[test]
    fn pull_requests_join_on_their_head_branch() {
        let work = WorkIndex {
            prs: vec![
                pr("OMEGA-1-a", ReviewState::None),
                pr("OMEGA-2-b", ReviewState::None),
            ],
            ..WorkIndex::default()
        };
        let rows = join(vec![ticket("OMEGA-1", StatusCategory::InProgress)], &work);
        assert_eq!(rows[0].work.prs.len(), 1);
    }

    #[test]
    fn sessions_join_through_the_worktree_path() {
        let work = WorkIndex {
            worktrees: vec![worktree("omega", "OMEGA-1-a")],
            sessions: vec![
                session("/w/omega/OMEGA-1-a", true),
                session("/elsewhere", true),
            ],
            ..WorkIndex::default()
        };
        let row = &join(vec![ticket("OMEGA-1", StatusCategory::ToDo)], &work)[0];
        assert_eq!(row.work.live_sessions, 1);
        assert!(row.work.attached);
    }

    #[test]
    fn a_session_in_a_subdirectory_counts_for_the_worktree() {
        let work = WorkIndex {
            worktrees: vec![worktree("omega", "OMEGA-1-a")],
            sessions: vec![session("/w/omega/OMEGA-1-a/crates/api", false)],
            ..WorkIndex::default()
        };
        let row = &join(vec![ticket("OMEGA-1", StatusCategory::ToDo)], &work)[0];
        assert_eq!(row.work.live_sessions, 1);
        assert!(!row.work.attached);
    }

    #[test]
    fn a_sibling_directory_with_a_shared_prefix_is_not_the_worktree() {
        let work = WorkIndex {
            worktrees: vec![worktree("omega", "OMEGA-1-a")],
            sessions: vec![session("/w/omega/OMEGA-1-ab", false)],
            ..WorkIndex::default()
        };
        let row = &join(vec![ticket("OMEGA-1", StatusCategory::ToDo)], &work)[0];
        assert_eq!(row.work.live_sessions, 0);
    }

    #[test]
    fn live_work_sorts_first_then_in_progress_then_to_do() {
        let work = WorkIndex {
            worktrees: vec![worktree("omega", "OMEGA-3-a")],
            sessions: vec![session("/w/omega/OMEGA-3-a", false)],
            ..WorkIndex::default()
        };
        let rows = join(
            vec![
                ticket("OMEGA-1", StatusCategory::ToDo),
                ticket("OMEGA-2", StatusCategory::InProgress),
                ticket("OMEGA-3", StatusCategory::ToDo),
            ],
            &work,
        );
        assert_eq!(keys(&rows), ["OMEGA-3", "OMEGA-2", "OMEGA-1"]);
        assert_eq!(rows[0].rank(), TicketRank::Live);
    }

    #[test]
    fn within_a_rank_the_more_urgent_pull_request_comes_first() {
        let work = WorkIndex {
            prs: vec![
                pr("OMEGA-1-a", ReviewState::None),
                pr("OMEGA-2-a", ReviewState::ChangesRequested),
            ],
            ..WorkIndex::default()
        };
        let rows = join(
            vec![
                ticket("OMEGA-1", StatusCategory::InProgress),
                ticket("OMEGA-2", StatusCategory::InProgress),
                ticket("OMEGA-0", StatusCategory::InProgress),
            ],
            &work,
        );
        // Changes requested outranks waiting; no pull request at all sorts last.
        assert_eq!(keys(&rows), ["OMEGA-2", "OMEGA-1", "OMEGA-0"]);
    }

    #[test]
    fn a_done_ticket_shows_only_while_work_remains() {
        let work = WorkIndex {
            worktrees: vec![worktree("omega", "OMEGA-1-a")],
            ..WorkIndex::default()
        };
        let rows = join(
            vec![
                ticket("OMEGA-1", StatusCategory::Done),
                ticket("OMEGA-2", StatusCategory::Done),
            ],
            &work,
        );
        assert_eq!(keys(&rows), ["OMEGA-1"]);
    }
}
