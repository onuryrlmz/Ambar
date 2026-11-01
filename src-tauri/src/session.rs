//! Per-run state: confirmed passwords (memory only) and temp folders for opened entries.

use crate::engine::plan::{normalize_entry_path, output_rel, safe_dest, Selection};
use crate::engine::{
    ArchiveEngine, ArchiveInfo, ConflictChoice, Entry, ExtractRequest, JobCtl, OpReport,
    OverwritePolicy, Password,
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

    /// Call only once the password is confirmed: encrypted headers opened with it, or an
    /// encrypted entry was processed with it (see `ConfirmEngine`).
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

    /// Removes one folder made by `new_temp_dir` (refuses anything outside this run).
    pub fn remove_temp_dir(&self, dir: &Path) {
        if dir.starts_with(&self.0.run_dir) && dir != self.0.run_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
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

/// True when a run proves `pw` right: no wrong-password failure, and at least one
/// selected encrypted entry went through without failing.
pub fn confirms_password(entries: &[Entry], selection: &[String], report: &OpReport) -> bool {
    if report.ok == 0
        || report
            .failed
            .iter()
            .any(|f| matches!(f.error, ArchiveError::WrongPassword | ArchiveError::PasswordRequired))
    {
        return false;
    }
    let selection = Selection::new(selection);
    entries
        .iter()
        .filter(|e| !e.is_dir && e.encrypted && selection.has(&e.path))
        .any(|e| !report.failed.iter().any(|f| f.path == e.path))
}

/// Wraps an engine so a password given by the user is cached once a run proves it
/// right. Listing alone cannot (plain-header archives list without a password).
pub struct ConfirmEngine {
    inner: Box<dyn ArchiveEngine>,
    session: Session,
    archive: PathBuf,
    given: Option<Password>,
    listed: Option<(ArchiveInfo, Vec<Entry>)>,
}

impl ConfirmEngine {
    pub fn new(
        inner: Box<dyn ArchiveEngine>,
        session: Session,
        archive: PathBuf,
        given: Option<Password>,
    ) -> Self {
        Self { inner, session, archive, given, listed: None }
    }

    fn listing(&mut self, pw: Option<&Password>) -> Result<&(ArchiveInfo, Vec<Entry>), ArchiveError> {
        if self.listed.is_none() {
            self.listed = Some(self.inner.list(pw)?);
        }
        Ok(self.listed.as_ref().unwrap())
    }

    fn confirm(&mut self, pw: Option<&Password>, selection: &[String], r: &OpReport) {
        let Some(given) = self.given.clone() else { return };
        if let Ok((_, entries)) = self.listing(pw) {
            if confirms_password(entries, selection, r) {
                self.session.remember_password(&self.archive, given);
            }
        }
    }
}

impl ArchiveEngine for ConfirmEngine {
    fn list(&mut self, pw: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
        Ok(self.listing(pw)?.clone())
    }

    fn extract(
        &mut self,
        pw: Option<&Password>,
        req: &ExtractRequest,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError> {
        let r = self.inner.extract(pw, req, ctl)?;
        self.confirm(pw, &req.selection, &r);
        Ok(r)
    }

    fn test(&mut self, pw: Option<&Password>, ctl: &mut dyn JobCtl) -> Result<OpReport, ArchiveError> {
        let r = self.inner.test(pw, ctl)?;
        self.confirm(pw, &[], &r);
        Ok(r)
    }
}

/// Extracts one file entry into `temp` through the engine's normal extract path and
/// returns where it landed. Folders are refused before anything is extracted.
pub fn extract_entry_to_temp(
    engine: &mut dyn ArchiveEngine,
    pw: Option<&Password>,
    temp: &Path,
    entry: &str,
) -> Result<PathBuf, ArchiveError> {
    let entry = normalize_entry_path(entry).trim_end_matches('/').to_string();
    let (_, entries) = engine.list(pw)?;
    match entries.iter().find(|e| e.path == entry) {
        None => return Err(ArchiveError::Io("entry not found".into())),
        Some(e) if e.is_dir => return Err(ArchiveError::Io("entry is a folder".into())),
        Some(_) => {}
    }
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
        for (i, mode) in [Mode::Ok, Mode::Failed, Mode::Panic, Mode::DiskFull].into_iter().enumerate() {
            jobs.start(format!("j{i}"), sink.clone(), engine(mode), pw.clone(), JobOp::Test).unwrap();
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
            let mk = |p: &str, d: bool| Entry {
                path: p.into(), is_dir: d, size: 1, packed: None, modified: None, crc: None, encrypted: false,
            };
            Ok((
                ArchiveInfo {
                    format: crate::engine::Format::Zip, file_size: 0, encrypted_headers: false,
                    solid: false, multi_volume: false, has_recovery: false, locked: false, comment: None,
                },
                vec![mk("docs/sub/a.txt", false), mk("a.txt", false), mk("docs", true)],
            ))
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
        let e = extract_entry_to_temp(&mut WritingEngine { fail: false }, None, d.path(), "docs/").unwrap_err();
        assert_eq!(e.detail(), "entry is a folder");
        let e = extract_entry_to_temp(&mut WritingEngine { fail: false }, None, d.path(), "nope.txt").unwrap_err();
        assert_eq!(e.detail(), "entry not found");
        assert!(std::fs::read_dir(d.path()).unwrap().next().is_none());
    }

    // --- real ZIP data ---
    use std::io::Write;

    fn aes_zip(path: &Path, name: &str, pw: &str) {
        use ::zip::write::SimpleFileOptions;
        let mut w = ::zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let o = SimpleFileOptions::default().compression_method(::zip::CompressionMethod::Stored);
        w.start_file(name, o.with_aes_encryption(::zip::AesMode::Aes256, pw)).unwrap();
        w.write_all(b"secret").unwrap();
        w.start_file("plain.txt", o).unwrap();
        w.write_all(b"open").unwrap();
        w.finish().unwrap();
    }

    fn run_job(sess: &Session, zip: &Path, pw: Option<&str>, sel: &[&str]) -> serde_json::Value {
        let out = zip.parent().unwrap().join(format!("out-{}", sel.len()));
        std::fs::create_dir_all(&out).unwrap();
        let sink = FakeSink::new();
        let jobs = Jobs::default();
        let (s2, p2, given) = (sess.clone(), zip.to_path_buf(), wrap_password(pw.map(String::from)));
        let eff = given.clone();
        jobs.start(
            "z".into(),
            sink.clone(),
            move || {
                let e = crate::engine::open_engine(&p2)?;
                Ok(Box::new(ConfirmEngine::new(e, s2, p2, given)) as Box<dyn ArchiveEngine>)
            },
            eff,
            JobOp::Extract(ExtractRequest {
                dest: out,
                selection: sel.iter().map(|s| s.to_string()).collect(),
                base: String::new(),
                keep_paths: true,
                overwrite: OverwritePolicy::Overwrite,
                keep_broken: false,
            }),
        )
        .unwrap();
        sink.wait_done()
    }

    #[test]
    fn password_given_at_extract_time_is_cached_only_when_proven() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        aes_zip(&z, "gizli.txt", "doğru");
        let s = Session::new(d.path().join(TEMP_DIR_NAME));
        // Wrong password: nothing cached.
        let r = run_job(&s, &z, Some("yanlış"), &["gizli.txt"]);
        assert_eq!(r["error"]["code"], "wrong_password");
        assert!(s.resolve_password(&z, None).is_none());
        // Right password, encrypted entry extracted: cached.
        let r = run_job(&s, &z, Some("doğru"), &["gizli.txt"]);
        assert_eq!(r["report"]["ok"], 1);
        assert_eq!(s.resolve_password(&z, None).unwrap().as_str(), "doğru");
        s.forget_password(&z);
        // Only the plain entry selected: the password proved nothing.
        let r = run_job(&s, &z, Some("herhangi"), &["plain.txt"]);
        assert_eq!(r["report"]["ok"], 1);
        assert!(s.resolve_password(&z, None).is_none());
    }

    #[test]
    fn open_entry_path_caches_password_and_matches_nested_turkish_name() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("t.zip");
        aes_zip(&z, "Klasör/İç Klasör/çalışma şifreli.txt", "doğru");
        let s = Session::new(d.path().join(TEMP_DIR_NAME));
        let temp = s.new_temp_dir().unwrap();
        let given = wrap_password(Some("doğru".into()));
        let mut eng = ConfirmEngine::new(crate::engine::open_engine(&z).unwrap(), s.clone(), z.clone(), given.clone());
        let out = extract_entry_to_temp(&mut eng, given.as_ref(), &temp, "Klasör/İç Klasör/çalışma şifreli.txt").unwrap();
        assert_eq!(out, temp.join("çalışma şifreli.txt"));
        assert_eq!(std::fs::read(&out).unwrap(), b"secret");
        assert_eq!(s.resolve_password(&z, None).unwrap().as_str(), "doğru");
    }

    #[test]
    fn failed_open_entry_temp_dir_can_be_removed() {
        let d = tempfile::tempdir().unwrap();
        let s = Session::new(d.path().join(TEMP_DIR_NAME));
        let t = s.new_temp_dir().unwrap();
        s.remove_temp_dir(&t);
        assert!(!t.exists());
        let outside = d.path().join("keep");
        std::fs::create_dir(&outside).unwrap();
        s.remove_temp_dir(&outside);
        assert!(outside.exists());
    }
}
