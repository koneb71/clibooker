//! Inbox of recently executed shell commands, written by the opt-in zsh hook
//! (`bk init zsh --record`). One record per command:
//!
//! `ts \x1f status \x1f pwd \x1f conda_env \x1f virtual_env \x1f command \x1e \n`
//!
//! The hook writes with zsh builtins only, so separators are ASCII unit/record separators.

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::model::EnvSpec;
use crate::paths;

const FIELD: char = '\x1f';
const RECORD: char = '\x1e';
/// Keep the file bounded; trimmed when it grows past this many records.
pub const MAX_RECORDS: usize = 500;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecentCommand {
    /// Unix seconds.
    pub ts: i64,
    pub status: i32,
    pub cwd: String,
    pub env: Option<EnvSpec>,
    pub command: String,
}

pub fn parse(text: &str) -> Vec<RecentCommand> {
    text.split(RECORD)
        .filter_map(|rec| {
            let rec = rec.trim_start_matches('\n');
            if rec.is_empty() {
                return None;
            }
            let mut f = rec.splitn(6, FIELD);
            let ts = f.next()?.trim().parse().ok()?;
            let status = f.next()?.trim().parse().unwrap_or(0);
            let cwd = f.next()?.to_string();
            let conda = f.next()?.trim().to_string();
            let venv = f.next()?.trim().to_string();
            let command = f.next()?.trim_end_matches('\n').to_string();
            if command.trim().is_empty() {
                return None;
            }
            let env = if !venv.is_empty() {
                let v = Path::new(&venv);
                let rel = v.strip_prefix(&cwd).map(paths::portable).unwrap_or_else(|_| paths::contract_tilde(v));
                Some(EnvSpec::Venv { path: rel })
            } else if !conda.is_empty() && conda != "base" {
                Some(EnvSpec::Conda { name: Some(conda), prefix: None })
            } else {
                None
            };
            Some(RecentCommand { ts, status, cwd: paths::contract_tilde(Path::new(&cwd)), env, command })
        })
        .collect()
}

/// Newest first, with consecutive duplicates (same command + folder) collapsed.
pub fn read(path: &Path, limit: usize) -> Result<Vec<RecentCommand>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let mut out: Vec<RecentCommand> = Vec::new();
    for r in parse(&text).into_iter().rev() {
        if out.iter().any(|o| o.command == r.command && o.cwd == r.cwd) {
            continue;
        }
        out.push(r);
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

/// Drop old records once the file grows past `MAX_RECORDS`.
pub fn trim(path: &Path) -> Result<()> {
    let Ok(text) = fs::read_to_string(path) else { return Ok(()) };
    let recs: Vec<&str> = text.split(RECORD).filter(|r| !r.trim().is_empty()).collect();
    if recs.len() <= MAX_RECORDS {
        return Ok(());
    }
    let keep = &recs[recs.len() - MAX_RECORDS..];
    let mut out = String::new();
    for r in keep {
        out.push_str(r.trim_start_matches('\n'));
        out.push(RECORD);
        out.push('\n');
    }
    crate::store::atomic_write(path, out.as_bytes())
}

pub fn clear(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(ts: i64, status: i32, pwd: &str, conda: &str, venv: &str, cmd: &str) -> String {
        format!("{ts}\x1f{status}\x1f{pwd}\x1f{conda}\x1f{venv}\x1f{cmd}\x1e\n")
    }

    #[test]
    fn parses_and_dedupes() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("recent.log");
        let text = [
            rec(1, 0, "/w/a", "sam2", "", "python train.py\n  --epochs 3"),
            rec(2, 1, "/w/b", "base", "/w/b/.venv", "pytest -x"),
            rec(3, 0, "/w/a", "sam2", "", "python train.py\n  --epochs 3"),
            rec(4, 0, "/w/a", "", "", "   "),
        ]
        .concat();
        fs::write(&p, text).unwrap();
        let r = read(&p, 10).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].ts, 3);
        assert_eq!(r[0].command, "python train.py\n  --epochs 3");
        assert_eq!(r[0].env, Some(EnvSpec::Conda { name: Some("sam2".into()), prefix: None }));
        assert_eq!(r[1].env, Some(EnvSpec::Venv { path: ".venv".into() }));
        assert_eq!(r[1].status, 1);
    }

    #[test]
    fn trims_to_max() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("recent.log");
        let text: String =
            (0..MAX_RECORDS + 20).map(|i| rec(i as i64, 0, "/w", "", "", &format!("echo {i}"))).collect();
        fs::write(&p, text).unwrap();
        trim(&p).unwrap();
        let all = parse(&fs::read_to_string(&p).unwrap());
        assert_eq!(all.len(), MAX_RECORDS);
        assert_eq!(all[0].ts, 20);
    }
}
