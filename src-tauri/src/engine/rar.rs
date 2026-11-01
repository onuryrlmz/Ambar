//! RAR engine on top of `unrar-ng` (the UnRAR library). Extraction never uses the
//! crate's own path handling (`extract_all`, `extract_with_base`): each entry goes
//! through `plan_entry` and is written to a temp file via `write_via_temp`, so the
//! security boundary stays in `plan.rs`.

use super::plan::{is_selected, normalize_entry_path, plan_entry, write_via_temp, PlanOutcome};
use super::{
    ArchiveEngine, ArchiveInfo, Entry, ExtractRequest, FileFailure, Format, JobCtl, OpReport,
    Password,
};
use crate::error::ArchiveError;
use std::path::{Path, PathBuf};
use unrar_ng::error::{Code, UnrarError, When};
use unrar_ng::{Archive, CursorBeforeHeader, FileHeader, List, OpenArchive, Process, VolumeInfo};

pub struct RarEngine {
    path: PathBuf,
}

impl RarEngine {
    pub fn new(path: &Path) -> RarEngine {
        RarEngine {
            path: path.to_path_buf(),
        }
    }

    fn archive<'a>(&'a self, pw: Option<&'a Password>) -> Result<Archive<'a>, ArchiveError> {
        let a = match pw {
            // UnRAR copies the password into a C string; a NUL cannot be represented.
            Some(p) if p.contains('\0') => return Err(ArchiveError::WrongPassword),
            Some(p) => Archive::with_password(&self.path, p.as_str()),
            None => Archive::new(&self.path),
        };
        Ok(a.as_first_part())
    }

    fn open_listing(
        &self,
        pw: Option<&Password>,
    ) -> Result<OpenArchive<List, CursorBeforeHeader>, ArchiveError> {
        self.archive(pw)?
            .open_for_listing()
            .map_err(|e| self.map_err(&e))
    }

    fn open_processing(
        &self,
        pw: Option<&Password>,
    ) -> Result<OpenArchive<Process, CursorBeforeHeader>, ArchiveError> {
        self.archive(pw)?
            .open_for_processing()
            .map_err(|e| self.map_err(&e))
    }

    fn map_err(&self, e: &UnrarError) -> ArchiveError {
        match e.code {
            Code::MissingPassword => ArchiveError::PasswordRequired,
            Code::BadPassword => ArchiveError::WrongPassword,
            Code::BadData | Code::BadArchive => ArchiveError::Corrupt(e.to_string()),
            Code::UnknownFormat => ArchiveError::NotAnArchive,
            Code::EOpen if e.when != When::Open => match self.missing_volume() {
                Some(name) => ArchiveError::MissingVolume(name),
                None => ArchiveError::Io(e.to_string()),
            },
            Code::LargeDict | Code::EReference => ArchiveError::Unsupported(e.to_string()),
            _ => ArchiveError::Io(e.to_string()),
        }
    }

    /// File name of the first volume after part 1 that is absent from disk.
    fn missing_volume(&self) -> Option<String> {
        let a = Archive::new(&self.path);
        (2..=9999).find_map(|n| {
            let p = a.nth_part(n)?;
            if std::fs::symlink_metadata(&p).is_ok() {
                None
            } else {
                Some(p.file_name()?.to_string_lossy().into_owned())
            }
        })
    }

    /// Re-opens the archive and moves past the first `n` entries (after a failure
    /// consumed the previous handle).
    fn reopen_after(
        &self,
        pw: Option<&Password>,
        n: usize,
    ) -> Result<OpenArchive<Process, CursorBeforeHeader>, ArchiveError> {
        let mut ar = self.open_processing(pw)?;
        for _ in 0..n {
            let h = ar
                .read_header()
                .map_err(|e| self.map_err(&e))?
                .ok_or_else(|| ArchiveError::Corrupt("unexpected end of archive".into()))?;
            ar = h.skip().map_err(|e| self.map_err(&e))?;
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
        let wanted = |path: &str| req.is_none_or(|r| is_selected(path, &r.selection));
        let total: u64 = entries
            .iter()
            .filter(|e| !e.is_dir && wanted(&e.path))
            .map(|e| e.size)
            .sum();

        let mut report = OpReport::default();
        let mut done = 0u64;
        let mut index = 0usize; // headers consumed from the current handle's start
        let mut ar = self.open_processing(pw)?;
        loop {
            if ctl.cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let Some(h) = ar.read_header().map_err(|e| self.map_err(&e))? else {
                break;
            };
            index += 1;
            let entry = to_entry(h.entry());
            if !wanted(&entry.path) {
                ar = h.skip().map_err(|e| self.map_err(&e))?;
                continue;
            }
            if is_link(h.entry()) {
                report.failed.push(FileFailure {
                    path: entry.path.clone(),
                    error: ArchiveError::Unsupported("symbolic link".into()),
                });
                ar = h.skip().map_err(|e| self.map_err(&e))?;
                continue;
            }
            ctl.progress(&entry.path, done, total);

            let attempt: Result<OpenArchive<Process, CursorBeforeHeader>, ArchiveError> = match req
            {
                // Test mode: verify files, nothing is written.
                None if entry.is_dir => {
                    ar = h.skip().map_err(|e| self.map_err(&e))?;
                    continue;
                }
                None => h.test().map_err(|e| self.map_err(&e)),
                Some(req) => {
                    let plan = match plan_entry(&entry, req, ctl) {
                        Ok(p) => p,
                        Err(e @ (ArchiveError::UnsafePath(_) | ArchiveError::DestConflict(_))) => {
                            report.failed.push(FileFailure {
                                path: entry.path.clone(),
                                error: e,
                            });
                            ar = h.skip().map_err(|e| self.map_err(&e))?;
                            continue;
                        }
                        Err(e) => return Err(e),
                    };
                    match plan {
                        PlanOutcome::Skip => {
                            report.skipped += 1;
                            ar = h.skip().map_err(|e| self.map_err(&e))?;
                            continue;
                        }
                        PlanOutcome::Write(dest) if entry.is_dir => {
                            std::fs::create_dir_all(&dest)?;
                            report.ok += 1;
                            ar = h.skip().map_err(|e| self.map_err(&e))?;
                            continue;
                        }
                        PlanOutcome::Write(dest) => {
                            let mut slot = Some(h);
                            let mut next = None;
                            write_via_temp(&dest, req.keep_broken, |tmp| {
                                match slot.take().expect("handle").extract_to(tmp) {
                                    Ok(a) => {
                                        next = Some(a);
                                        Ok(())
                                    }
                                    Err(e) => Err(self.map_err(&e)),
                                }
                            })
                            .map(|()| next.expect("extract_to returned a handle"))
                        }
                    }
                }
            };
            match attempt {
                Ok(next) => {
                    ar = next;
                    report.ok += 1;
                    done += entry.size;
                }
                // A damaged file consumes the handle; record it, then carry on
                // with the entries after it.
                Err(error @ ArchiveError::Corrupt(_)) => {
                    report.failed.push(FileFailure {
                        path: entry.path.clone(),
                        error,
                    });
                    match self.reopen_after(pw, index) {
                        Ok(a) => ar = a,
                        Err(error) => {
                            report.failed.push(FileFailure {
                                path: self.path.to_string_lossy().into_owned(),
                                error,
                            });
                            break;
                        }
                    }
                }
                Err(e) => return Err(e),
            }
        }
        Ok(report)
    }

    fn list_inner(&self, pw: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
        let mut ar = self.open_listing(pw)?;
        let info = ArchiveInfo {
            format: Format::Rar,
            file_size: std::fs::metadata(&self.path)?.len(),
            encrypted_headers: ar.has_encrypted_headers(),
            solid: ar.is_solid(),
            multi_volume: ar.volume_info() != VolumeInfo::None,
            has_recovery: ar.has_recovery_record(),
            locked: ar.is_locked(),
            // The UnRAR library only exposes that a comment exists, not its text.
            comment: ar.has_comment().then(String::new),
        };
        let mut entries = vec![];
        while let Some(h) = ar.read_header().map_err(|e| self.map_err(&e))? {
            entries.push(to_entry(h.entry()));
            ar = h.skip().map_err(|e| self.map_err(&e))?;
        }
        Ok((info, entries))
    }
}

fn entry_path(h: &FileHeader) -> String {
    normalize_entry_path(&h.filename.to_string_lossy())
        .trim_end_matches('/')
        .to_string()
}

fn to_entry(h: &FileHeader) -> Entry {
    Entry {
        path: entry_path(h),
        is_dir: h.is_directory(),
        size: if h.is_directory() { 0 } else { h.unpacked_size },
        packed: None,
        modified: dos_to_unix(h.file_time),
        // RAR5 stores a keyed hash instead of the CRC for encrypted files.
        crc: (!h.is_directory() && !h.is_encrypted()).then_some(h.file_crc),
        encrypted: h.is_encrypted(),
    }
}

/// Unix hosts store the mode in `file_attr`; S_IFLNK marks a symbolic link.
fn is_link(h: &FileHeader) -> bool {
    h.file_attr & 0o170000 == 0o120000
}

/// MS-DOS date/time (as returned by the UnRAR library), read as UTC.
fn dos_to_unix(t: u32) -> Option<i64> {
    let (date, time) = ((t >> 16) as i64, (t & 0xFFFF) as i64);
    let (year, month, day) = (1980 + (date >> 9), (date >> 5) & 0xF, date & 0x1F);
    let (hour, min, sec) = (time >> 11, (time >> 5) & 0x3F, (time & 0x1F) * 2);
    if t == 0 || !(1..=12).contains(&month) || day == 0 || hour > 23 || min > 59 || sec > 59 {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(days * 86400 + hour * 3600 + min * 60 + sec)
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
        assert!(get("kok.txt").modified.unwrap() > 1_700_000_000);
        assert!(get("kok.txt").crc.is_some());
        assert!(get("kok.txt").packed.is_none());
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
    fn extract_corrupt_keep_broken_cannot_keep_unrar_deletes_bad_output() {
        // UnRAR removes a file that fails its CRC before we get control back, so
        // keep_broken has nothing to keep; what matters is that nothing is left half-written.
        let dest = tempfile::tempdir().unwrap();
        let mut r = req(dest.path());
        r.keep_broken = true;
        let rep = RarEngine::new(&fixture("corrupt.rar"))
            .extract(None, &r, &mut ctl())
            .unwrap();
        assert_eq!(rep.failed.len(), 1);
        assert!(!dest.path().join("belgeler/rapor.txt").exists());
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
        assert!(info("comment.rar").comment.is_some());
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
    fn extract_to_writes_exactly_the_given_path() {
        let dest = tempfile::tempdir().unwrap();
        let target = dest.path().join("tam-bu.tmp");
        let ar = unrar_ng::Archive::new(&fixture("plain5.rar"))
            .open_for_processing()
            .unwrap();
        let mut ar = ar;
        loop {
            let h = ar.read_header().unwrap().unwrap();
            if h.entry().filename == Path::new("kok.txt") {
                h.extract_to(&target).unwrap();
                break;
            }
            ar = h.skip().unwrap();
        }
        assert_eq!(walk(dest.path()), vec!["tam-bu.tmp".to_string()]);
        assert_eq!(fs::read_to_string(&target).unwrap(), "kok\n");
    }
}
