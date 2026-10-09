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
    BranchName, CheckState, Date, LineRef, LiveSession, Mergeable, MyPullRequest, PullRequest,
    PullRequestStatus, RepoSlug, ReviewState, RootRepoName, SessionName, StatusCategory, Ticket,
    TicketKey, TicketStatus, TodoEdit, TodoItem, TodoList, Transition, WorkIndex, Worktree,
};
use zax_protocol::{category, TicketsTopic, TransitionsResult, WireTicket};

pub const TICKETS: &str = "tickets";
pub const WORK: &str = "work";
pub const TODO: &str = "todo";

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

/// The `todo` source spec: the todo file as config resolved it, absolute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TodoSpec {
    pub file: String,
}

/// The writes the `todo` source takes, one per `TodoEdit` variant.
pub mod todo_method {
    pub const ADD: &str = "todo/add";
    pub const TOGGLE: &str = "todo/toggle";
    pub const SET: &str = "todo/set";
    pub const DELETE: &str = "todo/delete";
    pub const SNOOZE: &str = "todo/snooze";
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

/// Reads a `work` snapshot. Only a snapshot of the wrong overall shape is an
/// error. A worktree, session or PR whose names do not parse is dropped on
/// its own, and an unknown state string falls back to the field's neutral
/// value, so one odd row (or a newer broker's new state) cannot blank the
/// whole index.
pub fn decode_work(value: &Value) -> Result<WorkIndex, String> {
    let dto: WorkDto = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    let worktrees = dto
        .worktrees
        .into_iter()
        .filter_map(|w| {
            Some(Worktree {
                repo: RootRepoName::parse(&w.repo).ok()?,
                branch: BranchName::parse_foreign(&w.branch).ok()?,
                path: PathBuf::from(w.path),
            })
        })
        .collect();
    let sessions = dto
        .sessions
        .into_iter()
        .filter_map(|s| {
            Some(LiveSession {
                name: SessionName::parse(&s.name).ok()?,
                attached: s.attached,
                path: PathBuf::from(s.path),
            })
        })
        .collect();
    let prs = dto
        .prs
        .into_iter()
        .filter_map(|p| pr_from_dto(p).map(MyPullRequest::new))
        .collect();
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

fn pr_from_dto(p: PrDto) -> Option<PullRequestStatus> {
    let checks = match p.checks.as_str() {
        "Passing" => CheckState::Passing,
        "Failing" => CheckState::Failing,
        "Pending" => CheckState::Pending,
        _ => CheckState::None,
    };
    let review = match p.review.as_str() {
        "Approved" => ReviewState::Approved,
        "ChangesRequested" => ReviewState::ChangesRequested,
        "ReviewRequired" => ReviewState::ReviewRequired,
        _ => ReviewState::None,
    };
    let mergeable = match p.mergeable.as_str() {
        "Mergeable" => Mergeable::Mergeable,
        "Conflicting" => Mergeable::Conflicting,
        _ => Mergeable::Unknown,
    };
    Some(PullRequestStatus {
        pr: PullRequest {
            repo: RepoSlug::parse(&p.repo).ok()?,
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
        // The head is optional, so one that does not parse is treated as
        // absent rather than costing the row.
        head: p.head.and_then(|h| BranchName::parse_foreign(&h).ok()),
    })
}

/// Seconds since the epoch, as a snapshot carries them, back to a time.
pub fn fetched_at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}

#[derive(Serialize, Deserialize)]
struct TodoDto {
    file: String,
    items: Vec<TodoLineDto>,
}

/// A task line travels as written; the pane reads it with the domain's own
/// parser, so there is one parser and no second copy of its fields.
#[derive(Serialize, Deserialize)]
struct TodoLineDto {
    line: usize,
    raw: String,
}

pub fn encode_todo(file: &str, items: &[TodoItem]) -> Value {
    serde_json::json!({
        "file": file,
        "items": items
            .iter()
            .map(|i| serde_json::json!({"line": i.line, "raw": i.raw}))
            .collect::<Vec<_>>(),
    })
}

pub fn decode_todo(value: &Value) -> Result<TodoList, String> {
    let dto: TodoDto = serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
    Ok(TodoList {
        file: dto.file,
        items: dto
            .items
            .into_iter()
            .filter_map(|l| TodoItem::parse(l.line, &l.raw))
            .collect(),
    })
}

/// Every field optional: which ones a method needs is checked per method.
#[derive(Deserialize)]
struct EditDto {
    line: Option<usize>,
    raw: Option<String>,
    text: Option<String>,
    due: Option<String>,
    today: Option<String>,
}

pub fn encode_todo_edit(edit: &TodoEdit) -> (&'static str, Value) {
    use serde_json::{json, Map};
    fn at(at: &LineRef, params: &mut Map<String, Value>) {
        params.insert("line".into(), json!(at.line));
        params.insert("raw".into(), json!(at.raw));
    }
    let mut params = Map::new();
    let method = match edit {
        TodoEdit::Add { text, due } => {
            params.insert("text".into(), json!(text));
            if let Some(d) = due {
                params.insert("due".into(), json!(d.to_string()));
            }
            todo_method::ADD
        }
        TodoEdit::Toggle { at: line, today } => {
            at(line, &mut params);
            params.insert("today".into(), json!(today.to_string()));
            todo_method::TOGGLE
        }
        TodoEdit::Set {
            at: line,
            text,
            due,
        } => {
            at(line, &mut params);
            params.insert("text".into(), json!(text));
            if let Some(d) = due {
                params.insert("due".into(), json!(d.to_string()));
            }
            todo_method::SET
        }
        TodoEdit::Delete { at: line } => {
            at(line, &mut params);
            todo_method::DELETE
        }
        TodoEdit::Snooze { at: line, today } => {
            at(line, &mut params);
            params.insert("today".into(), json!(today.to_string()));
            todo_method::SNOOZE
        }
    };
    (method, Value::Object(params))
}

pub fn decode_todo_edit(method: &str, params: &Value) -> Result<TodoEdit, String> {
    let dto: EditDto =
        serde_json::from_value(params.clone()).map_err(|e| format!("{method}: {e}"))?;
    let missing = |field: &str| format!("{method}: missing `{field}`");
    let date = |raw: Option<&String>, field: &str| -> Result<Date, String> {
        let raw = raw.ok_or_else(|| missing(field))?;
        Date::parse_iso(raw).map_err(|e| format!("{method}: {e}"))
    };
    let optional_date = |raw: Option<&String>| -> Result<Option<Date>, String> {
        raw.map(|r| Date::parse_iso(r).map_err(|e| format!("{method}: {e}")))
            .transpose()
    };
    let at = || -> Result<LineRef, String> {
        Ok(LineRef {
            line: dto.line.ok_or_else(|| missing("line"))?,
            raw: dto.raw.clone().ok_or_else(|| missing("raw"))?,
        })
    };
    let text = || dto.text.clone().ok_or_else(|| missing("text"));
    match method {
        todo_method::ADD => Ok(TodoEdit::Add {
            text: text()?,
            due: optional_date(dto.due.as_ref())?,
        }),
        todo_method::TOGGLE => Ok(TodoEdit::Toggle {
            at: at()?,
            today: date(dto.today.as_ref(), "today")?,
        }),
        todo_method::SET => Ok(TodoEdit::Set {
            at: at()?,
            text: text()?,
            due: optional_date(dto.due.as_ref())?,
        }),
        todo_method::DELETE => Ok(TodoEdit::Delete { at: at()? }),
        todo_method::SNOOZE => Ok(TodoEdit::Snooze {
            at: at()?,
            today: date(dto.today.as_ref(), "today")?,
        }),
        other => Err(format!("the todo source has no `{other}`")),
    }
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

    fn pr_row(
        repo: &str,
        checks: &str,
        review: &str,
        mergeable: &str,
        head: Option<&str>,
    ) -> serde_json::Value {
        json!({"repo":repo,"number":1,"title":"t","url":"u","is_draft":false,"created_at":5,
            "checks":checks,"review":review,"unresolved_threads":0,"mergeable":mergeable,"head":head})
    }

    #[test]
    fn a_bad_worktree_row_is_dropped_and_the_rest_kept() {
        let value = json!({"worktrees":[
            {"repo":"omega","branch":"OMEGA-1-a","path":"/w/a"},
            {"repo":"","branch":"OMEGA-2-b","path":"/w/b"}
        ],"sessions":[],"prs":[]});
        let work = decode_work(&value).unwrap();
        assert_eq!(work.worktrees.len(), 1);
        assert_eq!(work.worktrees[0].path, PathBuf::from("/w/a"));
    }

    #[test]
    fn a_bad_session_row_is_dropped_and_the_rest_kept() {
        let value = json!({"worktrees":[],"sessions":[
            {"name":"not a session","attached":false,"path":"/w/a"},
            {"name":"zax-omega-x-00000000","attached":true,"path":"/w/b"}
        ],"prs":[]});
        let work = decode_work(&value).unwrap();
        assert_eq!(work.sessions.len(), 1);
        assert!(work.sessions[0].attached);
    }

    #[test]
    fn a_bad_pr_row_is_dropped_and_the_rest_kept() {
        let value = json!({"worktrees":[],"sessions":[],"prs":[
            pr_row("nonsense", "Passing", "Approved", "Mergeable", None),
            pr_row("acme/omega", "Passing", "Approved", "Mergeable", None)
        ]});
        let work = decode_work(&value).unwrap();
        assert_eq!(work.prs.len(), 1);
    }

    #[test]
    fn unknown_state_strings_degrade_the_field_not_the_row() {
        let value = json!({"worktrees":[],"sessions":[],"prs":[
            pr_row("acme/omega", "Flaky", "Maybe", "Soon", Some("OMEGA-1-a"))
        ]});
        let work = decode_work(&value).unwrap();
        let status = &work.prs[0].status;
        assert_eq!(status.pr.checks, CheckState::None);
        assert_eq!(status.review, ReviewState::None);
        assert_eq!(status.mergeable, Mergeable::Unknown);
    }

    #[test]
    fn a_work_index_of_the_wrong_shape_is_an_error() {
        assert!(decode_work(&json!(null)).is_err());
    }

    fn todo_date(raw: &str) -> zax_domain::Date {
        zax_domain::Date::parse_iso(raw).unwrap()
    }

    #[test]
    fn a_todo_snapshot_carries_lines_and_reads_back_as_items() {
        let items =
            zax_domain::TodoDocument::parse("# T\n- [ ] a [due:: 2026-10-10]\nnote\n").items();
        let value = encode_todo("/h/todo.md", &items);
        assert_eq!(
            value,
            serde_json::json!({"file": "/h/todo.md", "items": [{"line": 1, "raw": "- [ ] a [due:: 2026-10-10]"}]})
        );
        let list = decode_todo(&value).unwrap();
        assert_eq!(list.file, "/h/todo.md");
        assert_eq!(list.items, items);
    }

    #[test]
    fn a_line_that_is_not_a_task_is_dropped_on_decode() {
        let value = serde_json::json!({"file": "/f", "items": [{"line": 0, "raw": "note"}, {"line": 1, "raw": "- [ ] a"}]});
        assert_eq!(decode_todo(&value).unwrap().items.len(), 1);
    }

    #[test]
    fn every_todo_edit_survives_the_wire() {
        let at = zax_domain::LineRef {
            line: 3,
            raw: "- [ ] a".into(),
        };
        let today = todo_date("2026-10-07");
        let edits = [
            zax_domain::TodoEdit::Add {
                text: "a".into(),
                due: Some(todo_date("2026-10-12")),
            },
            zax_domain::TodoEdit::Add {
                text: "a".into(),
                due: None,
            },
            zax_domain::TodoEdit::Toggle {
                at: at.clone(),
                today,
            },
            zax_domain::TodoEdit::Set {
                at: at.clone(),
                text: "b".into(),
                due: None,
            },
            zax_domain::TodoEdit::Set {
                at: at.clone(),
                text: "b".into(),
                due: Some(today),
            },
            zax_domain::TodoEdit::Delete { at: at.clone() },
            zax_domain::TodoEdit::Snooze { at, today },
        ];
        for edit in edits {
            let (method, params) = encode_todo_edit(&edit);
            assert_eq!(
                decode_todo_edit(method, &params).unwrap(),
                edit,
                "{method} {params}"
            );
        }
    }

    #[test]
    fn todo_edits_have_the_documented_shapes() {
        let (method, params) = encode_todo_edit(&zax_domain::TodoEdit::Toggle {
            at: zax_domain::LineRef {
                line: 2,
                raw: "- [ ] a".into(),
            },
            today: todo_date("2026-10-07"),
        });
        assert_eq!(method, "todo/toggle");
        assert_eq!(
            params,
            serde_json::json!({"line": 2, "raw": "- [ ] a", "today": "2026-10-07"})
        );
        let (method, params) = encode_todo_edit(&zax_domain::TodoEdit::Add {
            text: "a".into(),
            due: None,
        });
        assert_eq!(method, "todo/add");
        assert_eq!(params, serde_json::json!({"text": "a"}));
    }

    #[test]
    fn a_malformed_todo_edit_is_refused_with_the_reason() {
        let cases = [
            ("todo/nope", serde_json::json!({})),
            ("todo/toggle", serde_json::json!({"line": 1, "raw": "x"})),
            (
                "todo/toggle",
                serde_json::json!({"line": 1, "raw": "x", "today": "tuesday"}),
            ),
            ("todo/add", serde_json::json!({"due": "2026-10-07"})),
            ("todo/delete", serde_json::json!({"raw": "x"})),
        ];
        for (method, params) in cases {
            assert!(
                decode_todo_edit(method, &params).is_err(),
                "{method} {params}"
            );
        }
    }
}
