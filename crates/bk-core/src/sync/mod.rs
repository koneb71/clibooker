//! Sync between devices through a Telegram bot.
//!
//! The chat with your bot holds one pinned, encrypted snapshot of everything that syncs:
//! global bookmarks, servers, saved SSH keys, settings (shared ones, plus each OS's own) and
//! run history.
//! `sync` downloads it, merges it with this device item by item (the newest change wins,
//! deletions included), writes what changed here, and uploads a new snapshot if this device
//! had anything newer. A saved key is the exception to "newest wins": sync never replaces a
//! different key saved under the same name, and never deletes one this device hasn't synced.
//!
//! Syncing is automatic unless turned off on a device: [`pending`] says (without the network)
//! whether this device has changes to send, and [`remote_head`] (one cheap call) whether
//! another device sent some.

pub mod crypto;
#[cfg(any(test, feature = "fake-telegram"))]
pub mod fake;
pub mod telegram;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::history::{self, RunRecord};
use crate::keys::{self, ApplyError};
use crate::model::{BookFile, Bookmark, Server, Settings};
use crate::paths::Paths;
use crate::store;
use crypto::Key;
use telegram::Bot;

pub const FILE_NAME: &str = "clibooker-sync.bin";
const FORMAT: u32 = 1;
/// Deletions are remembered this long so every device gets to see them.
const TOMBSTONE_MS: i64 = 90 * 24 * 3600 * 1000;
/// Runs kept in the snapshot (newest first).
const HISTORY_CAP: usize = 10_000;

/// Settings that mean the same on every OS.
const SHARED_SETTINGS: &[&str] = &["theme", "terminal_font_size", "history", "keep_running", "hidden_servers"];
/// Settings that only make sense per OS (a Windows shell or terminal isn't a Mac one).
const OS_SETTINGS: &[&str] = &["shell", "terminal", "ssh", "hotkey", "show_in_dock"];

fn os() -> &'static str {
    std::env::consts::OS
}

fn now_ms() -> i64 {
    history::now_ms()
}

// ---------- this device's sync config ----------

/// Kept in the data folder (never in the bookmarks file, which may be shared).
#[derive(Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    pub token: String,
    pub chat_id: i64,
    pub user_id: i64,
    pub bot_username: String,
    pub device: String,
    salt: String,
    key: String,
    /// Sync by itself (after changes here, and when another device has sent some).
    #[serde(default = "yes")]
    pub auto: bool,
}

fn yes() -> bool {
    true
}

impl std::fmt::Debug for SyncConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyncConfig")
            .field("bot", &self.bot_username)
            .field("device", &self.device)
            .field("auto", &self.auto)
            .finish()
    }
}

fn sync_dir(paths: &Paths) -> PathBuf {
    paths.data_dir.join("sync")
}

impl SyncConfig {
    pub fn load(paths: &Paths) -> Option<SyncConfig> {
        serde_json::from_str(&fs::read_to_string(sync_dir(paths).join("config.json")).ok()?).ok()
    }

    fn save(&self, paths: &Paths) -> Result<()> {
        store::write_private(&sync_dir(paths).join("config.json"), serde_json::to_string_pretty(self)?.as_bytes())
    }

    fn key(&self) -> Result<Key> {
        Key::from_base64(&self.salt, &self.key)
    }

    pub fn bot(&self) -> Bot {
        Bot::new(&self.token)
    }

    /// Turn automatic syncing on or off on this device.
    pub fn set_auto(paths: &Paths, on: bool) -> Result<SyncConfig> {
        let mut cfg = SyncConfig::load(paths).context("sync isn't set up on this device")?;
        cfg.auto = on;
        cfg.save(paths)?;
        Ok(cfg)
    }

    /// Stop syncing on this device: remove its config and state (the chat is left alone).
    pub fn forget(paths: &Paths) -> Result<()> {
        match fs::remove_dir_all(sync_dir(paths)) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
            _ => Ok(()),
        }
    }
}

/// `2026-10-14 09:30` in this machine's time zone.
pub fn local_time(ms: i64) -> String {
    jiff::Timestamp::from_millisecond(ms)
        .map(|t| t.to_zoned(jiff::tz::TimeZone::system()).strftime("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_default()
}

/// This device's name: the one it syncs as, else its host name.
pub fn this_device(paths: &Paths) -> String {
    SyncConfig::load(paths).map(|c| c.device).unwrap_or_else(default_device_name)
}

pub fn default_device_name() -> String {
    let h = gethostname::gethostname().to_string_lossy().into_owned();
    let h = h.trim_end_matches(".local").trim();
    if h.is_empty() { "this device".into() } else { h.to_string() }
}

// ---------- setup ----------

/// Step 3 of setup: check the passphrase against what's already synced (or start fresh),
/// then save this device's config.
pub fn finish_setup(
    paths: &Paths,
    token: &str,
    bot_username: &str,
    contact: &telegram::Contact,
    passphrase: &str,
    device: &str,
) -> Result<SyncConfig> {
    let bot = Bot::new(token);
    let existing = remote_file(&bot, contact.chat_id)?.map(|(_, bytes)| bytes);
    let key = match &existing {
        Some(bytes) => {
            let key = Key::derive(passphrase, Some(crypto::salt_of(bytes)?))?;
            crypto::decrypt(&key, bytes)?;
            key
        }
        None => Key::derive(passphrase, None)?,
    };
    let (salt, key) = key.to_base64();
    let device = if device.trim().is_empty() { default_device_name() } else { device.trim().to_string() };
    let cfg = SyncConfig {
        token: token.trim().to_string(),
        chat_id: contact.chat_id,
        user_id: contact.user_id,
        bot_username: bot_username.to_string(),
        device: device.clone(),
        salt,
        key,
        auto: true,
    };
    cfg.save(paths)?;
    // A fresh device starts with no sync state: everything here counts as changed now.
    let _ = fs::remove_file(sync_dir(paths).join("state.json"));
    let _ = bot.send_message(contact.chat_id, &format!("✓ {device} is syncing with Clibooker"));
    Ok(cfg)
}

/// Whether the chat already holds a snapshot (so setup asks for the existing passphrase).
pub fn has_remote(token: &str, chat_id: i64) -> Result<bool> {
    Ok(remote_message(&Bot::new(token), chat_id)?.is_some())
}

// ---------- snapshot model ----------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub updated_ms: i64,
    /// `None`: deleted (or, for a setting, unset).
    pub value: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub format: u32,
    pub device: String,
    pub os: String,
    pub pushed_ms: i64,
    /// The sender's global bookmarks file, to map history entries onto this device's.
    #[serde(default)]
    pub global_file: String,
    pub items: BTreeMap<String, Item>,
    #[serde(default)]
    pub history: Vec<RunRecord>,
    /// Every device that syncs, as of its last upload.
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceInfo {
    /// `macos`, `linux` or `windows`.
    pub os: String,
    /// When it last sent changes.
    pub last_change_ms: i64,
    /// Its Clibooker version.
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct ItemState {
    hash: String,
    updated_ms: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct State {
    pub last_message_id: Option<i64>,
    pub last_sync_ms: Option<i64>,
    pub last_summary: Option<String>,
    #[serde(default)]
    items: BTreeMap<String, ItemState>,
    /// The devices in the latest snapshot (for status displays without the network).
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceInfo>,
}

impl State {
    pub fn load(paths: &Paths) -> State {
        fs::read_to_string(sync_dir(paths).join("state.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    fn save(&self, paths: &Paths) -> Result<()> {
        store::write_private(&sync_dir(paths).join("state.json"), serde_json::to_string(self)?.as_bytes())
    }
}

/// What a sync did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Summary {
    /// Items changed on this device.
    pub received: usize,
    /// Items this device had newer (sent up).
    pub sent: usize,
    pub history_added: usize,
    pub uploaded: bool,
    pub from_device: Option<String>,
    /// Items that couldn't be applied here, with why.
    pub skipped: Vec<String>,
    /// Set when another device synced at the same moment.
    pub warning: Option<String>,
}

impl Summary {
    pub fn line(&self) -> String {
        let mut parts = Vec::new();
        if self.received > 0 {
            let from = self.from_device.as_deref().map(|d| format!(" from {d}")).unwrap_or_default();
            parts.push(format!("↓ {} updated{from}", self.received));
        }
        if self.sent > 0 {
            parts.push(format!("↑ {} sent", self.sent));
        }
        if self.history_added > 0 {
            parts.push(format!("history +{}", self.history_added));
        }
        if parts.is_empty() {
            parts.push(if self.uploaded { "↑ uploaded".into() } else { "already up to date".into() });
        }
        parts.join(" · ")
    }
}

fn hash(v: &Value) -> String {
    let digest = Sha256::digest(v.to_string().as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

const GONE: &str = "-";
/// A synced key that clashes with a different one saved here under its name: this device keeps
/// its own (`clash:` + its hash), and neither sends it nor counts it as deleted.
const CLASH: &str = "clash:";

/// What this device has is what it last synced (or kept, in a clash).
fn same(s: &ItemState, v: &Value) -> bool {
    let h = hash(v);
    s.hash == h || s.hash.strip_prefix(CLASH) == Some(h.as_str())
}

/// Keys this device owns: bookmarks, servers, saved keys, shared settings and this OS's settings.
fn is_mine(key: &str) -> bool {
    match key.split_once(':') {
        Some(("bookmark" | "server" | "key", _)) => true,
        Some(("setting", rest)) => match rest.split_once(':') {
            Some((o, k)) => o == os() && OS_SETTINGS.contains(&k),
            None => SHARED_SETTINGS.contains(&rest),
        },
        _ => false,
    }
}

/// Everything that syncs, as it is on this device right now.
fn local_values(paths: &Paths) -> Result<BTreeMap<String, Value>> {
    let file = if paths.config_file.exists() { store::read_file(&paths.config_file)? } else { BookFile::default() };
    let mut out = BTreeMap::new();
    for (name, b) in &file.bookmarks {
        out.insert(format!("bookmark:{name}"), serde_json::to_value(b)?);
    }
    for (name, s) in &file.servers {
        out.insert(format!("server:{name}"), serde_json::to_value(s)?);
    }
    if let Value::Object(settings) = serde_json::to_value(&file.settings)? {
        for (k, v) in settings {
            if SHARED_SETTINGS.contains(&k.as_str()) {
                out.insert(format!("setting:{k}"), v);
            } else if OS_SETTINGS.contains(&k.as_str()) {
                out.insert(format!("setting:{}:{k}", os()), v);
            }
        }
    }
    for (name, v) in keys::synced_values(paths)? {
        out.insert(format!("key:{name}"), v);
    }
    Ok(out)
}

fn mtime(path: &Path) -> Option<i64> {
    let t = fs::metadata(path).and_then(|m| m.modified()).ok()?;
    Some(t.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis() as i64)
}

fn modified_ms(path: &Path) -> i64 {
    mtime(path).unwrap_or_else(now_ms)
}

/// When an item last changed here, as near as the files tell: a saved key's own files (a new
/// passphrase rewrites them in place) or its folder (keys added, removed or renamed); anything
/// else, the bookmarks file.
fn changed_ms(paths: &Paths) -> impl Fn(&str) -> i64 {
    let config = modified_ms(&paths.config_file);
    let keys = keys::dir(paths);
    move |k: &str| match k.strip_prefix("key:") {
        Some(name) => [keys.join(name), keys.join(format!("{name}.pub")), keys.clone()]
            .iter()
            .filter_map(|p| mtime(p))
            .max()
            .unwrap_or_else(now_ms),
        None => config,
    }
}

/// This device's items with change times: unchanged since the last sync keeps its time;
/// changed (or deleted) gets the time its file was last saved.
fn local_items(
    values: &BTreeMap<String, Value>,
    state: &State,
    changed_ms: impl Fn(&str) -> i64,
) -> BTreeMap<String, Item> {
    let mut out = BTreeMap::new();
    for (k, v) in values {
        let updated_ms = match state.items.get(k) {
            Some(s) if same(s, v) => s.updated_ms,
            _ => changed_ms(k),
        };
        out.insert(k.clone(), Item { updated_ms, value: Some(v.clone()) });
    }
    for (k, s) in &state.items {
        // A clashing key that's gone here was never the synced one: nothing to delete.
        if values.contains_key(k) || !is_mine(k) || s.hash.starts_with(CLASH) {
            continue;
        }
        let updated_ms = if s.hash == GONE { s.updated_ms } else { changed_ms(k) };
        out.insert(k.clone(), Item { updated_ms, value: None });
    }
    out
}

/// Sync never deletes a key this device hasn't synced (one saved here before the deletion
/// arrived, a different key under that name, or one changed since): against a deletion, this
/// device's key takes the name back.
fn keep_unsynced_keys(merged: &mut BTreeMap<String, Item>, before: &BTreeMap<String, Value>, state: &State) {
    for (k, item) in merged.iter_mut().filter(|(k, _)| k.starts_with("key:")) {
        if item.value.is_none()
            && let Some(v) = before.get(k)
            && state.items.get(k).is_none_or(|s| s.hash != hash(v))
        {
            *item = Item { updated_ms: now_ms(), value: Some(v.clone()) };
        }
    }
}

/// Item by item, the newest change wins (ties go to what's already synced). Old deletions
/// are forgotten.
pub fn merge(local: &BTreeMap<String, Item>, remote: &BTreeMap<String, Item>, now: i64) -> BTreeMap<String, Item> {
    let keys: BTreeSet<&String> = local.keys().chain(remote.keys()).collect();
    keys.into_iter()
        .filter_map(|k| {
            let item = match (local.get(k), remote.get(k)) {
                (Some(l), Some(r)) => {
                    if l.updated_ms > r.updated_ms {
                        l
                    } else {
                        r
                    }
                }
                (Some(i), None) | (None, Some(i)) => i,
                (None, None) => return None,
            };
            let expired = item.value.is_none() && now - item.updated_ms > TOMBSTONE_MS;
            (!expired).then(|| (k.clone(), item.clone()))
        })
        .collect()
}

/// Write the merged values that differ from this device's. Returns how many changed here;
/// keys that clash with a different one here go in `clashes`.
fn apply(
    paths: &Paths,
    before: &BTreeMap<String, Value>,
    merged: &BTreeMap<String, Item>,
    skipped: &mut Vec<String>,
    clashes: &mut BTreeSet<String>,
) -> Result<usize> {
    let file = &paths.config_file;
    let mut changed = 0;
    let mut settings: Option<serde_json::Map<String, Value>> = None;
    for (k, item) in merged {
        if !is_mine(k) || before.get(k) == item.value.as_ref() {
            continue;
        }
        let (kind, name) = k.split_once(':').expect("item keys have a kind");
        let result: Result<()> = match (kind, &item.value) {
            ("bookmark", None) => store::remove(file, name).map(drop),
            ("bookmark", Some(v)) => serde_json::from_value::<Bookmark>(v.clone())
                .map_err(anyhow::Error::from)
                .and_then(|b| store::upsert(file, name, &b)),
            ("server", None) => store::remove_server(file, name).map(drop),
            ("server", Some(v)) => serde_json::from_value::<Server>(v.clone())
                .map_err(anyhow::Error::from)
                .and_then(|s| store::upsert_server(file, name, &s)),
            ("key", None) => keys::remove(paths, name).map(drop),
            ("key", Some(v)) => match keys::apply_synced(paths, name, v) {
                Ok(()) => Ok(()),
                Err(ApplyError::Clash) => {
                    clashes.insert(k.clone());
                    skipped.push(format!(
                        "key `{name}`: a different key has this name here; rename one of them (bk keys mv)"
                    ));
                    continue;
                }
                Err(ApplyError::Other(e)) => Err(e),
            },
            ("setting", value) => {
                let obj = match &mut settings {
                    Some(o) => o,
                    None => {
                        let current =
                            if file.exists() { store::read_file(file)?.settings } else { Settings::default() };
                        let Value::Object(o) = serde_json::to_value(current)? else { bail!("settings aren't a table") };
                        settings.insert(o)
                    }
                };
                let key = name.rsplit(':').next().unwrap_or(name);
                match value {
                    Some(v) => obj.insert(key.to_string(), v.clone()),
                    None => obj.remove(key),
                };
                Ok(())
            }
            _ => Ok(()),
        };
        match result {
            Ok(()) => changed += 1,
            Err(e) => skipped.push(format!("{k}: {e:#}")),
        }
    }
    if let Some(obj) = settings {
        // A setting this version doesn't understand (from a newer Clibooker) mustn't stop the rest.
        match serde_json::from_value::<Settings>(Value::Object(obj)) {
            Ok(s) => store::write_settings(file, &s)?,
            Err(e) => skipped.push(format!("settings: {e}")),
        }
    }
    Ok(changed)
}

/// How automatic syncing last went on this device (`sync/auto.json`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AutoState {
    pub last_attempt_ms: Option<i64>,
    /// Why the last attempt failed (cleared by a sync that works).
    pub last_error: Option<String>,
}

impl AutoState {
    pub fn load(paths: &Paths) -> AutoState {
        fs::read_to_string(sync_dir(paths).join("auto.json"))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Note an attempt (before it starts, so other processes don't start one too) or its outcome.
    pub fn record(paths: &Paths, error: Option<String>) -> Result<()> {
        let st = AutoState { last_attempt_ms: Some(now_ms()), last_error: error };
        fs::create_dir_all(sync_dir(paths))?;
        store::atomic_write(&sync_dir(paths).join("auto.json"), serde_json::to_string(&st)?.as_bytes())
    }

    /// The last automatic sync failed, and nothing has synced since.
    pub fn failing(&self, state: &State) -> Option<&str> {
        let after_last_sync = self.last_attempt_ms > state.last_sync_ms;
        self.last_error.as_deref().filter(|_| after_last_sync)
    }
}

fn run_key(r: &RunRecord) -> (String, i64) {
    (r.id.clone(), r.started_ms)
}

// ---------- talking to Telegram ----------

/// The pinned snapshot message, if the pin is one.
fn remote_message(bot: &Bot, chat_id: i64) -> Result<Option<(i64, String)>> {
    Ok(bot.pinned(chat_id)?.and_then(|m| {
        let doc = m.document?;
        (doc.file_name.as_deref() == Some(FILE_NAME)).then_some((m.message_id, doc.file_id))
    }))
}

fn remote_file(bot: &Bot, chat_id: i64) -> Result<Option<(i64, Vec<u8>)>> {
    match remote_message(bot, chat_id)? {
        Some((id, file_id)) => Ok(Some((id, bot.download(&file_id)?))),
        None => Ok(None),
    }
}

/// The pinned snapshot's sender and time, for status displays.
pub fn remote_info(cfg: &SyncConfig) -> Result<Option<(String, i64)>> {
    let bot = cfg.bot();
    let Some((_, bytes)) = remote_file(&bot, cfg.chat_id)? else { return Ok(None) };
    let snap: Snapshot = serde_json::from_slice(&crypto::decrypt(&cfg.key()?, &bytes)?)?;
    Ok(Some((snap.device, snap.pushed_ms)))
}

/// The id of the snapshot pinned in the chat: when it isn't the one this device last saw,
/// another device has sent changes. One `getChat` call, no download.
pub fn remote_head(cfg: &SyncConfig) -> Result<Option<i64>> {
    Ok(remote_message(&cfg.bot(), cfg.chat_id)?.map(|(id, _)| id))
}

/// What this device has to send, found without the network.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct Pending {
    /// Bookmarks, servers and settings changed (or deleted) since the last sync.
    pub items: usize,
    /// Runs recorded since the last sync.
    pub new_runs: bool,
}

pub fn pending(paths: &Paths) -> Result<Pending> {
    let state = State::load(paths);
    let values = local_values(paths)?;
    let changed = values.iter().filter(|(k, v)| state.items.get(*k).is_none_or(|s| !same(s, v))).count();
    let deleted = state
        .items
        .iter()
        .filter(|(k, s)| s.hash != GONE && !s.hash.starts_with(CLASH) && is_mine(k) && !values.contains_key(*k))
        .count();
    let history = paths.history_file();
    let new_runs = history.exists() && state.last_sync_ms.is_none_or(|t| modified_ms(&history) > t);
    Ok(Pending { items: changed + deleted, new_runs })
}

/// Saved keys whose name another device uses for a different key (as of the last sync).
pub fn key_clashes(paths: &Paths) -> BTreeSet<String> {
    let state = State::load(paths);
    state
        .items
        .iter()
        .filter(|(_, s)| s.hash.starts_with(CLASH))
        .filter_map(|(k, _)| k.strip_prefix("key:").map(String::from))
        .collect()
}

fn lock_file(paths: &Paths) -> Result<fs::File> {
    fs::create_dir_all(sync_dir(paths))?;
    Ok(fs::OpenOptions::new().create(true).truncate(false).write(true).open(sync_dir(paths).join("lock"))?)
}

/// One sync: download, merge, write here, upload if this device had anything newer.
pub fn sync(paths: &Paths, cfg: &SyncConfig) -> Result<Summary> {
    let lock = lock_file(paths)?;
    lock.lock().context("waiting for another sync on this device")?;
    sync_locked(paths, cfg)
}

/// Sync unless another sync on this device is under way (then `None`): for background syncs.
pub fn sync_if_idle(paths: &Paths, cfg: &SyncConfig) -> Result<Option<Summary>> {
    let lock = lock_file(paths)?;
    match lock.try_lock() {
        Ok(()) => sync_locked(paths, cfg).map(Some),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

fn sync_locked(paths: &Paths, cfg: &SyncConfig) -> Result<Summary> {
    let bot = cfg.bot();
    let key = cfg.key()?;
    let mut state = State::load(paths);
    let mut summary = Summary::default();

    let remote = match remote_file(&bot, cfg.chat_id)? {
        Some((id, bytes)) => {
            let plain = crypto::decrypt(&key, &bytes)?;
            let snap: Snapshot = serde_json::from_slice(&plain).context("reading the synced snapshot")?;
            if snap.format > FORMAT {
                bail!("the synced data is from a newer Clibooker; update this device first");
            }
            Some((id, snap))
        }
        None => None,
    };
    let empty = Snapshot::default();
    let remote_snap = remote.as_ref().map(|(_, s)| s).unwrap_or(&empty);
    summary.from_device = remote.as_ref().map(|(_, s)| s.device.clone()).filter(|d| *d != cfg.device);

    // Bookmarks, servers, keys, settings.
    let before = local_values(paths)?;
    let local = local_items(&before, &state, changed_ms(paths));
    let mut merged = merge(&local, &remote_snap.items, now_ms());
    keep_unsynced_keys(&mut merged, &before, &state);
    let mut clashes = BTreeSet::new();
    summary.received = apply(paths, &before, &merged, &mut summary.skipped, &mut clashes)?;
    summary.sent = merged.iter().filter(|(k, item)| remote_snap.items.get(*k) != Some(*item)).count();

    // Run history: a union, unless this device keeps no history.
    let settings =
        if paths.config_file.exists() { store::read_file(&paths.config_file)?.settings } else { Settings::default() };
    let history_on = settings.history_enabled();
    let history_file = paths.history_file();
    let local_runs = if history_on { history::read(&history_file, usize::MAX, None)? } else { Vec::new() };
    let local_keys: BTreeSet<(String, i64)> = local_runs.iter().map(run_key).collect();
    let here = paths.config_file.display().to_string();
    let mut incoming: Vec<RunRecord> = Vec::new();
    if history_on {
        for r in &remote_snap.history {
            if !local_keys.contains(&run_key(r)) {
                let mut r = r.clone();
                if !remote_snap.global_file.is_empty()
                    && let Some(name) = r.entry_id.strip_prefix(&format!("{}#", remote_snap.global_file))
                {
                    r.entry_id = format!("{here}#{name}");
                }
                incoming.push(r);
            }
        }
        incoming.sort_by_key(|r| r.started_ms);
        for r in &incoming {
            history::append(&history_file, r)?;
        }
    }
    summary.history_added = incoming.len();
    let mut all_runs: Vec<RunRecord> = local_runs;
    all_runs.extend(incoming);
    let remote_keys: BTreeSet<(String, i64)> = remote_snap.history.iter().map(run_key).collect();
    all_runs.extend(remote_snap.history.iter().filter(|r| !history_on && !local_keys.contains(&run_key(r))).cloned());
    all_runs.sort_by_key(|r| std::cmp::Reverse(r.started_ms));
    all_runs.dedup_by_key(|r| run_key(r));
    all_runs.truncate(HISTORY_CAP);
    let new_runs = all_runs.iter().any(|r| !remote_keys.contains(&run_key(r)));

    // Upload when this device had anything the snapshot lacks (itself included, as a device).
    let mut devices = remote_snap.devices.clone();
    if remote.is_none() || summary.sent > 0 || new_runs || !devices.contains_key(&cfg.device) {
        devices.insert(
            cfg.device.clone(),
            DeviceInfo { os: os().to_string(), last_change_ms: now_ms(), version: env!("CARGO_PKG_VERSION").into() },
        );
        let snap = Snapshot {
            format: FORMAT,
            device: cfg.device.clone(),
            os: os().to_string(),
            pushed_ms: now_ms(),
            global_file: here.clone(),
            items: merged.clone(),
            history: all_runs,
            devices: devices.clone(),
        };
        let sealed = crypto::encrypt(&key, &serde_json::to_vec(&snap)?)?;
        let time = jiff::Timestamp::now().strftime("%Y-%m-%d %H:%M UTC").to_string();
        let caption = format!("Clibooker sync · {} · {time}", cfg.device);
        let id = bot.send_document(cfg.chat_id, FILE_NAME, &caption, &sealed)?;
        bot.pin(cfg.chat_id, id)?;
        if let Some((old, _)) = &remote
            && *old != id
        {
            bot.retire(cfg.chat_id, *old);
        }
        if remote_message(&bot, cfg.chat_id)?.map(|(m, _)| m) != Some(id) {
            summary.warning = Some("another device synced at the same moment; run sync again".into());
        }
        summary.uploaded = true;
        state.last_message_id = Some(id);
    } else {
        state.last_message_id = remote.as_ref().map(|(id, _)| *id);
    }

    // Remember what this device now has, so the next sync can tell what changed.
    let after = local_values(paths)?;
    state.items = merged
        .iter()
        .filter(|(k, _)| is_mine(k))
        .map(|(k, item)| {
            let hash = match after.get(k) {
                Some(v) if clashes.contains(k) => format!("{CLASH}{}", hash(v)),
                Some(v) => hash(v),
                None => GONE.to_string(),
            };
            (k.clone(), ItemState { hash, updated_ms: item.updated_ms })
        })
        .collect();
    state.last_sync_ms = Some(now_ms());
    state.last_summary = Some(summary.line());
    state.devices = devices;
    state.save(paths)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn item(t: i64, v: Option<Value>) -> Item {
        Item { updated_ms: t, value: v }
    }

    #[test]
    fn newest_wins_both_ways_and_tombstones_expire() {
        let now = 1_000 * 24 * 3600 * 1000;
        let local: BTreeMap<String, Item> = [
            ("bookmark:a".to_string(), item(now - 10, Some(json!({"command": "local-a"})))),
            ("bookmark:b".to_string(), item(now - 50, Some(json!({"command": "local-b"})))),
            ("bookmark:gone".to_string(), item(now - 5, None)),
            ("bookmark:tie".to_string(), item(now - 7, Some(json!({"command": "local"})))),
        ]
        .into();
        let remote: BTreeMap<String, Item> = [
            ("bookmark:a".to_string(), item(now - 20, Some(json!({"command": "remote-a"})))),
            ("bookmark:b".to_string(), item(now - 40, Some(json!({"command": "remote-b"})))),
            ("bookmark:gone".to_string(), item(now - 30, Some(json!({"command": "old"})))),
            ("bookmark:tie".to_string(), item(now - 7, Some(json!({"command": "remote"})))),
            ("bookmark:ancient".to_string(), item(now - TOMBSTONE_MS - 1, None)),
            ("setting:windows:shell".to_string(), item(now - 1, Some(json!("pwsh")))),
        ]
        .into();
        let m = merge(&local, &remote, now);
        assert_eq!(m["bookmark:a"].value, Some(json!({"command": "local-a"})));
        assert_eq!(m["bookmark:b"].value, Some(json!({"command": "remote-b"})));
        assert_eq!(m["bookmark:gone"].value, None, "a newer local delete wins");
        assert_eq!(m["bookmark:tie"].value, Some(json!({"command": "remote"})), "ties keep what's synced");
        assert!(!m.contains_key("bookmark:ancient"), "old tombstones are forgotten");
        assert!(m.contains_key("setting:windows:shell"), "other OSes' settings are carried along");
    }

    #[test]
    fn change_detection_uses_hashes_and_save_time() {
        let mut state = State::default();
        state.items.insert("bookmark:same".into(), ItemState { hash: hash(&json!({"command": "x"})), updated_ms: 5 });
        state
            .items
            .insert("bookmark:edited".into(), ItemState { hash: hash(&json!({"command": "old"})), updated_ms: 5 });
        state
            .items
            .insert("bookmark:deleted".into(), ItemState { hash: hash(&json!({"command": "d"})), updated_ms: 5 });
        state.items.insert("bookmark:long-gone".into(), ItemState { hash: GONE.into(), updated_ms: 3 });
        let values: BTreeMap<String, Value> = [
            ("bookmark:same".to_string(), json!({"command": "x"})),
            ("bookmark:edited".to_string(), json!({"command": "new"})),
            ("bookmark:fresh".to_string(), json!({"command": "f"})),
        ]
        .into();
        let l = local_items(&values, &state, |_| 99);
        assert_eq!(l["bookmark:same"].updated_ms, 5);
        assert_eq!(l["bookmark:edited"].updated_ms, 99);
        assert_eq!(l["bookmark:fresh"].updated_ms, 99);
        assert_eq!(l["bookmark:deleted"], item(99, None));
        assert_eq!(l["bookmark:long-gone"], item(3, None));
    }

    #[test]
    fn keys_that_were_never_synced_survive_deletions() {
        let (v, kept, edited) = (json!({"private": "a"}), json!({"private": "b"}), json!({"private": "c"}));
        let before: BTreeMap<String, Value> = [
            ("key:fresh".to_string(), v.clone()),
            ("key:synced".to_string(), kept.clone()),
            ("key:edited".to_string(), edited.clone()),
            ("key:clash".to_string(), v.clone()),
        ]
        .into();
        let mut state = State::default();
        let st = |hash: String| ItemState { hash, updated_ms: 1 };
        state.items.insert("key:synced".into(), st(hash(&kept)));
        state.items.insert("key:edited".into(), st(hash(&json!({"private": "before"}))));
        state.items.insert("key:clash".into(), st(format!("{CLASH}{}", hash(&v))));
        let mut merged: BTreeMap<String, Item> = ["key:fresh", "key:synced", "key:edited", "key:clash", "bookmark:b"]
            .map(|k| (k.to_string(), item(5, None)))
            .into();
        keep_unsynced_keys(&mut merged, &before, &state);
        assert_eq!(merged["key:fresh"].value, Some(v.clone()), "saved here before the deletion arrived");
        assert_eq!(merged["key:synced"].value, None, "exactly what this device synced: deleted");
        assert_eq!(merged["key:edited"].value, Some(edited));
        assert_eq!(merged["key:clash"].value, Some(v.clone()), "the other key was deleted: this one takes the name");
        assert_eq!(merged["bookmark:b"].value, None);

        // A clashing key is this device's own: unchanged, and gone without a deletion to send.
        let l = local_items(&before, &state, |_| 99);
        assert_eq!(l["key:clash"].updated_ms, 1);
        let l = local_items(&BTreeMap::new(), &state, |_| 99);
        assert!(!l.contains_key("key:clash"));
        assert_eq!(l["key:synced"], item(99, None));
    }

    fn device(dir: &Path, name: &str) -> Paths {
        Paths { config_file: dir.join(name).join("bookmarks.toml"), data_dir: dir.join(name).join("data") }
    }

    fn command(p: &Paths, name: &str) -> Option<String> {
        store::read_file(&p.config_file).ok()?.bookmarks.get(name)?.command.clone()
    }

    fn put(p: &Paths, name: &str, cmd: &str) {
        std::thread::sleep(std::time::Duration::from_millis(30));
        store::upsert(&p.config_file, name, &Bookmark { command: Some(cmd.into()), ..Default::default() }).unwrap();
    }

    fn run(id: &str, t: i64) -> RunRecord {
        RunRecord {
            id: id.into(),
            started_ms: t,
            duration_ms: 1,
            name: "hello".into(),
            entry_id: "x#hello".into(),
            source: "cli".into(),
            cwd: "~".into(),
            env: "inherit".into(),
            params: Default::default(),
            commands: vec!["echo".into()],
            exit_code: Some(0),
            server: None,
            device: None,
        }
    }

    /// Two devices syncing through the fake Bot API: setup, encryption, merging both ways,
    /// newest-wins on a conflict, deletions, history.
    #[test]
    fn two_devices_end_to_end() {
        let tg = fake::FakeTelegram::start();
        // SAFETY: only this test reads BK_TELEGRAM_API.
        unsafe { std::env::set_var("BK_TELEGRAM_API", tg.url()) };
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (device(d.path(), "a"), device(d.path(), "b"));
        let bot = Bot::new(fake::TOKEN);
        assert_eq!(bot.get_me().unwrap().username, "clibooker_test_bot");
        assert!(Bot::new("999:nope").get_me().unwrap_err().to_string().contains("bot token"));

        // Device A: a bookmark, a server, a setting, a run; first sync uploads.
        put(&a, "hello", "echo hi");
        store::upsert_server(&a.config_file, "gpu", &Server { host: "10.0.0.5".into(), ..Default::default() }).unwrap();
        store::write_settings(&a.config_file, &Settings { theme: Some("dark".into()), ..Default::default() }).unwrap();
        history::append(&a.history_file(), &run("r1", 100)).unwrap();
        tg.press_start();
        let contact = bot.wait_for_contact(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!(contact.chat_id, fake::CHAT_ID);
        let ca = finish_setup(&a, fake::TOKEN, "clibooker_test_bot", &contact, "correct horse", "A").unwrap();
        assert!(ca.auto, "syncs by itself unless turned off");
        assert!(pending(&a).unwrap().items > 0 && pending(&a).unwrap().new_runs, "never synced: everything is pending");
        let s = sync(&a, &ca).unwrap();
        assert!(s.uploaded && s.received == 0, "{s:?}");
        assert_eq!(pending(&a).unwrap(), Pending::default(), "a sync's own writes aren't changes");
        assert_eq!(remote_head(&ca).unwrap(), State::load(&a).last_message_id);
        let blob = tg.pinned_bytes().unwrap();
        assert!(blob.starts_with(b"CLBSYNC1") && !blob.windows(7).any(|w| w == b"echo hi"), "stored encrypted");

        // Device B: a wrong passphrase is refused; then it gets A's things and sends its own.
        put(&b, "other", "make");
        tg.press_start();
        let contact = bot.wait_for_contact(std::time::Duration::from_secs(5)).unwrap();
        let err = finish_setup(&b, fake::TOKEN, "clibooker_test_bot", &contact, "wrong horse!", "B").unwrap_err();
        assert!(err.to_string().contains("wrong passphrase"), "{err}");
        let cb = finish_setup(&b, fake::TOKEN, "clibooker_test_bot", &contact, "correct horse", "B").unwrap();
        let s = sync(&b, &cb).unwrap();
        assert_eq!(command(&b, "hello").as_deref(), Some("echo hi"));
        assert_eq!(store::read_file(&b.config_file).unwrap().servers["gpu"].host, "10.0.0.5");
        assert_eq!(store::read_file(&b.config_file).unwrap().settings.theme.as_deref(), Some("dark"));
        assert_eq!(history::read(&b.history_file(), 10, None).unwrap()[0].id, "r1");
        assert!(s.uploaded && s.from_device.as_deref() == Some("A"), "{s:?}");
        assert_eq!(tg.documents(), 1, "the older snapshot was retired");
        assert_ne!(remote_head(&ca).unwrap(), State::load(&a).last_message_id, "A can tell B sent something");
        assert_eq!(sync(&a, &ca).unwrap().received, 1);
        let devices = State::load(&a).devices;
        assert_eq!(devices.keys().collect::<Vec<_>>(), ["A", "B"]);
        assert_eq!(devices["B"].os, os());
        assert_eq!(command(&a, "other").as_deref(), Some("make"));
        assert_eq!(sync(&a, &ca).unwrap().line(), "already up to date");

        // A busy device skips a background sync instead of waiting.
        {
            let held = lock_file(&a).unwrap();
            held.lock().unwrap();
            assert!(sync_if_idle(&a, &ca).unwrap().is_none());
        }
        assert!(sync_if_idle(&a, &ca).unwrap().is_some());

        // Per-OS commands travel with the bookmark.
        let clean = Bookmark {
            command: Some("rm -rf build".into()),
            windows: Some(crate::model::OsVariant {
                command: Some("Remove-Item -Recurse build".into()),
                ..Default::default()
            }),
            ..Default::default()
        };
        std::thread::sleep(std::time::Duration::from_millis(30));
        store::upsert(&a.config_file, "clean", &clean).unwrap();
        assert_eq!(pending(&a).unwrap().items, 1);
        sync(&a, &ca).unwrap();
        sync(&b, &cb).unwrap();
        assert_eq!(store::read_file(&b.config_file).unwrap().bookmarks["clean"], clean);

        // Different bookmarks edited on each device: both edits survive.
        put(&a, "hello", "echo hello A");
        put(&b, "other", "make B");
        sync(&a, &ca).unwrap();
        sync(&b, &cb).unwrap();
        sync(&a, &ca).unwrap();
        for p in [&a, &b] {
            assert_eq!(command(p, "hello").as_deref(), Some("echo hello A"));
            assert_eq!(command(p, "other").as_deref(), Some("make B"));
        }

        // The same bookmark edited on both: the newer edit (B's) wins everywhere.
        put(&a, "hello", "older edit");
        put(&b, "hello", "newer edit");
        sync(&a, &ca).unwrap();
        sync(&b, &cb).unwrap();
        sync(&a, &ca).unwrap();
        assert_eq!(command(&a, "hello").as_deref(), Some("newer edit"));
        assert_eq!(command(&b, "hello").as_deref(), Some("newer edit"));

        // A deletion syncs too.
        std::thread::sleep(std::time::Duration::from_millis(30));
        store::remove(&a.config_file, "other").unwrap();
        sync(&a, &ca).unwrap();
        sync(&b, &cb).unwrap();
        assert_eq!(command(&b, "other"), None);

        // Saved keys travel, kept private, and a server that uses one comes along.
        let fp = |p: &Paths, name: &str| keys::info(p, name).map(|k| k.fingerprint).ok();
        std::thread::sleep(std::time::Duration::from_millis(30));
        keys::import(&a, "lab", &keys::fake::key(1), None, "ssh-keygen").unwrap();
        let lab = Server { host: "lab.example".into(), identity: Some("key:lab".into()), ..Default::default() };
        store::upsert_server(&a.config_file, "lab", &lab).unwrap();
        assert_eq!(pending(&a).unwrap().items, 2);
        sync(&a, &ca).unwrap();
        let s = sync(&b, &cb).unwrap();
        assert_eq!(s.received, 2, "{s:?}");
        assert!(fp(&b, "lab").is_some() && fp(&b, "lab") == fp(&a, "lab"));
        assert_eq!(store::read_file(&b.config_file).unwrap().servers["lab"], lab);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(keys::dir(&b).join("lab")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert_eq!(pending(&b).unwrap().items, 0);

        // A different key under a name the other device uses: neither device loses its own.
        keys::import(&a, "dup", &keys::fake::key(2), None, "ssh-keygen").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        keys::import(&b, "dup", &keys::fake::key(3), None, "ssh-keygen").unwrap();
        let (a_dup, b_dup) = (fp(&a, "dup"), fp(&b, "dup"));
        sync(&a, &ca).unwrap();
        sync(&b, &cb).unwrap();
        let s = sync(&a, &ca).unwrap();
        assert!(s.skipped.iter().any(|m| m.contains("key `dup`") && m.contains("rename")), "{s:?}");
        assert_eq!((fp(&a, "dup"), fp(&b, "dup")), (a_dup.clone(), b_dup.clone()));
        assert_eq!(key_clashes(&a), BTreeSet::from(["dup".to_string()]));
        assert_eq!(pending(&a).unwrap().items, 0, "a clash isn't a change to send");
        assert!(!sync(&a, &ca).unwrap().uploaded, "nor does it upload again");
        // Once A's own is gone, A gets B's.
        keys::remove(&a, "dup").unwrap();
        assert_eq!(pending(&a).unwrap().items, 0, "deleting the clashing key deletes nothing elsewhere");
        sync(&a, &ca).unwrap();
        assert_eq!(fp(&a, "dup"), b_dup);
        assert!(key_clashes(&a).is_empty());

        // Deleting a synced key deletes it everywhere.
        std::thread::sleep(std::time::Duration::from_millis(30));
        keys::remove(&b, "lab").unwrap();
        sync(&b, &cb).unwrap();
        sync(&a, &ca).unwrap();
        assert!(!keys::exists(&a, "lab"));

        // Runs from both devices end up everywhere, newest first.
        history::append(&b.history_file(), &run("r2", 200)).unwrap();
        sync(&b, &cb).unwrap();
        assert_eq!(sync(&a, &ca).unwrap().history_added, 1);
        let ids: Vec<String> = history::read(&a.history_file(), 10, None).unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["r2", "r1"]);
        assert!(tg.texts().iter().any(|t| t.contains("B is syncing")));
    }

    #[test]
    fn older_configs_sync_automatically() {
        let old = r#"{"token":"1:x","chat_id":1,"user_id":1,"bot_username":"b","device":"d","salt":"s","key":"k"}"#;
        assert!(serde_json::from_str::<SyncConfig>(old).unwrap().auto);
        let st = State { last_sync_ms: Some(10), ..Default::default() };
        let failed = AutoState { last_attempt_ms: Some(20), last_error: Some("offline".into()) };
        assert_eq!(failed.failing(&st), Some("offline"));
        let since = State { last_sync_ms: Some(30), ..Default::default() };
        assert_eq!(failed.failing(&since), None, "a sync since then worked");
    }

    #[test]
    fn settings_split_by_os() {
        assert!(is_mine("setting:theme"));
        assert!(is_mine("key:lab"));
        assert!(is_mine(&format!("setting:{}:shell", os())));
        assert!(!is_mine("setting:plan9:shell"));
        assert!(!is_mine("setting:launch_at_login"));
        assert!(!is_mine(&format!("setting:{}:theme", os())), "shared settings aren't per OS");
    }
}
