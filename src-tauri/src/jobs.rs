//! Background jobs: extraction and testing on their own threads, with throttled
//! progress, conflict prompts and cancellation. Events go through `EventSink` so no
//! Tauri runtime is needed in tests.

use crate::engine::{
    ArchiveEngine, ConflictChoice, ExtractRequest, JobCtl, OpReport, Password,
};
use crate::error::ArchiveError;
use serde::Serialize;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// At most 20 progress events per second.
const PROGRESS_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    pub id: String,
    pub current: String,
    pub done_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConflictEvent {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoneEvent {
    pub id: String,
    pub report: Option<OpReport>,
    pub error: Option<ArchiveError>,
}

/// Where job events go. The Tauri implementation lives in `commands.rs`.
pub trait EventSink: Send + Sync {
    fn progress(&self, ev: &ProgressEvent);
    fn conflict(&self, ev: &ConflictEvent);
    fn done(&self, ev: &DoneEvent);
}

pub enum JobOp {
    Extract(ExtractRequest),
    Test,
}

struct JobHandle {
    cancel: Arc<AtomicBool>,
    answers: Sender<ConflictChoice>,
}

#[derive(Default)]
pub struct Jobs {
    map: Arc<Mutex<HashMap<String, JobHandle>>>,
    counter: AtomicU64,
}

/// `dest` comes from the frontend: it must be an existing absolute folder.
pub fn validate_dest(dest: &Path) -> Result<(), ArchiveError> {
    if dest.is_absolute() && dest.is_dir() {
        Ok(())
    } else {
        Err(ArchiveError::Io(
            "destination must be an existing absolute folder".into(),
        ))
    }
}

pub fn internal_error() -> ArchiveError {
    // Deliberately generic: a panic payload could hold sensitive data.
    ArchiveError::Io("internal error".into())
}

struct JobContext {
    id: String,
    sink: Arc<dyn EventSink>,
    cancel: Arc<AtomicBool>,
    answers: Receiver<ConflictChoice>,
    last_emit: Option<Instant>,
    latest: Option<(String, u64, u64)>,
    pending: bool,
}

impl JobContext {
    fn emit(&mut self) {
        if let Some((current, done, total)) = &self.latest {
            self.sink.progress(&ProgressEvent {
                id: self.id.clone(),
                current: current.clone(),
                done_bytes: *done,
                total_bytes: *total,
            });
        }
        self.last_emit = Some(Instant::now());
        self.pending = false;
    }

    /// Emits the last throttled-away state so the UI ends on the true final numbers.
    fn flush(&mut self) {
        if self.pending {
            self.emit();
        }
    }
}

impl JobCtl for JobContext {
    fn progress(&mut self, current: &str, done_bytes: u64, total_bytes: u64) {
        self.latest = Some((current.to_string(), done_bytes, total_bytes));
        if self.last_emit.is_none_or(|t| t.elapsed() >= PROGRESS_INTERVAL) {
            self.emit();
        } else {
            self.pending = true;
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }

    fn resolve_conflict(&mut self, path: &Path) -> ConflictChoice {
        while self.answers.try_recv().is_ok() {} // drop stale answers
        self.sink.conflict(&ConflictEvent {
            id: self.id.clone(),
            path: path.to_string_lossy().into_owned(),
        });
        loop {
            if self.cancelled() {
                return ConflictChoice::Cancel;
            }
            match self.answers.recv_timeout(Duration::from_millis(25)) {
                Ok(c) => return c,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return ConflictChoice::Cancel,
            }
        }
    }
}

impl Jobs {
    /// Starts a job thread and returns its id. `make_engine` runs on that thread.
    pub fn start<F>(
        &self,
        sink: Arc<dyn EventSink>,
        make_engine: F,
        pw: Option<Password>,
        op: JobOp,
    ) -> Result<String, ArchiveError>
    where
        F: FnOnce() -> Result<Box<dyn ArchiveEngine>, ArchiveError> + Send + 'static,
    {
        let id = format!("job-{}", self.counter.fetch_add(1, Ordering::Relaxed) + 1);
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        self.map.lock().unwrap().insert(
            id.clone(),
            JobHandle { cancel: cancel.clone(), answers: tx },
        );
        let map = self.map.clone();
        let thread_id = id.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("ambar-{id}"))
            .spawn(move || {
                let mut ctx = JobContext {
                    id: thread_id.clone(),
                    sink: sink.clone(),
                    cancel,
                    answers: rx,
                    last_emit: None,
                    latest: None,
                    pending: false,
                };
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    let mut engine = make_engine()?;
                    match &op {
                        JobOp::Extract(req) => engine.extract(pw.as_ref(), req, &mut ctx),
                        JobOp::Test => engine.test(pw.as_ref(), &mut ctx),
                    }
                }));
                let result = outcome.unwrap_or_else(|_| Err(internal_error()));
                let _ = catch_unwind(AssertUnwindSafe(|| ctx.flush()));
                map.lock().unwrap_or_else(|e| e.into_inner()).remove(&thread_id);
                let ev = match result {
                    Ok(report) => DoneEvent { id: thread_id, report: Some(report), error: None },
                    Err(e) => DoneEvent { id: thread_id, report: None, error: Some(e) },
                };
                sink.done(&ev);
            });
        if let Err(e) = spawned {
            self.map.lock().unwrap().remove(&id);
            return Err(e.into());
        }
        Ok(id)
    }

    pub fn cancel(&self, id: &str) {
        if let Some(h) = self.map.lock().unwrap().get(id) {
            h.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn cancel_all(&self) {
        for h in self.map.lock().unwrap().values() {
            h.cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn answer_conflict(&self, id: &str, choice: ConflictChoice) {
        if let Some(h) = self.map.lock().unwrap().get(id) {
            let _ = h.answers.send(choice);
        }
    }
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use crate::engine::{ArchiveInfo, Entry};
    use std::sync::Condvar;

    #[derive(Clone, Copy, PartialEq)]
    pub enum Mode {
        Ok,
        WaitCancel,
        Conflict,
        Failed,
        DiskFull,
        Panic,
        Chatty,
    }

    pub struct FakeEngine(pub Mode);

    impl ArchiveEngine for FakeEngine {
        fn list(&mut self, _: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
            Err(ArchiveError::Unsupported("fake".into()))
        }
        fn extract(
            &mut self,
            _pw: Option<&Password>,
            _req: &ExtractRequest,
            ctl: &mut dyn JobCtl,
        ) -> Result<OpReport, ArchiveError> {
            self.run(ctl)
        }
        fn test(&mut self, _pw: Option<&Password>, ctl: &mut dyn JobCtl) -> Result<OpReport, ArchiveError> {
            self.run(ctl)
        }
    }

    impl FakeEngine {
        fn run(&mut self, ctl: &mut dyn JobCtl) -> Result<OpReport, ArchiveError> {
            match self.0 {
                Mode::Ok => {
                    ctl.progress("a", 1, 3);
                    Ok(OpReport { ok: 3, skipped: 0, failed: vec![] })
                }
                Mode::WaitCancel => loop {
                    if ctl.cancelled() {
                        return Err(ArchiveError::Cancelled);
                    }
                    std::thread::sleep(Duration::from_millis(5));
                },
                Mode::Conflict => match ctl.resolve_conflict(Path::new("/x/y")) {
                    ConflictChoice::Cancel => Err(ArchiveError::Cancelled),
                    ConflictChoice::Skip => {
                        Ok(OpReport { ok: 0, skipped: 1, failed: vec![] })
                    }
                    _ => Ok(OpReport { ok: 1, skipped: 0, failed: vec![] }),
                },
                Mode::Failed => Ok(OpReport {
                    ok: 1,
                    skipped: 0,
                    failed: vec![crate::engine::FileFailure {
                        path: "bad.txt".into(),
                        error: ArchiveError::Corrupt("crc".into()),
                    }],
                }),
                Mode::DiskFull => {
                    Err(std::io::Error::from(std::io::ErrorKind::StorageFull).into())
                }
                Mode::Panic => panic!("boom Sifre-Gizli-9"),
                Mode::Chatty => {
                    for i in 0..5000u64 {
                        ctl.progress("f", i, 5000);
                    }
                    ctl.progress("last", 5000, 5000);
                    Ok(OpReport::default())
                }
            }
        }
    }

    pub fn engine(m: Mode) -> impl FnOnce() -> Result<Box<dyn ArchiveEngine>, ArchiveError> + Send + 'static {
        move || Ok(Box::new(FakeEngine(m)) as Box<dyn ArchiveEngine>)
    }

    /// Records every event as JSON, in order.
    pub struct FakeSink {
        pub events: Mutex<Vec<(&'static str, serde_json::Value)>>,
        cv: Condvar,
    }

    impl FakeSink {
        pub fn new() -> Arc<Self> {
            Arc::new(Self { events: Mutex::new(vec![]), cv: Condvar::new() })
        }
        fn push(&self, k: &'static str, v: serde_json::Value) {
            self.events.lock().unwrap().push((k, v));
            self.cv.notify_all();
        }
        pub fn wait_for(&self, kind: &str) -> serde_json::Value {
            let deadline = Instant::now() + Duration::from_secs(10);
            let mut g = self.events.lock().unwrap();
            loop {
                if let Some((_, v)) = g.iter().find(|(k, _)| *k == kind) {
                    return v.clone();
                }
                let left = deadline.saturating_duration_since(Instant::now());
                assert!(!left.is_zero(), "timed out waiting for {kind}");
                g = self.cv.wait_timeout(g, left).unwrap().0;
            }
        }
        pub fn wait_done(&self) -> serde_json::Value {
            self.wait_for("done")
        }
        pub fn all_json(&self) -> String {
            serde_json::to_string(&self.events.lock().unwrap().iter().map(|e| e.1.clone()).collect::<Vec<_>>()).unwrap()
        }
    }

    impl EventSink for FakeSink {
        fn progress(&self, ev: &ProgressEvent) {
            self.push("progress", serde_json::to_value(ev).unwrap());
        }
        fn conflict(&self, ev: &ConflictEvent) {
            self.push("conflict", serde_json::to_value(ev).unwrap());
        }
        fn done(&self, ev: &DoneEvent) {
            self.push("done", serde_json::to_value(ev).unwrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;

    fn run(mode: Mode) -> (Arc<test_support::FakeSink>, serde_json::Value) {
        let sink = FakeSink::new();
        let jobs = Jobs::default();
        jobs.start(sink.clone(), engine(mode), None, JobOp::Test).unwrap();
        let d = sink.wait_done();
        (sink, d)
    }

    #[test]
    fn job_emits_done_with_report() {
        let (_, d) = run(Mode::Ok);
        assert_eq!(d["report"]["ok"], 3);
        assert!(d["error"].is_null());
        assert_eq!(d["id"], "job-1");
    }

    #[test]
    fn cancel_flag_reaches_engine_and_done_has_cancelled_error() {
        let sink = FakeSink::new();
        let jobs = Jobs::default();
        let id = jobs.start(sink.clone(), engine(Mode::WaitCancel), None, JobOp::Test).unwrap();
        jobs.cancel(&id);
        let d = sink.wait_done();
        assert_eq!(d["error"]["code"], "cancelled");
        assert!(d["report"].is_null());
    }

    #[test]
    fn conflict_blocks_until_answer() {
        let sink = FakeSink::new();
        let jobs = Jobs::default();
        let id = jobs.start(sink.clone(), engine(Mode::Conflict), None, JobOp::Test).unwrap();
        let c = sink.wait_for("conflict");
        assert_eq!(c["path"], "/x/y");
        std::thread::sleep(Duration::from_millis(150));
        assert!(sink.events.lock().unwrap().iter().all(|e| e.0 != "done"));
        jobs.answer_conflict(&id, ConflictChoice::Skip);
        let d = sink.wait_done();
        assert_eq!(d["report"]["skipped"], 1);
    }

    #[test]
    fn cancel_releases_pending_conflict() {
        let sink = FakeSink::new();
        let jobs = Jobs::default();
        let id = jobs.start(sink.clone(), engine(Mode::Conflict), None, JobOp::Test).unwrap();
        sink.wait_for("conflict");
        jobs.cancel(&id);
        assert_eq!(sink.wait_done()["error"]["code"], "cancelled");
    }

    #[test]
    fn failed_entries_are_not_success() {
        let (_, d) = run(Mode::Failed);
        assert_eq!(d["report"]["failed"][0]["path"], "bad.txt");
        assert_eq!(d["report"]["failed"][0]["error"]["code"], "corrupt");
    }

    #[test]
    fn disk_error_surfaces_as_disk_full() {
        let (_, d) = run(Mode::DiskFull);
        assert_eq!(d["error"]["code"], "disk_full");
    }

    #[test]
    fn engine_panic_becomes_error_without_payload() {
        let (sink, d) = run(Mode::Panic);
        assert_eq!(d["error"]["code"], "io");
        assert!(d["report"].is_null());
        assert!(!sink.all_json().contains("Sifre-Gizli-9"));
    }

    #[test]
    fn factory_error_is_reported() {
        let sink = FakeSink::new();
        let jobs = Jobs::default();
        jobs.start(sink.clone(), || Err(ArchiveError::NotAnArchive), None, JobOp::Test).unwrap();
        assert_eq!(sink.wait_done()["error"]["code"], "not_archive");
    }

    #[test]
    fn progress_is_throttled_and_final_state_is_emitted() {
        let (sink, _) = run(Mode::Chatty);
        let ev = sink.events.lock().unwrap();
        let prog: Vec<_> = ev.iter().filter(|e| e.0 == "progress").collect();
        assert!(prog.len() < 100, "got {}", prog.len());
        assert_eq!(prog.last().unwrap().1["current"], "last");
        assert_eq!(prog.last().unwrap().1["doneBytes"], 5000);
    }

    #[test]
    fn dest_must_be_existing_absolute_dir() {
        let d = tempfile::tempdir().unwrap();
        assert!(validate_dest(d.path()).is_ok());
        assert!(validate_dest(&d.path().join("nope")).is_err());
        assert!(validate_dest(Path::new("relative")).is_err());
        let f = d.path().join("f");
        std::fs::write(&f, b"x").unwrap();
        assert!(validate_dest(&f).is_err());
    }
}
