//! Folders on this device: where folders named in synced bookmarks live here.
//!
//! A bookmark synced from a Mac says `cwd = "~/work/sam2"`; on the Windows PC that project is
//! in `D:\dev\sam2`. Rewriting the bookmark would break it on the Mac, so each device keeps a
//! small map instead, never synced (`data_dir/folders.toml`):
//!
//! ```toml
//! "~/work" = "D:/dev"
//! ```
//!
//! It's only a fallback: a folder that exists as written is used as written, so a mapping
//! can't break a bookmark that works here, and one mapping fixes every bookmark under it.
//! Paths are compared as text, split on `/` and `\`, so a Windows path from another device
//! (`D:\dev\api`) can be mapped on a Mac, and the other way round.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use toml_edit::DocumentMut;

use crate::paths::{self, Paths};
use crate::store::{Entry, Library, atomic_write};

const HEADER: &str = "\
# Where folders from your other devices are on this one (Clibooker never syncs this file).
# A bookmark whose folder isn't here as written looks for it through these, e.g.
# \"~/work\" = \"D:/dev\"
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Root {
    /// `~`
    Home,
    /// `/` (macOS, Linux)
    Slash,
    /// `D:`
    Drive(char),
    /// `\\server\share`
    Unc,
}

/// A `~` or absolute path from any OS, as text: its root and its parts.
#[derive(Debug, Clone)]
struct PathKey {
    root: Root,
    parts: Vec<String>,
}

impl PathKey {
    /// `None` for a relative path.
    fn parse(s: &str) -> Option<PathKey> {
        let s = s.trim();
        let b = s.as_bytes();
        let is_drive =
            b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && b.get(2).is_none_or(|c| b"/\\".contains(c));
        let (root, rest) = if let Some(rest) =
            s.strip_prefix('~').filter(|r| r.is_empty() || r.starts_with(['/', '\\']))
        {
            (Root::Home, rest)
        } else if is_drive {
            (Root::Drive(b[0].to_ascii_uppercase() as char), &s[2..])
        } else if let Some(rest) = s.strip_prefix("\\\\").or_else(|| s.strip_prefix("//").filter(|_| cfg!(windows))) {
            (Root::Unc, rest)
        } else {
            (Root::Slash, s.strip_prefix('/')?)
        };
        let mut parts: Vec<String> = Vec::new();
        for p in rest.split(['/', '\\']) {
            match p {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                p => parts.push(p.to_string()),
            }
        }
        Some(PathKey { root, parts })
    }

    /// Windows paths ignore case; so does everything on a Windows device.
    fn ignores_case(&self) -> bool {
        cfg!(windows) || matches!(self.root, Root::Drive(_) | Root::Unc)
    }

    fn same_part(&self, a: &str, b: &str) -> bool {
        if self.ignores_case() { a.eq_ignore_ascii_case(b) } else { a == b }
    }

    fn starts_with(&self, prefix: &PathKey) -> bool {
        self.root == prefix.root
            && prefix.parts.len() <= self.parts.len()
            && prefix.parts.iter().zip(&self.parts).all(|(a, b)| self.same_part(a, b))
    }

    fn same(&self, other: &PathKey) -> bool {
        self.parts.len() == other.parts.len() && self.starts_with(other)
    }

    /// Just a root (`~`, `/`, `D:/`, `\\server\share`): too broad to map.
    fn is_root_only(&self) -> bool {
        match self.root {
            Root::Unc => self.parts.len() <= 2,
            _ => self.parts.is_empty(),
        }
    }

    /// Written in this OS's own syntax (so it can be checked on disk as it is).
    fn is_native(&self) -> bool {
        match self.root {
            Root::Home => true,
            Root::Slash => !cfg!(windows),
            Root::Drive(_) | Root::Unc => cfg!(windows),
        }
    }

    /// The text form, with `/` (which Windows accepts too), except for UNC paths.
    fn render(&self) -> String {
        let joined = self.parts.join("/");
        match self.root {
            Root::Home if joined.is_empty() => "~".into(),
            Root::Home => format!("~/{joined}"),
            Root::Slash => format!("/{joined}"),
            Root::Drive(d) => format!("{d}:/{joined}"),
            Root::Unc => format!("\\\\{}", self.parts.join("\\")),
        }
    }
}

/// The OS a path was clearly written on, when that isn't this one.
pub fn foreign_os(written: &str) -> Option<&'static str> {
    let w = written.trim();
    if cfg!(windows) {
        let unix = w.starts_with('/') && !w.starts_with("//");
        return unix.then(|| {
            if w.starts_with("/Users/") {
                "macOS"
            } else if w.starts_with("/home/") {
                "Linux"
            } else {
                "macOS or Linux"
            }
        });
    }
    if paths::is_windows_absolute(w) {
        Some("Windows")
    } else if cfg!(target_os = "macos") && w.starts_with("/home/") {
        Some("Linux")
    } else if !cfg!(target_os = "macos") && w.starts_with("/Users/") {
        Some("macOS")
    } else {
        None
    }
}

/// Where a bookmark's folder is on this device.
#[derive(Debug, Clone, PartialEq)]
pub enum Located {
    /// Where the bookmark says (or relative to its project).
    Here(PathBuf),
    /// Not where it says, but through a mapping on this device.
    Mapped { path: PathBuf, shared: String, local: String },
    /// Not on this device. `foreign` names the OS the path was written on, when it's
    /// clearly another one.
    Missing { written: String, path: PathBuf, foreign: Option<&'static str> },
}

impl Located {
    pub fn path(&self) -> &Path {
        match self {
            Located::Here(p) | Located::Mapped { path: p, .. } | Located::Missing { path: p, .. } => p,
        }
    }

    pub fn into_path(self) -> PathBuf {
        match self {
            Located::Here(p) | Located::Mapped { path: p, .. } | Located::Missing { path: p, .. } => p,
        }
    }

    pub fn is_missing(&self) -> bool {
        matches!(self, Located::Missing { .. })
    }

    /// "~/work/sam2 doesn't exist on this device" (or "D:\dev\api is a Windows path, not on
    /// this device").
    pub fn missing_reason(&self) -> Option<String> {
        let Located::Missing { written, foreign, .. } = self else { return None };
        Some(match foreign {
            Some(os) => format!("{written} is a {os} path, not on this device"),
            None => format!("{written} doesn't exist on this device"),
        })
    }
}

#[derive(Debug, Clone)]
pub struct Rule {
    /// The folder as other devices name it (`~/work`).
    pub shared: String,
    /// Where it is on this device (`D:/dev`).
    pub local: String,
    key: PathKey,
}

/// This device's folder mappings, most specific first.
#[derive(Debug, Clone, Default)]
pub struct FolderMap {
    rules: Vec<Rule>,
}

impl FolderMap {
    /// A missing or unreadable file is an empty map.
    pub fn load(paths: &Paths) -> FolderMap {
        let text = fs::read_to_string(paths.folders_file()).unwrap_or_default();
        let table: std::collections::BTreeMap<String, String> = toml::from_str(&text).unwrap_or_default();
        FolderMap::from_pairs(table)
    }

    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, String)>) -> FolderMap {
        let mut rules: Vec<Rule> = pairs
            .into_iter()
            .filter_map(|(shared, local)| {
                let key = PathKey::parse(&shared)?;
                (!local.trim().is_empty()).then_some(Rule { shared, local, key })
            })
            .collect();
        rules.sort_by_key(|r| std::cmp::Reverse(r.key.parts.len()));
        FolderMap { rules }
    }

    pub fn rules(&self) -> &[Rule] {
        &self.rules
    }

    /// Map `shared` to `local` on this device. Returns the local folder it replaced.
    pub fn add(paths: &Paths, shared: &str, local: &str) -> Result<Option<String>> {
        let key = PathKey::parse(shared).with_context(|| format!("`{shared}` isn't a ~ or absolute path"))?;
        if key.is_root_only() {
            bail!("`{shared}` is too broad to map; name a folder inside it");
        }
        if local.trim().is_empty() {
            bail!("say where `{shared}` is on this device");
        }
        let mut map = FolderMap::load(paths);
        let replaced = map.take(&key);
        map.rules.push(Rule { shared: key.render(), local: local.trim().to_string(), key });
        map.save(paths)?;
        Ok(replaced)
    }

    /// Forget the mapping for `shared` (any spelling of it). Returns false when there was none.
    pub fn remove(paths: &Paths, shared: &str) -> Result<bool> {
        let Some(key) = PathKey::parse(shared) else { return Ok(false) };
        let mut map = FolderMap::load(paths);
        let removed = map.take(&key).is_some();
        if removed {
            map.save(paths)?;
        }
        Ok(removed)
    }

    fn take(&mut self, key: &PathKey) -> Option<String> {
        let i = self.rules.iter().position(|r| r.key.same(key))?;
        Some(self.rules.remove(i).local)
    }

    /// The file is Clibooker's own, so it's written whole: the header, then the mappings.
    fn save(&self, paths: &Paths) -> Result<()> {
        let mut rules: Vec<&Rule> = self.rules.iter().collect();
        rules.sort_by(|a, b| a.shared.cmp(&b.shared));
        let mut doc = DocumentMut::new();
        for r in rules {
            doc.insert(&r.shared, toml_edit::value(&r.local));
        }
        fs::create_dir_all(&paths.data_dir)?;
        atomic_write(&paths.folders_file(), format!("{HEADER}\n{doc}").as_bytes())
    }

    /// Where `written` (a bookmark's folder, resolved against `base`) is on this device.
    pub fn find_here(&self, written: &str, base: &Path) -> Located {
        let Some(key) = PathKey::parse(written) else { return Located::Here(paths::resolve(written, base)) };
        let native = key.is_native().then(|| paths::resolve(written, base));
        if let Some(p) = native.as_ref().filter(|p| p.is_dir()) {
            return Located::Here(p.clone());
        }
        for rule in &self.rules {
            if key.starts_with(&rule.key) {
                let mut path = paths::expand_tilde(&rule.local);
                path.extend(&key.parts[rule.key.parts.len()..]);
                if path.is_dir() {
                    return Located::Mapped { path, shared: rule.shared.clone(), local: rule.local.clone() };
                }
            }
        }
        Located::Missing {
            written: written.to_string(),
            path: native.unwrap_or_else(|| PathBuf::from(written)),
            foreign: foreign_os(written),
        }
    }

    /// How many of `lib`'s bookmarks reach their folder through `rule`.
    pub fn used_by(&self, rule: &Rule, lib: &Library) -> usize {
        lib.entries
            .iter()
            .filter(|e| matches!(e.folder(), Some(Located::Mapped { ref shared, .. }) if *shared == rule.shared))
            .count()
    }
}

/// The mapping that turns `written` into `found`: the common trailing folders are dropped, so
/// locating `~/work/ml/sam2` at `D:\dev\ml\sam2` maps `~/work` to `D:/dev`, and every other
/// bookmark under `~/work` follows. Never shorter than one folder past a root.
pub fn infer(written: &str, found: &Path) -> Option<(String, String)> {
    pair(written, found, true)
}

/// `broad`: drop the common trailing folders; otherwise map exactly this folder.
fn pair(written: &str, found: &Path, broad: bool) -> Option<(String, String)> {
    let mut shared = PathKey::parse(written)?;
    let mut local = PathKey::parse(&paths::contract_tilde(found))?;
    while broad && shared.parts.len() > 1 && local.parts.len() > 1 {
        let (a, b) = (shared.parts.last()?, local.parts.last()?);
        if !(shared.same_part(a, b) || local.same_part(a, b)) {
            break;
        }
        shared.parts.pop();
        local.parts.pop();
    }
    if shared.is_root_only() || local.is_root_only() {
        return None;
    }
    Some((shared.render(), local.render()))
}

/// Folders on this device that may be a missing one: same name, next to the folders of
/// bookmarks that work here, next to registered projects, or in home.
pub struct Suggester {
    parents: Vec<PathBuf>,
}

impl Suggester {
    pub fn new(lib: &Library, project_dirs: &[PathBuf]) -> Suggester {
        let mut parents: Vec<PathBuf> = Vec::new();
        let known = lib
            .entries
            .iter()
            .filter_map(|e| e.folder())
            .filter(|l| !l.is_missing())
            .map(Located::into_path)
            .chain(project_dirs.iter().cloned());
        for dir in known {
            if let Some(parent) = dir.parent()
                && !parents.iter().any(|p| paths::same_path(p, parent))
            {
                parents.push(parent.to_path_buf());
            }
        }
        let home = paths::home();
        if !parents.iter().any(|p| paths::same_path(p, &home)) {
            parents.push(home);
        }
        Suggester { parents }
    }

    pub fn suggest(&self, written: &str) -> Vec<PathBuf> {
        let Some(name) = PathKey::parse(written).and_then(|k| k.parts.last().cloned()) else { return Vec::new() };
        self.parents.iter().map(|p| p.join(&name)).filter(|p| p.is_dir()).take(5).collect()
    }
}

/// What locating a bookmark's folder did.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LocateResult {
    pub shared: String,
    pub local: String,
    /// The local folder `shared` used to map to.
    pub replaced: Option<String>,
    /// Other bookmarks this fixed.
    pub fixed: Vec<String>,
}

/// "`e`'s folder is `dir` on this device": records the broadest mapping that says so.
pub fn locate(paths: &Paths, lib: &Library, e: &Entry, dir: &Path) -> Result<LocateResult> {
    if e.bookmark.is_remote() {
        bail!("`{}` runs on a server; its folder is on the server", e.name);
    }
    let Some(written) = e.bookmark.cwd.as_deref() else {
        bail!("`{}` has no folder of its own; it runs wherever it's started", e.name)
    };
    if !dir.is_dir() {
        bail!("{} is not a folder", dir.display());
    }
    match e.folder() {
        Some(Located::Here(p)) if p.is_dir() => {
            bail!("`{}`'s folder is already on this device ({})", e.name, paths::contract_tilde(&p))
        }
        _ => {}
    }
    let Some((mut shared, mut local)) = infer(written, dir) else {
        bail!("`{}`'s folder `{written}` is relative to where the bookmark lives; it moves with it", e.name)
    };
    // Re-pointing a mapping that other bookmarks already reach their folders through would
    // break them: map just this folder instead.
    let current = FolderMap::load(paths);
    let key = PathKey::parse(&shared).expect("inferred from a parsed path");
    let serves_others = current.rules().iter().any(|r| {
        r.key.same(&key)
            && lib.entries.iter().any(|o| {
                o.id != e.id && matches!(o.folder(), Some(Located::Mapped { shared: ref s, .. }) if *s == r.shared)
            })
    });
    if serves_others && let Some(exact) = pair(written, dir, false) {
        (shared, local) = exact;
    }
    let missing: Vec<&Entry> =
        lib.entries.iter().filter(|o| o.id != e.id && o.folder().is_some_and(|l| l.is_missing())).collect();
    let replaced = FolderMap::add(paths, &shared, &local)?;
    let map = FolderMap::load(paths);
    let fixed = missing
        .into_iter()
        .filter(|o| o.bookmark.cwd.as_deref().is_some_and(|c| !map.find_here(c, &o.base_dir).is_missing()))
        .map(|o| o.name.clone())
        .collect();
    Ok(LocateResult { shared, local, replaced, fixed })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(dir: &Path) -> Paths {
        Paths { config_file: dir.join("bookmarks.toml"), data_dir: dir.join("data") }
    }

    #[test]
    fn keys_from_every_os() {
        let k = PathKey::parse(r"c:\Work\.\x\..\Api").unwrap();
        assert_eq!(k.root, Root::Drive('C'));
        assert_eq!(k.parts, ["Work", "Api"]);
        assert!(k.same(&PathKey::parse("C:/work/api").unwrap()), "drive paths ignore case");
        assert_eq!(PathKey::parse(r"~\a\b").unwrap().render(), "~/a/b");
        assert_eq!(PathKey::parse("~").unwrap().render(), "~");
        assert_eq!(PathKey::parse(r"\\srv\share\x").unwrap().render(), r"\\srv\share\x");
        assert!(PathKey::parse(r"\\srv\share").unwrap().is_root_only());
        assert!(PathKey::parse("D:/").unwrap().is_root_only());
        assert!(PathKey::parse("relative/dir").is_none());
        assert!(PathKey::parse("/a/b/c").unwrap().starts_with(&PathKey::parse("/a/b/").unwrap()));
        assert!(!PathKey::parse("/a/bc").unwrap().starts_with(&PathKey::parse("/a/b").unwrap()));
    }

    #[test]
    fn infer_keeps_the_broadest_mapping() {
        let d = tempfile::tempdir().unwrap();
        let found = d.path().join("dev/ml/sam2");
        let (shared, local) = infer("~/work/ml/sam2", &found).unwrap();
        assert_eq!(shared, "~/work");
        assert_eq!(PathKey::parse(&local).unwrap().parts.last().unwrap(), "dev");
        // Stops one folder past a root.
        let (shared, _) = infer(r"D:\x", &d.path().join("x")).unwrap();
        assert_eq!(shared, "D:/x");
        // Nothing in common: the folders themselves.
        let (shared, local) = infer("/srv/api", &d.path().join("projects/backend")).unwrap();
        assert_eq!(shared, "/srv/api");
        assert!(local.ends_with("projects/backend"), "{local}");
        assert!(infer("rel/dir", d.path()).is_none());
    }

    #[test]
    fn find_here_falls_back_to_the_longest_mapping() {
        let d = tempfile::tempdir().unwrap();
        let here = d.path().join("here");
        for sub in ["dev/api", "dev/web", "special/web", "native"] {
            fs::create_dir_all(here.join(sub)).unwrap();
        }
        let p = |s: &str| here.join(s).display().to_string();
        let map = FolderMap::from_pairs([
            (r"D:\Work".to_string(), p("dev")),
            ("D:/work/web".to_string(), p("special/web")),
            (p("native"), p("dev")),
        ]);
        // A Windows path from another device, matched without regard to case or separators.
        let l = map.find_here(r"d:\WORK\api", d.path());
        assert!(
            matches!(&l, Located::Mapped { path, shared, .. } if *path == here.join("dev/api") && shared == r"D:\Work"),
            "{l:?}"
        );
        // The longest mapping wins.
        assert_eq!(map.find_here("D:/work/web", d.path()).path(), here.join("special/web"));
        // A folder that exists as written is used as written, mapping or not.
        assert_eq!(map.find_here(&p("native"), d.path()), Located::Here(here.join("native")));
        // Nothing there: missing, and a foreign path says which OS it's from.
        let missing = map.find_here(r"D:\work\gone", d.path());
        assert!(missing.is_missing());
        if cfg!(unix) {
            assert_eq!(missing.missing_reason().unwrap(), r"D:\work\gone is a Windows path, not on this device");
        }
        // Relative folders aren't mapped.
        assert_eq!(map.find_here("dev", &here), Located::Here(here.join("dev")));
    }

    #[test]
    fn add_replace_and_remove() {
        let d = tempfile::tempdir().unwrap();
        let paths = device(d.path());
        assert_eq!(FolderMap::add(&paths, "~/work/", "D:/dev").unwrap(), None);
        assert_eq!(
            FolderMap::add(&paths, r"~\work", "E:/dev").unwrap().as_deref(),
            Some("D:/dev"),
            "same folder, any spelling"
        );
        let text = fs::read_to_string(paths.folders_file()).unwrap();
        assert!(text.starts_with("# Where folders"), "{text}");
        assert!(text.contains("\"~/work\" = \"E:/dev\""), "{text}");
        assert_eq!(FolderMap::load(&paths).rules().len(), 1);
        assert!(FolderMap::add(&paths, "~", "D:/").is_err(), "too broad");
        assert!(FolderMap::remove(&paths, "~/work").unwrap());
        assert!(!FolderMap::remove(&paths, "~/work").unwrap());
        assert!(FolderMap::load(&paths).rules().is_empty());
    }

    #[test]
    fn locate_maps_a_root_and_fixes_its_neighbours() {
        let d = tempfile::tempdir().unwrap();
        let paths = device(d.path());
        let local = d.path().join("dev");
        for sub in ["sam2", "api", "elsewhere/x"] {
            fs::create_dir_all(local.join(sub)).unwrap();
        }
        let bm = |cwd: &str| crate::model::Bookmark {
            cwd: Some(cwd.into()),
            command: Some("ls".into()),
            ..Default::default()
        };
        crate::store::upsert(&paths.config_file, "train", &bm(r"Q:\work\sam2")).unwrap();
        crate::store::upsert(&paths.config_file, "api", &bm(r"Q:\work\api")).unwrap();
        crate::store::upsert(&paths.config_file, "other", &bm(r"Q:\else\y")).unwrap();
        let lib = Library::load(&paths, &[]);
        assert!(lib.find("api").unwrap().folder().unwrap().is_missing());

        // The suggestion finds `sam2` next to nothing yet (no working folders), so locate by hand.
        let out = locate(&paths, &lib, lib.find("train").unwrap(), &local.join("sam2")).unwrap();
        assert_eq!(out.shared, "Q:/work");
        assert_eq!(out.fixed, ["api"]);
        let lib = Library::load(&paths, &[]);
        assert_eq!(lib.find("api").unwrap().cwd().unwrap(), local.join("api"));
        assert!(matches!(lib.find("train").unwrap().folder(), Some(Located::Mapped { .. })));
        assert!(lib.find("other").unwrap().folder().unwrap().is_missing());
        let map = FolderMap::load(&paths);
        assert_eq!(map.used_by(&map.rules()[0], &lib), 2);

        // Now that folders under `dev` work here, a missing `x` is found next to them... and
        // `elsewhere/x` isn't (different parent).
        let s = Suggester::new(&lib, &[]);
        assert_eq!(s.suggest(r"Q:\else\sam2"), [local.join("sam2")]);
        assert!(s.suggest(r"Q:\else\x").is_empty());

        // A folder that's already here can't be located elsewhere.
        crate::store::upsert(&paths.config_file, "here", &bm(&local.display().to_string())).unwrap();
        let lib = Library::load(&paths, &[]);
        assert!(locate(&paths, &lib, lib.find("here").unwrap(), &local).unwrap_err().to_string().contains("already"));
    }
}
