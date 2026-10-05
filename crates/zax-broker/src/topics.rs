//! What the two topics carry, and how a pane reads them back.
//!
//! The domain has no serde, by design, so the `work` index crosses the
//! socket as the DTOs here. They are the broker's own: nothing outside zax
//! reads them, unlike the provider protocol.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use zax_domain::{
    BranchName, CheckState, LiveSession, Mergeable, MyPullRequest, PullRequest, PullRequestStatus,
    RepoSlug, ReviewState, RootRepoName, SessionName, StatusCategory, Ticket, TicketKey,
    TicketStatus, Transition, WorkIndex, Worktree,
};
use zax_protocol::{category, TicketsTopic, TransitionsResult, WireTicket};

pub const TICKETS: &str = "tickets";
pub const WORK: &str = "work";

/// The `tickets` source spec: the provider command as config resolved it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketsSpec {
    pub provider: Vec<String>,
}

/// The `work` source spec: the worktree root as config resolved it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkSpec {
    pub worktree_root: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TicketList {
    pub provider: String,
    pub transitions: bool,
    pub tickets: Vec<Ticket>,
    /// One line per ticket that was dropped, for the broker log or a pane.
    pub dropped: Vec<String>,
}

/// Reads a `tickets` snapshot. A malformed ticket is dropped on its own, so
/// one bad row from a provider cannot blank the whole list.
pub fn decode_tickets(value: &Value) -> Result<TicketList, String> {
    let topic: TicketsTopic = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    let mut tickets = Vec::new();
    let mut dropped = Vec::new();
    for wire in topic.tickets {
        match ticket_from_wire(wire) {
            Ok(ticket) => tickets.push(ticket),
            Err(reason) => dropped.push(reason),
        }
    }
    Ok(TicketList {
        provider: topic.name,
        transitions: topic.transitions,
        tickets,
        dropped,
    })
}

fn ticket_from_wire(wire: WireTicket) -> Result<Ticket, String> {
    let key = TicketKey::parse(&wire.key).map_err(|e| e.to_string())?;
    let category = match wire.status.category.as_str() {
        category::TO_DO => StatusCategory::ToDo,
        category::IN_PROGRESS => StatusCategory::InProgress,
        category::DONE => StatusCategory::Done,
        other => return Err(format!("{key}: unknown status category `{other}`")),
    };
    Ok(Ticket {
        key,
        summary: wire.summary,
        status: TicketStatus {
            category,
            name: wire.status.name,
        },
        priority: wire.priority,
        kind: wire.kind,
        url: wire.url,
    })
}

pub fn decode_transitions(value: &Value) -> Result<Vec<Transition>, String> {
    let result: TransitionsResult =
        serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    Ok(result
        .transitions
        .into_iter()
        .map(|t| Transition {
            id: t.id,
            name: t.name,
        })
        .collect())
}

#[derive(Serialize, Deserialize)]
struct WorkDto {
    worktrees: Vec<WorktreeDto>,
    sessions: Vec<SessionDto>,
    prs: Vec<PrDto>,
}

// Paths travel as strings: a `PathBuf` that is not UTF-8 cannot be
// serialized, and the encoder must not be able to fail.
#[derive(Serialize, Deserialize)]
struct WorktreeDto {
    repo: String,
    branch: String,
    path: String,
}

#[derive(Serialize, Deserialize)]
struct SessionDto {
    name: String,
    attached: bool,
    path: String,
}

#[derive(Serialize, Deserialize)]
struct PrDto {
    repo: String,
    number: u32,
    title: String,
    url: String,
    is_draft: bool,
    created_at: u64,
    checks: String,
    review: String,
    unresolved_threads: u32,
    mergeable: String,
    head: Option<String>,
}

fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

pub fn encode_work(work: &WorkIndex) -> Value {
    let dto = WorkDto {
        worktrees: work
            .worktrees
            .iter()
            .map(|w| WorktreeDto {
                repo: w.repo.as_str().to_owned(),
                branch: w.branch.as_str().to_owned(),
                path: path_to_string(&w.path),
            })
            .collect(),
        sessions: work
            .sessions
            .iter()
            .map(|s| SessionDto {
                name: s.name.as_str().to_owned(),
                attached: s.attached,
                path: path_to_string(&s.path),
            })
            .collect(),
        prs: work.prs.iter().map(|m| pr_to_dto(&m.status)).collect(),
    };
    // The DTOs hold only strings, integers, booleans and string-keyed
    // structs, none of which `serde_json` can fail to represent, so the
    // fallback is unreachable. Were it ever hit, `decode_work` rejects a
    // null snapshot with an error rather than reading it as an empty index.
    serde_json::to_value(dto).unwrap_or(Value::Null)
}

pub fn decode_work(value: &Value) -> Result<WorkIndex, String> {
    let dto: WorkDto = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    let worktrees = dto
        .worktrees
        .into_iter()
        .map(|w| {
            Ok(Worktree {
                repo: RootRepoName::parse(&w.repo).map_err(|e| e.to_string())?,
                branch: BranchName::parse_foreign(&w.branch).map_err(|e| e.to_string())?,
                path: PathBuf::from(w.path),
            })
        })
        .collect::<Result<_, String>>()?;
    let sessions = dto
        .sessions
        .into_iter()
        .map(|s| {
            Ok(LiveSession {
                name: SessionName::parse(&s.name).map_err(|e| e.to_string())?,
                attached: s.attached,
                path: PathBuf::from(s.path),
            })
        })
        .collect::<Result<_, String>>()?;
    let prs = dto
        .prs
        .into_iter()
        .map(|p| pr_from_dto(p).map(MyPullRequest::new))
        .collect::<Result<_, String>>()?;
    Ok(WorkIndex {
        worktrees,
        sessions,
        prs,
    })
}

// The state names are written out rather than taken from `Debug`, so
// renaming a variant cannot silently change the wire format. The matches
// are exhaustive, so adding a variant is a compile error here.
fn checks_name(state: CheckState) -> &'static str {
    match state {
        CheckState::Passing => "Passing",
        CheckState::Failing => "Failing",
        CheckState::Pending => "Pending",
        CheckState::None => "None",
    }
}

fn review_name(state: ReviewState) -> &'static str {
    match state {
        ReviewState::Approved => "Approved",
        ReviewState::ChangesRequested => "ChangesRequested",
        ReviewState::ReviewRequired => "ReviewRequired",
        ReviewState::None => "None",
    }
}

fn mergeable_name(state: Mergeable) -> &'static str {
    match state {
        Mergeable::Mergeable => "Mergeable",
        Mergeable::Conflicting => "Conflicting",
        Mergeable::Unknown => "Unknown",
    }
}

fn pr_to_dto(s: &PullRequestStatus) -> PrDto {
    PrDto {
        repo: format!("{}/{}", s.pr.repo.owner(), s.pr.repo.name()),
        number: s.pr.number,
        title: s.pr.title.clone(),
        url: s.pr.url.clone(),
        is_draft: s.pr.is_draft,
        created_at: s
            .pr
            .created_at
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        checks: checks_name(s.pr.checks).to_owned(),
        review: review_name(s.review).to_owned(),
        unresolved_threads: s.unresolved_threads,
        mergeable: mergeable_name(s.mergeable).to_owned(),
        head: s.head.as_ref().map(|b| b.as_str().to_owned()),
    }
}

fn pr_from_dto(p: PrDto) -> Result<PullRequestStatus, String> {
    let checks = match p.checks.as_str() {
        "Passing" => CheckState::Passing,
        "Failing" => CheckState::Failing,
        "Pending" => CheckState::Pending,
        "None" => CheckState::None,
        other => return Err(format!("unknown check state `{other}`")),
    };
    let review = match p.review.as_str() {
        "Approved" => ReviewState::Approved,
        "ChangesRequested" => ReviewState::ChangesRequested,
        "ReviewRequired" => ReviewState::ReviewRequired,
        "None" => ReviewState::None,
        other => return Err(format!("unknown review state `{other}`")),
    };
    let mergeable = match p.mergeable.as_str() {
        "Mergeable" => Mergeable::Mergeable,
        "Conflicting" => Mergeable::Conflicting,
        "Unknown" => Mergeable::Unknown,
        other => return Err(format!("unknown mergeable state `{other}`")),
    };
    Ok(PullRequestStatus {
        pr: PullRequest {
            repo: RepoSlug::parse(&p.repo).map_err(|e| e.to_string())?,
            number: p.number,
            title: p.title,
            url: p.url,
            is_draft: p.is_draft,
            created_at: fetched_at(p.created_at),
            checks,
        },
        review,
        unresolved_threads: p.unresolved_threads,
        mergeable,
        head: p.head.and_then(|h| BranchName::parse_foreign(&h).ok()),
    })
}

/// Seconds since the epoch, as a snapshot carries them, back to a time.
pub fn fetched_at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::path::PathBuf;
    use std::time::{Duration, UNIX_EPOCH};

    use serde_json::json;
    use zax_domain::*;

    use super::*;

    #[test]
    fn tickets_decode_into_the_domain() {
        let value = json!({"name":"Jira","transitions":true,"tickets":[
            {"key":"OMEGA-1","summary":"s","status":{"category":"in_progress","name":"In Review"},"priority":"High","url":"u"}
        ]});
        let list = decode_tickets(&value).unwrap();
        assert_eq!(list.provider, "Jira");
        assert!(list.transitions);
        assert_eq!(list.tickets[0].status.category, StatusCategory::InProgress);
        assert_eq!(list.tickets[0].priority.as_deref(), Some("High"));
    }

    #[test]
    fn a_bad_ticket_drops_that_ticket_not_the_list() {
        let value = json!({"name":"Jira","transitions":false,"tickets":[
            {"key":"lower-1","summary":"s","status":{"category":"to_do","name":"x"},"url":"u"},
            {"key":"OMEGA-2","summary":"s","status":{"category":"blocked","name":"x"},"url":"u"},
            {"key":"OMEGA-3","summary":"s","status":{"category":"done","name":"Done"},"url":"u"}
        ]});
        let list = decode_tickets(&value).unwrap();
        assert_eq!(list.tickets.len(), 1);
        assert_eq!(list.dropped.len(), 2);
        assert!(list.dropped[1].contains("blocked"), "{:?}", list.dropped);
    }

    #[test]
    fn the_work_index_round_trips() {
        let work = WorkIndex {
            worktrees: vec![Worktree {
                repo: RootRepoName::parse("omega").unwrap(),
                branch: BranchName::parse_foreign("OMEGA-1-a").unwrap(),
                path: PathBuf::from("/w/omega/OMEGA-1-a"),
            }],
            sessions: vec![LiveSession {
                name: SessionName::parse("zax-omega-x-00000000").unwrap(),
                attached: true,
                path: PathBuf::from("/w/omega/OMEGA-1-a"),
            }],
            prs: vec![MyPullRequest::new(PullRequestStatus {
                pr: PullRequest {
                    repo: RepoSlug::parse("acme/omega").unwrap(),
                    number: 7,
                    title: "t".into(),
                    url: "u".into(),
                    is_draft: true,
                    created_at: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
                    checks: CheckState::Failing,
                },
                review: ReviewState::ChangesRequested,
                unresolved_threads: 2,
                mergeable: Mergeable::Conflicting,
                head: Some(BranchName::parse_foreign("OMEGA-1-a").unwrap()),
            })],
        };
        assert_eq!(decode_work(&encode_work(&work)).unwrap(), work);
    }

    #[test]
    fn transitions_decode() {
        let t =
            decode_transitions(&json!({"transitions":[{"id":"31","name":"In Progress"}]})).unwrap();
        assert_eq!(
            t,
            vec![Transition {
                id: "31".into(),
                name: "In Progress".into()
            }]
        );
    }

    #[test]
    fn spec_keys_are_stable() {
        let key = zax_protocol::SourceKey::new(
            TICKETS,
            &TicketsSpec {
                provider: vec!["zax-jira".into()],
            },
        );
        assert_eq!(key.source, r#"{"provider":["zax-jira"]}"#);
    }
}
