//! Watch bookmark files and data files so edits from the CLI (or an editor) show up live.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tauri::{AppHandle, Emitter};

const LIBRARY: u8 = 1;
const HISTORY: u8 = 2;
const INBOX: u8 = 4;

pub struct FileWatcher {
    watcher: Mutex<RecommendedWatcher>,
    watched: Mutex<HashSet<PathBuf>>,
}

impl FileWatcher {
    pub fn start(app: AppHandle, config_file: PathBuf, data_dir: PathBuf) -> notify::Result<Arc<Self>> {
        let pending = Arc::new(AtomicU8::new(0));
        let flags = pending.clone();
        let cfg = config_file.clone();
        let data = data_dir.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let Ok(ev) = res else { return };
            if matches!(ev.kind, EventKind::Access(_)) {
                return;
            }
            for p in &ev.paths {
                flags.fetch_or(classify(p, &cfg, &data), Ordering::SeqCst);
            }
        })?;

        // Debounce: editors and atomic renames produce bursts of events.
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(150));
                let f = pending.swap(0, Ordering::SeqCst);
                if f & LIBRARY != 0 {
                    let _ = app.emit("library-changed", ());
                }
                if f & HISTORY != 0 {
                    let _ = app.emit("history-changed", ());
                }
                if f & INBOX != 0 {
                    let _ = app.emit("inbox-changed", ());
                }
            }
        });

        let fw = Arc::new(FileWatcher { watcher: Mutex::new(watcher), watched: Mutex::new(HashSet::new()) });
        if let Some(dir) = config_file.parent() {
            let _ = std::fs::create_dir_all(dir);
            fw.watch_dir(dir);
        }
        let _ = std::fs::create_dir_all(&data_dir);
        fw.watch_dir(&data_dir);
        Ok(fw)
    }

    /// Watch a directory (non-recursively); safe to call repeatedly.
    pub fn watch_dir(&self, dir: &Path) {
        let mut watched = self.watched.lock().expect("watched lock");
        if watched.contains(dir) {
            return;
        }
        if self.watcher.lock().expect("watcher lock").watch(dir, RecursiveMode::NonRecursive).is_ok() {
            watched.insert(dir.to_path_buf());
        }
    }

    pub fn watch_projects(&self, files: &[PathBuf]) {
        for f in files {
            if let Some(d) = f.parent() {
                self.watch_dir(d);
            }
        }
    }
}

fn classify(p: &Path, config_file: &Path, data_dir: &Path) -> u8 {
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if p.starts_with(data_dir) {
        return match name {
            "history.jsonl" => HISTORY,
            "recent.log" => INBOX,
            "projects.json" | "trust.json" => LIBRARY,
            _ => 0,
        };
    }
    let same_dir_as_config = p.parent() == config_file.parent();
    if name == ".bk.toml" || (same_dir_as_config && name.ends_with(".toml")) || p == config_file {
        return LIBRARY;
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_paths() {
        let cfg = Path::new("/h/.config/bk/bookmarks.toml");
        let data = Path::new("/h/.local/share/bk");
        assert_eq!(classify(cfg, cfg, data), LIBRARY);
        assert_eq!(classify(Path::new("/h/.config/bk/.tmpXYZ.toml"), cfg, data), LIBRARY);
        assert_eq!(classify(Path::new("/w/p/.bk.toml"), cfg, data), LIBRARY);
        assert_eq!(classify(Path::new("/h/.local/share/bk/history.jsonl"), cfg, data), HISTORY);
        assert_eq!(classify(Path::new("/h/.local/share/bk/recent.log"), cfg, data), INBOX);
        assert_eq!(classify(Path::new("/w/p/README.md"), cfg, data), 0);
    }
}
