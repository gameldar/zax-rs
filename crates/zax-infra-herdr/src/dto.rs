//! The parts of herdr's JSON replies zax reads. Unknown fields are ignored:
//! herdr adds to its replies, and a new field must not break an old zax.

use serde::Deserialize;

/// Every reply is `{"id": .., "result": ..}` on success.
#[derive(Debug, Deserialize)]
pub struct Reply<T> {
    pub result: T,
}

/// A failure is `{"id": .., "error": {"code", "message"}}` on stderr.
#[derive(Debug, Deserialize)]
pub struct Failure {
    pub error: FailureDetail,
}

#[derive(Debug, Deserialize)]
pub struct FailureDetail {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Deserialize)]
pub struct WorktreeList {
    pub source: WorktreeSource,
    pub worktrees: Vec<WorktreeEntry>,
}

/// The repository's own checkout, which herdr opens new worktrees from.
#[derive(Debug, Deserialize)]
pub struct WorktreeSource {
    pub source_checkout_path: String,
}

#[derive(Debug, Deserialize)]
pub struct WorktreeEntry {
    pub path: String,
    pub open_workspace_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceList {
    pub workspaces: Vec<Workspace>,
}

#[derive(Debug, Deserialize)]
pub struct Workspace {
    pub workspace_id: String,
    pub label: String,
}

/// `already_open` is not read: `worktree list` has already been asked, and
/// only a worktree no space had open is opened.
#[derive(Debug, Deserialize)]
pub struct WorktreeOpened {
    pub root_pane: Pane,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceCreated {
    pub root_pane: Pane,
}

#[derive(Debug, Deserialize)]
pub struct Pane {
    pub pane_id: String,
}
