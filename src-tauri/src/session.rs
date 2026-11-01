//! Per-run state: confirmed passwords (memory only) and temp folders for opened entries.

use crate::engine::plan::{normalize_entry_path, output_rel, safe_dest};
use crate::engine::{
    ArchiveEngine, ConflictChoice, ExtractRequest, JobCtl, OverwritePolicy, Password,
};
use crate::error::ArchiveError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use zeroize::Zeroizing;

/// Name of the only folder (inside the app cache) this module ever deletes from.
pub const TEMP_DIR_NAME: &str = "ambar-temp";
const STALE_AFTER: Duration = Duration::from_secs(24 * 3600);

struct Inner {
    passwords: Mutex<HashMap<PathBuf, Password>>,
    temp_root: PathBuf,
    /// This run's own folder, `<temp_root>/s<pid>-<nanos>`.
    run_dir: PathBuf,
    counter: AtomicU64,
}

#[derive(Clone)]
pub struct Session(Arc<Inner>);

fn key(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

impl Session {
    /// `temp_root` must be the app's own `ambar-temp` folder.
    pub fn new(temp_root: PathBuf) -> Self {
        let nanos = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let run_dir = temp_root.join(format!("s{}-{nanos}", std::process::id()));
        Session(Arc::new(Inner {
            passwords: Mutex::new(HashMap::new()),
            temp_root,
            run_dir,
            counter: AtomicU64::new(0),
        }))
    }

    /// The given password wins; otherwise the confirmed cached one.
    pub fn resolve_password(&self, path: &Path, given: Option<Password>) -> Option<Password> {
        given.or_else(|| {
            self.0
                .passwords
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&key(path))
                .cloned()
        })
    }

    /// Call only after the password was confirmed by a successful open.
    pub fn remember_password(&self, path: &Path, pw: Password) {
        self.0
            .passwords
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key(path), pw);
    }

    pub fn forget_password(&self, path: &Path) {
        self.0
            .passwords
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&key(path));
    }

    /// A fresh, private, empty folder for one `open_entry` call.
    pub fn new_temp_dir(&self) -> Result<PathBuf, ArchiveError> {
        let n = self.0.counter.fetch_add(1, Ordering::Relaxed) + 1;
        let dir = self.0.run_dir.join(format!("e{n}"));
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(&dir)?;
        Ok(dir)
    }

    /// Removes this run's temp folder.
    pub fn cleanup(&self) {
        let _ = std::fs::remove_dir_all(&self.0.run_dir);
    }

    /// Startup: removes old run folders left by earlier (crashed) runs. Touches only
    /// real directories directly inside a folder named `ambar-temp`.
    pub fn cleanup_stale(&self) {
        if self.0.temp_root.file_name().is_none_or(|n| n != TEMP_DIR_NAME) {
            return;
        }
        let Ok(rd) = std::fs::read_dir(&self.0.temp_root) else { return };
        for item in rd.flatten() {
            let p = item.path();
            if p == self.0.run_dir {
                continue;
            }
            let Ok(m) = std::fs::symlink_metadata(&p) else { continue };
            if !m.is_dir() {
                continue;
            }
            let old = m
                .modified()
                .ok()
                .and_then(|t| SystemTime::now().duration_since(t).ok())
                .is_some_and(|age| age >= STALE_AFTER);
            if old {
                let _ = std::fs::remove_dir_all(&p);
            }
        }
    }
}

struct QuietCtl;
impl JobCtl for QuietCtl {
    fn progress(&mut self, _: &str, _: u64, _: u64) {}
    fn cancelled(&self) -> bool {
        false
    }
    fn resolve_conflict(&mut self, _: &Path) -> ConflictChoice {
        ConflictChoice::Overwrite
    }
}

/// Extracts one entry into `temp` through the engine's normal extract path and returns
/// where the file landed. The path comes from the same planning the engine uses.
pub fn extract_entry_to_temp(
    engine: &mut dyn ArchiveEngine,
    pw: Option<&Password>,
    temp: &Path,
    entry: &str,
) -> Result<PathBuf, ArchiveError> {
    let entry = normalize_entry_path(entry);
    let req = ExtractRequest {
        dest: temp.to_path_buf(),
        selection: vec![entry.clone()],
        base: String::new(),
        keep_paths: false,
        overwrite: OverwritePolicy::Overwrite,
        keep_broken: false,
    };
    let report = engine.extract(pw, &req, &mut QuietCtl)?;
    if let Some(f) = report.failed.into_iter().next() {
        return Err(f.error);
    }
    let rel = output_rel(&entry, &req).ok_or_else(|| ArchiveError::UnsafePath(entry.clone()))?;
    let out = safe_dest(temp, &rel)?;
    match std::fs::symlink_metadata(&out) {
        Ok(m) if m.is_file() => Ok(out),
        _ => Err(ArchiveError::Io("entry was not extracted".into())),
    }
}

/// Helper so callers never hold a plain `String` password longer than needed.
pub fn wrap_password(p: Option<String>) -> Option<Password> {
    p.map(Zeroizing::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::plan::{plan_entry, PlanOutcome};
    use crate::engine::{ArchiveInfo, Entry, OpReport};
    use crate::jobs::test_support::*;
    use crate::jobs::{JobOp, Jobs};

    #[test]
    fn password_cached_per_path_and_forgotten() {
        let d = tempfile::tempdir().unwrap();
        let s = Session::new(d.path().join(TEMP_DIR_NAME));
        let a = d.path().join("a.rar");
        let b = d.path().join("b.rar");
        assert!(s.resolve_password(&a, None).is_none());
        s.remember_password(&a, Zeroizing::new("pw-a".into()));
        assert_eq!(s.resolve_password(&a, None).unwrap().as_str(), "pw-a");
        assert!(s.resolve_password(&b, None).is_none());
        let given = s.resolve_password(&a, Some(Zeroizing::new("other".into())));
        assert_eq!(given.unwrap().as_str(), "other");
        s.forget_password(&a);
        assert!(s.resolve_password(&a, None).is_none());
    }

    #[test]
    fn serialized_events_contain_no_password() {
        let sink = FakeSink::new();
        let jobs = Jobs::default();
        let pw = wrap_password(Some("Sifre-Gizli-9".into()));
        for mode in [Mode::Ok, Mode::Failed, Mode::Panic, Mode::DiskFull] {
            jobs.start(sink.clone(), engine(mode), pw.clone(), JobOp::Test).unwrap();
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while sink.events.lock().unwrap().iter().filter(|e| e.0 == "done").count() < 4 {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!sink.all_json().contains("Sifre-Gizli-9"));
    }

    #[test]
    fn temp_dir_removed_on_cleanup() {
        let d = tempfile::tempdir().unwrap();
        let s = Session::new(d.path().join(TEMP_DIR_NAME));
        let t1 = s.new_temp_dir().unwrap();
        let t2 = s.new_temp_dir().unwrap();
        assert_ne!(t1, t2);
        std::fs::write(t1.join("f"), b"x").unwrap();
        s.cleanup();
        assert!(!t1.exists() && !t2.exists());
    }

    #[test]
    fn stale_cleanup_only_touches_old_dirs_in_ambar_temp() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join(TEMP_DIR_NAME);
        std::fs::create_dir_all(root.join("old")).unwrap();
        std::fs::create_dir_all(root.join("fresh")).unwrap();
        let old = std::fs::File::open(root.join("old")).unwrap();
        old.set_modified(SystemTime::now() - Duration::from_secs(48 * 3600)).unwrap();
        Session::new(root.clone()).cleanup_stale();
        assert!(!root.join("old").exists());
        assert!(root.join("fresh").exists());
        // Wrong folder name: refuse to delete anything.
        let other = d.path().join("other");
        std::fs::create_dir_all(other.join("old")).unwrap();
        std::fs::File::open(other.join("old"))
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(48 * 3600))
            .unwrap();
        Session::new(other.clone()).cleanup_stale();
        assert!(other.join("old").exists());
    }

    struct WritingEngine {
        fail: bool,
    }
    impl ArchiveEngine for WritingEngine {
        fn list(&mut self, _: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
            Err(ArchiveError::Unsupported("x".into()))
        }
        fn extract(
            &mut self,
            _: Option<&Password>,
            req: &ExtractRequest,
            ctl: &mut dyn JobCtl,
        ) -> Result<OpReport, ArchiveError> {
            let e = Entry {
                path: req.selection[0].clone(),
                is_dir: false,
                size: 1,
                packed: None,
                modified: None,
                crc: None,
                encrypted: false,
            };
            let mut report = OpReport::default();
            if self.fail {
                report.failed.push(crate::engine::FileFailure {
                    path: e.path,
                    error: ArchiveError::WrongPassword,
                });
                return Ok(report);
            }
            if let PlanOutcome::Write(p) = plan_entry(&e, req, ctl)? {
                std::fs::write(p, b"data")?;
                report.ok = 1;
            }
            Ok(report)
        }
        fn test(&mut self, _: Option<&Password>, _: &mut dyn JobCtl) -> Result<OpReport, ArchiveError> {
            Ok(OpReport::default())
        }
    }

    #[test]
    fn entry_extracts_to_planned_path_under_temp() {
        let d = tempfile::tempdir().unwrap();
        let out = extract_entry_to_temp(&mut WritingEngine { fail: false }, None, d.path(), "docs/sub/a.txt").unwrap();
        assert_eq!(out, d.path().join("a.txt"));
        assert_eq!(std::fs::read(&out).unwrap(), b"data");
    }

    #[test]
    fn partial_report_is_an_error_and_traversal_is_refused() {
        let d = tempfile::tempdir().unwrap();
        let e = extract_entry_to_temp(&mut WritingEngine { fail: true }, None, d.path(), "a.txt").unwrap_err();
        assert_eq!(e.code(), "wrong_password");
        let e = extract_entry_to_temp(&mut WritingEngine { fail: false }, None, d.path(), "a/..").unwrap_err();
        assert!(matches!(e.code(), "unsafe_path" | "io"), "{}", e.code());
    }
}
