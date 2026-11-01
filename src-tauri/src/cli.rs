//! The command line: `ambar extract` and `ambar test`, in the same binary as the window.
//! Nothing here touches Tauri, so no window or Dock icon appears. Jobs run through the same
//! job layer as the window: engines on their own threads, panics caught, `.ambar-part`
//! output removed on cancel.

use crate::batch::{needs_password, plan_batch, prepare_own_folder, OwnFolderEngine, PlannedArchive};
use crate::engine::{open_engine, ArchiveEngine, ExtractRequest, OverwritePolicy, Password};
use crate::error::ArchiveError;
use crate::jobs::{ConflictEvent, DoneEvent, EventSink, JobOp, Jobs, ProgressEvent};
use std::ffi::{OsStr, OsString};
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use zeroize::Zeroizing;

const EXIT_OK: i32 = 0;
const EXIT_FAILED: i32 = 1;
const EXIT_USAGE: i32 = 2;
const EXIT_CANCELLED: i32 = 130;

/// A few times a second is plenty for a person watching a terminal.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);
const PROGRESS_NAME_CHARS: usize = 60;

/// For a later volume of a RAR set: it has no run of its own, and is still accounted for.
pub fn joined_line(part: &Path, first: &Path) -> String {
    let name = |p: &Path| clean(&p.file_name().map_or_else(|| p.to_string_lossy(), |n| n.to_string_lossy()));
    format!("{}: volume of {}, handled with it", name(part), name(first))
}

/// Creates the destination and returns the levels this call made, deepest first.
pub fn create_dest(dest: &Path) -> std::io::Result<Vec<PathBuf>> {
    let made: Vec<PathBuf> = dest
        .ancestors()
        .take_while(|p| !p.as_os_str().is_empty() && std::fs::symlink_metadata(p).is_err())
        .map(Path::to_path_buf)
        .collect();
    std::fs::create_dir_all(dest).inspect_err(|_| remove_empty_dest(&made))?;
    Ok(made)
}

/// Takes back the levels `create_dest` made, as far as they are empty.
pub fn remove_empty_dest(made: &[PathBuf]) {
    for level in made {
        match std::fs::remove_dir(level) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => break,
            _ => {}
        }
    }
}

const HELP: &str = "\
Ambar archive manager

Usage:
  ambar extract <archive>... --dest <dir> [options]
  ambar test <archive>... [--password-stdin]
  ambar --help | --version

Archives run one at a time in numeric name order, so part 2 precedes part 10.
All the parts of a RAR set run once. One failed archive never stops the others.

Options for extract:
  --dest <dir>                       Destination folder, created if missing (required)
  --overwrite skip|overwrite|rename  What to do with existing files (default: skip)
  --separate                         Give each archive its own folder under the destination
  --no-paths                         Do not keep the folder structure
  --keep-broken                      Keep the output of damaged files

Options for both:
  --password-stdin                   Read one line from stdin as the password, tried for
                                     every archive that needs one. Passwords are never
                                     accepted as an argument.

Exit codes:
  0  nothing failed (skipped files alone are a success)
  1  an entry or an archive failed
  2  usage error
  130  cancelled with Ctrl+C
";

/// Only these exact words: `-psn_*` and any other launch argument still opens the window.
/// Our own options count, so a command typed without its word is a usage error.
pub fn wants_cli(first: Option<&OsStr>) -> bool {
    first.and_then(OsStr::to_str).is_some_and(|a| {
        matches!(
            a,
            "extract" | "test" | "--help" | "-h" | "--version"
                | "--password-stdin" | "--dest" | "--overwrite" | "--separate" | "--no-paths" | "--keep-broken"
        ) || a.starts_with("--dest=")
            || a.starts_with("--overwrite=")
    })
}

#[derive(Debug, PartialEq)]
pub struct ExtractArgs {
    pub archives: Vec<PathBuf>,
    pub dest: PathBuf,
    pub overwrite: OverwritePolicy,
    pub separate: bool,
    pub keep_paths: bool,
    pub keep_broken: bool,
    pub password_stdin: bool,
    /// The run's source files. Filled from the plan, not from the arguments.
    pub protected: Vec<PathBuf>,
}

#[derive(Debug, PartialEq)]
pub enum Command {
    Help,
    Version,
    Extract(ExtractArgs),
    Test { archives: Vec<PathBuf>, password_stdin: bool },
}

/// Never repeats a password back: an option is named only up to its `=`, and a short one by
/// its first letter, since `-pSecret` is one argument.
fn option_name(arg: &str) -> String {
    if arg.starts_with("--") {
        clean(arg.split('=').next().unwrap_or(arg))
    } else {
        clean(&arg.chars().take(2).collect::<String>())
    }
}

fn looks_like_password(arg: &str) -> bool {
    let a = arg.to_lowercase();
    a.starts_with("--pass") || a.starts_with("--pw") || a.starts_with("-p") && !a.starts_with("--")
}

fn parse_policy(value: &str) -> Result<OverwritePolicy, String> {
    match value {
        "skip" => Ok(OverwritePolicy::Skip),
        "overwrite" => Ok(OverwritePolicy::Overwrite),
        "rename" => Ok(OverwritePolicy::Rename),
        other => Err(format!("--overwrite must be skip, overwrite or rename, not {}", clean(other))),
    }
}

/// Turns the arguments after the program name into a command, without touching anything.
pub fn parse(args: &[OsString]) -> Result<Command, String> {
    let Some(first) = args.first().and_then(|a| a.to_str()) else {
        return Err("expected extract, test, --help or --version".into());
    };
    let is_extract = match first {
        "--help" | "-h" => return Ok(Command::Help),
        "--version" => return Ok(Command::Version),
        "extract" => true,
        "test" => false,
        other if other.starts_with('-') => return Err("a command must come first: extract or test".into()),
        other => return Err(format!("unknown command {}", option_name(other))),
    };

    let mut archives = Vec::new();
    let mut dest: Option<OsString> = None;
    let mut overwrite: Option<OverwritePolicy> = None;
    let (mut separate, mut no_paths, mut keep_broken, mut password_stdin) = (false, false, false, false);
    let extract_only = |name: &str| -> Result<(), String> {
        if is_extract {
            Ok(())
        } else {
            Err(format!("{name} is only for extract"))
        }
    };

    let mut rest = args[1..].iter();
    let mut options_done = false;
    while let Some(arg) = rest.next() {
        if options_done || !arg.as_encoded_bytes().starts_with(b"-") {
            archives.push(PathBuf::from(arg));
            continue;
        }
        let Some(text) = arg.to_str() else {
            return Err("unknown option".into());
        };
        let (name, value) = match text.split_once('=') {
            Some((n, v)) if n.starts_with("--") => (n, Some(v)),
            _ => (text, None),
        };
        let no_value = |name: &str| -> Result<(), String> {
            match value {
                Some(_) => Err(format!("{name} takes no value")),
                None => Ok(()),
            }
        };
        match name {
            "--" => options_done = true,
            "--help" | "-h" => return Ok(Command::Help),
            "--password-stdin" => {
                no_value(name)?;
                password_stdin = true;
            }
            "--separate" => {
                extract_only(name)?;
                no_value(name)?;
                separate = true;
            }
            "--no-paths" => {
                extract_only(name)?;
                no_value(name)?;
                no_paths = true;
            }
            "--keep-broken" => {
                extract_only(name)?;
                no_value(name)?;
                keep_broken = true;
            }
            "--dest" => {
                extract_only(name)?;
                if dest.is_some() {
                    return Err("--dest given twice".into());
                }
                let given = match value {
                    Some(v) => OsString::from(v),
                    None => rest
                        .next()
                        .filter(|v| !v.as_encoded_bytes().starts_with(b"-"))
                        .cloned()
                        .ok_or("--dest needs a folder; for a name starting with - use --dest=<name>")?,
                };
                if given.is_empty() {
                    return Err("--dest needs a folder".into());
                }
                dest = Some(given);
            }
            "--overwrite" => {
                extract_only(name)?;
                if overwrite.is_some() {
                    return Err("--overwrite given twice".into());
                }
                let given = match value {
                    Some(v) => v,
                    None => rest
                        .next()
                        .and_then(|v| v.to_str())
                        .filter(|v| !v.starts_with('-'))
                        .ok_or("--overwrite must be skip, overwrite or rename")?,
                };
                overwrite = Some(parse_policy(given)?);
            }
            other if looks_like_password(other) => {
                return Err("passwords are not accepted as arguments; use --password-stdin".into());
            }
            other => return Err(format!("unknown option {}", option_name(other))),
        }
    }

    if archives.is_empty() {
        return Err("no archives given".into());
    }
    if !is_extract {
        return Ok(Command::Test { archives, password_stdin });
    }
    Ok(Command::Extract(ExtractArgs {
        archives,
        dest: PathBuf::from(dest.ok_or("--dest is required")?),
        overwrite: overwrite.unwrap_or(OverwritePolicy::Skip),
        separate,
        keep_paths: !no_paths,
        keep_broken,
        password_stdin,
        protected: Vec::new(),
    }))
}

/// Names come from archives: one could hold terminal escapes, line breaks or a bidi
/// override. Same set as `safeName` in the window.
pub fn clean(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}' | '\u{61c}' | '\u{200e}' | '\u{200f}'
            | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => '\u{fffd}',
            c => c,
        })
        .collect()
}

#[derive(Debug, Default)]
pub struct Failure {
    pub path: String,
    pub code: &'static str,
    pub detail: String,
}

impl Failure {
    fn of(path: &str, e: &ArchiveError) -> Self {
        Failure { path: path.to_string(), code: e.code(), detail: e.detail() }
    }
}

#[derive(Debug, Default)]
pub struct ArchiveResult {
    pub name: String,
    pub ok: u64,
    pub skipped: u64,
    pub failures: Vec<Failure>,
    pub cancelled: bool,
}

/// Cancelled wins, then any failure. Skipped files alone are a success.
pub fn exit_code(results: &[ArchiveResult], cancelled: bool) -> i32 {
    if cancelled || results.iter().any(|r| r.cancelled) {
        EXIT_CANCELLED
    } else if results.iter().any(|r| !r.failures.is_empty()) {
        EXIT_FAILED
    } else {
        EXIT_OK
    }
}

fn describe(code: &str) -> &'static str {
    match code {
        "not_archive" => "not a supported archive",
        "password_required" => "this archive needs a password; pass one with --password-stdin",
        "wrong_password" => "the password is wrong",
        "missing_volume" => "a volume of the archive is missing",
        "corrupt" => "the data is corrupt",
        "disk_full" => "the disk is full",
        "permission" => "permission denied",
        "unsafe_path" => "the path is unsafe and was not written",
        "dest_conflict" => "there is a conflict at the destination",
        "unsupported" => "not supported",
        "cancelled" => "cancelled",
        _ => "an I/O error occurred",
    }
}

/// One line for the archive, then one per failure.
pub fn archive_line(r: &ArchiveResult) -> Vec<String> {
    let name = clean(&r.name);
    if r.cancelled {
        return vec![format!("{name}: cancelled")];
    }
    let mut lines = vec![format!("{name}: ok {}, skipped {}, failed {}", r.ok, r.skipped, r.failures.len())];
    for f in &r.failures {
        let mut line = format!("  failed: {} [{}] {}", clean(&f.path), f.code, describe(f.code));
        if !f.detail.is_empty() {
            line.push_str(": ");
            line.push_str(&clean(&f.detail));
        }
        lines.push(line);
    }
    lines
}

fn policy_word(policy: OverwritePolicy) -> &'static str {
    match policy {
        OverwritePolicy::Skip => "skip",
        OverwritePolicy::Overwrite => "overwrite",
        OverwritePolicy::Rename => "rename",
        OverwritePolicy::Ask => "ask",
    }
}

/// The last line of a run.
pub fn totals_line(
    results: &[ArchiveResult],
    planned: usize,
    joined: usize,
    policy: Option<OverwritePolicy>,
) -> String {
    let ok: u64 = results.iter().map(|r| r.ok).sum();
    let skipped: u64 = results.iter().map(|r| r.skipped).sum();
    let failed: usize = results.iter().map(|r| r.failures.len()).sum();
    let plural = if planned == 1 { "" } else { "s" };
    let mut line = format!("Total: {planned} archive{plural}");
    if joined > 0 {
        line.push_str(&format!(", {joined} joined"));
    }
    let with_failures = results.iter().filter(|r| !r.failures.is_empty()).count();
    if with_failures > 0 {
        line.push_str(&format!(", {with_failures} with failures"));
    }
    let cancelled = results.iter().filter(|r| r.cancelled).count();
    if cancelled > 0 {
        line.push_str(&format!(", {cancelled} cancelled"));
    }
    if planned > results.len() {
        line.push_str(&format!(", {} not started", planned - results.len()));
    }
    line.push_str(&format!("; ok {ok}, skipped {skipped}, failed {failed}"));
    if let Some(p) = policy {
        line.push_str(&format!("; overwrite policy: {}", policy_word(p)));
    }
    line
}

pub type Factory = Arc<dyn Fn(&Path) -> Result<Box<dyn ArchiveEngine>, ArchiveError> + Send + Sync>;

enum Finished {
    Report { ok: u64, skipped: u64, failures: Vec<Failure> },
    Error(Failure),
    Cancelled,
}

struct CliSink {
    name: String,
    /// `[12/31]`, shown ahead of the name.
    position: String,
    terminal: bool,
    last: Mutex<Option<Instant>>,
    drew: AtomicBool,
    done: Mutex<Sender<Finished>>,
}

impl EventSink for CliSink {
    fn progress(&self, ev: &ProgressEvent) {
        if !self.terminal {
            return;
        }
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|t| t.elapsed() < PROGRESS_EVERY) {
            return;
        }
        *last = Some(Instant::now());
        let name: String = clean(&self.name).chars().take(PROGRESS_NAME_CHARS).collect();
        let percent = match ev.done_bytes.saturating_mul(100).checked_div(ev.total_bytes) {
            Some(p) => format!(" {}%", p.min(100)),
            None => String::new(),
        };
        let mut err = std::io::stderr().lock();
        let _ = write!(err, "\r\x1b[K{} {name}{percent}", self.position);
        let _ = err.flush();
        self.drew.store(true, Ordering::Relaxed);
    }

    /// The command line has no Ask policy, so no engine asks. Should one, the job would wait
    /// for an answer that never comes: `run_one` is the only reader of `done`.
    fn conflict(&self, _ev: &ConflictEvent) {}

    fn done(&self, ev: &DoneEvent) {
        if self.drew.load(Ordering::Relaxed) {
            let mut err = std::io::stderr().lock();
            let _ = write!(err, "\r\x1b[K");
            let _ = err.flush();
        }
        let finished = match (&ev.report, &ev.error) {
            (Some(r), _) => Finished::Report {
                ok: r.ok,
                skipped: r.skipped,
                failures: r.failed.iter().map(|f| Failure::of(&f.path, &f.error)).collect(),
            },
            (None, Some(ArchiveError::Cancelled)) => Finished::Cancelled,
            (None, Some(e)) => Finished::Error(Failure::of(&self.name, e)),
            (None, None) => Finished::Error(Failure::of(&self.name, &crate::jobs::internal_error())),
        };
        let _ = self.done.lock().unwrap_or_else(|e| e.into_inner()).send(finished);
    }
}

#[allow(clippy::too_many_arguments)]
fn run_one(
    index: usize,
    planned: usize,
    archive: &PlannedArchive,
    extract: Option<&ExtractArgs>,
    password: Option<&Password>,
    make_engine: &Factory,
    jobs: &Jobs,
    cancel: &Arc<AtomicBool>,
) -> ArchiveResult {
    let path = archive.path.as_path();
    let name = path
        .file_name()
        .map_or_else(|| path.to_string_lossy().into_owned(), |n| n.to_string_lossy().into_owned());
    let mut result = ArchiveResult { name: name.clone(), ..Default::default() };
    let failed = |mut result: ArchiveResult, e: &ArchiveError| {
        result.failures.push(Failure::of(&name, e));
        result
    };
    // No password at all: say so before anything is written. With one that turns out wrong,
    // the report still counts the files that got out before it stopped the run.
    if password.is_none() && needs_password(path) {
        return failed(result, &ArchiveError::PasswordRequired);
    }
    let mut created = None;
    let op = match extract {
        None => JobOp::Test,
        Some(x) => {
            let mut req = ExtractRequest {
                dest: x.dest.clone(),
                selection: vec![],
                base: String::new(),
                keep_paths: x.keep_paths,
                overwrite: x.overwrite,
                keep_broken: x.keep_broken,
                protected: x.protected.clone(),
            };
            if x.separate {
                created = match prepare_own_folder(&mut req, path, archive.folder.as_deref()) {
                    Ok(c) => c,
                    Err(e) => return failed(result, &e),
                };
            }
            JobOp::Extract(req)
        }
    };
    let (tx, rx) = channel();
    let sink = Arc::new(CliSink {
        name: name.clone(),
        position: format!("[{}/{planned}]", index + 1),
        terminal: std::io::stderr().is_terminal(),
        last: Mutex::new(None),
        drew: AtomicBool::new(false),
        done: Mutex::new(tx),
    });
    let (factory, target) = (make_engine.clone(), path.to_path_buf());
    let started = jobs.start(
        format!("cli-{index}"),
        sink,
        move || {
            let inner = factory(&target)?;
            Ok(Box::new(OwnFolderEngine { inner, created }) as Box<dyn ArchiveEngine>)
        },
        password.cloned(),
        op,
    );
    if let Err(e) = started {
        return failed(result, &e);
    }
    // A Ctrl+C just before the job was registered did not reach it.
    if cancel.load(Ordering::Relaxed) {
        jobs.cancel_all();
    }
    match rx.recv() {
        Ok(Finished::Report { ok, skipped, failures }) => {
            result.ok = ok;
            result.skipped = skipped;
            result.failures = failures;
            result
        }
        Ok(Finished::Cancelled) => {
            result.cancelled = true;
            result
        }
        Ok(Finished::Error(f)) => {
            result.failures.push(f);
            result
        }
        Err(_) => failed(result, &crate::jobs::internal_error()),
    }
}

/// Runs the archives one at a time. A failure is that archive's alone; cancel ends the run.
pub fn run_archives(
    archives: &[PlannedArchive],
    extract: Option<&ExtractArgs>,
    password: Option<Password>,
    make_engine: Factory,
    jobs: &Jobs,
    cancel: &Arc<AtomicBool>,
    on_result: &mut dyn FnMut(&ArchiveResult),
) -> Vec<ArchiveResult> {
    let mut results = Vec::with_capacity(archives.len());
    for (index, archive) in archives.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let r = run_one(index, archives.len(), archive, extract, password.as_ref(), &make_engine, jobs, cancel);
        on_result(&r);
        let stop = r.cancelled;
        results.push(r);
        if stop {
            break;
        }
    }
    results
}

/// A person at a terminal gets a prompt with echo off. Otherwise one line from stdin, line
/// ending removed, sized up front so no growing leaves a copy behind.
fn read_password() -> Result<Password, String> {
    if std::io::stdin().is_terminal() {
        eprint!("Password: ");
        let typed = rpassword::read_password()
            .map(Zeroizing::new)
            .map_err(|_| "--password-stdin: could not read from the terminal".to_string())?;
        if typed.is_empty() {
            return Err("--password-stdin: no password given".into());
        }
        return Ok(typed);
    }
    let mut line = Zeroizing::new(String::with_capacity(4096));
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|_| "--password-stdin: could not read a line of text from stdin".to_string())?;
    let kept = line.trim_end_matches(['\n', '\r']).len();
    line.truncate(kept);
    if line.is_empty() {
        return Err("--password-stdin: no password on stdin".into());
    }
    Ok(line)
}

fn usage_error(message: &str) -> i32 {
    eprintln!("error: {message}\nTry 'ambar --help'.");
    EXIT_USAGE
}

/// A GUI-subsystem release binary starts with no console: borrow the parent's, if any.
#[cfg(windows)]
fn attach_parent_console() {
    const ATTACH_PARENT_PROCESS: u32 = u32::MAX;
    #[link(name = "kernel32")]
    extern "system" {
        fn AttachConsole(process_id: u32) -> i32;
    }
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
}

fn execute(mut extract: Option<ExtractArgs>, archives: Vec<PathBuf>, password_stdin: bool) -> i32 {
    let password = if password_stdin {
        match read_password() {
            Ok(p) => Some(p),
            Err(m) => return usage_error(&m),
        }
    } else {
        None
    };
    let plan = plan_batch(archives, password.as_ref());
    let mut made = Vec::new();
    if let Some(x) = &mut extract {
        x.protected.clone_from(&plan.protected);
        if x.dest.is_relative() {
            match std::env::current_dir() {
                Ok(cwd) => x.dest = cwd.join(&x.dest),
                Err(_) => {
                    eprintln!("error: cannot resolve the destination against the current folder");
                    return EXIT_FAILED;
                }
            }
        }
        made = match create_dest(&x.dest) {
            Ok(made) => made,
            Err(e) => {
                eprintln!("error: cannot create the destination {}: {}", clean(&x.dest.to_string_lossy()), clean(&e.to_string()));
                return EXIT_FAILED;
            }
        };
    }

    let jobs = Jobs::default();
    let cancel = Arc::new(AtomicBool::new(false));
    {
        let (jobs, cancel) = (jobs.clone(), cancel.clone());
        // Without the handler Ctrl+C still ends the run, only less tidily, so go on.
        let _ = ctrlc::set_handler(move || {
            // A second press is for an engine that does not get back to its cancel check.
            if cancel.swap(true, Ordering::Relaxed) {
                std::process::exit(EXIT_CANCELLED);
            }
            jobs.cancel_all();
        });
    }

    let mut stdout = std::io::stdout();
    for part in &plan.joined {
        let _ = writeln!(stdout, "{}", joined_line(&part.part, &part.first));
    }
    let results = run_archives(
        &plan.archives,
        extract.as_ref(),
        password,
        Arc::new(|p: &Path| open_engine(p)),
        &jobs,
        &cancel,
        &mut |r| {
            for line in archive_line(r) {
                let _ = writeln!(stdout, "{line}");
            }
            let _ = stdout.flush();
        },
    );
    let _ = writeln!(stdout, "{}", totals_line(&results, plan.archives.len(), plan.joined.len(), extract.as_ref().map(|x| x.overwrite)));
    let _ = stdout.flush();
    let code = exit_code(&results, cancel.load(Ordering::Relaxed));
    // A destination this run made, and nothing came of it: it does not stay.
    if let (Some(x), false) = (&extract, made.is_empty()) {
        if code != EXIT_OK && results.iter().map(|r| r.ok).sum::<u64>() == 0 {
            crate::batch::prune_empty_tree(&x.dest);
        }
        remove_empty_dest(&made);
    }
    code
}

/// Runs the command and returns the process exit code.
pub fn run(args: &[OsString]) -> i32 {
    #[cfg(windows)]
    attach_parent_console();
    // Release builds: a panic payload could hold archive-derived data, so print nothing.
    #[cfg(not(any(debug_assertions, test)))]
    std::panic::set_hook(Box::new(|_| {}));
    match parse(args) {
        Err(m) => usage_error(&m),
        // Write errors are ignored: `ambar --help | head` must not panic on a closed pipe.
        Ok(Command::Help) => {
            let _ = write!(std::io::stdout(), "{HELP}");
            EXIT_OK
        }
        Ok(Command::Version) => {
            let _ = writeln!(std::io::stdout(), "ambar {}", env!("CARGO_PKG_VERSION"));
            EXIT_OK
        }
        Ok(Command::Test { archives, password_stdin }) => execute(None, archives, password_stdin),
        Ok(Command::Extract(mut x)) => {
            let archives = std::mem::take(&mut x.archives);
            let password_stdin = x.password_stdin;
            execute(Some(x), archives, password_stdin)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ArchiveInfo, Entry, ExtractRequest, FileFailure, JobCtl, OpReport, Password};
    use crate::jobs::Jobs;
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    fn parsed(list: &[&str]) -> Command {
        parse(&args(list)).unwrap_or_else(|e| panic!("{list:?}: {e}"))
    }

    fn bad(list: &[&str]) -> String {
        parse(&args(list)).expect_err(&format!("{list:?} should be a usage error"))
    }

    #[test]
    fn routing_takes_only_the_exact_command_words() {
        for a in ["extract", "test", "--help", "-h", "--version"] {
            assert!(wants_cli(Some(OsStr::new(a))), "{a}");
        }
        // Our own options, first by mistake, are a usage error rather than a window.
        for a in ["--password-stdin", "--dest", "--dest=d", "--overwrite", "--overwrite=skip", "--separate", "--no-paths", "--keep-broken"] {
            assert!(wants_cli(Some(OsStr::new(a))), "{a}");
            assert!(parse(&args(&[a])).is_err(), "{a}");
        }
        for a in ["-psn_0_12345", "Extract", "extract.zip", "/Users/x/a.zip", "--helpful", "-v", "", "testler"] {
            assert!(!wants_cli(Some(OsStr::new(a))), "{a}");
        }
        assert!(!wants_cli(None));
    }

    #[test]
    fn extract_defaults() {
        assert_eq!(
            parsed(&["extract", "a.zip", "b.zip", "--dest", "d"]),
            Command::Extract(ExtractArgs {
                archives: vec!["a.zip".into(), "b.zip".into()],
                dest: "d".into(),
                overwrite: OverwritePolicy::Skip,
                separate: false,
                keep_paths: true,
                keep_broken: false,
                password_stdin: false,
                protected: vec![],
            })
        );
    }

    #[test]
    fn extract_takes_every_option_in_any_position() {
        let Command::Extract(x) = parsed(&[
            "extract", "--separate", "--dest=d", "a.zip", "--overwrite", "rename", "--no-paths", "--keep-broken", "--password-stdin",
        ]) else {
            panic!()
        };
        assert_eq!(x.dest, PathBuf::from("d"));
        assert_eq!(x.overwrite, OverwritePolicy::Rename);
        assert!(x.separate && !x.keep_paths && x.keep_broken && x.password_stdin);
        let Command::Extract(x) = parsed(&["extract", "a", "--dest", "d", "--overwrite=overwrite"]) else { panic!() };
        assert_eq!(x.overwrite, OverwritePolicy::Overwrite);
        let Command::Extract(x) = parsed(&["extract", "a", "--dest", "d", "--overwrite", "skip"]) else { panic!() };
        assert_eq!(x.overwrite, OverwritePolicy::Skip);
    }

    #[test]
    fn double_dash_ends_the_options() {
        let Command::Extract(x) = parsed(&["extract", "--dest", "d", "--", "--separate", "-x.zip"]) else { panic!() };
        assert_eq!(x.archives, vec![PathBuf::from("--separate"), PathBuf::from("-x.zip")]);
        assert!(!x.separate);
    }

    #[test]
    fn a_dest_starting_with_a_dash_is_only_taken_with_the_equals_form() {
        let Command::Extract(x) = parsed(&["extract", "a", "--dest=-d"]) else { panic!() };
        assert_eq!(x.dest, PathBuf::from("-d"));
        let m = bad(&["extract", "a", "--dest", "-d"]);
        assert!(m.contains("--dest="), "{m}");
    }

    #[test]
    fn test_command_takes_archives_and_the_password_flag() {
        assert_eq!(
            parsed(&["test", "a.zip", "--password-stdin", "b.zip"]),
            Command::Test { archives: vec!["a.zip".into(), "b.zip".into()], password_stdin: true }
        );
    }

    #[test]
    fn help_and_version() {
        assert_eq!(parsed(&["--help"]), Command::Help);
        assert_eq!(parsed(&["-h"]), Command::Help);
        assert_eq!(parsed(&["--version"]), Command::Version);
        assert_eq!(parsed(&["extract", "--help"]), Command::Help);
        assert_eq!(parsed(&["test", "a", "-h"]), Command::Help);
    }

    #[test]
    fn usage_errors() {
        for list in [
            &["extract"][..],
            &["extract", "a"],
            &["extract", "--dest", "d"],
            &["extract", "a", "--dest"],
            &["extract", "a", "--dest", ""],
            &["extract", "a", "--dest=", "b"],
            &["extract", "a", "--dest", "d", "--dest", "e"],
            &["extract", "a", "--dest", "d", "--overwrite"],
            &["extract", "a", "--dest", "d", "--overwrite", "ask"],
            &["extract", "a", "--dest", "d", "--overwrite", "skip", "--overwrite", "rename"],
            &["extract", "a", "--dest", "d", "--bilinmez"],
            &["extract", "a", "--dest", "d", "-x"],
            &["extract", "a", "--dest", "d", "--separate=yes"],
            &["extract", "a", "--dest", "--separate"],
            &["extract", "a", "--dest", "-d"],
            &["extract", "a", "--dest", "d", "--overwrite", "-x"],
            &["test"],
            &["test", "a", "--dest", "d"],
            &["test", "a", "--overwrite", "skip"],
            &["test", "a", "--separate"],
            &["test", "a", "--no-paths"],
            &["test", "a", "--keep-broken"],
            &["bilinmez"],
            &[],
        ] {
            assert!(!bad(list).is_empty());
        }
    }

    #[test]
    fn a_password_argument_is_refused_without_its_value_in_the_message() {
        for list in [
            &["extract", "a", "--dest", "d", "--password", "Gizli-9"][..],
            &["extract", "a", "--dest", "d", "--password=Gizli-9"],
            &["extract", "a", "--dest", "d", "-p", "Gizli-9"],
            &["extract", "a", "--dest", "d", "-pGizli-9"],
            &["extract", "a", "--dest", "d", "--pass=Gizli-9"],
            &["test", "a", "--pw", "Gizli-9"],
        ] {
            let m = bad(list);
            assert!(m.contains("--password-stdin"), "{list:?}: {m}");
            assert!(!m.contains("Gizli-9"), "{list:?}: {m}");
        }
    }

    #[test]
    fn an_unknown_option_echoes_no_value() {
        let m = bad(&["extract", "a", "--dest", "d", "--bilinmez=Gizli-9"]);
        assert!(m.contains("--bilinmez") && !m.contains("Gizli-9"), "{m}");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_survive_parsing() {
        use std::os::unix::ffi::OsStrExt;
        let raw = OsStr::from_bytes(b"caf\xe9.zip").to_os_string();
        let list = vec![OsString::from("test"), raw.clone()];
        assert_eq!(parse(&list).unwrap(), Command::Test { archives: vec![PathBuf::from(raw)], password_stdin: false });
    }

    #[test]
    fn terminal_escapes_and_bidi_never_reach_the_output() {
        assert_eq!(clean("a\x1b[31mb"), "a\u{fffd}[31mb");
        assert_eq!(clean("a\nb\r\tc"), "a\u{fffd}b\u{fffd}\u{fffd}c");
        assert_eq!(clean("x\u{202e}txt.sh\u{9b}"), "x\u{fffd}txt.sh\u{fffd}");
        assert_eq!(clean("çalışma/şifreli 😀.txt"), "çalışma/şifreli 😀.txt");
    }

    fn result(ok: u64, skipped: u64, failed: usize) -> ArchiveResult {
        ArchiveResult {
            name: "a.zip".into(),
            ok,
            skipped,
            failures: (0..failed).map(|_| Failure { path: "p".into(), code: "corrupt", detail: String::new() }).collect(),
            cancelled: false,
        }
    }

    #[test]
    fn exit_codes() {
        assert_eq!(exit_code(&[], false), 0);
        assert_eq!(exit_code(&[result(3, 0, 0)], false), 0);
        assert_eq!(exit_code(&[result(0, 5, 0)], false), 0, "skipped alone is success");
        assert_eq!(exit_code(&[result(3, 0, 0), result(9, 0, 1)], false), 1);
        assert_eq!(exit_code(&[result(3, 0, 1)], true), 130);
        assert_eq!(exit_code(&[result(3, 0, 0)], true), 130);
    }

    #[test]
    fn lines_show_counts_and_failures_and_stay_clean() {
        let mut r = result(2, 1, 0);
        r.name = "a\x1b.zip".into();
        r.failures.push(Failure { path: "kö\nt\x1bü".into(), code: "corrupt", detail: "ayrıntı\x1b".into() });
        let lines = archive_line(&r);
        assert_eq!(lines[0], "a\u{fffd}.zip: ok 2, skipped 1, failed 1");
        assert!(lines[1].starts_with("  failed: kö\u{fffd}t\u{fffd}ü [corrupt]"), "{}", lines[1]);
        assert!(lines[1].contains("ayrıntı\u{fffd}"), "{}", lines[1]);
        assert!(lines.iter().all(|l| !l.contains('\n')));
    }

    #[test]
    fn a_cancelled_archive_says_so_and_is_not_ok() {
        let mut r = result(0, 0, 0);
        r.cancelled = true;
        assert_eq!(archive_line(&r), vec!["a.zip: cancelled".to_string()]);
    }

    #[test]
    fn a_joined_part_gets_a_line_of_its_own() {
        let line = joined_line(Path::new("/d/a.part2.rar"), Path::new("/d/a.part1.rar"));
        assert_eq!(line, "a.part2.rar: volume of a.part1.rar, handled with it");
    }

    #[test]
    fn dest_levels_made_by_this_run_are_removed_only_if_empty() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("a/b/c");
        let made = create_dest(&dest).unwrap();
        assert_eq!(made.len(), 3);
        assert!(dest.is_dir());
        remove_empty_dest(&made);
        assert!(!d.path().join("a").exists());
        let made = create_dest(&dest).unwrap();
        std::fs::write(dest.join("f"), "x").unwrap();
        remove_empty_dest(&made);
        assert!(dest.join("f").exists());
        std::fs::remove_file(dest.join("f")).unwrap();
        assert!(create_dest(&dest).unwrap().is_empty(), "already there: not ours");
        remove_empty_dest(&[]);
        assert!(dest.is_dir());
    }

    #[test]
    fn totals_add_up_and_name_the_policy() {
        let t = totals_line(&[result(2, 1, 0), result(3, 0, 2)], 2, 0, Some(OverwritePolicy::Skip));
        assert!(t.starts_with("Total: 2 archives"), "{t}");
        assert!(t.contains("ok 5, skipped 1, failed 2"), "{t}");
        assert!(t.contains("overwrite policy: skip"), "{t}");
        assert!(!totals_line(&[result(1, 0, 0)], 1, 0, None).contains("policy"));
        assert!(totals_line(&[result(1, 0, 0)], 1, 2, None).contains("2 joined"));
        let t = totals_line(&[result(1, 0, 0)], 3, 0, None);
        assert!(t.contains("2 not started"), "{t}");
    }

    // --- the run itself, with scripted engines ---

    #[derive(Clone, Copy)]
    enum Act {
        Ok,
        Panic,
        Fail,
        Partial,
        WaitCancel,
    }

    struct Scripted(Act, Option<Arc<Mutex<Option<String>>>>);
    impl ArchiveEngine for Scripted {
        fn list(&mut self, _: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
            Err(ArchiveError::NotAnArchive)
        }
        fn extract(&mut self, pw: Option<&Password>, _: &ExtractRequest, ctl: &mut dyn JobCtl) -> Result<OpReport, ArchiveError> {
            self.test(pw, ctl)
        }
        fn test(&mut self, pw: Option<&Password>, ctl: &mut dyn JobCtl) -> Result<OpReport, ArchiveError> {
            if let Some(seen) = &self.1 {
                *seen.lock().unwrap() = pw.map(|p| p.to_string());
            }
            match self.0 {
                Act::Ok => Ok(OpReport { ok: 2, skipped: 1, failed: vec![], decrypted: 0 }),
                Act::Panic => panic!("boom Sifre-Gizli-9"),
                Act::Fail => Err(ArchiveError::Corrupt("damaged".into())),
                Act::Partial => Ok(OpReport {
                    ok: 1,
                    skipped: 0,
                    failed: vec![FileFailure { path: "bad.txt".into(), error: ArchiveError::Corrupt("crc".into()) }],
                    decrypted: 0,
                }),
                Act::WaitCancel => loop {
                    if ctl.cancelled() {
                        return Err(ArchiveError::Cancelled);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                },
            }
        }
    }

    fn by_name(script: Vec<(&'static str, Act)>) -> Factory {
        Arc::new(move |p: &Path| {
            let name = p.to_str().unwrap();
            let act = script.iter().find(|(n, _)| *n == name).unwrap().1;
            Ok(Box::new(Scripted(act, None)) as Box<dyn ArchiveEngine>)
        })
    }

    fn planned(names: &[&str]) -> Vec<PlannedArchive> {
        names.iter().map(|n| PlannedArchive { path: PathBuf::from(n), folder: None }).collect()
    }

    fn drive(names: &[&'static str], script: Vec<(&'static str, Act)>) -> Vec<ArchiveResult> {
        let paths = planned(names);
        let cancel = Arc::new(AtomicBool::new(false));
        run_archives(&paths, None, None, by_name(script), &Jobs::default(), &cancel, &mut |_| {})
    }

    #[test]
    fn one_panic_or_failure_never_stops_the_others() {
        let r = drive(
            &["a", "b", "c", "d"],
            vec![("a", Act::Panic), ("b", Act::Fail), ("c", Act::Partial), ("d", Act::Ok)],
        );
        assert_eq!(r.len(), 4);
        assert_eq!(r[0].failures[0].code, "io");
        assert!(!format!("{r:?}").contains("Sifre-Gizli-9"), "a panic payload must not surface");
        assert_eq!(r[1].failures[0].code, "corrupt");
        assert_eq!((r[2].ok, r[2].failures[0].path.as_str()), (1, "bad.txt"));
        assert_eq!((r[3].ok, r[3].skipped, r[3].failures.len()), (2, 1, 0));
        assert_eq!(exit_code(&r, false), 1);
    }

    #[test]
    fn factory_errors_count_as_that_archive_failing() {
        let f: Factory = Arc::new(|_| Err(ArchiveError::NotAnArchive));
        let cancel = Arc::new(AtomicBool::new(false));
        let mut seen = vec![];
        let r = run_archives(&planned(&["a.txt"]), None, None, f, &Jobs::default(), &cancel, &mut |x| seen.push(x.name.clone()));
        assert_eq!(r[0].failures[0].code, "not_archive");
        assert_eq!(r[0].failures[0].path, "a.txt");
        assert_eq!(seen, vec!["a.txt".to_string()]);
    }

    #[test]
    fn cancel_stops_the_current_archive_and_the_rest() {
        let paths = planned(&["a", "b", "c"]);
        let cancel = Arc::new(AtomicBool::new(false));
        let jobs = Jobs::default();
        let f = by_name(vec![("a", Act::Ok), ("b", Act::WaitCancel), ("c", Act::Ok)]);
        let (c2, j2) = (cancel.clone(), jobs.clone());
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            c2.store(true, Ordering::Relaxed);
            j2.cancel_all();
        });
        let r = run_archives(&paths, None, None, f, &jobs, &cancel, &mut |_| {});
        assert_eq!(r.len(), 2, "c must not run");
        assert!(!r[0].cancelled && r[1].cancelled);
        assert_eq!(exit_code(&r, true), 130);
    }

    #[test]
    fn nothing_runs_once_cancelled() {
        let cancel = Arc::new(AtomicBool::new(true));
        let r = run_archives(&planned(&["a"]), None, None, by_name(vec![("a", Act::Ok)]), &Jobs::default(), &cancel, &mut |_| {});
        assert!(r.is_empty());
    }

    #[test]
    fn the_password_reaches_each_engine() {
        let seen = Arc::new(Mutex::new(None));
        let s2 = seen.clone();
        let f: Factory = Arc::new(move |_| Ok(Box::new(Scripted(Act::Ok, Some(s2.clone()))) as Box<dyn ArchiveEngine>));
        let cancel = Arc::new(AtomicBool::new(false));
        let pw = Some(Password::new("parola".into()));
        run_archives(&planned(&["a"]), None, pw, f, &Jobs::default(), &cancel, &mut |_| {});
        assert_eq!(seen.lock().unwrap().as_deref(), Some("parola"));
    }
}
