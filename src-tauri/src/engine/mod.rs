pub mod detect;
pub mod plan;
pub mod rar;
pub mod zip;

use crate::error::ArchiveError;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub use detect::{detect_format, Format};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub packed: Option<u64>,
    pub modified: Option<i64>,
    pub crc: Option<u32>,
    pub encrypted: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveInfo {
    pub format: Format,
    pub file_size: u64,
    pub encrypted_headers: bool,
    pub solid: bool,
    pub multi_volume: bool,
    pub has_recovery: bool,
    pub locked: bool,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OverwritePolicy {
    Ask,
    Overwrite,
    Skip,
    Rename,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ConflictChoice {
    Overwrite,
    Skip,
    Rename,
    Cancel,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractRequest {
    pub dest: PathBuf,
    pub selection: Vec<String>,
    pub base: String,
    pub keep_paths: bool,
    pub overwrite: OverwritePolicy,
    pub keep_broken: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileFailure {
    pub path: String,
    pub error: ArchiveError,
}

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpReport {
    pub ok: u64,
    pub skipped: u64,
    pub failed: Vec<FileFailure>,
}

pub trait JobCtl: Send {
    fn progress(&mut self, current: &str, done_bytes: u64, total_bytes: u64);
    fn cancelled(&self) -> bool;
    fn resolve_conflict(&mut self, path: &Path) -> ConflictChoice;
}

pub type Password = zeroize::Zeroizing<String>;

pub trait ArchiveEngine: Send {
    fn list(&mut self, pw: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError>;
    fn extract(
        &mut self,
        pw: Option<&Password>,
        req: &ExtractRequest,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError>;
    fn test(
        &mut self,
        pw: Option<&Password>,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError>;
}

/// Opens the engine for the archive at `path`. Engines for the other formats are
/// wired in later tasks.
pub fn open_engine(path: &Path) -> Result<Box<dyn ArchiveEngine>, ArchiveError> {
    match detect_format(path)? {
        Format::Rar => Ok(Box::new(rar::RarEngine::new(path))),
        Format::Zip => Ok(Box::new(zip::ZipEngine::new(path))),
        format => Err(ArchiveError::Unsupported(format!("{format:?}"))),
    }
}

/// Shared `JobCtl` test double for engine tests.
#[cfg(test)]
pub struct TestCtl {
    pub cancel_after: Option<usize>,
    pub choice: ConflictChoice,
    pub seen: Vec<String>,
}

#[cfg(test)]
impl JobCtl for TestCtl {
    fn progress(&mut self, current: &str, _done_bytes: u64, _total_bytes: u64) {
        self.seen.push(current.to_string());
    }
    fn cancelled(&self) -> bool {
        self.cancel_after.is_some_and(|n| self.seen.len() >= n)
    }
    fn resolve_conflict(&mut self, _path: &Path) -> ConflictChoice {
        self.choice
    }
}
