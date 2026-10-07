//! The `todo` source: a markdown file the broker reads and writes for every
//! pane, so two panes can never interleave a write.
//!
//! A fetch is a `stat` until the file changes. A write reads the file fresh,
//! applies the edit, writes a temporary file beside the target and renames
//! it over the target, but only if the target has not changed since it was
//! read: an editor saving underneath wins, and the edit is retried once on
//! top of what it saved.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};
use zax_domain::{TodoDocument, TodoError};

use crate::source::Source;
use crate::topics::{decode_todo_edit, encode_todo};

pub const TODO_INTERVAL: Duration = Duration::from_secs(2);
/// As the kernel's own limit on symlink hops.
const MAX_LINKS: usize = 40;

/// Enough of a file's metadata to tell that it changed: a rename gives a
/// new inode, an in-place write a new length or modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    ino: u64,
    len: u64,
    mtime: i64,
    mtime_nsec: i64,
}

/// `None` for a file that does not exist. Follows symlinks.
fn stamp(path: &Path) -> io::Result<Option<Stamp>> {
    match fs::metadata(path) {
        Ok(m) => Ok(Some(Stamp {
            ino: m.ino(),
            len: m.len(),
            mtime: m.mtime(),
            mtime_nsec: m.mtime_nsec(),
        })),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

type Hook = Box<dyn FnMut(&Path) + Send>;

pub struct TodoSource {
    file: PathBuf,
    cached: Option<(Option<Stamp>, Value)>,
    before_rename: Hook,
}

enum WriteError {
    Changed,
    Io(String),
}

impl TodoSource {
    pub fn new(file: PathBuf) -> Self {
        Self {
            file,
            cached: None,
            before_rename: Box::new(|_| {}),
        }
    }

    /// Runs after the temporary file is written and before the target is
    /// checked again: where a test plays an editor saving underneath.
    #[cfg(test)]
    fn with_hook(mut self, hook: impl FnMut(&Path) + Send + 'static) -> Self {
        self.before_rename = Box::new(hook);
        self
    }

    fn describe(&self, e: &io::Error) -> String {
        format!("{}: {e}", self.file.display())
    }

    /// The stamp is taken before the read, so a change between the two makes
    /// the stamp look old, never new: the worst case is one extra read.
    fn read(&self) -> Result<(Option<Stamp>, String), String> {
        let before = stamp(&self.file).map_err(|e| self.describe(&e))?;
        let text = match fs::read(&self.file) {
            Ok(bytes) => String::from_utf8(bytes)
                .map_err(|_| format!("{} is not UTF-8 text", self.file.display()))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(self.describe(&e)),
        };
        Ok((before, text))
    }

    fn write(&mut self, text: &str, expected: Option<Stamp>) -> Result<(), WriteError> {
        let io_error =
            |path: &Path, e: io::Error| WriteError::Io(format!("{}: {e}", path.display()));
        let target = resolve_links(&self.file).map_err(|e| io_error(&self.file, e))?;
        let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
            return Err(WriteError::Io(format!(
                "{} is not a file path",
                target.display()
            )));
        };
        fs::create_dir_all(dir).map_err(|e| io_error(dir, e))?;
        let temporary = dir.join(format!(
            ".{}.zax-{}",
            name.to_string_lossy(),
            std::process::id()
        ));

        let hook = &mut self.before_rename;
        let attempt = (|| -> io::Result<bool> {
            let mut file = fs::File::create(&temporary)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            if let Ok(meta) = fs::metadata(&target) {
                fs::set_permissions(&temporary, meta.permissions())?;
            }
            hook(&target);
            if stamp(&target)? != expected {
                return Ok(false);
            }
            fs::rename(&temporary, &target)?;
            Ok(true)
        })();
        match attempt {
            Ok(true) => Ok(()),
            Ok(false) => {
                let _ = fs::remove_file(&temporary);
                Err(WriteError::Changed)
            }
            Err(e) => {
                let _ = fs::remove_file(&temporary);
                Err(io_error(&target, e))
            }
        }
    }
}

/// The file a write should replace: `path` with every symlink followed, so
/// a todo file linked into a dotfiles repository keeps its link. A link to
/// a file that does not exist yet resolves to where that file would be.
fn resolve_links(path: &Path) -> io::Result<PathBuf> {
    let mut current = path.to_path_buf();
    for _ in 0..MAX_LINKS {
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = fs::read_link(&current)?;
                current = match current.parent() {
                    Some(parent) if link.is_relative() => parent.join(link),
                    _ => link,
                };
            }
            Ok(_) => return Ok(current),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(current),
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("too many levels of symbolic links"))
}

impl Source for TodoSource {
    fn fetch(&mut self) -> Result<Value, String> {
        let now = stamp(&self.file).map_err(|e| self.describe(&e))?;
        if let Some((cached, value)) = &self.cached {
            if *cached == now {
                return Ok(value.clone());
            }
        }
        let (stamp, text) = self.read()?;
        let value = encode_todo(
            &self.file.display().to_string(),
            &TodoDocument::parse(&text).items(),
        );
        self.cached = Some((stamp, value.clone()));
        Ok(value)
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let edit = decode_todo_edit(method, &params)?;
        // Twice at most: once, and once more on top of a save that landed
        // during the first attempt. A file changing under every attempt is
        // being edited right now, and the edit is better refused than raced.
        for _ in 0..2 {
            let (before, text) = self.read()?;
            let mut document = TodoDocument::parse(&text);
            document.apply(&edit).map_err(|e| e.to_string())?;
            match self.write(&document.render(), before) {
                Ok(()) => return Ok(json!({})),
                Err(WriteError::Changed) => {}
                Err(WriteError::Io(e)) => return Err(e),
            }
        }
        Err(TodoError::Stale.to_string())
    }

    fn interval(&self) -> Duration {
        TODO_INTERVAL
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use serde_json::json;

    use super::*;
    use crate::topics::decode_todo;

    fn add(text: &str) -> (&'static str, Value) {
        ("todo/add", json!({"text": text}))
    }

    #[test]
    fn a_missing_file_is_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let mut source = TodoSource::new(dir.path().join("nope").join("todo.md"));
        let list = decode_todo(&source.fetch().unwrap()).unwrap();
        assert!(list.items.is_empty());
        assert!(list.file.ends_with("todo.md"));
    }

    #[test]
    fn a_fetch_lists_the_tasks() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("todo.md");
        fs::write(&file, "# T\n- [ ] a\n- [x] b\n").unwrap();
        let list = decode_todo(&TodoSource::new(file).fetch().unwrap()).unwrap();
        assert_eq!(list.items.len(), 2);
    }

    #[test]
    fn a_file_that_is_not_utf8_is_an_error_naming_it() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("todo.md");
        fs::write(&file, [0xff, 0xfe, 0x00]).unwrap();
        let err = TodoSource::new(file).fetch().unwrap_err();
        assert!(err.contains("todo.md") && err.contains("UTF-8"), "{err}");
    }

    #[test]
    fn an_unchanged_file_is_not_read_again() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("todo.md");
        fs::write(&file, "- [ ] a\n").unwrap();
        let mut source = TodoSource::new(file.clone());
        let first = source.fetch().unwrap();
        // Same stamp, different bytes: only a cache hit gives back the old list.
        let stamp = stamp(&file).unwrap();
        source.cached = Some((stamp, json!("cached")));
        assert_eq!(source.fetch().unwrap(), json!("cached"));
        fs::write(&file, "- [ ] a\n- [ ] b\n").unwrap();
        assert_ne!(source.fetch().unwrap(), first);
    }

    #[test]
    fn an_add_creates_the_file_and_its_directory() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("zax").join("todo.md");
        let mut source = TodoSource::new(file.clone());
        let (method, params) = add("first");
        assert_eq!(source.call(method, params).unwrap(), json!({}));
        assert_eq!(fs::read_to_string(&file).unwrap(), "- [ ] first\n");
        // And the next fetch sees it.
        assert_eq!(
            decode_todo(&source.fetch().unwrap()).unwrap().items.len(),
            1
        );
    }

    #[test]
    fn a_write_leaves_no_temporary_file_behind() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("todo.md");
        let (method, params) = add("a");
        TodoSource::new(file).call(method, params).unwrap();
        let names: Vec<String> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["todo.md"]);
    }

    #[test]
    fn a_write_keeps_the_files_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("todo.md");
        fs::write(&file, "- [ ] a\n").unwrap();
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
        let (method, params) = add("b");
        TodoSource::new(file.clone()).call(method, params).unwrap();
        assert_eq!(
            fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn a_symlinked_file_keeps_its_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("dotfiles").join("todo.md");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, "- [ ] a\n").unwrap();
        let link = dir.path().join("todo.md");
        std::os::unix::fs::symlink("dotfiles/todo.md", &link).unwrap();

        let (method, params) = add("b");
        TodoSource::new(link.clone()).call(method, params).unwrap();

        assert!(fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "- [ ] a\n- [ ] b\n");
    }

    #[test]
    fn a_stale_edit_is_refused_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("todo.md");
        fs::write(&file, "- [ ] moved\n- [ ] a\n").unwrap();
        let err = TodoSource::new(file.clone())
            .call("todo/delete", json!({"line": 0, "raw": "- [ ] a"}))
            .unwrap_err();
        assert_eq!(err, "the todo file changed - refreshed");
        assert_eq!(fs::read_to_string(&file).unwrap(), "- [ ] moved\n- [ ] a\n");
    }

    #[test]
    fn a_change_during_one_write_is_retried_onto_the_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("todo.md");
        fs::write(&file, "- [ ] a\n").unwrap();
        let mut edited = false;
        let mut source = TodoSource::new(file.clone()).with_hook(move |target| {
            if !edited {
                edited = true;
                fs::write(target, "- [ ] a\n- [ ] by hand\n").unwrap();
            }
        });
        let (method, params) = add("b");
        source.call(method, params).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "- [ ] a\n- [ ] by hand\n- [ ] b\n"
        );
    }

    #[test]
    fn a_change_during_every_write_attempt_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("todo.md");
        fs::write(&file, "- [ ] a\n").unwrap();
        let mut n = 0;
        let mut source = TodoSource::new(file.clone()).with_hook(move |target| {
            n += 1;
            fs::write(target, format!("- [ ] edit {n}\n")).unwrap();
        });
        let (method, params) = add("b");
        assert_eq!(
            source.call(method, params).unwrap_err(),
            "the todo file changed - refreshed"
        );
        // The hand edit wins; the broker's edit was not written.
        assert_eq!(fs::read_to_string(&file).unwrap(), "- [ ] edit 2\n");
    }

    #[test]
    fn an_unknown_method_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let err = TodoSource::new(dir.path().join("t.md"))
            .call("todo/nope", json!({}))
            .unwrap_err();
        assert!(err.contains("todo/nope"), "{err}");
    }

    #[test]
    fn it_polls_every_two_seconds() {
        assert_eq!(
            TodoSource::new(PathBuf::from("/x")).interval(),
            Duration::from_secs(2)
        );
    }
}
