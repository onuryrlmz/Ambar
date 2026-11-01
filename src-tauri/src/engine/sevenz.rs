//! 7z engine. Extraction never uses the crate's `decompress*` helpers: it walks the
//! blocks with `BlockDecoder`, so every output path goes through `plan_entry` and
//! every file is written via `write_via_temp`, with cancel checked in the copy loop.
//! Link attributes are never materialised.

use super::plan::{
    copy_stream, is_selected, normalize_entry_path, plan_entry, write_entry, PlanOutcome,
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
            ctl,
            report: OpReport::default(),
            done: 0,
            total: 0,
        };
        run.total = (0..metas.len())
            .filter(|&i| run.is_work(i) && !metas[i].link && !metas[i].entry.is_dir)
            .map(|i| metas[i].entry.size)
            .sum();
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
            if !(start..start + dec.entry_count()).any(|i| run.is_work(i)) {
                continue;
            }
            let failed_before = run.report.failed.len();
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
    ctl: &'a mut dyn JobCtl,
    report: OpReport,
    done: u64,
    total: u64,
}

impl Run<'_> {
    fn is_work(&self, i: usize) -> bool {
        let m = &self.metas[i];
        !m.entry.path.is_empty()
            && !m.anti
            && self
                .req
                .is_none_or(|r| is_selected(&m.entry.path, &r.selection))
    }

    fn fail(&mut self, path: &str, error: ArchiveError) {
        self.report.failed.push(FileFailure {
            path: path.to_string(),
            error,
        });
    }

    /// Handles one entry; `Err` ends the whole run.
    fn handle(&mut self, idx: usize, data: &mut dyn Read) -> Result<(), ArchiveError> {
        let metas = self.metas;
        let m = &metas[idx];
        let entry = &m.entry;
        if self.ctl.cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        if entry.path.is_empty()
            || m.anti
            || !self
                .req
                .is_none_or(|r| is_selected(&entry.path, &r.selection))
        {
            return Ok(());
        }
        if m.link {
            if self.req.is_some() {
                self.fail(&entry.path, ArchiveError::Unsupported("link".into()));
            }
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
                        return Ok(());
                    }
                    Err(e) => return Err(e),
                };
                match plan {
                    PlanOutcome::Skip => {
                        self.report.skipped += 1;
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
                        &entry.path,
                        self.ctl,
                        done,
                        total,
                    ),
                }
            }
        };
        match result {
            Ok(()) => {
                self.report.ok += 1;
                self.done += entry.size;
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
}
