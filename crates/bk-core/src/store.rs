//! Loading bookmarks from the global + project files, and comment-preserving writes.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Serialize;
use toml_edit::{Array, DocumentMut, Item, Table};

use crate::folders::{FolderMap, Located};
use crate::model::{BookFile, Bookmark, Server, Settings};
use crate::paths::{self, Paths};

/// Names that would collide with `bk` subcommands (the `bk <name>` shorthand).
pub const RESERVED_NAMES: &[&str] = &[
    "add",
    "app",
    "completions",
    "doctor",
    "edit",
    "export",
    "folders",
    "go",
    "help",
    "history",
    "import",
    "init",
    "keys",
    "list",
    "locate",
    "ls",
    "mv",
    "pick",
    "remove",
    "rename",
    "rm",
    "run",
    "save",
    "scan",
    "server",
    "servers",
    "settings",
    "show",
    "ssh",
    "sync",
    "trust",
    "update",
];

pub fn validate_name(name: &str) -> Result<()> {
    let ok_chars = name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if name.is_empty() || name.len() > 64 || !ok_chars || name.starts_with('-') {
        bail!("invalid name `{name}`: use letters, digits, `_` and `-` (max 64 chars)");
    }
    if RESERVED_NAMES.contains(&name) {
        bail!("`{name}` is a bk subcommand; pick another name");
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Global,
    Project,
}

/// A bookmark together with where it came from.
#[derive(Debug, Clone, Serialize)]
pub struct Entry {
    /// Stable id: `<file>#<name>`.
    pub id: String,
    pub name: String,
    pub scope: Scope,
    /// File the bookmark lives in.
    pub file: PathBuf,
    /// Directory relative paths resolve against (project dir, or `~` for global).
    pub base_dir: PathBuf,
    pub bookmark: Bookmark,
    /// Where folders from other devices are on this one.
    #[serde(skip)]
    pub folders: Arc<FolderMap>,
}

impl Entry {
    pub fn new(name: &str, scope: Scope, file: &Path, bookmark: Bookmark) -> Self {
        let base_dir = match scope {
            Scope::Global => paths::home(),
            Scope::Project => file.parent().map(Path::to_path_buf).unwrap_or_else(paths::home),
        };
        Entry {
            id: format!("{}#{}", file.display(), name),
            name: name.to_string(),
            scope,
            file: file.to_path_buf(),
            base_dir,
            bookmark,
            folders: Arc::default(),
        }
    }

    pub fn with_folders(mut self, folders: Arc<FolderMap>) -> Self {
        self.folders = folders;
        self
    }

    /// The working directory on this device, or `None` when the bookmark doesn't pin one.
    /// A folder that isn't here still gives its path (see [`Entry::folder`]).
    pub fn cwd(&self) -> Option<PathBuf> {
        if self.bookmark.is_remote() {
            return self.bookmark.cwd.as_deref().map(|c| paths::resolve(c, &self.base_dir));
        }
        self.folder().map(Located::into_path)
    }

    /// Where the bookmark's folder is on this device: as written, through a folder mapping,
    /// or missing. `None` for server bookmarks and ones that run wherever they're started.
    pub fn folder(&self) -> Option<Located> {
        if self.bookmark.is_remote() {
            return None;
        }
        match &self.bookmark.cwd {
            Some(c) => Some(self.folders.find_here(c, &self.base_dir)),
            None if self.scope == Scope::Project => Some(Located::Here(self.base_dir.clone())),
            None => None,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct LoadError {
    pub file: PathBuf,
    pub message: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Library {
    pub settings: Settings,
    /// SSH servers (global file only).
    pub servers: std::collections::BTreeMap<String, Server>,
    /// Project entries first (in the order given), then global.
    pub entries: Vec<Entry>,
    pub errors: Vec<LoadError>,
    /// Where folders from other devices are on this one.
    #[serde(skip)]
    pub folders: Arc<FolderMap>,
}

impl Library {
    pub fn load(paths: &Paths, project_files: &[PathBuf]) -> Library {
        let mut lib = Library { folders: Arc::new(FolderMap::load(paths)), ..Library::default() };
        for pf in project_files {
            match read_file(pf) {
                Ok(f) => lib.push_all(f, Scope::Project, pf),
                Err(e) => lib.errors.push(LoadError { file: pf.clone(), message: format!("{e:#}") }),
            }
        }
        match read_file(&paths.config_file) {
            Ok(f) => {
                lib.settings = f.settings.clone();
                lib.servers = f.servers.clone();
                lib.push_all(f, Scope::Global, &paths.config_file);
            }
            Err(e) => lib.errors.push(LoadError { file: paths.config_file.clone(), message: format!("{e:#}") }),
        }
        lib
    }

    fn push_all(&mut self, f: BookFile, scope: Scope, file: &Path) {
        for (name, b) in f.bookmarks {
            self.entries.push(Entry::new(&name, scope.clone(), file, b).with_folders(self.folders.clone()));
        }
    }

    /// First match wins, so project bookmarks shadow global ones.
    pub fn find(&self, name: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.name == name)
    }

    pub fn by_id(&self, id: &str) -> Option<&Entry> {
        self.entries.iter().find(|e| e.id == id)
    }

    /// True when an entry with the same name appears earlier (so it is shadowed).
    pub fn is_shadowed(&self, entry: &Entry) -> bool {
        self.find(&entry.name).is_some_and(|first| first.id != entry.id)
    }
}

/// Parse a bookmarks file; a missing file is empty.
pub fn read_file(path: &Path) -> Result<BookFile> {
    match fs::read_to_string(path) {
        Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BookFile::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn parse_str(text: &str) -> Result<BookFile> {
    toml::from_str(text).context("parsing bookmarks")
}

/// A standalone `[bookmarks.NAME]` table, as it would appear in a file.
pub fn render_bookmark(name: &str, b: &Bookmark) -> Result<String> {
    let mut doc = DocumentMut::new();
    bookmarks_table(&mut doc)?.insert(name, Item::Table(bookmark_table(b)?));
    Ok(doc.to_string())
}

/// Insert or replace a bookmark, keeping the rest of the file (comments, order) intact.
pub fn upsert(file: &Path, name: &str, bookmark: &Bookmark) -> Result<()> {
    validate_name(name)?;
    bookmark.validate().map_err(anyhow::Error::msg)?;
    let mut table = bookmark_table(bookmark)?;
    edit_document(file, |doc| {
        let books = bookmarks_table(doc)?;
        if let Some(Item::Table(old)) = books.get(name) {
            table.decor_mut().clone_from(old.decor());
            table.set_position(old.position());
        }
        books.insert(name, Item::Table(table));
        Ok(())
    })
}

/// Remove a bookmark. Returns false when it didn't exist.
pub fn remove(file: &Path, name: &str) -> Result<bool> {
    edit_document(file, |doc| Ok(bookmarks_table(doc)?.remove(name).is_some()))
}

/// Rename a bookmark in place.
pub fn rename(file: &Path, old: &str, new: &str) -> Result<()> {
    validate_name(new)?;
    edit_document(file, |doc| {
        let books = bookmarks_table(doc)?;
        if books.contains_key(new) {
            bail!("`{new}` already exists");
        }
        let item = books.remove(old).with_context(|| format!("no bookmark named `{old}`"))?;
        books.insert(new, item);
        Ok(())
    })
}

/// Insert or replace an SSH server in the `[servers]` table.
pub fn upsert_server(file: &Path, name: &str, server: &Server) -> Result<()> {
    crate::servers::validate_server(name, server).map_err(anyhow::Error::msg)?;
    let mut table = to_table(server)?;
    edit_document(file, |doc| {
        let item = doc.entry("servers").or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        });
        let servers = item.as_table_mut().context("`servers` must be a table")?;
        if let Some(Item::Table(old)) = servers.get(name) {
            table.decor_mut().clone_from(old.decor());
            table.set_position(old.position());
        }
        servers.insert(name, Item::Table(table));
        Ok(())
    })
}

pub fn remove_server(file: &Path, name: &str) -> Result<bool> {
    edit_document(file, |doc| {
        Ok(doc.get_mut("servers").and_then(Item::as_table_mut).is_some_and(|t| t.remove(name).is_some()))
    })
}

/// Add or remove `name` in `settings.hidden_servers`. Returns whether anything changed.
pub fn set_server_hidden(file: &Path, name: &str, hidden: bool) -> Result<bool> {
    edit_document(file, |doc| {
        let settings = doc.entry("settings").or_insert_with(|| {
            let mut t = Table::new();
            t.set_position(Some(0));
            Item::Table(t)
        });
        let table = settings.as_table_like_mut().context("`settings` must be a table")?;
        let list = table.entry("hidden_servers").or_insert(Item::Value(Array::new().into()));
        let arr = list.as_array_mut().context("`settings.hidden_servers` must be a list")?;
        let pos = arr.iter().position(|v| v.as_str() == Some(name));
        let changed = match (hidden, pos) {
            (true, None) => {
                arr.push(name);
                true
            }
            (false, Some(i)) => {
                arr.remove(i);
                true
            }
            _ => false,
        };
        arr.fmt();
        if arr.is_empty() {
            table.remove("hidden_servers");
        }
        Ok(changed)
    })
}

/// Replace the `[settings]` table.
pub fn write_settings(file: &Path, settings: &Settings) -> Result<()> {
    let mut table = to_table(settings)?;
    edit_document(file, |doc| {
        if let Some(Item::Table(old)) = doc.get("settings") {
            table.decor_mut().clone_from(old.decor());
            table.set_position(old.position());
        } else {
            // Keep settings at the top of a fresh file.
            table.set_position(Some(0));
        }
        doc.insert("settings", Item::Table(table));
        Ok(())
    })
}

fn bookmarks_table(doc: &mut DocumentMut) -> Result<&mut Table> {
    let item = doc.entry("bookmarks").or_insert_with(|| {
        let mut t = Table::new();
        t.set_implicit(true);
        Item::Table(t)
    });
    item.as_table_mut().context("`bookmarks` must be a table")
}

/// Key order in written bookmarks (the serializer would otherwise put tables last).
const FIELD_ORDER: &[&str] = &[
    "description",
    "server",
    "cwd",
    "env",
    "runtimes",
    "path",
    "command",
    "steps",
    "macos",
    "linux",
    "windows",
    "forward",
    "detach",
    "dotenv",
    "vars",
    "shell",
    "tags",
    "confirm",
    "notify",
    "pinned",
];

/// Serialize a bookmark into a table whose nested values (env, vars, steps) are inline.
pub fn bookmark_table(b: &Bookmark) -> Result<Table> {
    let mut t = to_table(b)?;
    let rank = |k: &str| FIELD_ORDER.iter().position(|f| *f == k).unwrap_or(FIELD_ORDER.len());
    t.sort_values_by(|k1, _, k2, _| rank(k1.get()).cmp(&rank(k2.get())));
    Ok(t)
}

fn to_table<T: Serialize>(v: &T) -> Result<Table> {
    let s = toml::to_string(v).context("serializing")?;
    let doc: DocumentMut = s.parse().context("re-parsing serialized toml")?;
    let mut table = doc.as_table().clone();
    inline_nested(&mut table);
    table.set_implicit(false);
    Ok(table)
}

fn inline_nested(t: &mut Table) {
    let keys: Vec<String> = t.iter().map(|(k, _)| k.to_string()).collect();
    for k in keys {
        let item = t.remove(&k).expect("key exists");
        let item = match item {
            Item::Table(sub) => Item::Value(toml_edit::Value::InlineTable(sub.into_inline_table())),
            Item::ArrayOfTables(a) => Item::Value(toml_edit::Value::Array(a.into_array())),
            other => other,
        };
        t.insert(&k, item);
    }
}

/// Lock, read, mutate, and atomically write back a TOML document.
fn edit_document<T>(file: &Path, f: impl FnOnce(&mut DocumentMut) -> Result<T>) -> Result<T> {
    // Write through symlinks (dotfile managers) instead of replacing the link.
    let target = dunce::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let dir = target.parent().context("file has no parent directory")?;
    fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let lock_path = dir.join(format!(".{}.lock", target.file_name().unwrap_or_default().to_string_lossy()));
    let lock = OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path)?;
    lock.lock().context("locking bookmarks file")?;

    let text = match fs::read_to_string(&target) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", target.display())),
    };
    let mut doc: DocumentMut = text.parse().with_context(|| format!("parsing {}", target.display()))?;
    let out = f(&mut doc)?;

    let rendered = doc.to_string();
    // Refuse to write something we can't read back.
    toml::from_str::<BookFile>(&rendered).context("refusing to write an invalid file")?;
    atomic_write(&target, rendered.as_bytes())?;
    drop(lock);
    Ok(out)
}

pub fn atomic_write(target: &Path, bytes: &[u8]) -> Result<()> {
    let dir = target.parent().context("no parent directory")?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    if let Ok(meta) = fs::metadata(target) {
        fs::set_permissions(tmp.path(), meta.permissions())?;
    }
    tmp.persist(target).with_context(|| format!("writing {}", target.display()))?;
    Ok(())
}

/// Write a private file atomically: mode 0600 on Unix (whatever the file had before); on
/// Windows it inherits the per-user data folder's permissions.
pub fn write_private(target: &Path, bytes: &[u8]) -> Result<()> {
    let dir = target.parent().context("no parent directory")?;
    fs::create_dir_all(dir)?;
    // NamedTempFile creates its file 0600 on Unix.
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(target).with_context(|| format!("writing {}", target.display()))?;
    Ok(())
}

/// Ensure a file exists (used when adding to a new project file).
pub fn touch(path: &Path) -> Result<()> {
    if !path.exists() {
        if let Some(d) = path.parent() {
            fs::create_dir_all(d)?;
        }
        File::create(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{EnvSpec, OsVariant, Step};

    #[test]
    fn hiding_servers_keeps_the_rest_of_settings() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("b.toml");
        fs::write(&f, "[settings]\ntheme = \"dark\" # mine\n\n[bookmarks.a]\ncommand = \"ls\"\n").unwrap();
        assert!(set_server_hidden(&f, "orb", true).unwrap());
        assert!(set_server_hidden(&f, "box", true).unwrap());
        assert!(!set_server_hidden(&f, "orb", true).unwrap(), "already hidden");
        assert!(set_server_hidden(&f, "orb", false).unwrap());
        let text = fs::read_to_string(&f).unwrap();
        assert!(text.contains("theme = \"dark\" # mine"), "{text}");
        assert!(text.contains("hidden_servers = [\"box\"]"), "{text}");
        assert_eq!(read_file(&f).unwrap().settings.hidden_servers, ["box"]);
        assert!(set_server_hidden(&f, "box", false).unwrap());
        assert!(!fs::read_to_string(&f).unwrap().contains("hidden_servers"));

        // A fresh file gets a [settings] table.
        let g = dir.path().join("new.toml");
        assert!(set_server_hidden(&g, "orb", true).unwrap());
        assert_eq!(read_file(&g).unwrap().settings.hidden_servers, ["orb"]);
    }

    #[test]
    fn upsert_preserves_comments_and_order() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("b.toml");
        fs::write(
            &f,
            "# my bookmarks\n\n# training\n[bookmarks.train]\ncommand = \"python a.py\"  # keep me\n\n[bookmarks.zzz]\ncommand = \"ls\"\n",
        )
        .unwrap();
        let b = Bookmark {
            command: Some("python b.py\n--flag".into()),
            env: EnvSpec::Conda { name: Some("sam2".into()), prefix: None },
            vars: [("A".to_string(), "1".to_string())].into(),
            ..Default::default()
        };
        upsert(&f, "train", &b).unwrap();
        let out = fs::read_to_string(&f).unwrap();
        assert!(out.starts_with("# my bookmarks"), "{out}");
        assert!(out.contains("# training\n[bookmarks.train]"), "{out}");
        assert!(out.find("bookmarks.train").unwrap() < out.find("bookmarks.zzz").unwrap(), "{out}");
        assert!(out.contains("env = { type = \"conda\", name = \"sam2\" }"), "{out}");
        assert!(out.contains("vars = { A = \"1\" }"), "{out}");
        let back = read_file(&f).unwrap();
        assert_eq!(back.bookmarks["train"], b);
    }

    #[test]
    fn steps_round_trip_inline() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("b.toml");
        let b = Bookmark {
            steps: vec![
                Step::new("a"),
                Step { run: "b".into(), cwd: Some("web".into()), continue_on_error: true, ..Default::default() },
            ],
            ..Default::default()
        };
        upsert(&f, "wf", &b).unwrap();
        let out = fs::read_to_string(&f).unwrap();
        assert!(!out.contains("[bookmarks]\n"), "implicit parent table: {out}");
        assert!(!out.contains("[[bookmarks.wf.steps]]"), "{out}");
        assert_eq!(read_file(&f).unwrap().bookmarks["wf"], b);
    }

    #[test]
    fn os_variants_round_trip_inline() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("b.toml");
        let b = Bookmark {
            command: Some("rm -rf build && make".into()),
            windows: Some(OsVariant {
                steps: vec![
                    Step::new("Remove-Item -Recurse build\nmake"),
                    Step { run: "x".into(), cwd: Some("web".into()), ..Default::default() },
                ],
                ..Default::default()
            }),
            macos: Some(OsVariant { shell: Some("zsh".into()), ..Default::default() }),
            linux: Some(OsVariant::default()),
            ..Default::default()
        };
        upsert(&f, "clean", &b).unwrap();
        let out = fs::read_to_string(&f).unwrap();
        assert!(out.contains("windows = { steps = ["), "{out}");
        assert!(out.contains("macos = { shell = \"zsh\" }"), "{out}");
        assert!(!out.contains("linux"), "an empty variant isn't written: {out}");
        assert!(out.find("command").unwrap() < out.find("macos").unwrap(), "{out}");
        let back = read_file(&f).unwrap().bookmarks.remove("clean").unwrap();
        assert_eq!(back.windows, b.windows);
        assert_eq!(back.macos, b.macos);

        // Written by hand as a sub-table, with a typo caught.
        let parsed = parse_str("[bookmarks.x]\ncommand = \"ls\"\n[bookmarks.x.windows]\ncommand = \"dir\"\n").unwrap();
        assert_eq!(parsed.bookmarks["x"].windows.as_ref().unwrap().command.as_deref(), Some("dir"));
        let err = parse_str("[bookmarks.x]\ncommand = \"ls\"\nwindows = { cwd = \"C:/x\" }\n").unwrap_err();
        assert!(format!("{err:#}").contains("cwd"), "{err:#}");
    }

    #[test]
    fn rename_and_remove() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("b.toml");
        let b = Bookmark { command: Some("ls".into()), ..Default::default() };
        upsert(&f, "a", &b).unwrap();
        upsert(&f, "b", &b).unwrap();
        assert!(rename(&f, "a", "b").is_err());
        rename(&f, "a", "c").unwrap();
        assert!(remove(&f, "c").unwrap());
        assert!(!remove(&f, "c").unwrap());
        assert_eq!(read_file(&f).unwrap().bookmarks.len(), 1);
    }

    #[test]
    fn names() {
        assert!(validate_name("train-2_x").is_ok());
        assert!(validate_name("run").is_err());
        assert!(validate_name("a b").is_err());
        assert!(validate_name("-x").is_err());
    }

    #[test]
    fn project_shadows_global() {
        let dir = tempfile::tempdir().unwrap();
        let g = dir.path().join("g.toml");
        let p = dir.path().join("proj").join(".bk.toml");
        let b = Bookmark { command: Some("ls".into()), ..Default::default() };
        upsert(&g, "dev", &b).unwrap();
        upsert(&p, "dev", &b).unwrap();
        let paths = Paths { config_file: g, data_dir: dir.path().into() };
        let lib = Library::load(&paths, std::slice::from_ref(&p));
        let e = lib.find("dev").unwrap();
        assert_eq!(e.scope, Scope::Project);
        assert_eq!(e.cwd().unwrap(), dir.path().join("proj"));
        assert!(lib.is_shadowed(&lib.entries[1]));
    }
}
