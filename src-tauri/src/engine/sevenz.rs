//! 7z engine. Extraction never uses the crate's `decompress*` helpers: it walks the
//! blocks with `BlockDecoder`, so every output path goes through `plan_entry` and
//! every file is written via `write_via_temp`, with cancel checked in the copy loop.
//! Link attributes are never materialised.

use super::plan::{
    copy_stream, normalize_entry_path, plan_entry, skip_stream, write_entry, Exact, Selection,
    PlanOutcome,
};
use super::{
    ArchiveEngine, ArchiveInfo, Entry, ExtractRequest, FileFailure, Format, JobCtl, OpReport,
    Password,
};
use crate::error::ArchiveError;
use sevenz_rust2::{Archive, BlockDecoder, EncoderMethod, Error as SzError};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;
const UNIX_EXTENSION: u32 = 0x8000;
const REPARSE_POINT: u32 = 0x400;
/// 100 ns ticks between 1601-01-01 and 1970-01-01, in seconds.
const NT_TO_UNIX_SECS: i64 = 11_644_473_600;

pub struct SevenZEngine {
    path: PathBuf,
}

struct Meta {
    entry: Entry,
    /// Links and special files: never created.
    link: bool,
    anti: bool,
}

struct Opened {
    file: File,
    archive: Archive,
    header_encrypted: bool,
    pw: sevenz_rust2::Password,
}

fn map_err(e: SzError) -> ArchiveError {
    match e {
        SzError::PasswordRequired => ArchiveError::PasswordRequired,
        SzError::MaybeBadPassword(_) => ArchiveError::WrongPassword,
        SzError::Unsupported(s) => ArchiveError::Unsupported(s.into_owned()),
        SzError::UnsupportedCompressionMethod(s) => ArchiveError::Unsupported(s),
        SzError::UnsupportedVersion { major, minor } => {
            ArchiveError::Unsupported(format!("7z version {major}.{minor}"))
        }
        SzError::ExternalUnsupported => ArchiveError::Unsupported("external header".into()),
        SzError::MaxMemLimited { max_kb, actaul_kb } => ArchiveError::Unsupported(format!(
            "needs {actaul_kb} KiB of memory, limit is {max_kb} KiB"
        )),
        // Everything else the reader reports is a damaged or cut archive.
        other => ArchiveError::Corrupt(other.to_string()),
    }
}

fn is_link(ae: &sevenz_rust2::ArchiveEntry) -> bool {
    if !ae.has_windows_attributes {
        return false;
    }
    let a = ae.windows_attributes;
    if a & REPARSE_POINT != 0 {
        return true;
    }
    if a & UNIX_EXTENSION != 0 {
        let t = (a >> 16) & S_IFMT;
        return !matches!(t, 0 | S_IFREG | S_IFDIR);
    }
    false
}

impl SevenZEngine {
    pub fn new(path: &Path) -> SevenZEngine {
        SevenZEngine {
            path: path.to_path_buf(),
        }
    }

    fn open(&self, pw: Option<&Password>) -> Result<Opened, ArchiveError> {
        let mut file = File::open(&self.path)?;
        // Without a password only an archive with encrypted headers refuses to open.
        let (archive, header_encrypted, spw) =
            match Archive::read(&mut file, &sevenz_rust2::Password::empty()) {
                Ok(a) => (a, false, sevenz_rust2::Password::empty()),
                Err(SzError::PasswordRequired) => {
                    let pw = pw.ok_or(ArchiveError::PasswordRequired)?;
                    let spw = sevenz_rust2::Password::new(pw.as_str());
                    // The header is encrypted, so a failure while reading it is the key.
                    let a = Archive::read(&mut file, &spw).map_err(|e| match map_err(e) {
                        e @ (ArchiveError::Unsupported(_) | ArchiveError::Io(_)) => e,
                        _ => ArchiveError::WrongPassword,
                    })?;
                    (a, true, spw)
                }
                Err(e) => return Err(map_err(e)),
            };
        let spw = match pw {
            Some(p) if !header_encrypted => sevenz_rust2::Password::new(p.as_str()),
            _ => spw,
        };
        Ok(Opened {
            file,
            archive,
            header_encrypted,
            pw: spw,
        })
    }

    fn block_encrypted(archive: &Archive, block: Option<usize>) -> bool {
        block.is_some_and(|b| {
            archive.blocks[b]
                .coders
                .iter()
                .any(|c| c.encoder_method_id() == EncoderMethod::ID_AES256_SHA256)
        })
    }

    fn metas(archive: &Archive) -> Vec<Meta> {
        archive
            .files
            .iter()
            .enumerate()
            .map(|(i, ae)| {
                let block = archive
                    .stream_map
                    .file_block_index
                    .get(i)
                    .copied()
                    .flatten();
                Meta {
                    entry: Entry {
                        path: normalize_entry_path(&ae.name)
                            .trim_end_matches('/')
                            .to_string(),
                        is_dir: ae.is_directory,
                        size: if ae.is_directory { 0 } else { ae.size },
                        size_unknown: false,
                        packed: None,
                        modified: ae.has_last_modified_date.then(|| {
                            (u64::from(ae.last_modified_date) / 10_000_000) as i64 - NT_TO_UNIX_SECS
                        }),
                        crc: (ae.has_crc && !ae.is_directory).then_some(ae.crc as u32),
                        encrypted: ae.has_stream && Self::block_encrypted(archive, block),
                    },
                    link: is_link(ae),
                    anti: ae.is_anti_item,
                }
            })
            .collect()
    }

    fn process(
        &self,
        pw: Option<&Password>,
        req: Option<&ExtractRequest>,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError> {
        let Opened {
            mut file,
            archive,
            pw: spw,
            ..
        } = self.open(pw)?;
        let metas = Self::metas(&archive);
        let mut run = Run {
            metas: &metas,
            req,
            selection: Selection::of(req),
            ctl,
            report: OpReport::default(),
            done: 0,
            total: 0,
            block_damaged: false,
        };
        run.total = (0..metas.len())
            .filter(|&i| run.is_work(i) && !metas[i].link && !metas[i].entry.is_dir)
            .fold(0u64, |a, i| a.saturating_add(metas[i].entry.size));
        if pw.is_none()
            && (0..metas.len())
                .any(|i| run.is_work(i) && !metas[i].link && metas[i].entry.encrypted)
        {
            return Err(ArchiveError::PasswordRequired);
        }

        for b in 0..archive.blocks.len() {
            let start = archive.stream_map.block_first_file_index[b];
            let dec = BlockDecoder::new(1, b, &archive, &spw, &mut file);
            // Nothing wanted in this block: do not decode it.
            if req.is_some() && !(start..start + dec.entry_count()).any(|i| run.is_work(i)) {
                continue;
            }
            let failed_before = run.report.failed.len();
            run.block_damaged = false;
            let mut cur = start;
            let mut fatal = None;
            let res = dec.for_each_entries(&mut |_, data| {
                let i = cur;
                cur += 1;
                match run.handle(i, data) {
                    Ok(()) => Ok(true),
                    Err(e) => {
                        fatal = Some(e);
                        Ok(false)
                    }
                }
            });
            if let Some(e) = fatal {
                return Err(e);
            }
            if let Err(e) = res {
                // The rest of this block cannot be read. Say so unless an entry of it
                // already carries the failure.
                let encrypted = Self::block_encrypted(&archive, Some(b));
                let err = match map_err(e) {
                    ArchiveError::PasswordRequired => return Err(ArchiveError::PasswordRequired),
                    ArchiveError::WrongPassword | ArchiveError::Corrupt(_) if encrypted => {
                        return Err(ArchiveError::WrongPassword)
                    }
                    ArchiveError::WrongPassword => ArchiveError::Corrupt("read error".into()),
                    other => other,
                };
                if run.report.failed.len() == failed_before {
                    let path = metas.get(cur).unwrap_or(&metas[start]).entry.path.clone();
                    run.fail(&path, err);
                }
            }
        }
        // Entries without data (empty files, folders) are not part of any block.
        run.block_damaged = false;
        for i in 0..metas.len() {
            if archive.stream_map.file_block_index[i].is_none() {
                run.handle(i, &mut std::io::empty())?;
            }
        }
        Ok(run.report)
    }
}

struct Run<'a> {
    metas: &'a [Meta],
    req: Option<&'a ExtractRequest>,
    selection: Selection<'a>,
    ctl: &'a mut dyn JobCtl,
    report: OpReport,
    done: u64,
    total: u64,
    /// A read failed earlier in the block being decoded.
    block_damaged: bool,
}

impl Run<'_> {
    fn is_work(&self, i: usize) -> bool {
        let m = &self.metas[i];
        !m.entry.path.is_empty()
            && !m.anti
            && self.selection.has(&m.entry.path)
    }

    fn fail(&mut self, path: &str, error: ArchiveError) {
        self.report.failed.push(FileFailure {
            path: path.to_string(),
            error,
        });
    }

    /// Reads and drops the rest of an entry, polling cancel. A read error means the
    /// data behind it in the same block cannot be trusted: it is returned, and the
    /// block is marked damaged.
    fn drain(&mut self, data: &mut dyn Read) -> Result<Option<ArchiveError>, ArchiveError> {
        match skip_stream(data, "", self.ctl) {
            Ok(()) => Ok(None),
            Err(ArchiveError::Cancelled) => Err(ArchiveError::Cancelled),
            Err(e) => {
                self.block_damaged = true;
                Ok(Some(e))
            }
        }
    }

    /// Handles one entry; `Err` ends the whole run.
    fn handle(&mut self, idx: usize, data: &mut dyn Read) -> Result<(), ArchiveError> {
        let metas = self.metas;
        let m = &metas[idx];
        let entry = &m.entry;
        if self.ctl.cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        // The crate's reader passes an early end of data through as a normal end, so
        // every entry is held to the size its header declares.
        let mut data = Exact {
            inner: data,
            left: if entry.is_dir { 0 } else { entry.size },
        };
        let named = if entry.path.is_empty() {
            format!("#{idx}")
        } else {
            entry.path.clone()
        };
        let selected = self.selection.has(&entry.path);
        let unwanted = entry.path.is_empty() || m.anti || !selected;
        if unwanted || m.link {
            let link_failed = m.link && !unwanted && self.req.is_some();
            if link_failed {
                self.fail(&entry.path, ArchiveError::Unsupported("link".into()));
            }
            // Read what the crate would skip itself, so that cancel is polled. A read
            // error counts unless the data is truly unselected (or already failed).
            if let Some(e) = self.drain(&mut data)? {
                if !link_failed && (self.req.is_none() || selected) {
                    self.fail(&named, e);
                }
            }
            return Ok(());
        }
        let data: &mut dyn Read = &mut data;
        // Earlier data of this block failed: only a CRC of its own can vouch for this one.
        if self.block_damaged && entry.crc.is_none() && !entry.is_dir && entry.size > 0 {
            let _ = self.drain(&mut *data)?;
            if entry.encrypted {
                return Err(ArchiveError::WrongPassword);
            }
            self.fail(
                &entry.path,
                ArchiveError::Corrupt("follows damaged data in the same block".into()),
            );
            return Ok(());
        }
        self.ctl.progress(&entry.path, self.done, self.total);
        let (done, total) = (self.done, self.total);
        let result = match self.req {
            None if entry.is_dir => return Ok(()),
            None => copy_stream(data, None, &entry.path, self.ctl, done, total).map(|_| ()),
            Some(req) => {
                let plan = match plan_entry(entry, req, self.ctl) {
                    Ok(p) => p,
                    Err(e @ (ArchiveError::UnsafePath(_) | ArchiveError::DestConflict(_))) => {
                        self.fail(&entry.path, e);
                        let _ = self.drain(&mut *data)?;
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };
                match plan {
                    PlanOutcome::Skip => {
                        self.report.skipped += 1;
                        let _ = self.drain(&mut *data)?;
                        return Ok(());
                    }
                    PlanOutcome::Nothing => {
                        let _ = self.drain(&mut *data)?;
                        return Ok(());
                    }
                    PlanOutcome::Write(dest) if entry.is_dir => {
                        std::fs::create_dir_all(&dest)?;
                        self.report.ok += 1;
                        return Ok(());
                    }
                    // Garbage from a wrong key is not worth keeping.
                    PlanOutcome::Write(dest) => write_entry(
                        &dest,
                        req.keep_broken && !entry.encrypted,
                        data,
                        entry,
                        self.ctl,
                        done,
                        total,
                    ),
                }
            }
        };
        if matches!(result, Err(ArchiveError::Corrupt(_))) {
            self.block_damaged = true;
        }
        match result {
            Ok(()) => {
                self.report.ok += 1;
                self.done = self.done.saturating_add(entry.size);
                Ok(())
            }
            Err(ArchiveError::Corrupt(_)) if entry.encrypted => Err(ArchiveError::WrongPassword),
            Err(e @ ArchiveError::Corrupt(_)) => {
                self.fail(&entry.path, e);
                Ok(())
            }
            Err(e) => Err(e),
        }
    }
}

impl ArchiveEngine for SevenZEngine {
    fn list(&mut self, pw: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
        let o = self.open(pw)?;
        let entries = Self::metas(&o.archive)
            .into_iter()
            .filter(|m| !m.entry.path.is_empty() && !m.anti)
            .map(|m| m.entry)
            .collect();
        let info = ArchiveInfo {
            format: Format::SevenZ,
            file_size: std::fs::metadata(&self.path)?.len(),
            encrypted_headers: o.header_encrypted,
            solid: o.archive.is_solid,
            multi_volume: false,
            has_recovery: false,
            locked: false,
            comment: None,
        };
        Ok((info, entries))
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
    use ::sevenz_rust2::encoder_options::AesEncoderOptions;
    use ::sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderMethod};
    use std::fs;
    use std::io::Cursor;

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

    #[derive(Default)]
    struct Opts {
        password: Option<&'static str>,
        encrypt_header: bool,
        copy: bool,
    }

    /// Entries: (name, data, attributes). Names ending in `/` are directories.
    fn build(path: &Path, files: &[(&str, &[u8], Option<u32>)], o: &Opts) {
        let mut w = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
        let mut methods = vec![];
        if let Some(p) = o.password {
            methods.push(AesEncoderOptions::new(::sevenz_rust2::Password::new(p)).into());
        }
        methods.push(if o.copy {
            EncoderMethod::COPY.into()
        } else {
            EncoderMethod::LZMA2.into()
        });
        w.set_content_methods(methods);
        w.set_encrypt_header(o.encrypt_header);
        for (n, d, attr) in files {
            if let Some(dir) = n.strip_suffix('/') {
                w.push_archive_entry::<&[u8]>(ArchiveEntry::new_directory(dir), None)
                    .unwrap();
            } else {
                let mut e = ArchiveEntry::new_file(n);
                e.has_last_modified_date = true;
                e.last_modified_date =
                    ::sevenz_rust2::NtTime::from((1_700_000_000u64 + 11_644_473_600) * 10_000_000);
                if let Some(a) = attr {
                    e.has_windows_attributes = true;
                    e.windows_attributes = *a;
                }
                w.push_archive_entry(e, Some(*d)).unwrap();
            }
        }
        let c = w.finish().unwrap();
        fs::write(path, c.into_inner()).unwrap();
    }

    /// Every byte total a job reported, for a job that never listed first.
    struct Totals(Vec<u64>);
    impl JobCtl for Totals {
        fn progress(&mut self, _: &str, _: u64, total: u64) {
            self.0.push(total);
        }
        fn cancelled(&self) -> bool {
            false
        }
        fn resolve_conflict(&mut self, _: &Path) -> ConflictChoice {
            ConflictChoice::Overwrite
        }
    }

    #[test]
    fn progress_has_a_total_without_a_prior_list() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.7z");
        build(&a, &[("a.txt", b"12345", None), ("b.txt", b"678", None)], &Opts::default());
        let mut x = Totals(vec![]);
        SevenZEngine::new(&a).extract(None, &req(&d.path().join("o")), &mut x).unwrap();
        assert_eq!(x.0, [8, 8]);
        let mut t = Totals(vec![]);
        SevenZEngine::new(&a).test(None, &mut t).unwrap();
        assert_eq!(t.0, [8, 8]);
    }

    #[test]
    fn extracted_file_keeps_its_time() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a.7z");
        build(&a, &[("k/a.txt", b"dated", None)], &Opts::default());
        let out = d.path().join("out");
        let r = SevenZEngine::new(&a).extract(None, &req(&out), &mut ctl()).unwrap();
        assert_eq!((r.ok, r.failed.len()), (1, 0));
        assert_eq!(crate::engine::disk_mtime(&out.join("k/a.txt")), 1_700_000_000);
    }

    fn leftovers(root: &Path) -> Vec<String> {
        fn go(dir: &Path, root: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    go(&p, root, out);
                } else {
                    out.push(p.strip_prefix(root).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        let mut v = vec![];
        go(root, root, &mut v);
        v.sort();
        v
    }

    fn has_corrupt(r: &Result<OpReport, ArchiveError>) -> bool {
        match r {
            Err(ArchiveError::Corrupt(_)) => true,
            Ok(rep) => rep
                .failed
                .iter()
                .any(|f| matches!(f.error, ArchiveError::Corrupt(_))),
            _ => false,
        }
    }

    #[test]
    fn lists_and_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.7z");
        build(
            &a,
            &[
                ("d/", b"", None),
                ("d/x.txt", b"hello", None),
                ("empty.txt", b"", None),
                ("y.txt", b"world!", None),
            ],
            &Opts::default(),
        );
        let (info, entries) = SevenZEngine::new(&a).list(None).unwrap();
        assert_eq!(info.format, Format::SevenZ);
        assert!(!info.encrypted_headers);
        let x = entries.iter().find(|e| e.path == "d/x.txt").unwrap();
        assert_eq!((x.size, x.is_dir, x.encrypted), (5, false, false));
        assert_eq!(x.crc, Some(crc32fast_like(b"hello")));
        assert_eq!(x.packed, None);
        assert_eq!(x.modified, Some(1_700_000_000));
        assert!(entries.iter().find(|e| e.path == "d").unwrap().is_dir);

        let out = dir.path().join("out");
        let rep = SevenZEngine::new(&a)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
        assert_eq!(fs::read(out.join("d/x.txt")).unwrap(), b"hello");
        assert_eq!(fs::read(out.join("y.txt")).unwrap(), b"world!");
        assert_eq!(fs::read(out.join("empty.txt")).unwrap(), b"");
        let rep = SevenZEngine::new(&a).test(None, &mut ctl()).unwrap();
        assert!(rep.failed.is_empty());
        assert_eq!(rep.ok, 3);
    }

    fn crc32fast_like(data: &[u8]) -> u32 {
        let mut crc = !0u32;
        for &b in data {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    0xEDB88320 ^ (crc >> 1)
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn encrypted_needs_password() {
        for header in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let a = dir.path().join("a.7z");
            build(
                &a,
                &[("s.txt", b"secret data", None), ("t.txt", b"more", None)],
                &Opts {
                    password: Some("pw1"),
                    encrypt_header: header,
                    ..Default::default()
                },
            );
            let out = dir.path().join("out");
            // no password
            let r = SevenZEngine::new(&a).list(None);
            if header {
                assert!(matches!(r, Err(ArchiveError::PasswordRequired)), "{r:?}");
            } else {
                let (_, es) = r.unwrap();
                assert!(es.iter().all(|e| e.encrypted));
            }
            let r = SevenZEngine::new(&a).extract(None, &req(&out), &mut ctl());
            assert!(matches!(r, Err(ArchiveError::PasswordRequired)), "{r:?}");
            let r = SevenZEngine::new(&a).test(None, &mut ctl());
            assert!(matches!(r, Err(ArchiveError::PasswordRequired)), "{r:?}");
            // wrong password
            let bad = pw("nope");
            let r = SevenZEngine::new(&a).list(Some(&bad));
            if header {
                assert!(matches!(r, Err(ArchiveError::WrongPassword)), "{r:?}");
            }
            let r = SevenZEngine::new(&a).extract(Some(&bad), &req(&out), &mut ctl());
            assert!(matches!(r, Err(ArchiveError::WrongPassword)), "{r:?}");
            let mut rq = req(&out);
            rq.keep_broken = true;
            let r = SevenZEngine::new(&a).extract(Some(&bad), &rq, &mut ctl());
            assert!(matches!(r, Err(ArchiveError::WrongPassword)), "{r:?}");
            let r = SevenZEngine::new(&a).test(Some(&bad), &mut ctl());
            assert!(matches!(r, Err(ArchiveError::WrongPassword)), "{r:?}");
            assert!(!out.exists() || leftovers(&out).is_empty());
            // right password
            let good = pw("pw1");
            let (info, es) = SevenZEngine::new(&a).list(Some(&good)).unwrap();
            assert_eq!(info.encrypted_headers, header);
            assert!(es.iter().all(|e| e.encrypted));
            let rep = SevenZEngine::new(&a)
                .extract(Some(&good), &req(&out), &mut ctl())
                .unwrap();
            assert!(rep.failed.is_empty(), "{:?}", rep.failed);
            assert_eq!(fs::read(out.join("s.txt")).unwrap(), b"secret data");
            let rep = SevenZEngine::new(&a).test(Some(&good), &mut ctl()).unwrap();
            assert!(rep.failed.is_empty());
        }
    }

    #[test]
    fn selected_entry_only() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.7z");
        build(
            &a,
            &[
                ("a.txt", b"A", None),
                ("d/b.txt", b"B", None),
                ("d/c.txt", b"C", None),
            ],
            &Opts::default(),
        );
        let out = dir.path().join("out");
        let mut rq = req(&out);
        rq.selection = vec!["d/b.txt".into()];
        let rep = SevenZEngine::new(&a)
            .extract(None, &rq, &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty());
        assert_eq!(leftovers(&out), vec!["d/b.txt".to_string()]);
    }

    #[test]
    fn corrupt_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.7z");
        let big: Vec<u8> = (0..200_000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 13) as u8)
            .collect();
        build(
            &a,
            &[("one.bin", &big, None), ("two.txt", b"two", None)],
            &Opts {
                copy: true,
                ..Default::default()
            },
        );
        let bytes = fs::read(&a).unwrap();
        // flipped data: CRC mismatch
        let mut bad = bytes.clone();
        bad[1000] ^= 0xFF;
        let b = dir.path().join("bad.7z");
        fs::write(&b, &bad).unwrap();
        let out = dir.path().join("out");
        let r = SevenZEngine::new(&b).extract(None, &req(&out), &mut ctl());
        assert!(has_corrupt(&r), "{r:?}");
        assert!(!out.join("one.bin").exists());
        assert_eq!(fs::read(out.join("two.txt")).unwrap(), b"two");
        assert!(!leftovers(&out).iter().any(|p| p.contains("ambar-part")));
        let r = SevenZEngine::new(&b).test(None, &mut ctl());
        assert!(has_corrupt(&r), "{r:?}");
        // every truncation is corrupt, never clean
        let t = dir.path().join("cut.7z");
        let mut cuts: Vec<usize> = (7..bytes.len()).step_by(997).collect();
        cuts.extend([bytes.len() - 1, bytes.len() - 20, bytes.len() - 33, 40, 32]);
        for n in cuts {
            fs::write(&t, &bytes[..n]).unwrap();
            let l = SevenZEngine::new(&t).list(None);
            assert!(matches!(l, Err(ArchiveError::Corrupt(_))), "cut {n}: {l:?}");
            let r = SevenZEngine::new(&t).test(None, &mut ctl());
            assert!(has_corrupt(&r), "cut {n}: {r:?}");
        }
    }

    #[test]
    fn cancel_stops_extract() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.7z");
        let big = vec![7u8; 4 * 1024 * 1024];
        build(
            &a,
            &[("big.bin", &big, None), ("z.txt", b"z", None)],
            &Opts::default(),
        );
        for keep in [false, true] {
            let out = dir.path().join(format!("out{keep}"));
            let mut rq = req(&out);
            rq.keep_broken = keep;
            let mut c = ctl();
            c.cancel_after = Some(2);
            let r = SevenZEngine::new(&a).extract(None, &rq, &mut c);
            assert!(matches!(r, Err(ArchiveError::Cancelled)), "{r:?}");
            assert!(
                !out.exists() || leftovers(&out).is_empty(),
                "{:?}",
                leftovers(&out)
            );
        }
        let mut c = ctl();
        c.cancel_after = Some(2);
        let r = SevenZEngine::new(&a).test(None, &mut c);
        assert!(matches!(r, Err(ArchiveError::Cancelled)), "{r:?}");
    }

    #[test]
    fn link_and_traversal_entries_are_failed() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.7z");
        let symlink = 0x8000 | (0o120777u32 << 16) | 0x20;
        let reparse = 0x400u32;
        build(
            &a,
            &[
                ("link", b"/tmp", Some(symlink)),
                ("rp", b"x", Some(reparse)),
                ("../evil.txt", b"evil", None),
                ("ok.txt", b"ok", None),
            ],
            &Opts::default(),
        );
        let out = dir.path().join("out");
        let rep = SevenZEngine::new(&a)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        let kind = |p: &str| {
            rep.failed
                .iter()
                .find(|f| f.path == p)
                .map(|f| f.error.code())
        };
        assert_eq!(kind("link"), Some("unsupported"));
        assert_eq!(kind("rp"), Some("unsupported"));
        assert_eq!(kind("../evil.txt"), Some("unsafe_path"));
        assert_eq!(rep.ok, 1);
        assert_eq!(leftovers(&out), vec!["ok.txt".to_string()]);
        assert!(!dir.path().join("evil.txt").exists());
        let rep = SevenZEngine::new(&a).test(None, &mut ctl()).unwrap();
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
    }

    // --- hand-built archives with lying headers ---
    fn num(n: u64) -> Vec<u8> {
        let mut first = 0u8;
        let mut mask = 0x80u8;
        let mut extra = 0;
        for i in 0..8 {
            if n < (1u64 << (7 * (i + 1))) {
                first |= (n >> (8 * i)) as u8;
                extra = i;
                break;
            }
            first |= mask;
            mask >>= 1;
            extra = i + 1;
        }
        let mut o = vec![first];
        for i in 0..extra {
            o.push((n >> (8 * i)) as u8);
        }
        o
    }

    /// One file `f.bin`, one block, plain header; the sizes are whatever the caller claims.
    fn plain7z(
        pack: &[u8],
        pack_size: u64,
        unpack: u64,
        crc_v: Option<u32>,
        coder: &[u8],
    ) -> Vec<u8> {
        let mut h = vec![1u8, 4, 6];
        h.extend(num(0));
        h.extend(num(1));
        h.push(9);
        h.extend(num(pack_size));
        h.push(0);
        h.extend([7u8, 0x0B]);
        h.extend(num(1));
        h.push(0);
        h.extend(num(1));
        h.extend(coder);
        h.push(0x0C);
        h.extend(num(unpack));
        if let Some(c) = crc_v {
            h.extend([0x0A, 1]);
            h.extend(c.to_le_bytes());
        }
        h.push(0);
        h.extend([8u8, 0]);
        h.push(0);
        h.push(5);
        h.extend(num(1));
        let name: Vec<u8> = "f.bin\0"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        h.push(0x11);
        h.extend(num(name.len() as u64 + 1));
        h.push(0);
        h.extend(&name);
        h.push(0);
        h.push(0);
        let mut out = b"7z\xBC\xAF\x27\x1C\x00\x04".to_vec();
        let mut sh = vec![];
        sh.extend((pack.len() as u64).to_le_bytes());
        sh.extend((h.len() as u64).to_le_bytes());
        sh.extend(crc32fast_like(&h).to_le_bytes());
        out.extend(crc32fast_like(&sh).to_le_bytes());
        out.extend(&sh);
        out.extend(pack);
        out.extend(&h);
        out
    }

    fn not_clean(r: &Result<OpReport, ArchiveError>) -> bool {
        match r {
            Err(_) => true,
            Ok(rep) => !rep.failed.is_empty(),
        }
    }

    #[test]
    fn short_entries_are_never_clean() {
        let dir = tempfile::tempdir().unwrap();
        let data = b"0123456789abcdefghij";
        let c = crc32fast_like(data);
        let good = plain7z(data, 20, 20, Some(c), &[1, 0]);
        let a = dir.path().join("good.7z");
        fs::write(&a, &good).unwrap();
        let rep = SevenZEngine::new(&a).test(None, &mut ctl()).unwrap();
        assert!(rep.failed.is_empty(), "control: {:?}", rep.failed);
        let cases = [
            ("pack_5_of_20", plain7z(data, 5, 20, Some(c), &[1, 0])),
            ("unpack_40_data_20", plain7z(data, 20, 40, Some(c), &[1, 0])),
            (
                "unpack_2^62",
                plain7z(data, 20, 1u64 << 62, Some(c), &[1, 0]),
            ),
            ("unpack_5_of_20", plain7z(data, 20, 5, Some(c), &[1, 0])),
        ];
        for (name, bytes) in cases {
            let a = dir.path().join(format!("{name}.7z"));
            fs::write(&a, &bytes).unwrap();
            let out = dir.path().join(format!("o-{name}"));
            std::fs::create_dir_all(&out).unwrap();
            fs::write(out.join("f.bin"), b"precious").unwrap();
            let t = SevenZEngine::new(&a).test(None, &mut ctl());
            assert!(not_clean(&t), "{name} test: {t:?}");
            let x = SevenZEngine::new(&a).extract(None, &req(&out), &mut ctl());
            assert!(not_clean(&x), "{name} extract: {x:?}");
            assert_eq!(fs::read(out.join("f.bin")).unwrap(), b"precious", "{name}");
            assert_eq!(leftovers(&out), ["f.bin"], "{name}");
        }
    }

    fn lzma2_content_archive(path: &Path, data: &[u8], password: Option<&'static str>) {
        build(
            path,
            &[("f.bin", data, None)],
            &Opts {
                password,
                encrypt_header: false,
                ..Default::default()
            },
        );
    }

    #[test]
    fn zeroed_lzma2_control_byte_is_corrupt() {
        let dir = tempfile::tempdir().unwrap();
        let big: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let a = dir.path().join("a.7z");
        lzma2_content_archive(&a, &big, None);
        let mut b = fs::read(&a).unwrap();
        b[32] = 0;
        let c = dir.path().join("c.7z");
        fs::write(&c, &b).unwrap();
        let t = SevenZEngine::new(&c).test(None, &mut ctl());
        assert!(has_corrupt(&t), "{t:?}");
        let out = dir.path().join("o");
        let x = SevenZEngine::new(&c).extract(None, &req(&out), &mut ctl());
        assert!(has_corrupt(&x), "{x:?}");
        assert!(leftovers(&out).is_empty());
    }

    #[test]
    fn wrong_password_is_never_success_and_never_replaces_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let data: Vec<u8> = (0..5000u32).map(|i| (i % 251) as u8).collect();
        let a = dir.path().join("a.7z");
        lzma2_content_archive(&a, &data, Some("right"));
        let out = dir.path().join("out");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("f.bin"), b"precious").unwrap();
        for i in 0..3000 {
            let bad = pw(&format!("wrong{i}"));
            let r = SevenZEngine::new(&a).test(Some(&bad), &mut ctl());
            assert!(
                matches!(r, Err(ArchiveError::WrongPassword)),
                "test {i}: {r:?}"
            );
            if i % 5 == 0 {
                let r = SevenZEngine::new(&a).extract(Some(&bad), &req(&out), &mut ctl());
                assert!(
                    matches!(r, Err(ArchiveError::WrongPassword)),
                    "extract {i}: {r:?}"
                );
                assert_eq!(fs::read(out.join("f.bin")).unwrap(), b"precious", "{i}");
                assert_eq!(leftovers(&out), ["f.bin"]);
            }
        }
        let rep = SevenZEngine::new(&a)
            .extract(Some(&pw("right")), &req(&out), &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty());
        assert_eq!(fs::read(out.join("f.bin")).unwrap(), data);
    }

    #[test]
    fn cancel_is_polled_while_skipping_unselected_solid_data() {
        use crate::engine::PollCtl;
        use ::sevenz_rust2::SourceReader;
        let dir = tempfile::tempdir().unwrap();
        let big = vec![9u8; 8 * 1024 * 1024];
        let s = dir.path().join("s.7z");
        let mut w = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
        w.set_content_methods(vec![EncoderMethod::LZMA2.into()]);
        w.push_archive_entries(
            vec![
                ArchiveEntry::new_file("big.bin"),
                ArchiveEntry::new_file("z.txt"),
            ],
            vec![
                SourceReader::new(Cursor::new(big)),
                SourceReader::new(Cursor::new(b"z".to_vec())),
            ],
        )
        .unwrap();
        fs::write(&s, w.finish().unwrap().into_inner()).unwrap();
        let mut rq = req(&dir.path().join("o"));
        rq.selection = vec!["z.txt".into()];
        let mut c = PollCtl {
            calls: std::cell::Cell::new(0),
            cancel_at: u64::MAX,
        };
        let rep = SevenZEngine::new(&s).extract(None, &rq, &mut c).unwrap();
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
        assert!(c.calls.get() >= 100, "{}", c.calls.get());
        let mut c = PollCtl {
            calls: std::cell::Cell::new(0),
            cancel_at: 20,
        };
        let r = SevenZEngine::new(&s).extract(None, &rq, &mut c);
        assert!(matches!(r, Err(ArchiveError::Cancelled)), "{r:?}");
    }

    #[test]
    fn deflate_and_bzip2_methods_open() {
        for method in [EncoderMethod::DEFLATE, EncoderMethod::BZIP2] {
            let dir = tempfile::tempdir().unwrap();
            let a = dir.path().join("a.7z");
            let mut w = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
            w.set_content_methods(vec![method.into()]);
            w.push_archive_entry(ArchiveEntry::new_file("f.txt"), Some(&b"deflated data"[..]))
                .unwrap();
            fs::write(&a, w.finish().unwrap().into_inner()).unwrap();
            let out = dir.path().join("out");
            let rep = SevenZEngine::new(&a)
                .extract(None, &req(&out), &mut ctl())
                .unwrap();
            assert!(rep.failed.is_empty(), "{:?}", rep.failed);
            assert_eq!(fs::read(out.join("f.txt")).unwrap(), b"deflated data");
        }
    }

    /// Hand-built solid block of two files `a` and `b` (one coder, plain header).
    /// A CRC of `None` leaves that file without one.
    fn solid7z(
        coder: &[u8],
        packed: &[u8],
        a_len: u64,
        b_len: u64,
        a_crc: Option<u32>,
        b_crc: Option<u32>,
    ) -> Vec<u8> {
        let mut h = vec![1u8, 4, 6];
        h.extend(num(0));
        h.extend(num(1));
        h.push(9);
        h.extend(num(packed.len() as u64));
        h.push(0);
        h.extend([7u8, 0x0B]);
        h.extend(num(1));
        h.push(0);
        h.extend(num(1));
        h.extend(coder);
        h.push(0x0C);
        h.extend(num(a_len + b_len));
        h.push(0);
        h.push(8);
        h.extend([0x0D]);
        h.extend(num(2));
        h.push(9);
        h.extend(num(a_len));
        let crcs = [a_crc, b_crc];
        if crcs.iter().any(Option::is_some) {
            h.extend([0x0A, 0]);
            let mut bits = 0u8;
            for (i, c) in crcs.iter().enumerate() {
                if c.is_some() {
                    bits |= 0x80 >> i;
                }
            }
            h.push(bits);
            for c in crcs.iter().flatten() {
                h.extend(c.to_le_bytes());
            }
        }
        h.push(0);
        h.push(0);
        h.push(5);
        h.extend(num(2));
        let name: Vec<u8> = "a\0b\0"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        h.push(0x11);
        h.extend(num(name.len() as u64 + 1));
        h.push(0);
        h.extend(&name);
        h.push(0);
        h.push(0);
        let mut out = b"7z\xBC\xAF\x27\x1C\x00\x04".to_vec();
        let mut sh = vec![];
        sh.extend((packed.len() as u64).to_le_bytes());
        sh.extend((h.len() as u64).to_le_bytes());
        sh.extend(crc32fast_like(&h).to_le_bytes());
        out.extend(crc32fast_like(&sh).to_le_bytes());
        out.extend(&sh);
        out.extend(packed);
        out.extend(&h);
        out
    }

    fn delta1(plain: &[u8]) -> Vec<u8> {
        let mut prev = 0u8;
        plain
            .iter()
            .map(|&b| {
                let d = b.wrapping_sub(prev);
                prev = b;
                d
            })
            .collect()
    }

    #[test]
    fn damage_reaching_a_crcless_entry_through_an_unselected_one_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let a_plain = b"AAAAAAAAAAAAAAAA";
        let b_plain = b"bbbbbbbbbbbbbbbb";
        let all = [a_plain.as_slice(), b_plain.as_slice()].concat();
        let delta = [0x21u8, 0x03, 1, 0];
        let ca = crc32fast_like(a_plain);
        let cb = crc32fast_like(b_plain);
        // control: undamaged, b selected, no CRC for b
        let good = dir.path().join("good.7z");
        fs::write(
            &good,
            solid7z(&delta, &delta1(&all), 16, 16, Some(ca), None),
        )
        .unwrap();
        let mut rq = req(&dir.path().join("o-good"));
        rq.selection = vec!["b".into()];
        let rep = SevenZEngine::new(&good)
            .extract(None, &rq, &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
        assert_eq!(fs::read(dir.path().join("o-good/b")).unwrap(), b_plain);
        // a damaged byte inside a shifts everything after it through the delta coder
        let mut packed = delta1(&all);
        packed[3] ^= 0x55;
        for (name, bcrc) in [("nocrc", None), ("crc", Some(cb))] {
            let a = dir.path().join(format!("{name}.7z"));
            fs::write(&a, solid7z(&delta, &packed, 16, 16, Some(ca), bcrc)).unwrap();
            let out = dir.path().join(format!("o-{name}"));
            let mut rq = req(&out);
            rq.selection = vec!["b".into()];
            let rep = SevenZEngine::new(&a)
                .extract(None, &rq, &mut ctl())
                .unwrap();
            assert_eq!(rep.ok, 0, "{name}: {:?}", rep.failed);
            assert!(
                rep.failed
                    .iter()
                    .any(|f| f.path == "b" && matches!(f.error, ArchiveError::Corrupt(_))),
                "{name}: {:?}",
                rep.failed
            );
            assert!(!out.exists() || leftovers(&out).is_empty(), "{name}");
        }
    }

    #[test]
    fn test_reports_bad_crc_on_link_and_unnamed_entries() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.7z");
        let symlink = 0x8000 | (0o120777u32 << 16) | 0x20;
        build(
            &a,
            &[
                ("link", b"/some/target/path", Some(symlink)),
                ("", b"unnamed-entry-data", None),
                ("b.txt", b"fine", None),
            ],
            &Opts {
                copy: true,
                ..Default::default()
            },
        );
        let rep = SevenZEngine::new(&a).test(None, &mut ctl()).unwrap();
        assert!(rep.failed.is_empty(), "control: {:?}", rep.failed);
        let mut bytes = fs::read(&a).unwrap();
        bytes[32] ^= 0xFF; // first byte of the link's data
        let i = bytes
            .windows(18)
            .position(|w| w == b"unnamed-entry-data")
            .unwrap();
        bytes[i] ^= 0xFF;
        fs::write(&a, &bytes).unwrap();
        let rep = SevenZEngine::new(&a).test(None, &mut ctl()).unwrap();
        assert_eq!(rep.ok, 1, "{:?}", rep.failed);
        assert!(
            rep.failed
                .iter()
                .filter(|f| matches!(f.error, ArchiveError::Corrupt(_)))
                .count()
                == 2,
            "{:?}",
            rep.failed
        );
        // extract: the link is still just unsupported, nothing is written for it
        let out = dir.path().join("out");
        let rep = SevenZEngine::new(&a)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        assert!(rep
            .failed
            .iter()
            .any(|f| f.path == "link" && matches!(f.error, ArchiveError::Unsupported(_))));
        assert_eq!(leftovers(&out), ["b.txt"]);
    }

    #[test]
    fn cancel_is_polled_while_draining_planned_skips() {
        use crate::engine::PollCtl;
        use ::sevenz_rust2::SourceReader;
        let dir = tempfile::tempdir().unwrap();
        let s = dir.path().join("s.7z");
        let mut w = ArchiveWriter::new(Cursor::new(Vec::new())).unwrap();
        w.set_content_methods(vec![EncoderMethod::LZMA2.into()]);
        w.push_archive_entries(
            vec![
                ArchiveEntry::new_file("big.bin"),
                ArchiveEntry::new_file("z.txt"),
            ],
            vec![
                SourceReader::new(Cursor::new(vec![9u8; 8 * 1024 * 1024])),
                SourceReader::new(Cursor::new(b"z".to_vec())),
            ],
        )
        .unwrap();
        fs::write(&s, w.finish().unwrap().into_inner()).unwrap();
        let out = dir.path().join("o");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("big.bin"), b"mine").unwrap();
        let mut rq = req(&out);
        rq.overwrite = OverwritePolicy::Skip;
        let mut c = PollCtl {
            calls: std::cell::Cell::new(0),
            cancel_at: u64::MAX,
        };
        let rep = SevenZEngine::new(&s).extract(None, &rq, &mut c).unwrap();
        assert_eq!(rep.skipped, 1);
        assert!(c.calls.get() >= 100, "{}", c.calls.get());
        let mut c = PollCtl {
            calls: std::cell::Cell::new(0),
            cancel_at: 20,
        };
        let r = SevenZEngine::new(&s).extract(None, &rq, &mut c);
        assert!(matches!(r, Err(ArchiveError::Cancelled)), "{r:?}");
        assert_eq!(fs::read(out.join("big.bin")).unwrap(), b"mine");
    }
}
