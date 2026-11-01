pub mod detect;
pub mod plan;
pub mod rar;
pub mod sevenz;
pub mod tarlike;
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
    /// The format does not say without decoding it (BZ2, XZ): `size` means nothing. Shown as a dash.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub size_unknown: bool,
    /// Compressed bytes across all volumes. `None` for a RAR folder, and where the format
    /// has no per-entry size: 7z and the TAR family.
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
    /// Canonical paths no entry may write over, whatever the policy: the sources of the run.
    #[serde(default)]
    pub protected: Vec<PathBuf>,
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
    /// Files with content that a password opened and verified. Only this proves a password:
    /// skipped files and folders never touched the encrypted data.
    pub decrypted: u64,
}

/// What is left of a run a password stopped: files already written still count, so the
/// report is kept and says why it ended. Any other error, or a run with nothing out, stands.
pub fn stopped_by(mut report: OpReport, archive: &Path, error: ArchiveError) -> Result<OpReport, ArchiveError> {
    if report.ok > 0 && matches!(error, ArchiveError::WrongPassword | ArchiveError::PasswordRequired) {
        report.failed.push(FileFailure { path: archive.to_string_lossy().into_owned(), error });
        Ok(report)
    } else {
        Err(error)
    }
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

/// Opens the engine for the archive at `path`.
pub fn open_engine(path: &Path) -> Result<Box<dyn ArchiveEngine>, ArchiveError> {
    match detect_format(path)? {
        Format::Rar => Ok(Box::new(rar::RarEngine::new(path))),
        Format::Zip => Ok(Box::new(zip::ZipEngine::new(path))),
        Format::SevenZ => Ok(Box::new(sevenz::SevenZEngine::new(path))),
        format @ (Format::Tar
        | Format::TarGz
        | Format::TarBz2
        | Format::TarXz
        | Format::Gz
        | Format::Bz2
        | Format::Xz) => Ok(Box::new(tarlike::TarLikeEngine::new(path, format))),
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

/// A file's modification time on disk, in Unix seconds.
#[cfg(test)]
pub fn disk_mtime(path: &Path) -> i64 {
    let t = std::fs::metadata(path).unwrap().modified().unwrap();
    t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

/// Counts `cancelled()` polls and cancels once `cancel_at` of them were made.
#[cfg(test)]
pub struct PollCtl {
    pub calls: std::cell::Cell<u64>,
    pub cancel_at: u64,
}

#[cfg(test)]
impl JobCtl for PollCtl {
    fn progress(&mut self, _current: &str, _done_bytes: u64, _total_bytes: u64) {}
    fn cancelled(&self) -> bool {
        self.calls.set(self.calls.get() + 1);
        self.calls.get() >= self.cancel_at
    }
    fn resolve_conflict(&mut self, _path: &Path) -> ConflictChoice {
        ConflictChoice::Overwrite
    }
}
