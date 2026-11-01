//! RAR engine on top of the UnRAR library (`unrar-ng-sys`). The safe `unrar-ng`
//! wrapper cannot see redirect records, UTC times, the comment text or set
//! "keep broken", so this module drives the C API through a small private
//! wrapper (`ffi`). Extraction never uses UnRAR's own path handling: each entry goes
//! through `plan_entry` and is written to a temp file via `write_via_temp`, so the
//! security boundary stays in `plan.rs`. Redirect entries (symbolic links, hard
//! links, file copies) are never extracted: UnRAR would resolve their targets
//! against the process working directory or create real links.

use super::plan::{normalize_entry_path, plan_entry, write_via_temp, PlanOutcome, Selection};
use super::{
    ArchiveEngine, ArchiveInfo, Entry, ExtractRequest, FileFailure, Format, JobCtl, OpReport,
    Password,
};
use crate::error::ArchiveError;
use ffi::{Arc, Fail, Header, When};
use std::path::{Path, PathBuf};
use unrar_ng::Archive;

/// Minimal unsafe wrapper over the UnRAR C API. Everything unsafe lives here.
mod ffi {
    use std::cell::Cell;
    use std::os::raw::{c_int, c_uint};
    use std::path::Path;
    use unrar_ng_sys as sys;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum When {
        Open,
        Read,
        Process,
    }

    #[derive(Debug)]
    pub struct Fail {
        pub code: c_int,
        pub when: When,
        /// File name of the volume UnRAR asked for last (set when one was missing).
        pub volume: Option<String>,
        /// The progress hook asked to stop (UnRAR then reports ERAR_UNKNOWN).
        pub cancelled: bool,
    }

    /// `HOST_UNIX` in UnRAR's headers.hpp.
    const HOST_UNIX: c_uint = 3;
    pub const OM_LIST: c_uint = sys::RAR_OM_LIST;
    pub const OM_EXTRACT: c_uint = sys::RAR_OM_EXTRACT;
    /// Listing that also returns the headers of a file's continuation in later volumes.
    pub const OM_LIST_INCSPLIT: c_uint = sys::RAR_OM_LIST_INCSPLIT;
    pub const OP_SKIP: c_int = sys::RAR_SKIP;
    pub const OP_TEST: c_int = sys::RAR_TEST;
    pub const OP_EXTRACT: c_int = sys::RAR_EXTRACT;
    pub const ERR_BAD_DATA: c_int = sys::ERAR_BAD_DATA;
    pub const ERR_BAD_ARCHIVE: c_int = sys::ERAR_BAD_ARCHIVE;
    pub const ERR_UNKNOWN_FORMAT: c_int = sys::ERAR_UNKNOWN_FORMAT;
    pub const ERR_EOPEN: c_int = sys::ERAR_EOPEN;
    pub const ERR_MISSING_PASSWORD: c_int = sys::ERAR_MISSING_PASSWORD;
    pub const ERR_BAD_PASSWORD: c_int = sys::ERAR_BAD_PASSWORD;
    pub const ERR_LARGE_DICT: c_int = sys::ERAR_LARGE_DICT;
    pub const ERR_EWRITE: c_int = sys::ERAR_EWRITE;
    pub const ERR_EREFERENCE: c_int = sys::ERAR_EREFERENCE;

    #[derive(Debug)]
    pub struct Header {
        pub filename: String,
        pub is_dir: bool,
        pub encrypted: bool,
        pub unpacked_size: u64,
        /// Bytes of this header's part: one volume's worth for a split file.
        pub packed_size: u64,
        /// Continues a file from the previous volume.
        pub split_before: bool,
        pub crc: u32,
        pub attr: u32,
        pub host_unix: bool,
        /// 0 = regular entry; anything else is a link or file reference.
        pub redir_type: u32,
        /// Modification time as Unix seconds (UTC), from the extended time field.
        pub mtime: Option<i64>,
    }

    pub struct Flags {
        pub volume: bool,
        pub locked: bool,
        pub solid: bool,
        pub recovery: bool,
        pub enc_headers: bool,
        pub has_comment: bool,
    }

    #[cfg(any(target_os = "linux", target_os = "netbsd"))]
    type OsPath = std::ffi::CString;
    #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
    type OsPath = Vec<sys::WCHAR>;

    #[cfg(any(target_os = "linux", target_os = "netbsd"))]
    fn os_path(p: &Path) -> Option<OsPath> {
        std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).ok()
    }
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "netbsd"))))]
    fn os_path(p: &Path) -> Option<OsPath> {
        let s = p.to_str()?;
        if s.contains('\0') {
            return None;
        }
        let mut v: Vec<sys::WCHAR> = s.chars().map(|c| c as u32 as sys::WCHAR).collect();
        v.push(0);
        Some(v)
    }
    #[cfg(windows)]
    fn os_path(p: &Path) -> Option<OsPath> {
        use std::os::windows::ffi::OsStrExt;
        let mut v: Vec<sys::WCHAR> = p.as_os_str().encode_wide().collect();
        if v.contains(&0) {
            return None;
        }
        v.push(0);
        Some(v)
    }

    /// Reads a NUL-terminated wchar_t string (at most `max` units).
    ///
    /// # Safety
    /// `p` must point to a readable, NUL-terminated wchar_t string or at least `max` units.
    unsafe fn wide_to_string(p: *const sys::WCHAR, max: usize) -> String {
        let mut units: Vec<u32> = Vec::new();
        for i in 0..max {
            let u = unsafe { std::ptr::read_unaligned(p.add(i)) };
            if u == 0 {
                break;
            }
            units.push(u as u32);
        }
        if std::mem::size_of::<sys::WCHAR>() == 2 {
            let u16s: Vec<u16> = units.iter().map(|&u| u as u16).collect();
            String::from_utf16_lossy(&u16s)
        } else {
            units
                .into_iter()
                .map(|u| char::from_u32(u).unwrap_or('\u{fffd}'))
                .collect()
        }
    }

    /// Called with the byte count of each chunk UnRAR produces; `false` aborts.
    type DataHook<'a> = dyn FnMut(u64) -> bool + 'a;

    #[derive(Default)]
    struct CbState {
        next_volume: Cell<Option<String>>,
        /// Set only for the duration of one `process` call.
        hook: Cell<Option<*mut DataHook<'static>>>,
        cancelled: Cell<bool>,
    }

    extern "C" fn callback(
        msg: sys::UINT,
        user: sys::LPARAM,
        p1: sys::LPARAM,
        p2: sys::LPARAM,
    ) -> c_int {
        if msg == sys::UCM_CHANGEVOLUMEW {
            if user != 0 && p1 != 0 && p2 == sys::RAR_VOL_ASK {
                // SAFETY: `user` is the address of the `CbState` boxed inside the `Arc`
                // that owns the handle, which outlives every call into the library;
                // `p1` is the library's NUL-terminated volume name.
                let st = unsafe { &*(user as *const CbState) };
                let name = unsafe { wide_to_string(p1 as *const sys::WCHAR, 2048) };
                st.next_volume.set(Some(name));
            }
            // Next volume missing (ASK): stop. Present (NOTIFY): continue.
            return if p2 == sys::RAR_VOL_ASK { -1 } else { 0 };
        }
        if msg == sys::UCM_PROCESSDATA && user != 0 {
            // SAFETY: as above; the hook pointer is valid while `process` runs, which is
            // the only time UnRAR delivers data.
            let st = unsafe { &*(user as *const CbState) };
            if let Some(h) = st.hook.get() {
                if !unsafe { (*h)(p2 as u64) } {
                    st.cancelled.set(true);
                    return -1;
                }
            }
        }
        0
    }

    pub struct Arc {
        handle: *const sys::Handle,
        cb: Box<CbState>,
        pub flags: Flags,
        pub comment: Option<String>,
    }

    // The handle is only used from one thread at a time, through `&mut self`.
    unsafe impl Send for Arc {}

    impl Drop for Arc {
        fn drop(&mut self) {
            // SAFETY: the handle came from RAROpenArchiveEx and is closed exactly once.
            unsafe { sys::RARCloseArchive(self.handle) };
        }
    }

    impl Arc {
        pub fn open(
            path: &Path,
            mode: c_uint,
            password: Option<&str>,
            keep_broken: bool,
            want_comment: bool,
        ) -> Result<Arc, Fail> {
            let fail = |code| Fail {
                code,
                when: When::Open,
                volume: None,
                cancelled: false,
            };
            let name = os_path(path).ok_or_else(|| fail(sys::ERAR_EOPEN))?;
            // NUL-terminated copy that is wiped on drop (a CString would keep it in freed heap).
            let pw: Option<zeroize::Zeroizing<Vec<u8>>> = match password {
                Some(p) if p.as_bytes().contains(&0) => {
                    return Err(fail(sys::ERAR_BAD_PASSWORD));
                }
                Some(p) => {
                    let mut v = zeroize::Zeroizing::new(Vec::with_capacity(p.len() + 1));
                    v.extend_from_slice(p.as_bytes());
                    v.push(0);
                    Some(v)
                }
                None => None,
            };
            let cb: Box<CbState> = Box::default();
            // Asking for the comment makes UnRAR read it during open, which poisons the
            // handle for header-encrypted archives (the password is set afterwards).
            let mut cmt: Vec<sys::WCHAR> = vec![0; if want_comment { 65536 } else { 0 }];
            #[cfg(any(target_os = "linux", target_os = "netbsd"))]
            let mut data = sys::OpenArchiveDataEx::new(name.as_ptr(), mode);
            #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
            let mut data = sys::OpenArchiveDataEx::new(name.as_ptr(), mode);
            if want_comment {
                data.comment_buffer_w = cmt.as_mut_ptr();
                data.comment_buffer_size = cmt.len() as c_uint;
            }
            if keep_broken {
                data.op_flags = sys::ROADOF_KEEPBROKEN;
            }
            // SAFETY: `data` and the buffers it points to outlive the call.
            let handle = unsafe { sys::RAROpenArchiveEx(&mut data as *mut _) };
            let result = data.open_result as c_int;
            if handle.is_null() {
                return Err(fail(result));
            }
            // From here the Drop impl closes the handle, also on the error path.
            let flags_raw = data.flags;
            let cmt_size = data.comment_size;
            let cmt_state = data.comment_state;
            let mut arc = Arc {
                handle,
                cb,
                flags: Flags {
                    volume: flags_raw & sys::ROADF_VOLUME != 0,
                    locked: flags_raw & sys::ROADF_LOCK != 0,
                    solid: flags_raw & sys::ROADF_SOLID != 0,
                    recovery: flags_raw & sys::ROADF_RECOVERY != 0,
                    enc_headers: flags_raw & sys::ROADF_ENCHEADERS != 0,
                    has_comment: flags_raw & sys::ROADF_COMMENT != 0,
                },
                comment: None,
            };
            if result != sys::ERAR_SUCCESS {
                return Err(Fail {
                    code: result,
                    when: When::Open,
                    volume: arc.cb.next_volume.take(),
                    cancelled: false,
                });
            }
            if cmt_state == 1 || cmt_state == sys::ERAR_SMALL_BUF as c_uint {
                let n = (cmt_size as usize).min(cmt.len());
                // SAFETY: the library wrote at most `n` units and a terminating NUL into `cmt`.
                arc.comment = Some(unsafe { wide_to_string(cmt.as_ptr(), n) });
            }
            // Set after opening: a callback present during open is asked for the
            // password of encrypted headers and answers "none".
            // SAFETY: valid handle; `cb` is heap-allocated and lives as long as the handle.
            unsafe {
                sys::RARSetCallback(
                    arc.handle,
                    Some(callback),
                    &*arc.cb as *const CbState as sys::LPARAM,
                )
            };
            if let Some(pw) = pw {
                // SAFETY: valid handle and NUL-terminated string; the library copies it.
                unsafe { sys::RARSetPassword(arc.handle, pw.as_ptr().cast()) };
            }
            Ok(arc)
        }

        fn fail(&self, code: c_int, when: When) -> Fail {
            Fail {
                code,
                when,
                volume: self.cb.next_volume.take(),
                cancelled: self.cb.cancelled.take(),
            }
        }

        pub fn read_header(&mut self) -> Result<Option<Header>, Fail> {
            // SAFETY: all-zero is a valid HeaderDataEx (integers and null pointers).
            let mut hd: Box<sys::HeaderDataEx> = Box::new(unsafe { std::mem::zeroed() });
            // SAFETY: valid handle, valid output struct.
            let code = unsafe { sys::RARReadHeaderEx(self.handle, &mut *hd as *mut _) };
            if code == sys::ERAR_END_ARCHIVE {
                return Ok(None);
            }
            if code != sys::ERAR_SUCCESS {
                return Err(self.fail(code, When::Read));
            }
            // Packed struct: copy scalars out by value, never take references.
            let flags = hd.flags;
            let (lo, hi) = (hd.unp_size, hd.unp_size_high);
            let (plo, phi) = (hd.pack_size, hd.pack_size_high);
            let (ml, mh) = (hd.mtime_low, hd.mtime_high);
            let win = ((mh as u64) << 32) | ml as u64;
            // SAFETY: `filename_w` is a NUL-terminated array inside the struct.
            let filename =
                unsafe { wide_to_string(&raw const hd.filename_w as *const sys::WCHAR, 1024) };
            Ok(Some(Header {
                filename,
                is_dir: flags & sys::RHDF_DIRECTORY != 0,
                encrypted: flags & sys::RHDF_ENCRYPTED != 0,
                unpacked_size: ((hi as u64) << 32) | lo as u64,
                packed_size: ((phi as u64) << 32) | plo as u64,
                split_before: flags & sys::RHDF_SPLITBEFORE != 0,
                crc: hd.file_crc,
                attr: hd.file_attr,
                host_unix: hd.host_os == HOST_UNIX,
                redir_type: hd.redir_type,
                mtime: (win != 0).then(|| (win / 10_000_000) as i64 - 11_644_473_600),
            }))
        }

        /// Processes the entry whose header was just read. `dest` is the exact file to
        /// write (only for `OP_EXTRACT`); the library's own directory logic is not used.
        pub fn process(
            &mut self,
            op: c_int,
            dest: Option<&Path>,
            hook: Option<&mut DataHook<'_>>,
        ) -> Result<(), Fail> {
            let dest = match dest {
                Some(d) => {
                    Some(os_path(d).ok_or_else(|| self.fail(sys::ERAR_ECREATE, When::Process))?)
                }
                None => None,
            };
            let hook_ptr = hook.map(|h| {
                // SAFETY: only the lifetime is erased; the pointer is cleared again below,
                // before `hook` can go out of scope.
                unsafe { std::mem::transmute::<*mut DataHook<'_>, *mut DataHook<'static>>(h) }
            });
            self.cb.hook.set(hook_ptr);
            let dp = dest.as_ref().map_or(std::ptr::null(), |d| d.as_ptr());
            #[cfg(any(target_os = "linux", target_os = "netbsd"))]
            // SAFETY: valid handle; `dp` is null or NUL-terminated and outlives the call.
            let code = unsafe { sys::RARProcessFile(self.handle, op, std::ptr::null(), dp) };
            #[cfg(not(any(target_os = "linux", target_os = "netbsd")))]
            // SAFETY: as above, wide variant.
            let code = unsafe { sys::RARProcessFileW(self.handle, op, std::ptr::null(), dp) };
            self.cb.hook.set(None);
            if code == sys::ERAR_SUCCESS {
                Ok(())
            } else {
                Err(self.fail(code, When::Process))
            }
        }
    }
}

pub struct RarEngine {
    path: PathBuf,
}

impl RarEngine {
    pub fn new(path: &Path) -> RarEngine {
        RarEngine {
            path: path.to_path_buf(),
        }
    }

    /// Volume 1 of a multi-volume set (the opened path itself otherwise).
    fn first_part(&self) -> PathBuf {
        Archive::new(&self.path).first_part()
    }

    fn open(
        &self,
        mode: u32,
        pw: Option<&Password>,
        keep_broken: bool,
        want_comment: bool,
    ) -> Result<Arc, ArchiveError> {
        Arc::open(
            &self.first_part(),
            mode,
            pw.map(|p| p.as_str()),
            keep_broken,
            want_comment,
        )
        .map_err(|f| self.map_err(f))
    }

    fn map_err(&self, f: Fail) -> ArchiveError {
        if f.cancelled {
            return ArchiveError::Cancelled;
        }
        match f.code {
            ffi::ERR_EWRITE => ArchiveError::DiskFull,
            ffi::ERR_MISSING_PASSWORD => ArchiveError::PasswordRequired,
            ffi::ERR_BAD_PASSWORD => ArchiveError::WrongPassword,
            ffi::ERR_BAD_DATA | ffi::ERR_BAD_ARCHIVE => ArchiveError::Corrupt(
                match f.when {
                    When::Open => "archive header damaged",
                    When::Read => "file header damaged",
                    When::Process => "file data damaged (CRC error)",
                }
                .into(),
            ),
            ffi::ERR_UNKNOWN_FORMAT => ArchiveError::NotAnArchive,
            ffi::ERR_EOPEN => {
                let first = self.first_part();
                let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
                if f.when == When::Open {
                    // Opening `partN` redirects to part 1; if that is absent say so.
                    if std::fs::symlink_metadata(&first).is_err()
                        && Archive::new(&self.path).is_multipart()
                    {
                        if let Some(n) = name(&first) {
                            return ArchiveError::MissingVolume(n);
                        }
                    }
                    return ArchiveError::Io("could not open archive".into());
                }
                let from_cb = f
                    .volume
                    .and_then(|v| name(Path::new(&v)).filter(|n| !n.is_empty()));
                match from_cb.or_else(|| self.missing_volume()) {
                    Some(n) => ArchiveError::MissingVolume(n),
                    None => ArchiveError::Io("could not open next volume".into()),
                }
            }
            ffi::ERR_LARGE_DICT | ffi::ERR_EREFERENCE => {
                ArchiveError::Unsupported(format!("unrar error {}", f.code))
            }
            c => ArchiveError::Io(format!("unrar error {c}")),
        }
    }

    /// File name of the first volume (from 1) that is absent from disk.
    fn missing_volume(&self) -> Option<String> {
        let a = Archive::new(&self.path);
        (1..=9999).find_map(|n| {
            let p = a.nth_part(n)?;
            if std::fs::symlink_metadata(&p).is_ok() {
                None
            } else {
                Some(p.file_name()?.to_string_lossy().into_owned())
            }
        })
    }

    /// Re-opens the archive positioned after the first `n` entries (a failed
    /// extraction leaves the handle in an unknown state).
    fn reopen_after(
        &self,
        pw: Option<&Password>,
        keep_broken: bool,
        n: usize,
    ) -> Result<Arc, ArchiveError> {
        let mut ar = self.open(ffi::OM_EXTRACT, pw, keep_broken, false)?;
        for _ in 0..n {
            ar.read_header()
                .map_err(|f| self.map_err(f))?
                .ok_or_else(|| ArchiveError::Corrupt("unexpected end of archive".into()))?;
            ar.process(ffi::OP_SKIP, None, None)
                .map_err(|f| self.map_err(f))?;
        }
        Ok(ar)
    }

    fn process(
        &self,
        pw: Option<&Password>,
        req: Option<&ExtractRequest>,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError> {
        // Listing first surfaces password problems early and gives the byte total.
        let (_, entries) = self.list_inner(pw)?;
        let selection = Selection::of(req);
        let wanted = |path: &str| selection.has(path);
        let total: u64 = entries
            .iter()
            .filter(|e| !e.is_dir && wanted(&e.path))
            .fold(0u64, |acc, e| acc.saturating_add(e.size));
        let keep_broken = req.is_some_and(|r| r.keep_broken);

        let mut report = OpReport::default();
        let mut done = 0u64;
        let mut index = 0usize; // headers consumed so far
        let mut broke = false;
        let mut ar = self.open(ffi::OM_EXTRACT, pw, keep_broken, false)?;
        loop {
            if ctl.cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let Some(h) = ar.read_header().map_err(|f| self.map_err(f))? else {
                break;
            };
            index += 1;
            let entry = to_entry(&h);
            let skip = |ar: &mut Arc| {
                ar.process(ffi::OP_SKIP, None, None)
                    .map_err(|f| self.map_err(f))
            };
            if !wanted(&entry.path) {
                skip(&mut ar)?;
                continue;
            }
            // Extraction only: links and file references are refused outright.
            if req.is_some() && is_redirect(&h) {
                report.failed.push(FileFailure {
                    path: entry.path.clone(),
                    error: ArchiveError::Unsupported("link".into()),
                });
                skip(&mut ar)?;
                continue;
            }
            ctl.progress(&entry.path, done, total);

            // `Err((error, true))`: UnRAR itself failed on this entry (the handle is in an
            // unknown state); `Err((error, false))`: our own post-check failed.
            let attempt: Result<(), (ArchiveError, bool)> = match req {
                // Test mode: verify files, nothing is written.
                None if entry.is_dir => {
                    skip(&mut ar)?;
                    continue;
                }
                None => {
                    let mut bytes = 0u64;
                    let path = entry.path.as_str();
                    let mut hook = |n: u64| {
                        bytes += n;
                        ctl.progress(path, done + bytes, total);
                        !ctl.cancelled()
                    };
                    ar.process(ffi::OP_TEST, None, Some(&mut hook))
                        .map_err(|f| (self.map_err(f), true))
                }
                Some(req) => {
                    let plan = match plan_entry(&entry, req, ctl) {
                        Ok(p) => p,
                        Err(e @ (ArchiveError::UnsafePath(_) | ArchiveError::DestConflict(_))) => {
                            report.failed.push(FileFailure {
                                path: entry.path.clone(),
                                error: e,
                            });
                            skip(&mut ar)?;
                            continue;
                        }
                        Err(e) => return Err(e),
                    };
                    match plan {
                        PlanOutcome::Skip => {
                            report.skipped += 1;
                            skip(&mut ar)?;
                            continue;
                        }
                        PlanOutcome::Nothing => {
                            skip(&mut ar)?;
                            continue;
                        }
                        PlanOutcome::Write(dest) if entry.is_dir => {
                            std::fs::create_dir_all(&dest)?;
                            report.ok += 1;
                            skip(&mut ar)?;
                            continue;
                        }
                        PlanOutcome::Write(dest) => {
                            let mut extract_failed = false;
                            write_via_temp(&dest, req.keep_broken, |tmp| {
                                let mut bytes = 0u64;
                                let path = entry.path.as_str();
                                let mut hook = |n: u64| {
                                    bytes += n;
                                    ctl.progress(path, done + bytes, total);
                                    !ctl.cancelled()
                                };
                                if let Err(f) =
                                    ar.process(ffi::OP_EXTRACT, Some(tmp), Some(&mut hook))
                                {
                                    extract_failed = true;
                                    let e = self.map_err(f);
                                    // Keep (when keep_broken) only output UnRAR flagged as
                                    // damaged data. A cancel, a missing password or a disk
                                    // error is not corruption, so the partial file goes.
                                    // Note a wrong password on a RAR4 file also surfaces
                                    // as a data error, so its garbage can be kept.
                                    if !matches!(e, ArchiveError::Corrupt(_)) {
                                        let _ = std::fs::remove_file(tmp);
                                    }
                                    return Err(e);
                                }
                                ensure_plain_file(tmp)
                            })
                            .map_err(|e| (e, extract_failed))
                        }
                    }
                }
            };
            match attempt {
                Ok(()) => {
                    report.ok += 1;
                    done += entry.size;
                }
                // A damaged file leaves the handle in an unknown state: record it,
                // then carry on with the entries after it on a fresh handle.
                Err((error @ ArchiveError::Corrupt(_), true)) => {
                    report.failed.push(FileFailure {
                        path: entry.path.clone(),
                        error,
                    });
                    match self.reopen_after(pw, keep_broken, index) {
                        Ok(a) => ar = a,
                        Err(error) => {
                            report.failed.push(FileFailure {
                                path: self.path.to_string_lossy().into_owned(),
                                error,
                            });
                            broke = true;
                            break;
                        }
                    }
                }
                // Extracted fine but the result was not a plain file (defence in
                // depth against redirects): the temp file is gone, go on.
                Err((error @ ArchiveError::Unsupported(_), false)) => {
                    report.failed.push(FileFailure {
                        path: entry.path.clone(),
                        error,
                    });
                }
                Err((e, _)) => return Err(e),
            }
        }
        if !broke {
            if let Some(error) = self.truncation(pw) {
                report.failed.push(FileFailure {
                    path: self.path.to_string_lossy().into_owned(),
                    error,
                });
            }
        }
        Ok(report)
    }

    /// UnRAR reports a normal end when the file simply stops at a header boundary, so
    /// verify that the last volume ends with an end-of-archive block. Not possible for
    /// header-encrypted archives (the block is encrypted).
    fn truncation(&self, pw: Option<&Password>) -> Option<ArchiveError> {
        if let Ok(a) = self.open(ffi::OM_LIST, pw, false, false) {
            if a.flags.enc_headers {
                return None;
            }
        }
        let first = Archive::new(&self.path);
        let last = (1..=9999)
            .map_while(|n| {
                first
                    .nth_part(n)
                    .filter(|p| std::fs::symlink_metadata(p).is_ok())
            })
            .last()
            .unwrap_or_else(|| self.path.clone());
        match end_marker_state(&last) {
            Some(false) => Some(ArchiveError::Corrupt(
                "archive is truncated: end-of-archive block missing".into(),
            )),
            _ => None,
        }
    }

    fn list_inner(&self, pw: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
        let mut ar = self.open(ffi::OM_LIST_INCSPLIT, pw, false, false)?;
        if ar.flags.has_comment {
            // Only archives with plain headers can have a readable comment here.
            ar = self.open(ffi::OM_LIST_INCSPLIT, pw, false, true)?;
        }
        let info = ArchiveInfo {
            format: Format::Rar,
            file_size: std::fs::metadata(&self.path)?.len(),
            encrypted_headers: ar.flags.enc_headers,
            solid: ar.flags.solid,
            multi_volume: ar.flags.volume,
            has_recovery: ar.flags.recovery,
            locked: ar.flags.locked,
            comment: if ar.flags.has_comment {
                Some(ar.comment.clone().unwrap_or_default())
            } else {
                None
            },
        };
        let mut entries = vec![];
        while let Some(h) = ar.read_header().map_err(|f| self.map_err(f))? {
            let e = to_entry(&h);
            if h.split_before {
                // Another volume's part of the file before it, not a row of its own. With
                // the set opened at a later volume, the file's start is not there to add to.
                if let Some(start) = entries.last_mut().filter(|s: &&mut Entry| s.path == e.path) {
                    start.packed = start.packed.map(|p| p.saturating_add(h.packed_size));
                }
            } else {
                entries.push(e);
            }
            ar.process(ffi::OP_SKIP, None, None)
                .map_err(|f| self.map_err(f))?;
        }
        Ok((info, entries))
    }
}

fn to_entry(h: &Header) -> Entry {
    Entry {
        path: normalize_entry_path(&h.filename)
            .trim_end_matches('/')
            .to_string(),
        is_dir: h.is_dir,
        size: if h.is_dir { 0 } else { h.unpacked_size },
        packed: (!h.is_dir).then_some(h.packed_size),
        modified: h.mtime,
        // RAR5 stores a keyed hash instead of the CRC for encrypted files.
        crc: (!h.is_dir && !h.encrypted).then_some(h.crc),
        encrypted: h.encrypted,
    }
}

/// Symbolic links, hard links and file references. The authoritative signal is the
/// redirect record (`redir_type`); the Unix mode bits are only a second opinion,
/// because the attributes alone can be forged to look like a regular file.
fn is_redirect(h: &Header) -> bool {
    h.redir_type != 0 || (h.host_unix && h.attr & 0o170000 == 0o120000)
}

/// `Some(true)` if the file ends with an end-of-archive block, `Some(false)` if it does
/// not, `None` if the format is not recognised (then nothing is claimed).
fn end_marker_state(path: &Path) -> Option<bool> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let mut sig = [0u8; 8];
    let n = f.read(&mut sig).ok()?;
    let rar5 = n >= 8 && sig == *b"Rar!\x1a\x07\x01\x00";
    let rar4 = n >= 7 && sig[..7] == *b"Rar!\x1a\x07\x00";
    if !rar5 && !rar4 {
        return None;
    }
    let want = len.min(32) as usize;
    let mut tail = vec![0u8; want];
    f.seek(SeekFrom::Start(len - want as u64)).ok()?;
    f.read_exact(&mut tail).ok()?;
    if rar5 {
        // CRC32 | size=3 | type=5 | header flags (0, or 4 as rar writes it) | end flags (bit 0: more volumes)
        let ok = tail.len() >= 8 && {
            let b = &tail[tail.len() - 8..];
            b[4] == 3
                && b[5] == 5
                && b[6] & !0x04 == 0
                && b[7] <= 1
                && u32::from_le_bytes([b[0], b[1], b[2], b[3]]) == crc32(&b[4..])
        };
        Some(ok)
    } else {
        // CRC16 | type 0x7B | flags (2) | size (2, includes optional fields)
        let ok = (7..=tail.len().min(20)).any(|k| {
            let b = &tail[tail.len() - k..];
            b[2] == 0x7B && u16::from_le_bytes([b[5], b[6]]) as usize == k
        });
        Some(ok)
    }
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Second layer: what UnRAR left at the temp path must be a plain file with a
/// single name. Otherwise it is removed (the link itself, never its target).
fn ensure_plain_file(p: &Path) -> Result<(), ArchiveError> {
    let m = std::fs::symlink_metadata(p)?;
    #[cfg(unix)]
    let single = {
        use std::os::unix::fs::MetadataExt;
        m.nlink() == 1
    };
    #[cfg(not(unix))]
    let single = true;
    if m.is_file() && single {
        return Ok(());
    }
    let _ = std::fs::remove_file(p);
    Err(ArchiveError::Unsupported("link".into()))
}

impl ArchiveEngine for RarEngine {
    fn list(&mut self, pw: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
        self.list_inner(pw)
    }

    fn extract(
        &mut self,
        pw: Option<&Password>,
        req: &ExtractRequest,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError> {
        self.process(pw, Some(req), ctl)
    }

    fn test(
        &mut self,
        pw: Option<&Password>,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError> {
        self.process(pw, None, ctl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ConflictChoice, OverwritePolicy, TestCtl};
    use std::fs;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn pw(s: &str) -> Password {
        Password::new(s.to_string())
    }

    fn ctl() -> TestCtl {
        TestCtl {
            cancel_after: None,
            choice: ConflictChoice::Overwrite,
            seen: vec![],
        }
    }

    fn req(dest: &Path) -> ExtractRequest {
        ExtractRequest {
            dest: dest.to_path_buf(),
            selection: vec![],
            base: String::new(),
            keep_paths: true,
            overwrite: OverwritePolicy::Overwrite,
            keep_broken: false,
        }
    }

    /// Every path under `root` (files, dirs, links), relative, sorted.
    fn walk(root: &Path) -> Vec<String> {
        fn go(dir: &Path, root: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(dir).unwrap() {
                let e = e.unwrap();
                let p = e.path();
                out.push(p.strip_prefix(root).unwrap().to_string_lossy().into_owned());
                if fs::symlink_metadata(&p).unwrap().is_dir() {
                    go(&p, root, out);
                }
            }
        }
        let mut out = vec![];
        go(root, root, &mut out);
        out.sort();
        out
    }

    fn files_only(root: &Path) -> Vec<String> {
        walk(root)
            .into_iter()
            .filter(|p| !fs::symlink_metadata(root.join(p)).unwrap().is_dir())
            .collect()
    }

    fn list_plain(name: &str) -> Vec<Entry> {
        RarEngine::new(&fixture(name)).list(None).unwrap().1
    }

    #[test]
    fn lists_rar5_with_turkish_and_emoji_names() {
        let (info, entries) = RarEngine::new(&fixture("plain5.rar")).list(None).unwrap();
        assert!(!info.encrypted_headers);
        let get = |p: &str| {
            entries
                .iter()
                .find(|e| e.path == p)
                .unwrap_or_else(|| panic!("{p}"))
        };
        let emoji = get("türkçe ğüşıöç/😀.txt");
        assert_eq!(
            (emoji.size, emoji.is_dir, emoji.encrypted),
            (6, false, false)
        );
        assert_eq!(get("belgeler/alt/notlar.txt").size, 7);
        assert_eq!(get("belgeler/rapor.txt").size, 14);
        assert_eq!(get("kok.txt").size, 4);
        for d in ["belgeler", "belgeler/alt", "türkçe ğüşıöç"] {
            assert!(get(d).is_dir, "{d}");
        }
        // make-fixtures.sh sets every mtime to 2020-01-02 03:04:05 UTC.
        assert_eq!(get("kok.txt").modified, Some(1_577_934_245));
        assert!(get("kok.txt").crc.is_some());
        // Stored or compressed, a 4-byte file takes up something; a folder has no data.
        assert!(get("kok.txt").packed.is_some_and(|p| p > 0));
        assert_eq!(get("belgeler").packed, None);
    }

    #[test]
    fn listing_a_set_with_its_last_volume_gone_says_so() {
        let src = tempfile::tempdir().unwrap();
        let only = src.path().join("multi.part1.rar");
        std::fs::copy(fixture("multi.part1.rar"), &only).unwrap();
        match RarEngine::new(&only).list(None) {
            Err(ArchiveError::MissingVolume(n)) => assert_eq!(n, "multi.part2.rar"),
            other => panic!("{:?}", other.map(|(_, e)| e.len())),
        }
    }

    #[test]
    fn packed_size_of_a_split_entry_adds_up_its_volumes() {
        let entries = list_plain("multi.part1.rar");
        let big: Vec<&Entry> = entries.iter().filter(|e| e.path == "buyuk.bin").collect();
        assert_eq!(big.len(), 1, "one row, however many volumes it spans");
        assert_eq!(big[0].size, 20480);
        // Random bytes do not compress, and neither volume alone can hold them.
        let part1 = std::fs::metadata(fixture("multi.part1.rar")).unwrap().len();
        let packed = big[0].packed.unwrap();
        assert!(packed > part1, "only the first volume was counted: {packed}");
        assert!((20480..21000).contains(&packed), "{packed}");
        assert!(entries.iter().any(|e| e.path == "kok.txt" && e.packed.is_some()));
    }

    #[test]
    fn lists_rar4() {
        let entries = list_plain("plain4.rar");
        let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
        assert!(paths.contains(&"belgeler/alt/notlar.txt"));
        assert!(paths.contains(&"kok.txt"));
        assert!(entries.iter().any(|e| e.path == "belgeler" && e.is_dir));
    }

    #[test]
    fn listing_header_encrypted_without_password_requires_it() {
        let r = RarEngine::new(&fixture("enc-headers.rar")).list(None);
        assert!(matches!(r, Err(ArchiveError::PasswordRequired)), "{r:?}");
    }

    #[test]
    fn listing_header_encrypted_with_wrong_password() {
        let r = RarEngine::new(&fixture("enc-headers.rar")).list(Some(&pw("yanlis")));
        assert!(matches!(r, Err(ArchiveError::WrongPassword)), "{r:?}");
    }

    #[test]
    fn listing_header_encrypted_with_password_works() {
        let (info, entries) = RarEngine::new(&fixture("enc-headers.rar"))
            .list(Some(&pw("Test123")))
            .unwrap();
        assert!(info.encrypted_headers);
        assert!(entries.iter().any(|e| e.path == "kok.txt" && e.encrypted));
    }

    #[test]
    fn enc_lists_without_password_marks_encrypted() {
        let (info, entries) = RarEngine::new(&fixture("enc.rar")).list(None).unwrap();
        assert!(!info.encrypted_headers);
        for e in entries.iter().filter(|e| !e.is_dir) {
            assert!(e.encrypted, "{}", e.path);
        }
    }

    #[test]
    fn extract_enc_wrong_password_errors_no_partial_files() {
        let dest = tempfile::tempdir().unwrap();
        let r = RarEngine::new(&fixture("enc.rar")).extract(
            Some(&pw("yanlis")),
            &req(dest.path()),
            &mut ctl(),
        );
        let failed = match &r {
            Err(ArchiveError::WrongPassword) => true,
            Ok(rep) => !rep.failed.is_empty(),
            _ => false,
        };
        assert!(failed, "{r:?}");
        assert_eq!(files_only(dest.path()), Vec::<String>::new());
        assert!(!walk(dest.path()).iter().any(|p| p.contains("ambar-part")));
    }

    #[test]
    fn folders_with_no_output_are_not_counted_as_skipped() {
        let dest = tempfile::tempdir().unwrap();
        let mut r = req(dest.path());
        r.selection = vec!["belgeler".into()];
        r.base = "belgeler".into();
        let rep = RarEngine::new(&fixture("plain5.rar")).extract(None, &r, &mut ctl()).unwrap();
        assert_eq!(rep.skipped, 0, "the base folder itself");
        assert!(dest.path().join("rapor.txt").is_file());

        let dest = tempfile::tempdir().unwrap();
        let mut r = req(dest.path());
        r.keep_paths = false;
        let rep = RarEngine::new(&fixture("plain5.rar")).extract(None, &r, &mut ctl()).unwrap();
        assert_eq!(rep.skipped, 0, "folders when flattening");
        assert!(dest.path().join("rapor.txt").is_file());
    }

    #[test]
    fn extract_enc_without_password_requires_it() {
        let dest = tempfile::tempdir().unwrap();
        let r = RarEngine::new(&fixture("enc.rar")).extract(None, &req(dest.path()), &mut ctl());
        assert!(matches!(r, Err(ArchiveError::PasswordRequired)), "{r:?}");
        assert_eq!(files_only(dest.path()), Vec::<String>::new());
    }

    #[test]
    fn extract_enc_with_password_works() {
        let dest = tempfile::tempdir().unwrap();
        let rep = RarEngine::new(&fixture("enc.rar"))
            .extract(Some(&pw("Test123")), &req(dest.path()), &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty());
        assert_eq!(
            fs::read_to_string(dest.path().join("kok.txt")).unwrap(),
            "kok\n"
        );
    }

    #[test]
    fn extract_all_content_matches() {
        let dest = tempfile::tempdir().unwrap();
        let rep = RarEngine::new(&fixture("plain5.rar"))
            .extract(None, &req(dest.path()), &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
        let rd = |p: &str| fs::read_to_string(dest.path().join(p)).unwrap();
        assert_eq!(rd("belgeler/rapor.txt"), "rapor icerigi\n");
        assert_eq!(rd("belgeler/alt/notlar.txt"), "notlar\n");
        assert_eq!(rd("türkçe ğüşıöç/😀.txt"), "emoji\n");
        assert_eq!(rd("kok.txt"), "kok\n");
        assert!(dest.path().join("belgeler/alt").is_dir());
        assert!(!walk(dest.path()).iter().any(|p| p.contains("ambar-part")));
    }

    #[test]
    fn extract_rar4_content_matches() {
        let dest = tempfile::tempdir().unwrap();
        let rep = RarEngine::new(&fixture("plain4.rar"))
            .extract(None, &req(dest.path()), &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
        assert_eq!(
            fs::read_to_string(dest.path().join("belgeler/alt/notlar.txt")).unwrap(),
            "notlar\n"
        );
    }

    #[test]
    fn extract_selection_only_with_base() {
        let dest = tempfile::tempdir().unwrap();
        let mut r = req(dest.path());
        r.selection = vec!["belgeler/alt".into()];
        r.base = "belgeler".into();
        RarEngine::new(&fixture("plain5.rar"))
            .extract(None, &r, &mut ctl())
            .unwrap();
        assert_eq!(files_only(dest.path()), vec!["alt/notlar.txt".to_string()]);
    }

    #[test]
    fn extract_solid_selected_late_file_correct() {
        let dest = tempfile::tempdir().unwrap();
        let mut r = req(dest.path());
        r.selection = vec!["kok.txt".into(), "türkçe ğüşıöç/😀.txt".into()];
        let rep = RarEngine::new(&fixture("solid.rar"))
            .extract(None, &r, &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
        assert_eq!(
            files_only(dest.path()),
            vec!["kok.txt".to_string(), "türkçe ğüşıöç/😀.txt".to_string()]
        );
        assert_eq!(
            fs::read_to_string(dest.path().join("kok.txt")).unwrap(),
            "kok\n"
        );
        assert_eq!(
            fs::read_to_string(dest.path().join("türkçe ğüşıöç/😀.txt")).unwrap(),
            "emoji\n"
        );
    }

    fn buyuk() -> Vec<u8> {
        fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/buyuk.bin")).unwrap()
    }

    #[test]
    fn extract_from_part1_and_part2_reassembles() {
        let dest = tempfile::tempdir().unwrap();
        let rep = RarEngine::new(&fixture("multi.part1.rar"))
            .extract(None, &req(dest.path()), &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
        assert_eq!(fs::read(dest.path().join("buyuk.bin")).unwrap(), buyuk());
        assert_eq!(
            fs::read_to_string(dest.path().join("kok.txt")).unwrap(),
            "kok\n"
        );
    }

    #[test]
    fn opening_part2_uses_first_part() {
        let mut eng = RarEngine::new(&fixture("multi.part2.rar"));
        let (info, entries) = eng.list(None).unwrap();
        assert!(info.multi_volume);
        assert!(entries
            .iter()
            .any(|e| e.path == "buyuk.bin" && e.size == 20480));
        let dest = tempfile::tempdir().unwrap();
        eng.extract(None, &req(dest.path()), &mut ctl()).unwrap();
        assert_eq!(fs::read(dest.path().join("buyuk.bin")).unwrap(), buyuk());
    }

    #[test]
    fn missing_part2_reports_missing_volume() {
        let src = tempfile::tempdir().unwrap();
        fs::copy(
            fixture("multi.part1.rar"),
            src.path().join("multi.part1.rar"),
        )
        .unwrap();
        let dest = tempfile::tempdir().unwrap();
        let r = RarEngine::new(&src.path().join("multi.part1.rar")).extract(
            None,
            &req(dest.path()),
            &mut ctl(),
        );
        match r {
            Err(ArchiveError::MissingVolume(n)) => assert_eq!(n, "multi.part2.rar"),
            other => panic!("{other:?}"),
        }
        assert_eq!(files_only(dest.path()), Vec::<String>::new());
    }

    #[test]
    fn test_ok_on_plain() {
        let rep = RarEngine::new(&fixture("plain5.rar"))
            .test(None, &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty());
        assert_eq!(rep.ok, 4);
    }

    #[test]
    fn test_reports_crc_failure_on_corrupt() {
        let rep = RarEngine::new(&fixture("corrupt.rar"))
            .test(None, &mut ctl())
            .unwrap();
        assert_eq!(rep.failed.len(), 1, "{:?}", rep.failed);
        assert_eq!(rep.failed[0].path, "belgeler/rapor.txt");
        assert!(matches!(rep.failed[0].error, ArchiveError::Corrupt(_)));
        // The other three files are still verified after the bad one.
        assert_eq!(rep.ok, 3);
    }

    #[test]
    fn extract_corrupt_fails_one_file_and_leaves_no_partial() {
        let dest = tempfile::tempdir().unwrap();
        let rep = RarEngine::new(&fixture("corrupt.rar"))
            .extract(None, &req(dest.path()), &mut ctl())
            .unwrap();
        assert_eq!(rep.failed.len(), 1);
        assert_eq!(rep.failed[0].path, "belgeler/rapor.txt");
        assert!(!dest.path().join("belgeler/rapor.txt").exists());
        assert!(!walk(dest.path()).iter().any(|p| p.contains("ambar-part")));
        assert_eq!(
            fs::read_to_string(dest.path().join("kok.txt")).unwrap(),
            "kok\n"
        );
    }

    #[test]
    fn extract_corrupt_keep_broken_keeps_the_file() {
        let dest = tempfile::tempdir().unwrap();
        let mut r = req(dest.path());
        r.keep_broken = true;
        let rep = RarEngine::new(&fixture("corrupt.rar"))
            .extract(None, &r, &mut ctl())
            .unwrap();
        assert_eq!(rep.failed.len(), 1);
        let kept = dest.path().join("belgeler/rapor.txt");
        assert_eq!(fs::read(&kept).unwrap().len(), 14);
        assert_ne!(fs::read(&kept).unwrap(), b"rapor icerigi\n");
        assert!(!walk(dest.path()).iter().any(|p| p.contains("ambar-part")));
    }

    #[test]
    fn overwrite_policy_skip_keeps_existing_file() {
        let dest = tempfile::tempdir().unwrap();
        fs::write(dest.path().join("kok.txt"), "benim").unwrap();
        let mut r = req(dest.path());
        r.overwrite = OverwritePolicy::Skip;
        let rep = RarEngine::new(&fixture("plain5.rar"))
            .extract(None, &r, &mut ctl())
            .unwrap();
        assert_eq!(rep.skipped, 1);
        assert_eq!(
            fs::read_to_string(dest.path().join("kok.txt")).unwrap(),
            "benim"
        );
    }

    #[test]
    fn info_flags() {
        let info = |n: &str| RarEngine::new(&fixture(n)).list(None).unwrap().0;
        let plain = info("plain5.rar");
        assert!(!plain.solid && !plain.locked && !plain.has_recovery && !plain.multi_volume);
        assert!(plain.comment.is_none());
        assert_eq!(info("comment.rar").comment.as_deref(), Some("yorum"));
        assert!(info("locked.rar").locked);
        assert!(info("recovery.rar").has_recovery);
        assert!(info("solid.rar").solid);
        assert!(info("multi.part1.rar").multi_volume);
    }

    #[test]
    fn cancel_stops_extract() {
        let dest = tempfile::tempdir().unwrap();
        let mut c = ctl();
        c.cancel_after = Some(1);
        let r = RarEngine::new(&fixture("plain5.rar")).extract(None, &req(dest.path()), &mut c);
        assert!(matches!(r, Err(ArchiveError::Cancelled)), "{r:?}");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_entry_is_failed_not_created() {
        let dest = tempfile::tempdir().unwrap();
        let rep = RarEngine::new(&fixture("symlink.rar"))
            .extract(None, &req(dest.path()), &mut ctl())
            .unwrap();
        assert_eq!(rep.failed.len(), 1, "{:?}", rep.failed);
        assert_eq!(rep.failed[0].path, "l");
        assert!(matches!(rep.failed[0].error, ArchiveError::Unsupported(_)));
        assert_eq!(rep.ok, 1);
        assert_eq!(
            fs::read_to_string(dest.path().join("kok.txt")).unwrap(),
            "kok\n"
        );
        for p in walk(dest.path()) {
            let m = fs::symlink_metadata(dest.path().join(&p)).unwrap();
            assert!(!m.file_type().is_symlink(), "{p}");
        }
        assert!(!dest.path().join("l").exists());
    }

    #[test]
    fn extract_writes_exactly_the_given_path() {
        let dest = tempfile::tempdir().unwrap();
        let target = dest.path().join("tam-bu.tmp");
        let mut ar =
            ffi::Arc::open(&fixture("plain5.rar"), ffi::OM_EXTRACT, None, false, false).unwrap();
        loop {
            let h = ar.read_header().unwrap().unwrap();
            if h.filename == "kok.txt" {
                ar.process(ffi::OP_EXTRACT, Some(&target), None).unwrap();
                break;
            }
            ar.process(ffi::OP_SKIP, None, None).unwrap();
        }
        assert_eq!(walk(dest.path()), vec!["tam-bu.tmp".to_string()]);
        assert_eq!(fs::read_to_string(&target).unwrap(), "kok\n");
    }

    /// A file in the working directory that a hard link / file reference would
    /// resolve to; removed on drop. Tests only read it, so sharing the CWD is safe.
    struct CwdBait(PathBuf);
    impl CwdBait {
        fn new(name: &str) -> CwdBait {
            let p = std::env::current_dir().unwrap().join(name);
            fs::write(&p, "COLLIDING FILE IN CWD").unwrap();
            CwdBait(p)
        }
    }
    impl Drop for CwdBait {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn assert_redirect_refused(archive: &str, bait: &str, link: &str, others: &[&str]) {
        let bait = CwdBait::new(bait);
        let dest = tempfile::tempdir().unwrap();
        let rep = RarEngine::new(&fixture(archive))
            .extract(None, &req(dest.path()), &mut ctl())
            .unwrap();
        assert_eq!(rep.failed.len(), 1, "{:?}", rep.failed);
        assert_eq!(rep.failed[0].path, link);
        assert!(matches!(rep.failed[0].error, ArchiveError::Unsupported(_)));
        // The entries around the link still extract; the report is kept.
        let mut want: Vec<String> = others.iter().map(|s| s.to_string()).collect();
        want.sort();
        assert_eq!(files_only(dest.path()), want);
        assert_eq!(rep.ok, others.len() as u64);
        assert!(!dest.path().join(link).exists());
        // The bait in the working directory was neither linked nor overwritten.
        assert_eq!(
            fs::read_to_string(&bait.0).unwrap(),
            "COLLIDING FILE IN CWD"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(&bait.0).unwrap().nlink(), 1);
        }
    }

    #[test]
    fn hard_link_entry_is_failed_and_cwd_untouched() {
        assert_redirect_refused(
            "hardlink.rar",
            "ambar-hl-src.txt",
            "ambar-hl-link.txt",
            &["ambar-hl-src.txt", "ambar-hl-after.txt"],
        );
    }

    #[test]
    fn file_reference_entry_is_failed_and_cwd_untouched() {
        assert_redirect_refused(
            "fileref.rar",
            "ambar-ref-a.txt",
            "ambar-ref-b.txt",
            &["ambar-ref-a.txt", "ambar-ref-after.txt"],
        );
    }

    #[test]
    fn test_mode_tests_links_instead_of_failing_them() {
        for (name, ok) in [("symlink.rar", 2), ("hardlink.rar", 3), ("fileref.rar", 3)] {
            let rep = RarEngine::new(&fixture(name))
                .test(None, &mut ctl())
                .unwrap();
            assert!(rep.failed.is_empty(), "{name}: {:?}", rep.failed);
            assert_eq!(rep.ok, ok, "{name}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn disguised_symlink_is_refused_even_when_flattened() {
        // Mode bits say "regular file"; only the redirect record says symlink.
        let (_, entries) = RarEngine::new(&fixture("symlink-disguised.rar"))
            .list(None)
            .unwrap();
        assert!(entries.iter().any(|e| e.path == "a/b/l"));
        let dest = tempfile::tempdir().unwrap();
        let mut r = req(dest.path());
        r.keep_paths = false;
        let rep = RarEngine::new(&fixture("symlink-disguised.rar"))
            .extract(None, &r, &mut ctl())
            .unwrap();
        assert_eq!(rep.failed.len(), 1, "{:?}", rep.failed);
        assert_eq!(rep.failed[0].path, "a/b/l");
        assert!(matches!(rep.failed[0].error, ArchiveError::Unsupported(_)));
        assert_eq!(files_only(dest.path()), vec!["kok.txt".to_string()]);
        assert!(fs::symlink_metadata(dest.path().join("l")).is_err());
        assert!(fs::symlink_metadata(dest.path().join("ESCAPED")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn ensure_plain_file_rejects_symlink_and_hard_link() {
        let d = tempfile::tempdir().unwrap();
        let real = d.path().join("real");
        fs::write(&real, "x").unwrap();
        assert!(ensure_plain_file(&real).is_ok());
        let sym = d.path().join("sym");
        std::os::unix::fs::symlink(&real, &sym).unwrap();
        assert!(matches!(
            ensure_plain_file(&sym),
            Err(ArchiveError::Unsupported(_))
        ));
        assert!(fs::symlink_metadata(&sym).is_err());
        let hard = d.path().join("hard");
        fs::hard_link(&real, &hard).unwrap();
        assert!(matches!(
            ensure_plain_file(&hard),
            Err(ArchiveError::Unsupported(_))
        ));
        // Only the extra name is removed, never the target.
        assert_eq!(fs::read_to_string(&real).unwrap(), "x");
    }

    #[test]
    fn missing_part1_reports_missing_volume() {
        let src = tempfile::tempdir().unwrap();
        fs::copy(
            fixture("multi.part2.rar"),
            src.path().join("multi.part2.rar"),
        )
        .unwrap();
        let r = RarEngine::new(&src.path().join("multi.part2.rar")).list(None);
        match r {
            Err(ArchiveError::MissingVolume(n)) => assert_eq!(n, "multi.part1.rar"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn keep_paths_false_flattens_into_dest() {
        let dest = tempfile::tempdir().unwrap();
        let mut r = req(dest.path());
        r.keep_paths = false;
        RarEngine::new(&fixture("plain5.rar"))
            .extract(None, &r, &mut ctl())
            .unwrap();
        assert_eq!(
            files_only(dest.path()),
            vec!["kok.txt", "notlar.txt", "rapor.txt", "😀.txt"]
        );
    }

    #[test]
    fn rename_policy_keeps_both_files() {
        let dest = tempfile::tempdir().unwrap();
        fs::write(dest.path().join("kok.txt"), "benim").unwrap();
        let mut r = req(dest.path());
        r.overwrite = OverwritePolicy::Rename;
        RarEngine::new(&fixture("plain5.rar"))
            .extract(None, &r, &mut ctl())
            .unwrap();
        assert_eq!(
            fs::read_to_string(dest.path().join("kok.txt")).unwrap(),
            "benim"
        );
        assert_eq!(
            fs::read_to_string(dest.path().join("kok (1).txt")).unwrap(),
            "kok\n"
        );
    }

    #[test]
    fn ask_policy_follows_the_choice() {
        for (choice, kept, skipped) in [
            (ConflictChoice::Skip, "benim", 1),
            (ConflictChoice::Overwrite, "kok\n", 0),
        ] {
            let dest = tempfile::tempdir().unwrap();
            fs::write(dest.path().join("kok.txt"), "benim").unwrap();
            let mut r = req(dest.path());
            r.overwrite = OverwritePolicy::Ask;
            let mut c = ctl();
            c.choice = choice;
            let rep = RarEngine::new(&fixture("plain5.rar"))
                .extract(None, &r, &mut c)
                .unwrap();
            assert_eq!(rep.skipped, skipped);
            assert_eq!(
                fs::read_to_string(dest.path().join("kok.txt")).unwrap(),
                kept
            );
        }
        let dest = tempfile::tempdir().unwrap();
        fs::write(dest.path().join("kok.txt"), "benim").unwrap();
        let mut r = req(dest.path());
        r.overwrite = OverwritePolicy::Ask;
        let mut c = ctl();
        c.choice = ConflictChoice::Cancel;
        let res = RarEngine::new(&fixture("plain5.rar")).extract(None, &r, &mut c);
        assert!(matches!(res, Err(ArchiveError::Cancelled)), "{res:?}");
    }

    #[test]
    fn cancel_inside_a_file_leaves_nothing_in_both_keep_broken_modes() {
        for keep in [false, true] {
            let dest = tempfile::tempdir().unwrap();
            let mut r = req(dest.path());
            r.keep_broken = keep;
            let mut c = ctl();
            // Not cancelled before the first entry starts, cancelled at its first data chunk.
            c.cancel_after = Some(1);
            let res = RarEngine::new(&fixture("stored.rar")).extract(None, &r, &mut c);
            assert!(
                matches!(res, Err(ArchiveError::Cancelled)),
                "keep={keep}: {res:?}"
            );
            assert_eq!(walk(dest.path()), Vec::<String>::new(), "keep={keep}");
            // A new engine on the same archive still extracts everything.
            let rep = RarEngine::new(&fixture("stored.rar"))
                .extract(None, &r, &mut ctl())
                .unwrap();
            assert!(rep.failed.is_empty());
            assert_eq!(fs::read(dest.path().join("buyuk.bin")).unwrap(), buyuk());
        }
    }

    #[test]
    fn cancel_inside_a_file_during_test_mode() {
        let mut c = ctl();
        c.cancel_after = Some(1);
        let res = RarEngine::new(&fixture("stored.rar")).test(None, &mut c);
        assert!(matches!(res, Err(ArchiveError::Cancelled)), "{res:?}");
    }

    #[test]
    fn progress_reports_bytes_while_a_file_is_written() {
        struct P(Vec<u64>);
        impl JobCtl for P {
            fn progress(&mut self, _: &str, done: u64, _: u64) {
                self.0.push(done);
            }
            fn cancelled(&self) -> bool {
                false
            }
            fn resolve_conflict(&mut self, _: &Path) -> ConflictChoice {
                ConflictChoice::Overwrite
            }
        }
        let dest = tempfile::tempdir().unwrap();
        let mut p = P(vec![]);
        RarEngine::new(&fixture("stored.rar"))
            .extract(None, &req(dest.path()), &mut p)
            .unwrap();
        assert!(p.0.contains(&20480), "{:?}", p.0);
    }

    #[test]
    fn truncated_archive_is_never_reported_clean() {
        let whole = fs::read(fixture("plain5.rar")).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let cut = dir.path().join("cut.rar");
        for len in 0..whole.len() {
            fs::write(&cut, &whole[..len]).unwrap();
            let dest = tempfile::tempdir().unwrap();
            let t = RarEngine::new(&cut).test(None, &mut ctl());
            assert!(
                t.is_err() || t.as_ref().is_ok_and(|r| !r.failed.is_empty()),
                "test at {len}: {t:?}"
            );
            let x = RarEngine::new(&cut).extract(None, &req(dest.path()), &mut ctl());
            assert!(
                x.is_err() || x.as_ref().is_ok_and(|r| !r.failed.is_empty()),
                "extract at {len}: {x:?}"
            );
        }
    }

    #[test]
    fn every_whole_fixture_passes_the_end_marker_check() {
        for n in [
            "plain5.rar",
            "plain4.rar",
            "solid.rar",
            "comment.rar",
            "locked.rar",
            "recovery.rar",
            "multi.part1.rar",
            "multi.part2.rar",
            "stored.rar",
            "enc.rar",
        ] {
            let rep = RarEngine::new(&fixture(n)).test(None, &mut ctl());
            let rep = match (n, rep) {
                ("enc.rar", Err(ArchiveError::PasswordRequired)) => continue,
                (_, r) => r.unwrap_or_else(|e| panic!("{n}: {e:?}")),
            };
            assert!(rep.failed.is_empty(), "{n}: {:?}", rep.failed);
        }
        let rep = RarEngine::new(&fixture("enc-headers.rar"))
            .test(Some(&pw("Test123")), &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    }

    #[test]
    fn missing_data_to_disk_full_mapping() {
        let e = RarEngine::new(Path::new("x.rar")).map_err(ffi::Fail {
            code: ffi::ERR_EWRITE,
            when: ffi::When::Process,
            volume: None,
            cancelled: false,
        });
        assert!(matches!(e, ArchiveError::DiskFull), "{e:?}");
    }
}
