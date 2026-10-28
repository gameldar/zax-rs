//! Leader-key bindings for zax panes.
//!
//! Bindings live in a trie per pane context. A partial sequence resolves to
//! the keys that could come next, which is what the which-key popup shows, and
//! a sequence can never be both a command and a prefix. Everything here is
//! pure: the terminal layer turns real key events into [`KeyChord`]s.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;

/// A key, without modifiers. Shift is carried by the character itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    /// A printable character. Space is `Char(' ')`.
    Char(char),
    /// The return key.
    Enter,
    /// The escape key.
    Esc,
    /// Cursor up.
    Up,
    /// Cursor down.
    Down,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
    /// The tab key.
    Tab,
    /// The backspace key.
    Backspace,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Home.
    Home,
    /// End.
    End,
}

/// One key press with its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyChord {
    /// The key pressed.
    pub code: Key,
    /// Whether control was held.
    pub ctrl: bool,
    /// Whether alt was held.
    pub alt: bool,
}

impl KeyChord {
    /// A chord with no modifiers.
    #[must_use]
    pub const fn plain(code: Key) -> Self {
        Self {
            code,
            ctrl: false,
            alt: false,
        }
    }

    /// A plain character chord.
    #[must_use]
    pub const fn char(c: char) -> Self {
        Self::plain(Key::Char(c))
    }

    /// The leader key, space.
    #[must_use]
    pub const fn leader() -> Self {
        Self::char(' ')
    }
}

/// Why a key spelling or sequence could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyParseError {
    /// The sequence held no keys.
    #[error("a key sequence needs at least one key")]
    Empty,
    /// A key spelling was not recognised.
    #[error("`{0}` is not a key; use SPC, RET, TAB, DEL, <Esc>, <Up>, <Down>, <Left>, <Right>, <PageUp>, <PageDown>, <Home>, <End>, C-x, M-x or a single character")]
    Unknown(String),
}

impl FromStr for KeyChord {
    type Err = KeyParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let unknown = || KeyParseError::Unknown(text.to_owned());
        let (mut ctrl, mut alt) = (false, false);
        let mut rest = text;
        loop {
            if let Some(after) = rest.strip_prefix("C-").filter(|s| !s.is_empty()) {
                ctrl = true;
                rest = after;
            } else if let Some(after) = rest.strip_prefix("M-").filter(|s| !s.is_empty()) {
                alt = true;
                rest = after;
            } else {
                break;
            }
        }
        let code = match rest {
            "SPC" => Key::Char(' '),
            "RET" => Key::Enter,
            "TAB" => Key::Tab,
            "DEL" => Key::Backspace,
            "<Esc>" => Key::Esc,
            "<Up>" => Key::Up,
            "<Down>" => Key::Down,
            "<Left>" => Key::Left,
            "<Right>" => Key::Right,
            "<PageUp>" => Key::PageUp,
            "<PageDown>" => Key::PageDown,
            "<Home>" => Key::Home,
            "<End>" => Key::End,
            _ => {
                let mut chars = rest.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) if !c.is_whitespace() => Key::Char(c),
                    _ => return Err(unknown()),
                }
            }
        };
        Ok(Self { code, ctrl, alt })
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("C-")?;
        }
        if self.alt {
            f.write_str("M-")?;
        }
        match self.code {
            Key::Char(' ') => f.write_str("SPC"),
            Key::Char(c) => write!(f, "{c}"),
            Key::Enter => f.write_str("RET"),
            Key::Tab => f.write_str("TAB"),
            Key::Backspace => f.write_str("DEL"),
            Key::Esc => f.write_str("<Esc>"),
            Key::Up => f.write_str("<Up>"),
            Key::Down => f.write_str("<Down>"),
            Key::Left => f.write_str("<Left>"),
            Key::Right => f.write_str("<Right>"),
            Key::PageUp => f.write_str("<PageUp>"),
            Key::PageDown => f.write_str("<PageDown>"),
            Key::Home => f.write_str("<Home>"),
            Key::End => f.write_str("<End>"),
        }
    }
}

/// A non-empty run of chords, such as `SPC w o`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeySequence(Vec<KeyChord>);

impl KeySequence {
    /// Build a sequence; an empty run is rejected.
    ///
    /// # Errors
    /// [`KeyParseError::Empty`] when `chords` is empty.
    pub fn new(chords: Vec<KeyChord>) -> Result<Self, KeyParseError> {
        if chords.is_empty() {
            Err(KeyParseError::Empty)
        } else {
            Ok(Self(chords))
        }
    }

    /// Parse whitespace-separated spellings such as `SPC w o`.
    ///
    /// # Errors
    /// [`KeyParseError`] for an empty sequence or an unknown spelling.
    pub fn parse(text: &str) -> Result<Self, KeyParseError> {
        let chords = text
            .split_whitespace()
            .map(str::parse)
            .collect::<Result<Vec<KeyChord>, _>>()?;
        Self::new(chords)
    }

    /// The chords, in press order.
    #[must_use]
    pub fn chords(&self) -> &[KeyChord] {
        &self.0
    }
}

impl FromStr for KeySequence {
    type Err = KeyParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::parse(text)
    }
}

impl fmt::Display for KeySequence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, chord) in self.0.iter().enumerate() {
            if index > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{chord}")?;
        }
        Ok(())
    }
}

/// Something a pane can be asked to do from the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Command {
    /// Move the selection down (scroll, in an exec pane).
    MoveDown,
    /// Move the selection up (scroll, in an exec pane).
    MoveUp,
    /// Move the selection down a pane's height (scroll, in an exec pane).
    PageDown,
    /// Move the selection up a pane's height (scroll, in an exec pane).
    PageUp,
    /// Move the selection to the first row (scroll, in an exec pane).
    Top,
    /// Move the selection to the last row (scroll, in an exec pane).
    Bottom,
    /// Open the selection in the browser.
    Open,
    /// Copy the selection's url.
    CopyUrl,
    /// Refresh the pane.
    Refresh,
    /// Close the pane.
    Quit,
    /// Open the selection's worktree.
    WorktreeOpen,
    /// Open the selection's worktree and start a review.
    WorktreeReview,
    /// List the selection's worktrees.
    WorktreeList,
    /// Open the selection's pull request.
    OpenPr,
    /// Transition the selected ticket.
    Transition,
    /// Add a todo.
    TodoAdd,
    /// Edit the selected todo.
    TodoEdit,
    /// Check or uncheck the selected todo.
    TodoToggle,
    /// Snooze the selected todo.
    TodoSnooze,
    /// Delete the selected todo.
    TodoDelete,
    /// Open the todo file in an editor.
    TodoOpenFile,
    /// Show or hide completed todos.
    TodoShowDone,
}

impl Command {
    const ALL: [Self; 22] = [
        Self::MoveDown,
        Self::MoveUp,
        Self::PageDown,
        Self::PageUp,
        Self::Top,
        Self::Bottom,
        Self::Open,
        Self::CopyUrl,
        Self::Refresh,
        Self::Quit,
        Self::WorktreeOpen,
        Self::WorktreeReview,
        Self::WorktreeList,
        Self::OpenPr,
        Self::Transition,
        Self::TodoAdd,
        Self::TodoEdit,
        Self::TodoToggle,
        Self::TodoSnooze,
        Self::TodoDelete,
        Self::TodoOpenFile,
        Self::TodoShowDone,
    ];

    /// The kebab-case name used in configuration files.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MoveDown => "move-down",
            Self::MoveUp => "move-up",
            Self::PageDown => "page-down",
            Self::PageUp => "page-up",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Open => "open",
            Self::CopyUrl => "copy-url",
            Self::Refresh => "refresh",
            Self::Quit => "quit",
            Self::WorktreeOpen => "worktree-open",
            Self::WorktreeReview => "worktree-review",
            Self::WorktreeList => "worktree-list",
            Self::OpenPr => "open-pr",
            Self::Transition => "transition",
            Self::TodoAdd => "todo-add",
            Self::TodoEdit => "todo-edit",
            Self::TodoToggle => "todo-toggle",
            Self::TodoSnooze => "todo-snooze",
            Self::TodoDelete => "todo-delete",
            Self::TodoOpenFile => "todo-open-file",
            Self::TodoShowDone => "todo-show-done",
        }
    }

    /// The which-key label for this command.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::MoveDown => "down",
            Self::MoveUp => "up",
            Self::PageDown => "page down",
            Self::PageUp => "page up",
            Self::Top => "top",
            Self::Bottom => "bottom",
            Self::Open => "open in browser",
            Self::CopyUrl => "copy url",
            Self::Refresh => "refresh",
            Self::Quit => "close pane",
            Self::WorktreeOpen => "open worktree",
            Self::WorktreeReview => "open worktree and review",
            Self::WorktreeList => "list worktrees",
            Self::OpenPr => "open pull request",
            Self::Transition => "transition",
            Self::TodoAdd => "add",
            Self::TodoEdit => "edit",
            Self::TodoToggle => "check / uncheck",
            Self::TodoSnooze => "snooze",
            Self::TodoDelete => "delete",
            Self::TodoOpenFile => "open todo file",
            Self::TodoShowDone => "show / hide done",
        }
    }
}

/// A command name that is not one of the known commands.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{0}` is not a command")]
pub struct UnknownCommand(pub String);

impl FromStr for Command {
    type Err = UnknownCommand;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|command| command.as_str() == text)
            .ok_or_else(|| UnknownCommand(text.to_owned()))
    }
}

impl fmt::Display for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The pane a keymap belongs to. Names match the built-in pane names, plus
/// `exec` for command panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PaneContext {
    /// The review queue.
    PrsForReview,
    /// The user's own pull requests.
    MyPrs,
    /// The current branch's status.
    BranchStatus,
    /// Tickets.
    Tickets,
    /// The todo list.
    Todo,
    /// A pane running a command.
    Exec,
}

const PR_LIST: &[Command] = &[
    Command::MoveDown,
    Command::MoveUp,
    Command::PageDown,
    Command::PageUp,
    Command::Top,
    Command::Bottom,
    Command::Open,
    Command::CopyUrl,
    Command::Refresh,
    Command::Quit,
    Command::WorktreeOpen,
    Command::WorktreeReview,
];
const BRANCH: &[Command] = &[
    Command::Open,
    Command::CopyUrl,
    Command::Refresh,
    Command::Quit,
];
const TICKETS: &[Command] = &[
    Command::MoveDown,
    Command::MoveUp,
    Command::PageDown,
    Command::PageUp,
    Command::Top,
    Command::Bottom,
    Command::Open,
    Command::CopyUrl,
    Command::Refresh,
    Command::Quit,
    Command::WorktreeOpen,
    Command::WorktreeList,
    Command::OpenPr,
    Command::Transition,
];
const TODO: &[Command] = &[
    Command::MoveDown,
    Command::MoveUp,
    Command::PageDown,
    Command::PageUp,
    Command::Top,
    Command::Bottom,
    Command::Refresh,
    Command::Quit,
    Command::TodoAdd,
    Command::TodoEdit,
    Command::TodoToggle,
    Command::TodoSnooze,
    Command::TodoDelete,
    Command::TodoOpenFile,
    Command::TodoShowDone,
];
const EXEC: &[Command] = &[
    Command::MoveDown,
    Command::MoveUp,
    Command::PageDown,
    Command::PageUp,
    Command::Top,
    Command::Bottom,
    Command::Refresh,
    Command::Quit,
];

impl PaneContext {
    /// Every context.
    pub const ALL: [Self; 6] = [
        Self::PrsForReview,
        Self::MyPrs,
        Self::BranchStatus,
        Self::Tickets,
        Self::Todo,
        Self::Exec,
    ];

    /// The name used in configuration files.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PrsForReview => "prs-for-review",
            Self::MyPrs => "my-prs",
            Self::BranchStatus => "branch-status",
            Self::Tickets => "tickets",
            Self::Todo => "todo",
            Self::Exec => "exec",
        }
    }

    /// The commands a pane in this context accepts.
    #[must_use]
    pub const fn commands(self) -> &'static [Command] {
        match self {
            Self::PrsForReview | Self::MyPrs => PR_LIST,
            Self::BranchStatus => BRANCH,
            Self::Tickets => TICKETS,
            Self::Todo => TODO,
            Self::Exec => EXEC,
        }
    }

    /// Whether this context accepts the command.
    #[must_use]
    pub fn supports(self, command: Command) -> bool {
        self.commands().contains(&command)
    }
}

/// A table name that is not a pane context.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "`{0}` is not a pane context; use prs-for-review, my-prs, branch-status, tickets, todo or exec"
)]
pub struct UnknownContext(pub String);

impl FromStr for PaneContext {
    type Err = UnknownContext;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|context| context.as_str() == text)
            .ok_or_else(|| UnknownContext(text.to_owned()))
    }
}

impl fmt::Display for PaneContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a binding could not be added.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BindError {
    /// The new sequence is a strict prefix of an existing binding, so the
    /// existing one would become unreachable.
    #[error("{sequence} is a prefix of an existing binding")]
    PrefixOfExisting {
        /// The sequence that was rejected.
        sequence: KeySequence,
    },
    /// A shorter binding already fires before this sequence completes.
    #[error("shadowed by the existing binding {existing}")]
    ShadowedBy {
        /// The binding that fires first.
        existing: KeySequence,
    },
    /// `<Esc>` follows another key, but Esc always cancels a pending prefix,
    /// so the binding could never fire.
    #[error("<Esc> cannot follow another key in {sequence}; Esc cancels a pending sequence")]
    EscapeAfterPrefix {
        /// The sequence that was rejected.
        sequence: KeySequence,
    },
    /// The context does not accept the command.
    #[error("{command} is not available in {context}")]
    Unsupported {
        /// The context bound in.
        context: PaneContext,
        /// The command that is not accepted there.
        command: Command,
    },
}

/// One continuation offered by a prefix, as shown in the which-key popup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The key that would be pressed next.
    pub chord: KeyChord,
    /// The command it runs, if this key completes a binding.
    pub command: Option<Command>,
    /// The label to show: a command description, or a group name.
    pub label: String,
}

/// What a run of chords resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Lookup {
    /// The sequence is complete and runs this command.
    Command(Command),
    /// The sequence is a prefix; these keys may follow.
    Prefix(Vec<Candidate>),
    /// Nothing is bound on this path.
    Unbound,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Node {
    command: Option<Command>,
    group_label: Option<String>,
    children: BTreeMap<KeyChord, Node>,
}

impl Node {
    fn label(&self) -> String {
        if let Some(command) = self.command {
            return command.description().to_owned();
        }
        self.group_label.clone().unwrap_or_else(|| "+prefix".into())
    }

    fn candidates(&self) -> Vec<Candidate> {
        self.children
            .iter()
            .map(|(chord, child)| Candidate {
                chord: *chord,
                command: child.command,
                label: child.label(),
            })
            .collect()
    }

    fn find(&self, command: Command, path: &mut Vec<KeyChord>) -> bool {
        if self.command == Some(command) {
            return true;
        }
        for (chord, child) in &self.children {
            path.push(*chord);
            if child.find(command, path) {
                return true;
            }
            path.pop();
        }
        false
    }
}

/// Key sequences mapped to commands for one pane context.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Keymap {
    root: Node,
}

impl Keymap {
    /// A keymap with nothing bound.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Bind a sequence to a command.
    ///
    /// # Errors
    /// [`BindError::ShadowedBy`] when a shorter binding already fires first,
    /// [`BindError::PrefixOfExisting`] when the sequence is a prefix of a
    /// binding, [`BindError::EscapeAfterPrefix`] when `<Esc>` is not the first
    /// key. Rebinding exactly the same sequence replaces its command.
    pub fn bind(&mut self, sequence: &KeySequence, command: Command) -> Result<(), BindError> {
        let chords = sequence.chords();
        if chords.iter().skip(1).any(|chord| chord.code == Key::Esc) {
            return Err(BindError::EscapeAfterPrefix {
                sequence: sequence.clone(),
            });
        }
        let mut node = &mut self.root;
        for (index, chord) in chords.iter().enumerate() {
            if node.command.is_some() {
                let consumed = chords.get(..index).unwrap_or_default().to_vec();
                return Err(BindError::ShadowedBy {
                    existing: KeySequence(consumed),
                });
            }
            node = node.children.entry(*chord).or_default();
        }
        if !node.children.is_empty() {
            return Err(BindError::PrefixOfExisting {
                sequence: sequence.clone(),
            });
        }
        node.command = Some(command);
        Ok(())
    }

    /// Give a prefix a group label, in the `+open` style.
    pub fn label_prefix(&mut self, sequence: &KeySequence, label: impl Into<String>) {
        let mut node = &mut self.root;
        for chord in sequence.chords() {
            node = node.children.entry(*chord).or_default();
        }
        node.group_label = Some(label.into());
    }

    /// Remove a binding. Returns whether anything was removed. Prefixes left
    /// with nothing under them are pruned, labels included.
    pub fn unbind(&mut self, sequence: &KeySequence) -> bool {
        fn remove(node: &mut Node, chords: &[KeyChord]) -> bool {
            match chords.split_first() {
                None => node.command.take().is_some(),
                Some((head, tail)) => {
                    let Some(child) = node.children.get_mut(head) else {
                        return false;
                    };
                    let removed = remove(child, tail);
                    if child.command.is_none() && child.children.is_empty() {
                        node.children.remove(head);
                    }
                    removed
                }
            }
        }
        remove(&mut self.root, sequence.chords())
    }

    /// Resolve a run of chords.
    #[must_use]
    pub fn lookup(&self, chords: &[KeyChord]) -> Lookup {
        let mut node = &self.root;
        for chord in chords {
            match node.children.get(chord) {
                Some(child) => node = child,
                None => return Lookup::Unbound,
            }
        }
        if let Some(command) = node.command {
            Lookup::Command(command)
        } else if node.children.is_empty() {
            Lookup::Unbound
        } else {
            Lookup::Prefix(node.candidates())
        }
    }

    /// The keys offered at the top of the trie, for the status bar.
    #[must_use]
    pub fn top_level(&self) -> Vec<Candidate> {
        self.root.candidates()
    }

    /// The first sequence bound to a command, in key order.
    #[must_use]
    pub fn sequence_for(&self, command: Command) -> Option<KeySequence> {
        let mut path = Vec::new();
        if self.root.find(command, &mut path) {
            KeySequence::new(path).ok()
        } else {
            None
        }
    }
}

/// One [`Keymap`] per pane context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymaps {
    maps: BTreeMap<PaneContext, Keymap>,
}

impl Keymaps {
    /// Every context with nothing bound.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            maps: PaneContext::ALL
                .into_iter()
                .map(|context| (context, Keymap::empty()))
                .collect(),
        }
    }

    /// The built-in bindings: plain keys for movement and the primary action,
    /// everything else under the `SPC` leader.
    #[must_use]
    pub fn defaults() -> Self {
        let mut keymaps = Self::empty();
        for context in PaneContext::ALL {
            let table = default_table(context);
            for (sequence, command) in table {
                if context.supports(command) {
                    keymaps.bind_default(context, sequence, command);
                }
            }
            for (sequence, label) in GROUPS {
                if keymaps.group_has_bindings(context, sequence) {
                    keymaps
                        .keymap_mut(context)
                        .label_prefix(&seq(sequence), label);
                }
            }
        }
        keymaps
    }

    fn bind_default(&mut self, context: PaneContext, sequence: &str, command: Command) {
        // The default table is fixed and conflict-free; a failure here is a
        // bug caught by the defaults tests, so it is not surfaced at runtime.
        let _ = self.keymap_mut(context).bind(&seq(sequence), command);
    }

    fn group_has_bindings(&self, context: PaneContext, sequence: &str) -> bool {
        matches!(
            self.for_context(context).lookup(seq(sequence).chords()),
            Lookup::Prefix(_)
        )
    }

    fn keymap_mut(&mut self, context: PaneContext) -> &mut Keymap {
        self.maps.entry(context).or_default()
    }

    /// The keymap for a pane context.
    #[must_use]
    pub fn for_context(&self, context: PaneContext) -> &Keymap {
        static EMPTY: Keymap = Keymap {
            root: Node {
                command: None,
                group_label: None,
                children: BTreeMap::new(),
            },
        };
        self.maps.get(&context).unwrap_or(&EMPTY)
    }

    /// Bind a sequence in a context.
    ///
    /// # Errors
    /// [`BindError::Unsupported`] when the context does not accept the
    /// command, otherwise whatever [`Keymap::bind`] reports.
    pub fn bind(
        &mut self,
        context: PaneContext,
        sequence: &KeySequence,
        command: Command,
    ) -> Result<(), BindError> {
        if !context.supports(command) {
            return Err(BindError::Unsupported { context, command });
        }
        self.keymap_mut(context).bind(sequence, command)
    }

    /// Remove a binding in a context. Returns whether anything was removed.
    pub fn unbind(&mut self, context: PaneContext, sequence: &KeySequence) -> bool {
        self.keymap_mut(context).unbind(sequence)
    }
}

/// Parse a spelling from the fixed default table.
fn seq(text: &str) -> KeySequence {
    // Only ever called with literals from this module; the defaults tests
    // parse all of them.
    KeySequence::parse(text).unwrap_or_else(|_| KeySequence(vec![KeyChord::leader()]))
}

const GROUPS: [(&str, &str); 4] = [
    ("SPC o", "+open"),
    ("SPC w", "+worktree"),
    ("SPC d", "+delete"),
    ("SPC t", "+toggle"),
];

fn default_table(context: PaneContext) -> Vec<(&'static str, Command)> {
    let mut table = vec![
        ("j", Command::MoveDown),
        ("<Down>", Command::MoveDown),
        ("k", Command::MoveUp),
        ("<Up>", Command::MoveUp),
        ("<PageDown>", Command::PageDown),
        ("C-d", Command::PageDown),
        ("<PageUp>", Command::PageUp),
        ("C-u", Command::PageUp),
        ("<Home>", Command::Top),
        ("<End>", Command::Bottom),
        ("SPC r", Command::Refresh),
        ("SPC q", Command::Quit),
    ];
    let specific: &[(&str, Command)] = match context {
        PaneContext::PrsForReview | PaneContext::MyPrs => &[
            ("RET", Command::Open),
            ("SPC o b", Command::Open),
            ("SPC y", Command::CopyUrl),
            ("SPC w o", Command::WorktreeOpen),
            ("SPC w r", Command::WorktreeReview),
        ],
        PaneContext::BranchStatus => &[
            ("RET", Command::Open),
            ("SPC o b", Command::Open),
            ("SPC y", Command::CopyUrl),
        ],
        PaneContext::Tickets => &[
            ("RET", Command::Open),
            ("SPC o b", Command::Open),
            ("SPC o p", Command::OpenPr),
            ("SPC y", Command::CopyUrl),
            ("SPC w o", Command::WorktreeOpen),
            ("SPC w l", Command::WorktreeList),
            ("SPC s", Command::Transition),
        ],
        PaneContext::Todo => &[
            ("x", Command::TodoToggle),
            ("RET", Command::TodoEdit),
            ("SPC a", Command::TodoAdd),
            ("SPC e", Command::TodoEdit),
            ("SPC z", Command::TodoSnooze),
            ("SPC d d", Command::TodoDelete),
            ("SPC o f", Command::TodoOpenFile),
            ("SPC t d", Command::TodoShowDone),
        ],
        PaneContext::Exec => &[],
    };
    table.extend_from_slice(specific);
    table
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    fn s(text: &str) -> KeySequence {
        KeySequence::parse(text).unwrap()
    }

    fn resolve(keymaps: &Keymaps, context: PaneContext, text: &str) -> Lookup {
        keymaps.for_context(context).lookup(s(text).chords())
    }

    #[test]
    fn spellings_round_trip() {
        for text in [
            "SPC w o",
            "RET",
            "<Down>",
            "C-d",
            "W",
            "M-x",
            "DEL",
            "TAB",
            "<Esc>",
            "C-SPC",
            "C-M-x",
            "C-RET",
            "M-<Up>",
            "C--",
            "<PageUp>",
            "C-<PageDown>",
            "<Home>",
            "M-<End>",
        ] {
            assert_eq!(s(text).to_string(), text);
        }
    }

    #[test]
    fn every_key_and_modifier_combination_round_trips() {
        let keys = [
            Key::Char('a'),
            Key::Char('Z'),
            Key::Char('-'),
            Key::Char('<'),
            Key::Char(' '),
            Key::Enter,
            Key::Esc,
            Key::Up,
            Key::Down,
            Key::Left,
            Key::Right,
            Key::Tab,
            Key::Backspace,
            Key::PageUp,
            Key::PageDown,
            Key::Home,
            Key::End,
        ];
        for code in keys {
            for (ctrl, alt) in [(false, false), (true, false), (false, true), (true, true)] {
                let chord = KeyChord { code, ctrl, alt };
                let text = chord.to_string();
                assert_eq!(text.parse::<KeyChord>(), Ok(chord), "spelling {text}");
            }
        }
    }

    #[test]
    fn esc_after_the_first_key_cannot_be_bound() {
        let mut keymap = Keymap::empty();
        assert!(matches!(
            keymap.bind(&s("SPC <Esc>"), Command::Refresh),
            Err(BindError::EscapeAfterPrefix { .. })
        ));
        assert_eq!(keymap.bind(&s("<Esc>"), Command::Refresh), Ok(()));
    }

    #[test]
    fn a_bad_spelling_or_empty_sequence_is_rejected() {
        assert_eq!(KeySequence::parse("  "), Err(KeyParseError::Empty));
        assert!(matches!(
            KeySequence::parse("SPC foo"),
            Err(KeyParseError::Unknown(text)) if text == "foo"
        ));
    }

    #[test]
    fn shift_is_carried_by_the_character() {
        assert_eq!(s("W").chords(), &[KeyChord::char('W')]);
    }

    #[test]
    fn command_names_round_trip() {
        for command in Command::ALL {
            assert_eq!(command.as_str().parse::<Command>(), Ok(command));
        }
        assert!("nope".parse::<Command>().is_err());
    }

    #[test]
    fn context_names_match_the_builtin_panes() {
        use crate::layout::BuiltinPane;
        for pane in [
            BuiltinPane::PrsForReview,
            BuiltinPane::MyPrs,
            BuiltinPane::BranchStatus,
            BuiltinPane::Tickets,
            BuiltinPane::Todo,
        ] {
            assert!(pane.as_str().parse::<PaneContext>().is_ok(), "{pane:?}");
        }
        assert_eq!("exec".parse::<PaneContext>(), Ok(PaneContext::Exec));
        assert!("nope".parse::<PaneContext>().is_err());
    }

    #[test]
    fn a_prefix_lists_its_candidates_with_labels() {
        let keymaps = Keymaps::defaults();
        let Lookup::Prefix(candidates) = resolve(&keymaps, PaneContext::Tickets, "SPC w") else {
            panic!("SPC w should be a prefix");
        };
        let shown: Vec<(String, &str)> = candidates
            .iter()
            .map(|c| (c.chord.to_string(), c.label.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("l".to_owned(), "list worktrees"),
                ("o".to_owned(), "open worktree")
            ]
        );
    }

    #[test]
    fn a_group_label_shows_on_its_prefix() {
        let keymaps = Keymaps::defaults();
        let top = keymaps.for_context(PaneContext::Tickets).top_level();
        let leader = top
            .iter()
            .find(|c| c.chord == KeyChord::leader())
            .expect("leader is bound");
        assert_eq!(leader.label, "+prefix");
        let Lookup::Prefix(children) = resolve(&keymaps, PaneContext::Tickets, "SPC") else {
            panic!("SPC should be a prefix");
        };
        let labels: Vec<&str> = children.iter().map(|c| c.label.as_str()).collect();
        assert!(labels.contains(&"+open") && labels.contains(&"+worktree"));
    }

    #[test]
    fn binding_a_prefix_of_an_existing_binding_fails() {
        let mut keymap = Keymap::empty();
        keymap.bind(&s("SPC w o"), Command::WorktreeOpen).unwrap();
        assert_eq!(
            keymap.bind(&s("SPC w"), Command::Refresh),
            Err(BindError::PrefixOfExisting {
                sequence: s("SPC w")
            })
        );
    }

    #[test]
    fn shadowing_an_existing_binding_fails() {
        let mut keymap = Keymap::empty();
        keymap.bind(&s("SPC w"), Command::Refresh).unwrap();
        assert_eq!(
            keymap.bind(&s("SPC w o"), Command::WorktreeOpen),
            Err(BindError::ShadowedBy {
                existing: s("SPC w")
            })
        );
    }

    #[test]
    fn binding_a_command_the_context_does_not_accept_fails() {
        let mut keymaps = Keymaps::empty();
        assert_eq!(
            keymaps.bind(PaneContext::Exec, &s("SPC w o"), Command::WorktreeOpen),
            Err(BindError::Unsupported {
                context: PaneContext::Exec,
                command: Command::WorktreeOpen
            })
        );
        assert_eq!(
            resolve(&keymaps, PaneContext::Exec, "SPC w o"),
            Lookup::Unbound
        );
    }

    #[test]
    fn the_defaults_resolve_as_designed() {
        let keymaps = Keymaps::defaults();
        let cmd = |context, text, command| {
            assert_eq!(
                resolve(&keymaps, context, text),
                Lookup::Command(command),
                "{context} {text}"
            );
        };
        for context in PaneContext::ALL {
            cmd(context, "SPC q", Command::Quit);
            cmd(context, "SPC r", Command::Refresh);
        }
        for context in [PaneContext::PrsForReview, PaneContext::MyPrs] {
            cmd(context, "SPC w r", Command::WorktreeReview);
            cmd(context, "SPC w o", Command::WorktreeOpen);
            cmd(context, "RET", Command::Open);
            cmd(context, "j", Command::MoveDown);
            cmd(context, "<Up>", Command::MoveUp);
            cmd(context, "<PageDown>", Command::PageDown);
            cmd(context, "C-u", Command::PageUp);
            cmd(context, "<Home>", Command::Top);
            cmd(context, "<End>", Command::Bottom);
        }
        cmd(PaneContext::BranchStatus, "SPC o b", Command::Open);
        cmd(PaneContext::Tickets, "SPC o p", Command::OpenPr);
        cmd(PaneContext::Tickets, "SPC w l", Command::WorktreeList);
        cmd(PaneContext::Tickets, "SPC s", Command::Transition);
        cmd(PaneContext::Todo, "SPC d d", Command::TodoDelete);
        cmd(PaneContext::Todo, "SPC t d", Command::TodoShowDone);
        cmd(PaneContext::Todo, "SPC o f", Command::TodoOpenFile);
        cmd(PaneContext::Todo, "x", Command::TodoToggle);
        cmd(PaneContext::Todo, "RET", Command::TodoEdit);
        assert_eq!(
            resolve(&keymaps, PaneContext::Exec, "SPC w r"),
            Lookup::Unbound
        );
        assert_eq!(resolve(&keymaps, PaneContext::Exec, "RET"), Lookup::Unbound);
    }

    #[test]
    fn every_default_binding_is_bound_where_supported_and_skipped_elsewhere() {
        let keymaps = Keymaps::defaults();
        for context in PaneContext::ALL {
            for (text, command) in default_table(context) {
                if !context.supports(command) {
                    // The common movement keys are skipped where unsupported.
                    assert_eq!(
                        resolve(&keymaps, context, text),
                        Lookup::Unbound,
                        "{context} {text}"
                    );
                    continue;
                }
                assert_eq!(
                    resolve(&keymaps, context, text),
                    Lookup::Command(command),
                    "{context} {text}"
                );
            }
        }
    }

    #[test]
    fn groups_a_context_never_uses_carry_no_label() {
        let keymaps = Keymaps::defaults();
        assert_eq!(
            resolve(&keymaps, PaneContext::Exec, "SPC o"),
            Lookup::Unbound
        );
        let top = keymaps.for_context(PaneContext::Exec).top_level();
        let leader = top.iter().find(|c| c.chord == KeyChord::leader()).unwrap();
        assert_eq!(leader.label, "+prefix");
    }

    #[test]
    fn sequence_for_finds_a_command() {
        let keymaps = Keymaps::defaults();
        let tickets = keymaps.for_context(PaneContext::Tickets);
        assert_eq!(
            tickets.sequence_for(Command::WorktreeList),
            Some(s("SPC w l"))
        );
        assert_eq!(tickets.sequence_for(Command::TodoAdd), None);
    }

    #[test]
    fn top_level_lists_the_leader_children_for_the_status_bar() {
        let keymaps = Keymaps::defaults();
        let Lookup::Prefix(children) = resolve(&keymaps, PaneContext::Tickets, "SPC") else {
            panic!("SPC should be a prefix");
        };
        let keys: Vec<String> = children.iter().map(|c| c.chord.to_string()).collect();
        assert_eq!(keys, ["o", "q", "r", "s", "w", "y"]);
        let top: Vec<String> = keymaps
            .for_context(PaneContext::Tickets)
            .top_level()
            .iter()
            .map(|c| c.chord.to_string())
            .collect();
        assert!(top.contains(&"SPC".to_owned()) && top.contains(&"RET".to_owned()));
    }

    #[test]
    fn unbinding_prunes_empty_prefixes_and_allows_a_rebind() {
        let mut keymaps = Keymaps::defaults();
        assert!(keymaps.unbind(PaneContext::Todo, &s("SPC d d")));
        assert_eq!(
            resolve(&keymaps, PaneContext::Todo, "SPC d"),
            Lookup::Unbound
        );
        keymaps
            .bind(PaneContext::Todo, &s("SPC d"), Command::TodoDelete)
            .unwrap();
        assert!(!keymaps.unbind(PaneContext::Todo, &s("SPC x x")));
    }
}
