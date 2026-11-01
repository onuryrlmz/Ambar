//! Tauri commands. Everything that needs a Tauri runtime lives here; the logic it calls
//! is tested without one.

use crate::engine::plan::normalize_entry_path;
use crate::engine::{
    open_engine, ArchiveEngine, ArchiveInfo, ConflictChoice, Entry, ExtractRequest, Password,
};
use crate::error::ArchiveError;
use crate::jobs::{
    internal_error, validate_dest, ConflictEvent, DoneEvent, EventSink, JobOp, Jobs, ProgressEvent,
};
use crate::local_fs::{self, LocalEntry};
use crate::open_guard;
use crate::session::{extract_entry_to_temp, wrap_password, ConfirmEngine, Session};
use serde::Serialize;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, State};
use tauri_plugin_opener::OpenerExt;

struct TauriSink(AppHandle);

impl EventSink for TauriSink {
    // A closed window must never panic a job thread: ignore emit errors.
    fn progress(&self, ev: &ProgressEvent) {
        let _ = self.0.emit("job-progress", ev);
    }
    fn conflict(&self, ev: &ConflictEvent) {
        let _ = self.0.emit("job-conflict", ev);
    }
    fn done(&self, ev: &DoneEvent) {
        let _ = self.0.emit("job-done", ev);
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenResult {
    pub info: ArchiveInfo,
    pub entries: Vec<Entry>,
}

/// Lists an archive. The password is cached only when the archive has encrypted
/// headers, since only then does a successful list prove it (other formats list
/// without reading any encrypted data). A panic in the engine becomes a generic error.
pub fn open_archive_blocking<F>(
    session: &Session,
    path: &Path,
    password: Option<Password>,
    make_engine: F,
) -> Result<OpenResult, ArchiveError>
where
    F: FnOnce(&Path) -> Result<Box<dyn ArchiveEngine>, ArchiveError>,
{
    let pw = session.resolve_password(path, password);
    let (info, entries) = catch_unwind(AssertUnwindSafe(|| {
        let mut engine = make_engine(path)?;
        engine.list(pw.as_ref())
    }))
    .unwrap_or_else(|_| Err(internal_error()))?;
    if info.encrypted_headers {
        if let Some(pw) = pw {
            session.remember_password(path, pw);
        }
    }
    Ok(OpenResult { info, entries })
}

/// Runs blocking work off the command thread; a join failure is a generic error.
async fn blocking<T, F>(f: F) -> Result<T, ArchiveError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, ArchiveError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .unwrap_or_else(|_| Err(internal_error()))
}

#[tauri::command]
pub async fn open_archive(
    session: State<'_, Session>,
    path: String,
    password: Option<String>,
) -> Result<OpenResult, ArchiveError> {
    let session = session.inner().clone();
    let pw = wrap_password(password);
    blocking(move || open_archive_blocking(&session, Path::new(&path), pw, open_engine)).await
}

#[allow(clippy::too_many_arguments)]
fn start_job(
    app: AppHandle,
    jobs: Jobs,
    session: Session,
    id: String,
    path: String,
    password: Option<String>,
    op: JobOp,
) -> Result<(), ArchiveError> {
    if let JobOp::Extract(req) = &op {
        validate_dest(&req.dest)?;
    }
    let p = PathBuf::from(path);
    let given = wrap_password(password);
    let pw = session.resolve_password(&p, given.clone());
    jobs.start(
        id,
        Arc::new(TauriSink(app)),
        move || {
            let engine = open_engine(&p)?;
            Ok(Box::new(ConfirmEngine::new(engine, session, p, given)) as Box<dyn ArchiveEngine>)
        },
        pw,
        op,
    )
}

#[tauri::command]
pub async fn start_extract(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    session: State<'_, Session>,
    id: String,
    path: String,
    password: Option<String>,
    req: ExtractRequest,
) -> Result<(), ArchiveError> {
    let (jobs, session) = (jobs.inner().clone(), session.inner().clone());
    blocking(move || start_job(app, jobs, session, id, path, password, JobOp::Extract(req))).await
}

#[tauri::command]
pub async fn start_test(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    session: State<'_, Session>,
    id: String,
    path: String,
    password: Option<String>,
) -> Result<(), ArchiveError> {
    let (jobs, session) = (jobs.inner().clone(), session.inner().clone());
    blocking(move || start_job(app, jobs, session, id, path, password, JobOp::Test)).await
}

#[tauri::command]
pub fn cancel_job(jobs: State<'_, Jobs>, id: String) {
    jobs.cancel(&id);
}

#[tauri::command]
pub fn answer_conflict(jobs: State<'_, Jobs>, id: String, choice: ConflictChoice) {
    jobs.answer_conflict(&id, choice);
}

#[tauri::command]
pub async fn forget_password(session: State<'_, Session>, path: String) -> Result<(), ArchiveError> {
    let session = session.inner().clone();
    blocking(move || {
        session.forget_password(Path::new(&path));
        Ok(())
    })
    .await
}

#[tauri::command]
pub async fn list_local(path: String) -> Result<Vec<LocalEntry>, ArchiveError> {
    blocking(move || local_fs::list_local(Path::new(&path))).await
}

#[tauri::command]
pub fn home_dir(app: AppHandle) -> Result<String, ArchiveError> {
    app.path()
        .home_dir()
        .map(|p| p.to_string_lossy().into_owned())
        .map_err(|_| ArchiveError::Io("home folder unavailable".into()))
}

#[tauri::command]
pub fn parent_dir(path: String) -> Option<String> {
    local_fs::parent_dir(Path::new(&path))
}

/// Starts `qlmanage -p` without a shell and reaps it on its own thread.
#[cfg(target_os = "macos")]
fn quick_look(file: &Path) -> Result<(), ArchiveError> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("/usr/bin/qlmanage")
        .arg("-p")
        .arg(file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Extracts the entry into a fresh temp folder and hardens it. On any failure the
/// folder is removed again.
fn prepare_entry(
    session: &Session,
    archive: &Path,
    entry: &str,
    given: Option<Password>,
    mark: bool,
) -> Result<PathBuf, ArchiveError> {
    let pw = session.resolve_password(archive, given.clone());
    let temp = session.new_temp_dir()?;
    let res = catch_unwind(AssertUnwindSafe(|| {
        let engine = open_engine(archive)?;
        let mut engine =
            ConfirmEngine::new(engine, session.clone(), archive.to_path_buf(), given);
        let file = extract_entry_to_temp(&mut engine, pw.as_ref(), &temp, entry)?;
        open_guard::harden(&file, mark)?;
        Ok(file)
    }))
    .unwrap_or_else(|_| Err(internal_error()));
    if res.is_err() {
        session.remove_temp_dir(&temp);
    }
    res
}

#[tauri::command]
pub async fn open_entry(
    app: AppHandle,
    session: State<'_, Session>,
    path: String,
    entry: String,
    password: Option<String>,
    quick_look: bool,
    allow_risky: bool,
) -> Result<(), ArchiveError> {
    if quick_look && !cfg!(target_os = "macos") {
        return Err(ArchiveError::Unsupported("quick look".into()));
    }
    let name = normalize_entry_path(&entry);
    let name = name.rsplit('/').next().unwrap_or("").to_string();
    // Quick Look never launches the file, so only a real open is guarded.
    if !quick_look {
        open_guard::check_risky(&name, allow_risky)?;
    }
    let session = session.inner().clone();
    let pw = wrap_password(password);
    let file = blocking(move || {
        prepare_entry(&session, Path::new(&path), &entry, pw, !quick_look)
    })
    .await?;
    #[cfg(target_os = "macos")]
    if quick_look {
        return self::quick_look(&file);
    }
    app.opener()
        .open_path(file.to_string_lossy(), None::<&str>)
        .map_err(|e| ArchiveError::Io(e.to_string()))
}

/// Opens a file from the local pane with its default app. The check and the open both
/// happen here, so the frontend needs no opener permission for arbitrary paths.
#[tauri::command]
pub async fn open_local(app: AppHandle, path: String, allow_risky: bool) -> Result<(), ArchiveError> {
    // Off the main thread: a dead network share can hang these checks for a long time.
    blocking(move || {
        open_guard::check_local_open(Path::new(&path), allow_risky)?;
        app.opener()
            .open_path(path, None::<&str>)
            .map_err(|e| ArchiveError::Io(e.to_string()))
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn aes_zip(path: &Path) {
        use ::zip::write::SimpleFileOptions;
        let mut w = ::zip::ZipWriter::new(std::fs::File::create(path).unwrap());
        let o = SimpleFileOptions::default().compression_method(::zip::CompressionMethod::Stored);
        w.start_file("g.txt", o.with_aes_encryption(::zip::AesMode::Aes256, "doğru")).unwrap();
        w.write_all(b"secret").unwrap();
        w.finish().unwrap();
    }

    #[test]
    fn open_panic_is_error_and_failed_open_caches_nothing() {
        let d = tempfile::tempdir().unwrap();
        let s = Session::new(d.path().join("ambar-temp"));
        let p = d.path().join("a.zip");
        let pw = wrap_password(Some("Sifre-Gizli-9".into()));
        let r = open_archive_blocking(&s, &p, pw.clone(), |_| -> Result<Box<dyn ArchiveEngine>, _> {
            panic!("boom Sifre-Gizli-9")
        });
        let e = r.err().unwrap();
        assert_eq!(e.code(), "io");
        assert!(!e.detail().contains("Sifre"));
        assert!(s.resolve_password(&p, None).is_none());
    }

    #[test]
    fn open_archive_does_not_cache_unverified_zip_password() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        aes_zip(&z);
        let s = Session::new(d.path().join("ambar-temp"));
        // ZIP lists without reading any encrypted data: even a wrong password "opens".
        let r = open_archive_blocking(&s, &z, wrap_password(Some("yanlış".into())), open_engine).unwrap();
        assert!(!r.info.encrypted_headers);
        assert!(s.resolve_password(&z, None).is_none());
    }

    struct HeaderEngine(bool);
    impl ArchiveEngine for HeaderEngine {
        fn list(&mut self, _: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
            Ok((
                ArchiveInfo {
                    format: crate::engine::Format::Rar, file_size: 0, encrypted_headers: self.0,
                    solid: false, multi_volume: false, has_recovery: false, locked: false, comment: None,
                },
                vec![],
            ))
        }
        fn extract(&mut self, _: Option<&Password>, _: &ExtractRequest, _: &mut dyn crate::engine::JobCtl)
            -> Result<crate::engine::OpReport, ArchiveError> { unreachable!() }
        fn test(&mut self, _: Option<&Password>, _: &mut dyn crate::engine::JobCtl)
            -> Result<crate::engine::OpReport, ArchiveError> { unreachable!() }
    }

    #[test]
    fn open_archive_caches_password_when_headers_are_encrypted() {
        let d = tempfile::tempdir().unwrap();
        let s = Session::new(d.path().join("ambar-temp"));
        let p = d.path().join("h.rar");
        open_archive_blocking(&s, &p, wrap_password(Some("pw".into())), |_| {
            Ok(Box::new(HeaderEngine(true)) as Box<dyn ArchiveEngine>)
        })
        .unwrap();
        assert_eq!(s.resolve_password(&p, None).unwrap().as_str(), "pw");
        let q = d.path().join("plain.rar");
        open_archive_blocking(&s, &q, wrap_password(Some("pw".into())), |_| {
            Ok(Box::new(HeaderEngine(false)) as Box<dyn ArchiveEngine>)
        })
        .unwrap();
        assert!(s.resolve_password(&q, None).is_none());
    }

    #[test]
    fn prepare_entry_cleans_its_temp_folder_on_failure() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        aes_zip(&z);
        let s = Session::new(d.path().join("ambar-temp"));
        let e = prepare_entry(&s, &z, "g.txt", wrap_password(Some("yanlış".into())), false).unwrap_err();
        assert_eq!(e.code(), "wrong_password");
        assert!(s.resolve_password(&z, None).is_none());
        let run = std::fs::read_dir(d.path().join("ambar-temp")).unwrap().next().unwrap().unwrap().path();
        assert!(std::fs::read_dir(run).unwrap().next().is_none());
        let f = prepare_entry(&s, &z, "g.txt", wrap_password(Some("doğru".into())), false).unwrap();
        assert_eq!(std::fs::read(f).unwrap(), b"secret");
        assert_eq!(s.resolve_password(&z, None).unwrap().as_str(), "doğru");
    }
}
