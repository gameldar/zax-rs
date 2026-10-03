//! The three layers as editable documents.
//!
//! A `Draft` is what the config modal edits: the same three files `load` reads,
//! held open as `toml_edit` documents so a change rewrites one value and leaves
//! the comments and the key order of a file a person wrote exactly as they were.
//!
//! Nothing here validates anything itself. `validate` serialises the documents
//! and runs the text back through the `RawConfig` -> `merge` -> `from_raw` chain
//! a session start uses, so `deny_unknown_fields`, the override-name rule, the
//! prefix check, the copy-path check and the per-kind pane rules all apply to
//! the modal for free and cannot drift from what a session enforces.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use toml_edit::{Array, ArrayOfTables, DocumentMut, Item, Table, Value};
use zax_domain::settings::{
    ConfigView, LayerInfo, LayerSlot, Origin, PaneEdit, PaneField, PaneKindTag, PanesView, Pending,
    Saved, SettingKey, SettingRow, SettingShape, SettingValue,
};
use zax_domain::{PaneKind, PaneSpec, RootRepoName};

use crate::error::ConfigError;
use crate::{merge, parse, parse_layer_text};

/// One layer's file, as the editor holds it.
struct LayerDoc {
    slot: LayerSlot,
    path: PathBuf,
    /// The file's text as it was read. `None` when the file did not exist,
    /// which is how `read_layer` already treats one.
    on_open: Option<String>,
    doc: DocumentMut,
    /// The document as this editing session first saw it, so a row can say
    /// whether it changed. Refreshed by a save, because a saved change is still
    /// a change this session made.
    baseline: DocumentMut,
    /// The file's text is not TOML at all, so `doc` is an empty document and
    /// not this file.
    ///
    /// The draft still opens - the modal has to be reachable on exactly the
    /// config that is broken - but the document cannot stand in for the file,
    /// so validation reads the text instead and an edit to this layer is
    /// refused rather than allowed to overwrite what could not be parsed.
    unparsed: bool,
}

impl LayerDoc {
    fn read(slot: LayerSlot, path: PathBuf) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(ConfigError::Read {
                    path,
                    detail: e.to_string(),
                })
            }
        };
        let (doc, unparsed) = match &text {
            Some(text) => match text.parse::<DocumentMut>() {
                Ok(doc) => (doc, false),
                Err(_) => (DocumentMut::new(), true),
            },
            None => (DocumentMut::new(), false),
        };
        Ok(Self {
            slot,
            path,
            on_open: text,
            baseline: doc.clone(),
            doc,
            unparsed,
        })
    }

    fn rendered(&self) -> String {
        self.doc.to_string()
    }

    /// The text this layer contributes to the chain: what the editor holds, or
    /// the file itself when the editor could not parse it.
    fn text(&self) -> String {
        if self.unparsed {
            self.on_open.clone().unwrap_or_default()
        } else {
            self.rendered()
        }
    }

    /// Derived, never tracked: a flag is something a code path can forget to
    /// set, and an edit typed back to what it was is not a change.
    fn dirty(&self) -> bool {
        !self.unparsed && self.rendered() != self.on_open.clone().unwrap_or_default()
    }

    fn info(&self) -> LayerInfo {
        LayerInfo {
            slot: self.slot,
            path: self.path.clone(),
            exists: self.on_open.is_some(),
            dirty: self.dirty(),
        }
    }
}

/// The three layers, open for editing.
///
/// Three named fields rather than a `Vec`, so a draft missing a layer cannot be
/// constructed and every accessor is total.
pub struct Draft {
    global: LayerDoc,
    repo: LayerDoc,
    personal: LayerDoc,
    home: String,
    /// What this editing session has changed, which is not the same question as
    /// what is unsaved: a saved change still has to be reported as something a
    /// running session cannot pick up.
    touched: BTreeSet<SettingKey>,
    touched_panes: bool,
}

/// Why a save wrote nothing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SaveError {
    #[error("{0}")]
    Invalid(#[from] ConfigError),
    #[error(
        "{path} changed on disk since this editor opened it, so nothing was \
         written. Reload that layer to see it, then make the edit again"
    )]
    Changed { slot: LayerSlot, path: PathBuf },
    #[error("could not read {path} back to check for changes: {detail}")]
    Reread { path: PathBuf, detail: String },
    #[error("could not write {path}: {detail}")]
    Write { path: PathBuf, detail: String },
}

impl Draft {
    /// Open the three layers. Their text does not have to parse.
    pub fn open(
        config_home: &Path,
        main_worktree: &Path,
        repo: &RootRepoName,
        home: &str,
    ) -> Result<Draft, ConfigError> {
        let global = LayerDoc::read(LayerSlot::Global, crate::global_config_path(config_home))?;
        let repo_layer = LayerDoc::read(LayerSlot::Repo, crate::repo_config_path(main_worktree))?;
        let key = override_key_of(&repo_layer, repo);
        let personal = LayerDoc::read(
            LayerSlot::Personal,
            crate::override_config_path(config_home, &key),
        )?;

        Ok(Self {
            global,
            repo: repo_layer,
            personal,
            home: home.to_owned(),
            touched: BTreeSet::new(),
            touched_panes: false,
        })
    }

    pub fn set(&mut self, slot: LayerSlot, key: SettingKey, value: SettingValue) {
        if !key.allows(slot) {
            return;
        }
        let layer = self.layer_mut(slot);
        if layer.unparsed {
            return;
        }
        let Some(table) = table_mut(&mut layer.doc, key.group()) else {
            return;
        };
        write_value(table, key.label(), &value);
        self.touched.insert(key);
    }

    pub fn clear(&mut self, slot: LayerSlot, key: SettingKey) {
        let layer = self.layer_mut(slot);
        if layer.unparsed {
            return;
        }
        match key.group() {
            None => {
                layer.doc.as_table_mut().remove(key.label());
            }
            Some(group) => {
                if let Some(table) = layer
                    .doc
                    .as_table_mut()
                    .get_mut(group)
                    .and_then(Item::as_table_like_mut)
                {
                    table.remove(key.label());
                }
            }
        }
        self.touched.insert(key);
    }

    /// The whole list, because that is the array-replace rule in a signature:
    /// there is no API by which a caller could append to an inherited list and
    /// produce a layout nobody intended.
    pub fn set_panes(&mut self, slot: LayerSlot, panes: Vec<PaneEdit>) {
        let layer = self.layer_mut(slot);
        if layer.unparsed {
            return;
        }
        let Some(layout) = table_mut(&mut layer.doc, Some("layout")) else {
            return;
        };
        let mut tables = ArrayOfTables::new();
        for pane in &panes {
            tables.push(pane_table(pane));
        }
        layout.insert("panes", Item::ArrayOfTables(tables));
        self.touched_panes = true;
    }

    pub fn view(&self) -> ConfigView {
        let settings = SettingKey::ALL
            .into_iter()
            .map(|key| {
                let per_layer = [
                    read_setting(&self.global.doc, key),
                    read_setting(&self.repo.doc, key),
                    read_setting(&self.personal.doc, key),
                ];
                let changed = self.layers().into_iter().any(|layer| {
                    read_setting(&layer.doc, key) != read_setting(&layer.baseline, key)
                });
                SettingRow::new(key, per_layer).mark_changed(changed)
            })
            .collect();

        let panes_changed = self
            .layers()
            .into_iter()
            .any(|layer| read_panes(&layer.doc) != read_panes(&layer.baseline));
        let panes = LayerSlot::ALL
            .into_iter()
            .rev()
            .find_map(|slot| {
                read_panes(&self.layer(slot).doc).map(|panes| PanesView {
                    panes,
                    origin: Origin::Layer(slot),
                    changed: panes_changed,
                })
            })
            .unwrap_or_else(|| PanesView {
                panes: default_pane_edits(),
                origin: Origin::Default,
                changed: panes_changed,
            });

        ConfigView {
            settings,
            panes,
            layers: [self.global.info(), self.repo.info(), self.personal.info()],
            status: self.validate().map(|_| ()).map_err(|e| e.to_string()),
        }
    }

    pub fn validate(&self) -> Result<parse::Config, ConfigError> {
        let mut raws = Vec::with_capacity(3);
        for layer in self.layers() {
            raws.push(parse_layer_text(&layer.path, &layer.text())?);
        }
        parse::from_raw(merge::merge(raws), &self.home)
    }

    pub fn save(&mut self) -> Result<Saved, SaveError> {
        // Invalid writes nothing at all, not even the layers that are fine: a
        // half-saved config is a config that will not start a session.
        self.validate()?;

        let dirty: Vec<LayerSlot> = LayerSlot::ALL
            .into_iter()
            .filter(|slot| self.layer(*slot).dirty())
            .collect();

        // A layer changed underneath the draft - another modal, a hand edit, a
        // `git pull` touching `.zax.toml` - is refused before anything is
        // written, with the edits still in the buffer. Staged saves make this a
        // real race rather than a theoretical one.
        for slot in &dirty {
            let layer = self.layer(*slot);
            let current = match std::fs::read_to_string(&layer.path) {
                Ok(text) => Some(text),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => {
                    return Err(SaveError::Reread {
                        path: layer.path.clone(),
                        detail: e.to_string(),
                    })
                }
            };
            if current != layer.on_open {
                return Err(SaveError::Changed {
                    slot: *slot,
                    path: layer.path.clone(),
                });
            }
        }

        for slot in &dirty {
            let layer = self.layer_mut(*slot);
            let text = layer.rendered();
            write_atomically(&layer.path, &text)?;
            // So the modal can stay open, clean, and save again without seeing
            // its own work as a race.
            layer.on_open = Some(text);
        }

        let mut pending: Vec<Pending> = self
            .touched
            .iter()
            .copied()
            .filter(|key| key.needs_restart())
            .map(Pending::Setting)
            .collect();
        if self.touched_panes {
            pending.push(Pending::Panes);
        }

        Ok(Saved {
            written: dirty,
            pending,
        })
    }

    /// Re-read one layer from disk, discarding this draft's edits to it. The way
    /// out of a layer that changed underneath the draft.
    pub fn reload(&mut self, slot: LayerSlot) -> Result<(), ConfigError> {
        let path = self.layer(slot).path.clone();
        *self.layer_mut(slot) = LayerDoc::read(slot, path)?;
        Ok(())
    }

    fn layer(&self, slot: LayerSlot) -> &LayerDoc {
        match slot {
            LayerSlot::Global => &self.global,
            LayerSlot::Repo => &self.repo,
            LayerSlot::Personal => &self.personal,
        }
    }

    fn layer_mut(&mut self, slot: LayerSlot) -> &mut LayerDoc {
        match slot {
            LayerSlot::Global => &mut self.global,
            LayerSlot::Repo => &mut self.repo,
            LayerSlot::Personal => &mut self.personal,
        }
    }

    /// Lowest precedence first, which is the order the chain folds them in.
    fn layers(&self) -> [&LayerDoc; 3] {
        [&self.global, &self.repo, &self.personal]
    }
}

/// Where the personal layer lives, decided before anything can be edited.
///
/// Layer 2's `name` decides it, so a repo whose `name` the load path would
/// refuse must not decide it here either - it falls back to the repository
/// directory name, and `validate` reports the refusal. A changed `name` is a
/// pending item rather than a new path: re-pointing the personal layer mid-edit
/// would silently strand whatever had already been typed into it.
fn override_key_of(repo_layer: &LayerDoc, repo: &RootRepoName) -> String {
    repo_layer
        .doc
        .as_table()
        .get("name")
        .and_then(Item::as_str)
        .filter(|name| crate::validate_override_name(name).is_ok())
        .map(str::to_owned)
        .unwrap_or_else(|| repo.as_str().to_owned())
}

/// The table a setting lives in, creating it when it is missing.
///
/// A key whose table is currently something other than a table (`agent = 3`)
/// has that replaced. The modal was asked to write the setting, and the file it
/// would otherwise have to leave alone is one no session can start from anyway.
fn table_mut<'a>(doc: &'a mut DocumentMut, group: Option<&'static str>) -> Option<&'a mut Table> {
    let Some(group) = group else {
        return Some(doc.as_table_mut());
    };
    let root = doc.as_table_mut();
    if !root.get(group).is_some_and(Item::is_table) {
        let mut table = Table::new();
        // An array-of-tables parent writes no header of its own: a `[layout]`
        // line above `[[layout.panes]]` is noise the author did not write.
        table.set_implicit(true);
        root.insert(group, Item::Table(table));
    }
    root.get_mut(group).and_then(Item::as_table_mut)
}

/// Write one value, keeping the decoration around the value it replaces - which
/// is where a trailing comment on that line lives.
fn write_value(table: &mut Table, key: &str, value: &SettingValue) {
    let new = match value {
        SettingValue::Scalar(text) => Value::from(text.as_str()),
        SettingValue::List(items) => {
            let mut array = Array::new();
            for item in items {
                array.push(item.as_str());
            }
            Value::Array(array)
        }
    };
    match table.get_mut(key).and_then(Item::as_value_mut) {
        Some(existing) => {
            let decor = existing.decor().clone();
            *existing = new;
            *existing.decor_mut() = decor;
        }
        None => {
            table.insert(key, Item::Value(new));
        }
    }
}

fn read_setting(doc: &DocumentMut, key: SettingKey) -> Option<SettingValue> {
    let item = match key.group() {
        None => doc.as_table().get(key.label()),
        Some(group) => doc
            .as_table()
            .get(group)
            .and_then(Item::as_table_like)
            .and_then(|table| table.get(key.label())),
    }?;

    match key.shape() {
        SettingShape::Scalar => Some(SettingValue::Scalar(value_text(item.as_value()?))),
        // A key whose value is the wrong shape reads as undefined here and is
        // reported by `validate`, which says what the type should have been.
        SettingShape::List => Some(SettingValue::List(
            item.as_array()?.iter().map(value_text).collect(),
        )),
    }
}

/// A TOML value as the editor shows it: a string without its quotes, anything
/// else as it is written, so a `max_age = 14` is visible rather than blank.
fn value_text(value: &Value) -> String {
    match value.as_str() {
        Some(text) => text.to_owned(),
        None => value.to_string().trim().to_owned(),
    }
}

fn read_panes(doc: &DocumentMut) -> Option<Vec<PaneEdit>> {
    let tables = doc
        .as_table()
        .get("layout")
        .and_then(Item::as_table_like)?
        .get("panes")?
        .as_array_of_tables()?;

    // An entry whose `kind` the editor cannot name is left out of the list it
    // offers. Such a pane cannot start a session either, and `validate` is
    // already saying so on the error line.
    Some(tables.iter().filter_map(pane_edit_from_table).collect())
}

fn pane_edit_from_table(table: &Table) -> Option<PaneEdit> {
    let kind = match table.get("kind").and_then(Item::as_str)? {
        "builtin" => PaneKindTag::Builtin,
        "exec" => PaneKindTag::Exec,
        "raw" => PaneKindTag::Raw,
        _ => return None,
    };
    let text = |key: &str| table.get(key).and_then(Item::as_value).map(value_text);
    Some(PaneEdit {
        kind,
        name: text("name"),
        title: text("title"),
        every: text("every"),
        command: table
            .get("command")
            .and_then(Item::as_array)
            .map(|array| array.iter().map(value_text).collect()),
        size: text("size"),
    })
}

/// One `[[layout.panes]]` table, with only the fields its kind carries and in
/// the order the parser reads them.
fn pane_table(pane: &PaneEdit) -> Table {
    let mut table = Table::new();
    table.insert("kind", Item::Value(Value::from(pane.kind.as_str())));
    for field in pane.kind.fields() {
        match field {
            PaneField::Command => {
                if let Some(argv) = &pane.command {
                    let mut array = Array::new();
                    for arg in argv {
                        array.push(arg.as_str());
                    }
                    table.insert("command", Item::Value(Value::Array(array)));
                }
            }
            other => {
                if let Some(text) = pane.field(*other) {
                    table.insert(other.label(), Item::Value(Value::from(text.as_str())));
                }
            }
        }
    }
    table
}

/// The built-in layout, as the editor would hold it. Taken from the same
/// function a session start uses, so the default the modal offers to edit is
/// the default zax actually runs.
fn default_pane_edits() -> Vec<PaneEdit> {
    parse::default_panes()
        .unwrap_or_default()
        .iter()
        .map(pane_edit_from_spec)
        .collect()
}

fn pane_edit_from_spec(spec: &PaneSpec) -> PaneEdit {
    let size = spec.size.map(|size| format!("{}%", size.get()));
    match &spec.kind {
        PaneKind::Builtin(builtin) => PaneEdit {
            kind: PaneKindTag::Builtin,
            name: Some(builtin.as_str().to_owned()),
            title: None,
            every: None,
            command: None,
            size,
        },
        PaneKind::Exec {
            title,
            every,
            command,
        } => PaneEdit {
            kind: PaneKindTag::Exec,
            name: None,
            title: Some(title.clone()),
            every: Some(zax_domain::format_duration(*every)),
            command: Some(command.argv().to_vec()),
            size,
        },
        PaneKind::Raw { command } => PaneEdit {
            kind: PaneKindTag::Raw,
            name: None,
            title: None,
            every: None,
            command: Some(command.argv().to_vec()),
            size,
        },
    }
}

/// Write via a temporary file in the same directory, renamed over the target.
///
/// A crash or a full disk then leaves the old config, never a truncated one -
/// and a truncated config is one no session can start from. The temporary name
/// carries the process id so two modals saving at once cannot collide on it.
fn write_atomically(path: &Path, text: &str) -> Result<(), SaveError> {
    let failed = |detail: String| SaveError::Write {
        path: path.to_owned(),
        detail,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| failed(e.to_string()))?;
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".to_owned());
    let temp = path.with_file_name(format!(".{name}.zax-{}.tmp", std::process::id()));

    std::fs::write(&temp, text).map_err(|e| failed(e.to_string()))?;
    if let Err(e) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(failed(e.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use super::*;

    struct Fixture {
        _tmp: tempfile::TempDir,
        config_home: PathBuf,
        main_worktree: PathBuf,
    }

    impl Fixture {
        /// Three layers, each written only if its text is `Some`.
        fn new(global: Option<&str>, repo: Option<&str>, personal: Option<&str>) -> Self {
            let tmp = tempfile::TempDir::new().unwrap();
            let config_home = tmp.path().join("config");
            let main_worktree = tmp.path().join("src").join("zax");
            std::fs::create_dir_all(&main_worktree).unwrap();

            if let Some(text) = global {
                let path = crate::global_config_path(&config_home);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }
            if let Some(text) = repo {
                std::fs::write(crate::repo_config_path(&main_worktree), text).unwrap();
            }
            if let Some(text) = personal {
                let path = crate::override_config_path(&config_home, "zax");
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }

            Self {
                _tmp: tmp,
                config_home,
                main_worktree,
            }
        }

        fn draft(&self) -> Draft {
            Draft::open(
                &self.config_home,
                &self.main_worktree,
                &RootRepoName::parse("zax").unwrap(),
                "/home/me",
            )
            .unwrap()
        }

        fn path(&self, slot: LayerSlot) -> PathBuf {
            match slot {
                LayerSlot::Global => crate::global_config_path(&self.config_home),
                LayerSlot::Repo => crate::repo_config_path(&self.main_worktree),
                LayerSlot::Personal => crate::override_config_path(&self.config_home, "zax"),
            }
        }

        fn text(&self, slot: LayerSlot) -> String {
            std::fs::read_to_string(self.path(slot)).unwrap()
        }
    }

    fn scalar(text: &str) -> SettingValue {
        SettingValue::Scalar(text.to_owned())
    }

    fn list(items: &[&str]) -> SettingValue {
        SettingValue::List(items.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn a_changed_value_keeps_the_comments_and_the_key_order_around_it() {
        // The whole reason for `toml_edit`. Re-serialising a layer would drop
        // every comment in a file somebody wrote by hand and sort the keys.
        let original = "\
# zax, globally.

[worktree]
# Where worktrees live.
root = \"~/src/worktrees\"
copy = [\".env\"]        # dotfiles a new worktree needs

[tmux]
prefix = \"C-a\"
";
        let fixture = Fixture::new(Some(original), None, None);
        let mut draft = fixture.draft();

        draft.set(LayerSlot::Global, SettingKey::TmuxPrefix, scalar("C-q"));
        draft.save().unwrap();

        let saved = fixture.text(LayerSlot::Global);
        assert!(saved.contains("# zax, globally."), "got:\n{saved}");
        assert!(saved.contains("# Where worktrees live."), "got:\n{saved}");
        assert!(
            saved.contains("# dotfiles a new worktree needs"),
            "got:\n{saved}"
        );
        assert!(saved.contains("prefix = \"C-q\""), "got:\n{saved}");
        assert!(!saved.contains("C-a"), "got:\n{saved}");
        // Key order is the author's, not serde's.
        let root_at = saved.find("root").unwrap();
        let copy_at = saved.find("copy").unwrap();
        assert!(root_at < copy_at, "got:\n{saved}");
    }

    #[test]
    fn an_untouched_layer_is_byte_identical_after_a_save() {
        let repo_text = "# what this repo ships\n[agent]\ncommand = [\"claude\"]\n";
        let fixture = Fixture::new(Some("[tmux]\nprefix = \"C-a\"\n"), Some(repo_text), None);
        let mut draft = fixture.draft();

        draft.set(LayerSlot::Global, SettingKey::TmuxPrefix, scalar("C-q"));
        draft.save().unwrap();

        assert_eq!(fixture.text(LayerSlot::Repo), repo_text);
    }

    #[test]
    fn a_save_does_not_create_a_file_for_a_layer_that_was_not_edited() {
        let fixture = Fixture::new(None, None, None);
        let mut draft = fixture.draft();

        draft.set(LayerSlot::Personal, SettingKey::TmuxPrefix, scalar("C-q"));
        let saved = draft.save().unwrap();

        assert_eq!(saved.written, vec![LayerSlot::Personal]);
        assert!(fixture.path(LayerSlot::Personal).exists());
        assert!(!fixture.path(LayerSlot::Global).exists());
        assert!(!fixture.path(LayerSlot::Repo).exists());
    }

    #[test]
    fn dirty_is_derived_so_an_edit_undone_by_hand_leaves_nothing_to_save() {
        let fixture = Fixture::new(Some("[tmux]\nprefix = \"C-a\"\n"), None, None);
        let mut draft = fixture.draft();

        draft.set(LayerSlot::Global, SettingKey::TmuxPrefix, scalar("C-q"));
        assert!(draft.view().any_dirty());

        draft.set(LayerSlot::Global, SettingKey::TmuxPrefix, scalar("C-a"));
        assert!(
            !draft.view().any_dirty(),
            "a value typed back to what it was is not a change"
        );
    }

    #[test]
    fn a_draft_sees_every_layers_value_and_which_one_wins() {
        let fixture = Fixture::new(
            Some("[tmux]\nprefix = \"C-a\"\n[prs]\nmax_age = \"30d\"\n"),
            Some("[tmux]\nprefix = \"C-b\"\n"),
            Some("[tmux]\nprefix = \"C-q\"\n"),
        );
        let view = fixture.draft().view();
        let prefix = view.row(SettingKey::TmuxPrefix).unwrap();

        assert_eq!(prefix.at(LayerSlot::Global), Some(&scalar("C-a")));
        assert_eq!(prefix.at(LayerSlot::Repo), Some(&scalar("C-b")));
        assert_eq!(prefix.at(LayerSlot::Personal), Some(&scalar("C-q")));
        assert_eq!(prefix.origin, Origin::Layer(LayerSlot::Personal));

        let max_age = view.row(SettingKey::PrsMaxAge).unwrap();
        assert_eq!(max_age.origin, Origin::Layer(LayerSlot::Global));
        assert_eq!(max_age.display_effective(), "30d");
    }

    #[test]
    fn a_list_setting_reads_back_as_a_list_and_an_empty_one_stays_empty() {
        let fixture = Fixture::new(
            Some("[worktree]\nhooks = [\"direnv allow\"]\n"),
            None,
            Some("[worktree]\nhooks = []\n"),
        );
        let view = fixture.draft().view();
        let hooks = view.row(SettingKey::WorktreeHooks).unwrap();

        assert_eq!(hooks.at(LayerSlot::Global), Some(&list(&["direnv allow"])));
        // An explicit `[]` clears the inherited list, and must not read back as
        // an absent key, which would inherit it instead.
        assert_eq!(hooks.at(LayerSlot::Personal), Some(&list(&[])));
        assert_eq!(hooks.origin, Origin::Layer(LayerSlot::Personal));
    }

    #[test]
    fn clearing_a_setting_falls_back_to_the_layer_below() {
        let fixture = Fixture::new(
            Some("[tmux]\nprefix = \"C-a\"\n"),
            None,
            Some("[tmux]\nprefix = \"C-q\"\n"),
        );
        let mut draft = fixture.draft();

        draft.clear(LayerSlot::Personal, SettingKey::TmuxPrefix);
        let view = draft.view();
        let prefix = view.row(SettingKey::TmuxPrefix).unwrap();

        assert_eq!(prefix.at(LayerSlot::Personal), None);
        assert_eq!(prefix.origin, Origin::Layer(LayerSlot::Global));

        draft.save().unwrap();
        assert!(!fixture.text(LayerSlot::Personal).contains("prefix"));
    }

    #[test]
    fn setting_a_value_in_a_layer_with_no_file_writes_one_table_and_one_key() {
        let fixture = Fixture::new(None, None, None);
        let mut draft = fixture.draft();

        draft.set(
            LayerSlot::Personal,
            SettingKey::PrsInclude,
            list(&["acme/*", "gameldar/zax"]),
        );
        draft.save().unwrap();

        let text = fixture.text(LayerSlot::Personal);
        assert!(text.contains("[prs]"), "got:\n{text}");
        assert!(
            text.contains("include = [\"acme/*\", \"gameldar/zax\"]"),
            "got:\n{text}"
        );
    }

    #[test]
    fn validate_returns_the_error_the_load_path_returns_for_the_same_text() {
        // Not a second opinion: the same chain, so the same sentence.
        for text in [
            "[agent]\ncomand = [\"claude\"]\n",
            "[prs]\nmax_age = \"fortnight\"\n",
            "[tmux]\nprefix = \"\"\n",
            "[worktree]\ncopy = [\"../outside\"]\n",
            "[[layout.panes]]\nkind = \"exec\"\ntitle = \"Deploys\"\n",
        ] {
            let fixture = Fixture::new(Some(text), None, None);
            let from_draft = fixture.draft().validate().unwrap_err();
            let from_load = crate::load(
                &fixture.config_home,
                &fixture.main_worktree,
                &RootRepoName::parse("zax").unwrap(),
                "/home/me",
            )
            .unwrap_err();
            assert_eq!(from_draft, from_load, "for:\n{text}");
        }
    }

    #[test]
    fn a_draft_opens_on_a_layer_that_is_not_toml_and_validate_says_so() {
        // The modal has to be reachable on exactly the config that is broken.
        let fixture = Fixture::new(Some("[tmux\nprefix = \"C-a\"\n"), None, None);
        let draft = fixture.draft();

        let error = draft.validate().unwrap_err();
        assert!(matches!(error, ConfigError::Parse { .. }), "got: {error}");
        assert!(draft.view().status.is_err());
    }

    #[test]
    fn an_invalid_draft_writes_no_file_at_all_not_even_the_valid_layers() {
        // A half-saved config is a config that will not start a session.
        let global = "[tmux]\nprefix = \"C-a\"\n";
        let fixture = Fixture::new(Some(global), None, None);
        let mut draft = fixture.draft();

        draft.set(LayerSlot::Global, SettingKey::TmuxPrefix, scalar("C-q"));
        draft.set(
            LayerSlot::Personal,
            SettingKey::PrsMaxAge,
            scalar("fortnight"),
        );

        let error = draft.save().unwrap_err();
        assert!(matches!(error, SaveError::Invalid(_)), "got: {error}");
        assert_eq!(fixture.text(LayerSlot::Global), global);
        assert!(!fixture.path(LayerSlot::Personal).exists());
    }

    #[test]
    fn a_layer_changed_on_disk_under_the_draft_is_refused_by_name() {
        let fixture = Fixture::new(Some("[tmux]\nprefix = \"C-a\"\n"), None, None);
        let mut draft = fixture.draft();
        draft.set(LayerSlot::Global, SettingKey::TmuxPrefix, scalar("C-q"));

        // Another modal, a hand edit, a `git pull`.
        std::fs::write(
            fixture.path(LayerSlot::Global),
            "[tmux]\nprefix = \"C-z\"\n",
        )
        .unwrap();

        let error = draft.save().unwrap_err();
        match &error {
            SaveError::Changed { slot, path } => {
                assert_eq!(*slot, LayerSlot::Global);
                assert_eq!(path, &fixture.path(LayerSlot::Global));
            }
            other => panic!("expected Changed, got {other}"),
        }
        // The edits are still in the buffer, and the file is untouched.
        assert!(fixture.text(LayerSlot::Global).contains("C-z"));
        assert_eq!(
            draft
                .view()
                .row(SettingKey::TmuxPrefix)
                .unwrap()
                .at(LayerSlot::Global),
            Some(&scalar("C-q"))
        );
    }

    #[test]
    fn reload_is_the_way_out_of_a_layer_that_changed_underneath() {
        let fixture = Fixture::new(Some("[tmux]\nprefix = \"C-a\"\n"), None, None);
        let mut draft = fixture.draft();
        draft.set(LayerSlot::Global, SettingKey::TmuxPrefix, scalar("C-q"));
        std::fs::write(
            fixture.path(LayerSlot::Global),
            "[tmux]\nprefix = \"C-z\"\n",
        )
        .unwrap();

        draft.reload(LayerSlot::Global).unwrap();

        assert_eq!(
            draft
                .view()
                .row(SettingKey::TmuxPrefix)
                .unwrap()
                .at(LayerSlot::Global),
            Some(&scalar("C-z"))
        );
        assert!(!draft.view().any_dirty());
        draft.set(LayerSlot::Global, SettingKey::TmuxPrefix, scalar("C-q"));
        assert!(draft.save().is_ok());
    }

    #[test]
    fn a_save_creates_the_personal_layers_directory_when_it_does_not_exist() {
        let fixture = Fixture::new(None, None, None);
        let mut draft = fixture.draft();
        draft.set(LayerSlot::Personal, SettingKey::TmuxPrefix, scalar("C-q"));

        draft.save().unwrap();

        assert!(fixture.path(LayerSlot::Personal).exists());
        assert!(fixture.text(LayerSlot::Personal).contains("C-q"));
    }

    #[test]
    fn a_save_leaves_no_temporary_file_beside_the_config() {
        // The write is atomic: a temp file in the same directory, renamed over
        // the target, so a crash leaves the old config and never a partial one.
        let fixture = Fixture::new(Some("[tmux]\nprefix = \"C-a\"\n"), None, None);
        let mut draft = fixture.draft();
        draft.set(LayerSlot::Global, SettingKey::TmuxPrefix, scalar("C-q"));
        draft.save().unwrap();

        let dir = fixture.path(LayerSlot::Global).parent().unwrap().to_owned();
        let entries: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec!["config.toml".to_owned()], "got: {entries:?}");
    }

    #[test]
    fn a_second_save_after_a_first_one_is_not_refused_as_a_change_on_disk() {
        // The modal stays open after a save, so `on_open` has to be refreshed
        // from what was written or the next save sees its own work as a race.
        let fixture = Fixture::new(Some("[tmux]\nprefix = \"C-a\"\n"), None, None);
        let mut draft = fixture.draft();

        draft.set(LayerSlot::Global, SettingKey::TmuxPrefix, scalar("C-q"));
        draft.save().unwrap();
        assert!(!draft.view().any_dirty());

        draft.set(LayerSlot::Global, SettingKey::PrsMaxAge, scalar("14d"));
        draft.save().unwrap();
        assert!(fixture.text(LayerSlot::Global).contains("14d"));
    }

    #[test]
    fn set_panes_replaces_the_list_rather_than_appending_to_it() {
        let fixture = Fixture::new(
            None,
            Some(
                "[[layout.panes]]\nkind = \"builtin\"\nname = \"prs-for-review\"\n\n\
                 [[layout.panes]]\nkind = \"builtin\"\nname = \"branch-status\"\n",
            ),
            None,
        );
        let mut draft = fixture.draft();

        let mut lazygit = PaneEdit::new(PaneKindTag::Raw);
        lazygit.command = Some(vec!["lazygit".to_owned()]);
        draft.set_panes(LayerSlot::Repo, vec![lazygit]);

        let view = draft.view();
        assert_eq!(view.panes.panes.len(), 1);
        assert_eq!(view.panes.panes[0].kind, PaneKindTag::Raw);

        draft.save().unwrap();
        let text = fixture.text(LayerSlot::Repo);
        assert!(!text.contains("prs-for-review"), "got:\n{text}");
        assert_eq!(text.matches("[[layout.panes]]").count(), 1, "got:\n{text}");
    }

    #[test]
    fn a_pane_of_each_kind_is_written_in_the_shape_the_parser_accepts() {
        let fixture = Fixture::new(None, None, None);
        let mut draft = fixture.draft();

        let panes = vec![
            PaneEdit {
                kind: PaneKindTag::Builtin,
                name: Some("prs-for-review".to_owned()),
                title: None,
                every: None,
                command: None,
                size: Some("30%".to_owned()),
            },
            PaneEdit {
                kind: PaneKindTag::Exec,
                name: None,
                title: Some("Deploys".to_owned()),
                every: Some("60s".to_owned()),
                command: Some(vec![
                    "kubectl".to_owned(),
                    "get".to_owned(),
                    "pods".to_owned(),
                ]),
                size: None,
            },
            PaneEdit {
                kind: PaneKindTag::Raw,
                name: None,
                title: None,
                every: None,
                command: Some(vec!["lazygit".to_owned()]),
                size: Some("25%".to_owned()),
            },
        ];
        draft.set_panes(LayerSlot::Personal, panes.clone());

        // The proof it is the shape the parser accepts: the parse chain takes it.
        let config = draft.validate().unwrap();
        assert_eq!(config.layout.panes().len(), 3);
        // And it round-trips back into the editor unchanged.
        draft.save().unwrap();
        let reopened = fixture.draft().view();
        assert_eq!(reopened.panes.panes, panes);
        assert_eq!(reopened.panes.origin, Origin::Layer(LayerSlot::Personal));
    }

    #[test]
    fn with_no_layer_defining_panes_the_view_shows_the_built_in_default_layout() {
        // So adding a pane to the default layout works without first having to
        // know what the default was.
        let view = Fixture::new(None, None, None).draft().view();

        assert_eq!(view.panes.origin, Origin::Default);
        assert_eq!(view.panes.panes.len(), 2);
        assert_eq!(view.panes.panes[0].kind, PaneKindTag::Builtin);
        assert_eq!(view.panes.panes[0].name.as_deref(), Some("prs-for-review"));
        assert_eq!(view.panes.panes[1].name.as_deref(), Some("branch-status"));
    }

    #[test]
    fn the_pane_list_belongs_to_the_highest_layer_that_defines_one() {
        let fixture = Fixture::new(
            Some("[[layout.panes]]\nkind = \"raw\"\ncommand = [\"htop\"]\n"),
            Some("[[layout.panes]]\nkind = \"raw\"\ncommand = [\"lazygit\"]\n"),
            None,
        );
        let view = fixture.draft().view();

        assert_eq!(view.panes.origin, Origin::Layer(LayerSlot::Repo));
        assert_eq!(
            view.panes.panes[0].command,
            Some(vec!["lazygit".to_owned()])
        );
    }

    #[test]
    fn the_view_names_each_layers_file_and_whether_it_exists() {
        let fixture = Fixture::new(Some("[tmux]\nprefix = \"C-a\"\n"), None, None);
        let view = fixture.draft().view();

        let global = view.layer(LayerSlot::Global).unwrap();
        assert_eq!(global.path, fixture.path(LayerSlot::Global));
        assert!(global.exists);
        assert!(!view.layer(LayerSlot::Repo).unwrap().exists);
        assert!(!view.any_dirty());
    }

    #[test]
    fn the_personal_layer_follows_the_repo_layers_override_name() {
        let fixture = Fixture::new(None, Some("name = \"other-key\"\n"), None);
        let view = fixture.draft().view();

        assert_eq!(
            view.layer(LayerSlot::Personal).unwrap().path,
            crate::override_config_path(&fixture.config_home, "other-key")
        );
        assert_eq!(
            view.row(SettingKey::RepoKeyName)
                .unwrap()
                .at(LayerSlot::Repo),
            Some(&scalar("other-key"))
        );
    }

    #[test]
    fn an_override_name_the_repo_may_not_set_does_not_decide_where_your_layer_is() {
        // `name = "../elsewhere"` is refused by the load path; the draft must
        // not read a file from the place it names either, and the error is what
        // validate reports.
        let fixture = Fixture::new(None, Some("name = \"../elsewhere\"\n"), None);
        let draft = fixture.draft();

        assert_eq!(
            draft.view().layer(LayerSlot::Personal).unwrap().path,
            crate::override_config_path(&fixture.config_home, "zax")
        );
        let error = draft.validate().unwrap_err();
        assert!(
            matches!(error, ConfigError::InvalidLayer { field: "name", .. }),
            "got: {error}"
        );
    }

    #[test]
    fn a_save_reports_what_a_running_session_cannot_pick_up() {
        let fixture = Fixture::new(None, None, None);
        let mut draft = fixture.draft();

        draft.set(LayerSlot::Personal, SettingKey::TmuxPrefix, scalar("C-q"));
        draft.set(LayerSlot::Personal, SettingKey::PrsMaxAge, scalar("14d"));
        draft.set_panes(
            LayerSlot::Personal,
            vec![PaneEdit {
                kind: PaneKindTag::Raw,
                name: None,
                title: None,
                every: None,
                command: Some(vec!["lazygit".to_owned()]),
                size: None,
            }],
        );

        let saved = draft.save().unwrap();

        assert_eq!(saved.written, vec![LayerSlot::Personal]);
        assert!(saved
            .pending
            .contains(&Pending::Setting(SettingKey::TmuxPrefix)));
        assert!(saved.pending.contains(&Pending::Panes));
        // `prs.max_age` is read fresh by the pane that uses it, so it is not
        // something the user has to be told about.
        assert!(!saved
            .pending
            .contains(&Pending::Setting(SettingKey::PrsMaxAge)));
    }

    #[test]
    fn a_save_with_nothing_pending_says_nothing() {
        let fixture = Fixture::new(None, None, None);
        let mut draft = fixture.draft();
        draft.set(LayerSlot::Personal, SettingKey::PrsMaxAge, scalar("14d"));

        let saved = draft.save().unwrap();

        assert_eq!(saved.written, vec![LayerSlot::Personal]);
        assert!(saved.pending.is_empty());
    }

    #[test]
    fn a_row_this_draft_changed_is_marked_and_an_untouched_one_is_not() {
        // The `*` on the main screen: which rows this editing session changed,
        // which is not the same question as which layer is unsaved.
        let fixture = Fixture::new(Some("[tmux]\nprefix = \"C-a\"\n"), None, None);
        let mut draft = fixture.draft();
        assert!(!draft.view().row(SettingKey::TmuxPrefix).unwrap().changed);

        draft.set(LayerSlot::Global, SettingKey::TmuxPrefix, scalar("C-q"));
        let view = draft.view();
        assert!(view.row(SettingKey::TmuxPrefix).unwrap().changed);
        assert!(!view.row(SettingKey::PrsMaxAge).unwrap().changed);
        assert!(!view.panes.changed);

        // A value typed back to what it was is not a change, here either.
        draft.set(LayerSlot::Global, SettingKey::TmuxPrefix, scalar("C-a"));
        assert!(!draft.view().row(SettingKey::TmuxPrefix).unwrap().changed);
    }

    #[test]
    fn an_edited_pane_list_is_marked_changed() {
        let fixture = Fixture::new(None, None, None);
        let mut draft = fixture.draft();
        assert!(!draft.view().panes.changed);

        draft.set_panes(
            LayerSlot::Personal,
            vec![PaneEdit::new(PaneKindTag::Builtin)],
        );

        assert!(draft.view().panes.changed);
    }

    #[test]
    fn a_change_still_counts_as_changed_after_it_has_been_saved() {
        // The save report speaks of the whole editing session: a saved prefix is
        // still a prefix the running server cannot pick up.
        let fixture = Fixture::new(None, None, None);
        let mut draft = fixture.draft();
        draft.set(LayerSlot::Personal, SettingKey::TmuxPrefix, scalar("C-q"));
        draft.save().unwrap();

        let view = draft.view();
        assert!(view.row(SettingKey::TmuxPrefix).unwrap().changed);
        assert!(!view.any_dirty(), "saved work is not unsaved work");
    }

    #[test]
    fn a_row_knows_which_layers_this_draft_changed() {
        let fixture = Fixture::new(None, None, None);
        let mut draft = fixture.draft();
        draft.set(LayerSlot::Personal, SettingKey::TmuxPrefix, scalar("C-q"));

        let view = draft.view();
        assert!(view.layer(LayerSlot::Personal).unwrap().dirty);
        assert!(!view.layer(LayerSlot::Global).unwrap().dirty);
    }
}
