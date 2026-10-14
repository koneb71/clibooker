//! Append-only run log (`history.jsonl`). Never stores environment values.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;
use crate::plan::RunPlan;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunRecord {
    pub id: String,
    /// Start time, unix milliseconds.
    pub started_ms: i64,
    pub duration_ms: u64,
    pub name: String,
    pub entry_id: String,
    /// `cli` or `app`.
    pub source: String,
    pub cwd: String,
    pub env: String,
    #[serde(default)]
    pub params: BTreeMap<String, String>,
    pub commands: Vec<String>,
    /// `None` when the run was still going / killed without a status.
    pub exit_code: Option<i32>,
    /// Server it ran on (remote runs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    /// The device it ran on (runs sync between devices). `None` in older records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

impl RunRecord {
    pub fn from_plan(plan: &RunPlan, source: &str, started_ms: i64) -> Self {
        RunRecord {
            id: format!("{started_ms:x}-{:x}", std::process::id()),
            started_ms,
            duration_ms: 0,
            name: plan.name.clone(),
            entry_id: plan.entry_id.clone(),
            source: source.into(),
            cwd: paths::contract_tilde(&plan.cwd),
            env: plan.env_label.clone(),
            params: plan.params.clone(),
            commands: plan.steps.iter().filter(|s| !s.synthetic).map(|s| s.command.clone()).collect(),
            exit_code: None,
            server: None,
            device: None,
        }
    }

    pub fn from_remote(plan: &crate::remote::RemotePlan, source: &str, started_ms: i64) -> Self {
        RunRecord {
            id: format!("{started_ms:x}-{:x}", std::process::id()),
            started_ms,
            duration_ms: 0,
            name: plan.name.clone(),
            entry_id: plan.entry_id.clone(),
            source: source.into(),
            cwd: plan.cwd.clone(),
            env: plan.env_label.clone(),
            params: plan.params.clone(),
            commands: plan.steps.iter().filter(|s| !s.synthetic).map(|s| s.command.clone()).collect(),
            exit_code: None,
            server: Some(plan.target.name.clone()),
            device: None,
        }
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Append one record as a single write so concurrent writers don't interleave.
pub fn append(path: &Path, rec: &RunRecord) -> Result<()> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    let mut line = serde_json::to_string(rec)?;
    line.push('\n');
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).with_context(|| format!("opening {}", path.display()))?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// Newest first. Unparseable lines are skipped.
pub fn read(path: &Path, limit: usize, name: Option<&str>) -> Result<Vec<RunRecord>> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    // Newest first by start time (synced runs from other devices are appended out of order).
    let mut records: Vec<RunRecord> = text
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<RunRecord>(l).ok())
        .filter(|r| name.is_none_or(|n| r.name == n))
        .collect();
    records.sort_by_key(|r| std::cmp::Reverse(r.started_ms));
    records.truncate(limit);
    Ok(records)
}

/// Most recent record for a bookmark (to pre-fill params).
pub fn last_for(path: &Path, entry_id: &str) -> Option<RunRecord> {
    read(path, usize::MAX, None).ok()?.into_iter().find(|r| r.entry_id == entry_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str, t: i64) -> RunRecord {
        RunRecord {
            id: t.to_string(),
            started_ms: t,
            duration_ms: 5,
            name: name.into(),
            entry_id: format!("g#{name}"),
            source: "cli".into(),
            cwd: "~".into(),
            env: "inherit".into(),
            params: BTreeMap::new(),
            commands: vec!["ls".into()],
            exit_code: Some(0),
            server: None,
            device: None,
        }
    }

    #[test]
    fn older_records_have_no_device() {
        let old = r#"{"id":"1","started_ms":1,"duration_ms":0,"name":"a","entry_id":"g#a","source":"cli","cwd":"~","env":"inherit","commands":["ls"],"exit_code":0}"#;
        let r: RunRecord = serde_json::from_str(old).unwrap();
        assert_eq!(r.device, None);
        assert!(!serde_json::to_string(&r).unwrap().contains("device"), "nothing new is written for it");
    }

    #[test]
    fn append_and_read_newest_first() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("sub/h.jsonl");
        append(&p, &rec("a", 1)).unwrap();
        append(&p, &rec("b", 2)).unwrap();
        fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"garbage\n").unwrap();
        append(&p, &rec("a", 3)).unwrap();
        let all = read(&p, 10, None).unwrap();
        assert_eq!(all.iter().map(|r| r.started_ms).collect::<Vec<_>>(), vec![3, 2, 1]);
        assert_eq!(read(&p, 10, Some("a")).unwrap().len(), 2);
        assert_eq!(last_for(&p, "g#b").unwrap().started_ms, 2);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        }
    }
}
