//! Tauri commands. Everything that needs a Tauri runtime lives here; the logic it calls
//! is tested without one.

use crate::engine::{
    open_engine, ArchiveEngine, ArchiveInfo, ConflictChoice, Entry, ExtractRequest, Password,
};
use crate::error::ArchiveError;
use crate::jobs::{
    internal_error, validate_dest, ConflictEvent, DoneEvent, EventSink, JobOp, Jobs, ProgressEvent,
};
use crate::local_fs::{self, LocalEntry};
use crate::session::{extract_entry_to_temp, wrap_password, Session};
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

/// Lists an archive, caching the password only after the open succeeded. A panic in
/// the engine becomes a generic error.
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
    let res = catch_unwind(AssertUnwindSafe(|| {
        let mut engine = make_engine(path)?;
        engine.list(pw.as_ref())
    }))
    .unwrap_or_else(|_| Err(internal_error()))?;
    if let Some(pw) = pw {
        session.remember_password(path, pw);
    }
    Ok(OpenResult { info: res.0, entries: res.1 })
}

fn join_err<T>(r: Result<Result<T, ArchiveError>, tauri::Error>) -> Result<T, ArchiveError> {
    r.unwrap_or_else(|_| Err(internal_error()))
}

#[tauri::command]
pub async fn open_archive(
    session: State<'_, Session>,
    path: String,
    password: Option<String>,
) -> Result<OpenResult, ArchiveError> {
    let session = session.inner().clone();
    let pw = wrap_password(password);
    join_err(
        tauri::async_runtime::spawn_blocking(move || {
            open_archive_blocking(&session, Path::new(&path), pw, open_engine)
        })
        .await
        .map_err(Into::into),
    )
}

#[tauri::command]
pub fn start_extract(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    session: State<'_, Session>,
    path: String,
    password: Option<String>,
    req: ExtractRequest,
) -> Result<String, ArchiveError> {
    validate_dest(&req.dest)?;
    let p = PathBuf::from(path);
    let pw = session.resolve_password(&p, wrap_password(password));
    jobs.start(Arc::new(TauriSink(app)), move || open_engine(&p), pw, JobOp::Extract(req))
}

#[tauri::command]
pub fn start_test(
    app: AppHandle,
    jobs: State<'_, Jobs>,
    session: State<'_, Session>,
    path: String,
    password: Option<String>,
) -> Result<String, ArchiveError> {
    let p = PathBuf::from(path);
    let pw = session.resolve_password(&p, wrap_password(password));
    jobs.start(Arc::new(TauriSink(app)), move || open_engine(&p), pw, JobOp::Test)
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
pub fn forget_password(session: State<'_, Session>, path: String) {
    session.forget_password(Path::new(&path));
}

#[tauri::command]
pub fn list_local(path: String) -> Result<Vec<LocalEntry>, ArchiveError> {
    local_fs::list_local(Path::new(&path))
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
    let mut child = Command::new("qlmanage")
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

#[tauri::command]
pub async fn open_entry(
    app: AppHandle,
    session: State<'_, Session>,
    path: String,
    entry: String,
    password: Option<String>,
    quick_look: bool,
) -> Result<(), ArchiveError> {
    let session = session.inner().clone();
    let pw = wrap_password(password);
    let archive = PathBuf::from(path);
    let file = join_err(
        tauri::async_runtime::spawn_blocking(move || {
            let pw = session.resolve_password(&archive, pw);
            let temp = session.new_temp_dir()?;
            catch_unwind(AssertUnwindSafe(|| {
                let mut engine = open_engine(&archive)?;
                extract_entry_to_temp(engine.as_mut(), pw.as_ref(), &temp, &entry)
            }))
            .unwrap_or_else(|_| Err(internal_error()))
        })
        .await
        .map_err(Into::into),
    )?;
    #[cfg(target_os = "macos")]
    if quick_look {
        return self::quick_look(&file);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = quick_look;
    app.opener()
        .open_path(file.to_string_lossy(), None::<&str>)
        .map_err(|e| ArchiveError::Io(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
