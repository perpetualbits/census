//! Rollback journal: every committed write is recorded as an LDIF change record on
//! disk (replayable with `ldapmodify`) and, when reversible, pushed onto an in-app
//! undo stack. An in-memory copy of the recent records also feeds the preview tile.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::overlay::Action;

/// A compensating action plus a human label, held on the undo stack.
pub struct UndoStep {
    pub label: String,
    pub inverse: Action,
}

/// The session journal. Opens its file lazily on first write, so a read-only or
/// no-op session never creates one.
pub struct Journal {
    path: PathBuf,
    file: Option<File>,
    pub undo: Vec<UndoStep>,
    /// Recent LDIF records (newest last), for the change-preview tile.
    pub log: Vec<String>,
}

impl Journal {
    pub fn new() -> Self {
        let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let path = state_dir().join(format!("journal-{secs}.ldif"));
        Self { path, file: None, undo: Vec::new(), log: Vec::new() }
    }

    /// Append one record (a `# header` line followed by an LDIF block) to the log
    /// and the on-disk file. Returns the path on the first successful write so the
    /// caller can tell the operator where the journal lives.
    pub fn record(&mut self, header: &str, ldif: &str) -> std::io::Result<()> {
        let block = format!("# {header}\n{}\n", ldif.trim_end());
        self.log.push(block.clone());
        if self.log.len() > 500 {
            self.log.remove(0);
        }
        if self.file.is_none() {
            if let Some(parent) = self.path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            self.file = Some(OpenOptions::new().create(true).append(true).open(&self.path)?);
        }
        let f = self.file.as_mut().expect("just opened");
        f.write_all(block.as_bytes())?;
        f.flush()
    }

    /// Push a record to the in-memory log only (no file) — used in dry-run to
    /// preview the LDIF that *would* be written without touching disk.
    pub fn note(&mut self, header: &str, ldif: &str) {
        self.log.push(format!("# {header}\n{}\n", ldif.trim_end()));
        if self.log.len() > 500 {
            self.log.remove(0);
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// `$XDG_STATE_HOME/census` (falling back to `~/.local/state/census`).
fn state_dir() -> PathBuf {
    std::env::var("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("census")
}
