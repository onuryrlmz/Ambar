//! TAR (plain, gz, bz2, xz) and single-stream (gz, bz2, xz) engine. Everything is
//! read as a forward stream. Entry names come from the raw header bytes and every
//! output path goes through `plan_entry`; links and special files are never created.
//! A stream that ends without the end-of-archive zero block, or whose decoder fails,
//! is reported as `Corrupt`: a truncated archive is never clean.

use super::plan::PlanOutcome;
use super::plan::{copy_stream, is_selected, normalize_entry_path, plan_entry, write_entry};
use super::{
    ArchiveEngine, ArchiveInfo, Entry, ExtractRequest, FileFailure, Format, JobCtl, OpReport,
    Password,
};
use crate::error::ArchiveError;
use std::fs::File;
use std::io::{self, BufReader, Read};
use std::path::{Path, PathBuf};
use tar::EntryType;

const BLOCK: u64 = 512;

pub struct TarLikeEngine {
    path: PathBuf,
    format: Format,
    cache: Option<(ArchiveInfo, Vec<Entry>)>,
}

/// What an entry is, as far as extraction is concerned.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
    /// Symlink, hard link, device, fifo ...: never created.
    Unsupported,
}

struct Item {
    entry: Entry,
    kind: Kind,
}

enum Flow {
    Next,
    /// The data stream failed inside this entry; nothing after it can be read.
    Broken,
}

/// Counts what the tar reader consumed and remembers the last block, so that the
/// end-of-archive marker can be checked afterwards.
struct Probe<R> {
    inner: R,
    pos: u64,
    tail: Vec<u8>,
}

impl<R: Read> Read for Probe<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.pos += n as u64;
        let got = &buf[..n];
        if got.len() >= BLOCK as usize {
            self.tail = got[got.len() - BLOCK as usize..].to_vec();
        } else {
            self.tail.extend_from_slice(got);
            if self.tail.len() > BLOCK as usize {
                let cut = self.tail.len() - BLOCK as usize;
                self.tail.drain(..cut);
            }
        }
        Ok(n)
    }
}

/// Limits an entry to its declared size; a stream that ends earlier is an error
/// (the tar reader reports a plain end of data instead).
struct Exact<'a> {
    inner: &'a mut dyn Read,
    left: u64,
}

impl Read for Exact<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Ok(0);
        }
        let max = buf.len().min(self.left.min(usize::MAX as u64) as usize);
        let n = self.inner.read(&mut buf[..max])?;
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        self.left -= n as u64;
        Ok(n)
    }
}

impl TarLikeEngine {
    pub fn new(path: &Path, format: Format) -> TarLikeEngine {
        TarLikeEngine {
            path: path.to_path_buf(),
            format,
            cache: None,
        }
    }

    fn single(&self) -> bool {
        matches!(self.format, Format::Gz | Format::Bz2 | Format::Xz)
    }

    fn decoder(&self) -> Result<Box<dyn Read>, ArchiveError> {
        let f = BufReader::new(File::open(&self.path)?);
        Ok(match self.format {
            Format::Tar => Box::new(f),
            Format::TarGz | Format::Gz => Box::new(flate2::bufread::MultiGzDecoder::new(f)),
            Format::TarBz2 | Format::Bz2 => Box::new(bzip2::bufread::MultiBzDecoder::new(f)),
            Format::TarXz | Format::Xz => {
                Box::new(liblzma::bufread::XzDecoder::new_multi_decoder(f))
            }
            other => return Err(ArchiveError::Unsupported(format!("{other:?}"))),
        })
    }

    fn info(&self) -> Result<ArchiveInfo, ArchiveError> {
        Ok(ArchiveInfo {
            format: self.format,
            file_size: std::fs::metadata(&self.path)?.len(),
            encrypted_headers: false,
            solid: self.format != Format::Tar,
            multi_volume: false,
            has_recovery: false,
            locked: false,
            comment: None,
        })
    }

    /// The one entry of a single-stream file: its name without the last extension.
    fn single_entry(&self) -> Entry {
        let file = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = match file.rfind('.') {
            Some(i) => &file[..i],
            None => file.as_str(),
        };
        let name = normalize_entry_path(stem).trim_end_matches('/').to_string();
        let name = if name.split('/').any(|c| matches!(c, "" | "." | "..")) {
            "output".to_string()
        } else {
            name
        };
        Entry {
            path: name,
            is_dir: false,
            size: 0,
            packed: None,
            modified: None,
            crc: None,
            encrypted: false,
        }
    }

    /// Streams the tar once and calls `visit` for each entry with its data reader.
    /// `Ok(Some(msg))` means the stream itself is damaged (bad header, decoder
    /// failure, cut off, no end-of-archive marker).
    fn stream(
        &self,
        visit: &mut dyn FnMut(Item, &mut dyn Read) -> Result<Flow, ArchiveError>,
    ) -> Result<Option<String>, ArchiveError> {
        let mut probe = Probe {
            inner: self.decoder()?,
            pos: 0,
            tail: Vec::new(),
        };
        let mut next_header = 0u64;
        {
            let mut ar = tar::Archive::new(&mut probe);
            let entries = match ar.entries() {
                Ok(e) => e,
                Err(e) => return Ok(Some(e.to_string())),
            };
            for item in entries {
                let mut entry = match item {
                    Ok(e) => e,
                    Err(e) => return Ok(Some(e.to_string())),
                };
                let size = entry.size();
                next_header = entry.raw_file_position() + size.div_ceil(BLOCK) * BLOCK;
                let ty = entry.header().entry_type();
                if matches!(ty, EntryType::XGlobalHeader) {
                    continue;
                }
                let kind = match ty {
                    EntryType::Directory => Kind::Dir,
                    EntryType::Regular | EntryType::Continuous => Kind::File,
                    _ => Kind::Unsupported,
                };
                let raw = entry.path_bytes();
                let path = normalize_entry_path(&String::from_utf8_lossy(&raw))
                    .trim_end_matches('/')
                    .to_string();
                let item = Item {
                    entry: Entry {
                        path,
                        is_dir: kind == Kind::Dir,
                        size: if kind == Kind::File { size } else { 0 },
                        packed: None,
                        modified: entry.header().mtime().ok().map(|t| t as i64),
                        crc: None,
                        encrypted: false,
                    },
                    kind,
                };
                let mut data = Exact {
                    inner: &mut entry,
                    left: if kind == Kind::File { size } else { 0 },
                };
                match visit(item, &mut data)? {
                    Flow::Next => {}
                    Flow::Broken => return Ok(None),
                }
            }
        }
        // The reader stops at the first all-zero block, or at a plain end of input.
        let at_end = probe.pos;
        let zero_block = at_end == next_header + BLOCK
            && probe.tail.len() == BLOCK as usize
            && probe.tail.iter().all(|&b| b == 0);
        // Read the rest so that a decoder checksum or a cut trailer is noticed.
        if let Err(e) = io::copy(&mut probe, &mut io::sink()) {
            return Ok(Some(e.to_string()));
        }
        if !zero_block {
            return Ok(Some(
                "archive is truncated (no end-of-archive marker)".into(),
            ));
        }
        Ok(None)
    }

    fn list_tar(&self) -> Result<Vec<Entry>, ArchiveError> {
        let mut out = Vec::new();
        let bad = self.stream(&mut |item, _| {
            if !item.entry.path.is_empty() {
                out.push(item.entry);
            }
            Ok(Flow::Next)
        })?;
        match bad {
            Some(msg) => Err(ArchiveError::Corrupt(msg)),
            None => Ok(out),
        }
    }

    fn process_tar(
        &self,
        req: Option<&ExtractRequest>,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError> {
        let total: u64 = self
            .cache
            .as_ref()
            .map(|(_, es)| es.iter().map(|e| e.size).sum())
            .unwrap_or(0);
        let mut report = OpReport::default();
        let mut done = 0u64;
        let bad = self.stream(&mut |item, data| {
            if ctl.cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let entry = &item.entry;
            if entry.path.is_empty() || !req.is_none_or(|r| is_selected(&entry.path, &r.selection))
            {
                return Ok(Flow::Next);
            }
            let mut fail = |error: ArchiveError| {
                report.failed.push(FileFailure {
                    path: entry.path.clone(),
                    error,
                });
            };
            if item.kind == Kind::Unsupported {
                if req.is_some() {
                    fail(ArchiveError::Unsupported("link".into()));
                }
                return Ok(Flow::Next);
            }
            ctl.progress(&entry.path, done, total);
            let result = match req {
                None if entry.is_dir => return Ok(Flow::Next),
                None => copy_stream(data, None, &entry.path, ctl, done, total).map(|_| ()),
                Some(req) => {
                    let plan = match plan_entry(entry, req, ctl) {
                        Ok(p) => p,
                        Err(e @ (ArchiveError::UnsafePath(_) | ArchiveError::DestConflict(_))) => {
                            fail(e);
                            return Ok(Flow::Next);
                        }
                        Err(e) => return Err(e),
                    };
                    match plan {
                        PlanOutcome::Skip => {
                            report.skipped += 1;
                            return Ok(Flow::Next);
                        }
                        PlanOutcome::Write(dest) if entry.is_dir => {
                            std::fs::create_dir_all(&dest)?;
                            report.ok += 1;
                            return Ok(Flow::Next);
                        }
                        PlanOutcome::Write(dest) => {
                            write_entry(&dest, req.keep_broken, data, &entry.path, ctl, done, total)
                        }
                    }
                }
            };
            match result {
                Ok(()) => {
                    report.ok += 1;
                    done += entry.size;
                    Ok(Flow::Next)
                }
                Err(e @ ArchiveError::Corrupt(_)) => {
                    fail(e);
                    Ok(Flow::Broken)
                }
                Err(e) => Err(e),
            }
        })?;
        if let Some(msg) = bad {
            report.failed.push(FileFailure {
                path: self.path.to_string_lossy().into_owned(),
                error: ArchiveError::Corrupt(msg),
            });
        }
        Ok(report)
    }

    fn process_single(
        &self,
        req: Option<&ExtractRequest>,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError> {
        let entry = self.single_entry();
        let mut report = OpReport::default();
        if !req.is_none_or(|r| is_selected(&entry.path, &r.selection)) {
            return Ok(report);
        }
        let mut data = self.decoder()?;
        ctl.progress(&entry.path, 0, 0);
        let result = match req {
            None => copy_stream(&mut data, None, &entry.path, ctl, 0, 0).map(|_| ()),
            Some(req) => match plan_entry(&entry, req, ctl) {
                Ok(PlanOutcome::Skip) => {
                    report.skipped += 1;
                    return Ok(report);
                }
                Ok(PlanOutcome::Write(dest)) => {
                    write_entry(&dest, req.keep_broken, &mut data, &entry.path, ctl, 0, 0)
                }
                Err(e @ (ArchiveError::UnsafePath(_) | ArchiveError::DestConflict(_))) => Err(e),
                Err(e) => return Err(e),
            },
        };
        match result {
            Ok(()) => report.ok += 1,
            Err(
                e @ (ArchiveError::Corrupt(_)
                | ArchiveError::UnsafePath(_)
                | ArchiveError::DestConflict(_)),
            ) => report.failed.push(FileFailure {
                path: entry.path,
                error: e,
            }),
            Err(e) => return Err(e),
        }
        Ok(report)
    }
}

impl ArchiveEngine for TarLikeEngine {
    fn list(&mut self, _pw: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
        if let Some(c) = &self.cache {
            return Ok(c.clone());
        }
        let entries = if self.single() {
            vec![self.single_entry()]
        } else {
            self.list_tar()?
        };
        let c = (self.info()?, entries);
        self.cache = Some(c.clone());
        Ok(c)
    }

    fn extract(
        &mut self,
        _pw: Option<&Password>,
        req: &ExtractRequest,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError> {
        if self.single() {
            self.process_single(Some(req), ctl)
        } else {
            self.process_tar(Some(req), ctl)
        }
    }

    fn test(
        &mut self,
        _pw: Option<&Password>,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError> {
        if self.single() {
            self.process_single(None, ctl)
        } else {
            self.process_tar(None, ctl)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ConflictChoice, OverwritePolicy, TestCtl};
    use std::fs;
    use std::io::Write;

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

    fn leftovers(root: &Path) -> Vec<String> {
        fn go(dir: &Path, root: &Path, out: &mut Vec<String>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path();
                let m = fs::symlink_metadata(&p).unwrap();
                if m.is_dir() {
                    go(&p, root, out);
                } else {
                    out.push(p.strip_prefix(root).unwrap().to_string_lossy().into_owned());
                }
            }
        }
        let mut v = vec![];
        if root.exists() {
            go(root, root, &mut v);
        }
        v.sort();
        v
    }

    #[derive(Clone, Copy)]
    enum Kind {
        File,
        Dir,
        Symlink,
        Hardlink,
    }

    /// Raw ustar entry; `name` is written as is, so traversal names are possible.
    fn raw_entry(name: &str, kind: Kind, data: &[u8], link: &str) -> Vec<u8> {
        let mut h = ::tar::Header::new_ustar();
        {
            let old = h.as_old_mut();
            old.name[..name.len()].copy_from_slice(name.as_bytes());
            old.linkname[..link.len()].copy_from_slice(link.as_bytes());
        }
        h.set_entry_type(match kind {
            Kind::File => ::tar::EntryType::Regular,
            Kind::Dir => ::tar::EntryType::Directory,
            Kind::Symlink => ::tar::EntryType::Symlink,
            Kind::Hardlink => ::tar::EntryType::Link,
        });
        h.set_mode(0o644);
        h.set_mtime(1_700_000_000);
        h.set_size(if matches!(kind, Kind::File) {
            data.len() as u64
        } else {
            0
        });
        h.set_cksum();
        let mut out = h.as_bytes().to_vec();
        if matches!(kind, Kind::File) {
            out.extend_from_slice(data);
            out.resize(out.len().div_ceil(512) * 512, 0);
        }
        out
    }

    fn tar_of(entries: &[Vec<u8>], terminator: usize) -> Vec<u8> {
        let mut v: Vec<u8> = entries.concat();
        v.extend(std::iter::repeat_n(0u8, 512 * terminator));
        v
    }

    fn file(name: &str, data: &[u8]) -> Vec<u8> {
        raw_entry(name, Kind::File, data, "")
    }

    fn std_tar() -> Vec<u8> {
        tar_of(
            &[
                raw_entry("d/", Kind::Dir, b"", ""),
                file("a.txt", b"alpha"),
                file("d/b.txt", b"bravo bravo"),
            ],
            2,
        )
    }

    fn gz(d: &[u8]) -> Vec<u8> {
        let mut e = flate2::write::GzEncoder::new(vec![], flate2::Compression::default());
        e.write_all(d).unwrap();
        e.finish().unwrap()
    }
    fn bz(d: &[u8]) -> Vec<u8> {
        let mut e = bzip2::write::BzEncoder::new(vec![], bzip2::Compression::default());
        e.write_all(d).unwrap();
        e.finish().unwrap()
    }
    fn xz(d: &[u8]) -> Vec<u8> {
        let mut e = liblzma::write::XzEncoder::new(vec![], 6);
        e.write_all(d).unwrap();
        e.finish().unwrap()
    }

    fn pack(format: Format, tar: &[u8]) -> Vec<u8> {
        match format {
            Format::Tar => tar.to_vec(),
            Format::TarGz | Format::Gz => gz(tar),
            Format::TarBz2 | Format::Bz2 => bz(tar),
            Format::TarXz | Format::Xz => xz(tar),
            _ => unreachable!(),
        }
    }

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, bytes).unwrap();
        p
    }

    fn is_corrupt(r: &Result<OpReport, ArchiveError>) -> bool {
        match r {
            Err(ArchiveError::Corrupt(_)) => true,
            Ok(rep) => rep
                .failed
                .iter()
                .any(|f| matches!(f.error, ArchiveError::Corrupt(_))),
            _ => false,
        }
    }

    const TAR_FORMATS: [Format; 4] = [Format::Tar, Format::TarGz, Format::TarBz2, Format::TarXz];

    #[test]
    fn tar_gz_bz2_xz_list_and_extract() {
        for f in TAR_FORMATS {
            let dir = tempfile::tempdir().unwrap();
            let a = write(dir.path(), "a.tar", &pack(f, &std_tar()));
            let (info, entries) = TarLikeEngine::new(&a, f).list(None).unwrap();
            assert_eq!(info.format, f);
            let paths: Vec<_> = entries.iter().map(|e| e.path.as_str()).collect();
            assert_eq!(paths, ["d", "a.txt", "d/b.txt"], "{f:?}");
            assert!(entries[0].is_dir);
            let e = &entries[2];
            assert_eq!(
                (e.size, e.packed, e.crc, e.encrypted),
                (11, None, None, false)
            );
            assert_eq!(e.modified, Some(1_700_000_000));
            let out = dir.path().join("out");
            let rep = TarLikeEngine::new(&a, f)
                .extract(None, &req(&out), &mut ctl())
                .unwrap();
            assert!(rep.failed.is_empty(), "{f:?} {:?}", rep.failed);
            assert_eq!(rep.ok, 3);
            assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"alpha");
            assert_eq!(fs::read(out.join("d/b.txt")).unwrap(), b"bravo bravo");
            let rep = TarLikeEngine::new(&a, f).test(None, &mut ctl()).unwrap();
            assert!(rep.failed.is_empty(), "{f:?} {:?}", rep.failed);
        }
    }

    #[test]
    fn open_engine_wires_tar_and_single_stream() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.tar.gz", &pack(Format::TarGz, &std_tar()));
        let (info, es) = crate::engine::open_engine(&a).unwrap().list(None).unwrap();
        assert_eq!((info.format, es.len()), (Format::TarGz, 3));
        let b = write(dir.path(), "b.txt.xz", &xz(b"x"));
        let (info, es) = crate::engine::open_engine(&b).unwrap().list(None).unwrap();
        assert_eq!((info.format, es[0].path.as_str()), (Format::Xz, "b.txt"));
    }

    #[test]
    fn list_streams_once_and_caches() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.tar", &std_tar());
        let mut e = TarLikeEngine::new(&a, Format::Tar);
        let first = e.list(None).unwrap().1.len();
        fs::remove_file(&a).unwrap();
        assert_eq!(e.list(None).unwrap().1.len(), first);
    }

    #[test]
    fn tar_traversal_entry_is_failed() {
        let dir = tempfile::tempdir().unwrap();
        let t = tar_of(
            &[
                file("../evil.txt", b"evil"),
                file("a/../../evil2.txt", b"evil"),
                file("ok.txt", b"ok"),
                file("./sub//./n.txt/", b"n"),
                file("./", b""),
            ],
            2,
        );
        let a = write(dir.path(), "a.tar", &t);
        let out = dir.path().join("o/out");
        let rep = TarLikeEngine::new(&a, Format::Tar)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        let codes: Vec<_> = rep
            .failed
            .iter()
            .map(|f| (f.path.as_str(), f.error.code()))
            .collect();
        assert_eq!(
            codes,
            [
                ("../evil.txt", "unsafe_path"),
                ("a/../../evil2.txt", "unsafe_path")
            ]
        );
        assert_eq!(rep.ok, 2);
        assert_eq!(
            leftovers(dir.path().join("o").as_path()),
            ["out/ok.txt", "out/sub/n.txt"]
        );
        assert!(!dir.path().join("evil.txt").exists());
        assert!(!dir.path().join("o/evil.txt").exists());
    }

    #[test]
    fn tar_symlink_entry_cannot_escape() {
        for hard in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let outside = dir.path().join("outside");
            fs::create_dir(&outside).unwrap();
            let target = outside.to_str().unwrap();
            let t = tar_of(
                &[
                    raw_entry(
                        "l",
                        if hard { Kind::Hardlink } else { Kind::Symlink },
                        b"",
                        target,
                    ),
                    file("l/x", b"pwn"),
                    file("fine.txt", b"fine"),
                ],
                2,
            );
            let a = write(dir.path(), "a.tar", &t);
            let out = dir.path().join("out");
            let rep = TarLikeEngine::new(&a, Format::Tar)
                .extract(None, &req(&out), &mut ctl())
                .unwrap();
            assert_eq!(rep.failed.len(), 1, "{:?}", rep.failed);
            assert_eq!(rep.failed[0].path, "l");
            assert!(matches!(rep.failed[0].error, ArchiveError::Unsupported(_)));
            assert!(leftovers(&outside).is_empty());
            // nothing named l is a symlink; the plain file lands inside dest only
            assert!(!fs::symlink_metadata(out.join("l")).is_ok_and(|m| m.file_type().is_symlink()));
            assert_eq!(fs::read(out.join("fine.txt")).unwrap(), b"fine");
            // the test operation does not fail links
            let rep = TarLikeEngine::new(&a, Format::Tar)
                .test(None, &mut ctl())
                .unwrap();
            assert!(rep.failed.is_empty(), "{:?}", rep.failed);
        }
    }

    #[test]
    fn gz_single_stream_extracts_named_output() {
        for (f, ext, enc) in [
            (Format::Gz, "gz", gz as fn(&[u8]) -> Vec<u8>),
            (Format::Bz2, "bz2", bz),
            (Format::Xz, "xz", xz),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let a = write(dir.path(), &format!("a.txt.{ext}"), &enc(b"single stream"));
            let (info, es) = TarLikeEngine::new(&a, f).list(None).unwrap();
            assert_eq!(info.format, f);
            assert_eq!(es.len(), 1);
            assert_eq!(
                (es[0].path.as_str(), es[0].size, es[0].is_dir),
                ("a.txt", 0, false)
            );
            let out = dir.path().join("out");
            let rep = TarLikeEngine::new(&a, f)
                .extract(None, &req(&out), &mut ctl())
                .unwrap();
            assert!(rep.failed.is_empty());
            assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"single stream");
            let rep = TarLikeEngine::new(&a, f).test(None, &mut ctl()).unwrap();
            assert!(rep.failed.is_empty());
        }
        // fallback names
        for (name, want) in [
            (".gz", "output"),
            ("...gz", "output"),
            ("plain", "plain"),
            ("x.tar.gz", "x.tar"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let a = write(dir.path(), name, &gz(b"x"));
            let es = TarLikeEngine::new(&a, Format::Gz).list(None).unwrap().1;
            assert_eq!(es[0].path, want, "{name}");
            let out = dir.path().join("out");
            let rep = TarLikeEngine::new(&a, Format::Gz)
                .extract(None, &req(&out), &mut ctl())
                .unwrap();
            assert!(rep.failed.is_empty(), "{name}: {:?}", rep.failed);
            assert_eq!(leftovers(&out), [want.to_string()]);
        }
    }

    #[test]
    fn single_stream_overwrite_policy() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.txt.gz", &gz(b"new"));
        let out = dir.path().join("out");
        fs::create_dir(&out).unwrap();
        fs::write(out.join("a.txt"), b"old").unwrap();
        let mut rq = req(&out);
        rq.overwrite = OverwritePolicy::Skip;
        let rep = TarLikeEngine::new(&a, Format::Gz)
            .extract(None, &rq, &mut ctl())
            .unwrap();
        assert_eq!(rep.skipped, 1);
        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"old");
        rq.overwrite = OverwritePolicy::Rename;
        TarLikeEngine::new(&a, Format::Gz)
            .extract(None, &rq, &mut ctl())
            .unwrap();
        assert_eq!(fs::read(out.join("a (1).txt")).unwrap(), b"new");
    }

    #[test]
    fn truncated_single_stream_is_corrupt() {
        let payload: Vec<u8> = (0..3000u32).map(|i| (i * 7 % 251) as u8).collect();
        for f in [Format::Gz, Format::Bz2, Format::Xz] {
            let full = pack(f, &payload);
            for n in 6..full.len() {
                let dir = tempfile::tempdir().unwrap();
                let a = write(dir.path(), "a.bin.x", &full[..n]);
                let out = dir.path().join("out");
                let r = TarLikeEngine::new(&a, f).extract(None, &req(&out), &mut ctl());
                assert!(is_corrupt(&r), "{f:?} cut {n}/{}: {r:?}", full.len());
                assert!(
                    leftovers(&out).is_empty(),
                    "{f:?} cut {n}: {:?}",
                    leftovers(&out)
                );
                let r = TarLikeEngine::new(&a, f).test(None, &mut ctl());
                assert!(is_corrupt(&r), "{f:?} cut {n}: {r:?}");
            }
        }
    }

    #[test]
    fn multi_member_gzip() {
        let dir = tempfile::tempdir().unwrap();
        let mut full = gz(b"first ");
        full.extend(gz(b"second"));
        let a = write(dir.path(), "m.txt.gz", &full);
        let out = dir.path().join("out");
        let rep = TarLikeEngine::new(&a, Format::Gz)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty());
        assert_eq!(fs::read(out.join("m.txt")).unwrap(), b"first second");
        let first = gz(b"first ").len();
        for n in first + 1..full.len() {
            let a = write(dir.path(), "c.txt.gz", &full[..n]);
            let out = dir.path().join(format!("o{n}"));
            let r = TarLikeEngine::new(&a, Format::Gz).extract(None, &req(&out), &mut ctl());
            assert!(is_corrupt(&r), "cut {n}: {r:?}");
            assert!(leftovers(&out).is_empty());
        }
    }

    #[test]
    fn real_corruption_is_corrupt_not_io() {
        let payload: Vec<u8> = (0..20_000u32)
            .map(|i| (i.wrapping_mul(2654435761) >> 11) as u8)
            .collect();
        let tar = tar_of(&[file("big.bin", &payload), file("z.txt", b"z")], 2);
        for f in [
            Format::TarGz,
            Format::TarBz2,
            Format::TarXz,
            Format::Gz,
            Format::Bz2,
            Format::Xz,
        ] {
            let full = pack(f, &tar);
            for at in [full.len() / 3, full.len() / 2, full.len() - 12] {
                let mut bad = full.clone();
                for b in &mut bad[at..at + 6] {
                    *b ^= 0xA5;
                }
                let dir = tempfile::tempdir().unwrap();
                let a = write(dir.path(), "a.x", &bad);
                let out = dir.path().join("out");
                let r = TarLikeEngine::new(&a, f).extract(None, &req(&out), &mut ctl());
                match &r {
                    Ok(rep) => assert!(
                        !rep.failed.is_empty()
                            && rep
                                .failed
                                .iter()
                                .all(|x| matches!(x.error, ArchiveError::Corrupt(_))),
                        "{f:?} at {at}: {:?}",
                        rep.failed
                    ),
                    Err(e) => assert!(
                        matches!(e, ArchiveError::Corrupt(_)),
                        "{f:?} at {at}: {e:?}"
                    ),
                }
                let r = TarLikeEngine::new(&a, f).test(None, &mut ctl());
                assert!(is_corrupt(&r), "{f:?} at {at}: {r:?}");
            }
        }
    }

    #[test]
    fn truncated_tar_is_never_clean() {
        let tar = tar_of(
            &[
                file("a.txt", &[b'a'; 700]),
                file("b.txt", &[b'b'; 10]),
                file("c.txt", &[b'c'; 512]),
            ],
            2,
        );
        let full = tar.len();
        let mut cuts: Vec<usize> = (1..full - 512).step_by(512).collect();
        cuts.extend((0..full - 512).step_by(512).map(|c| c + 37));
        cuts.push(full - 1024);
        for f in [Format::Tar, Format::TarGz] {
            for &n in &cuts {
                // cut inside the tar stream, then compress
                let dir = tempfile::tempdir().unwrap();
                let a = write(dir.path(), "a.tar", &pack(f, &tar[..n]));
                let out = dir.path().join("out");
                let r = TarLikeEngine::new(&a, f).extract(None, &req(&out), &mut ctl());
                assert!(is_corrupt(&r), "{f:?} tar cut {n}: {r:?}");
                let r = TarLikeEngine::new(&a, f).test(None, &mut ctl());
                assert!(is_corrupt(&r), "{f:?} tar cut {n}: {r:?}");
                let l = TarLikeEngine::new(&a, f).list(None);
                assert!(
                    matches!(l, Err(ArchiveError::Corrupt(_))),
                    "{f:?} cut {n}: {l:?}"
                );
            }
        }
        // compressed stream cut at a few places
        let gzd = gz(&tar);
        for n in (20..gzd.len()).step_by(7) {
            let dir = tempfile::tempdir().unwrap();
            let a = write(dir.path(), "a.tar.gz", &gzd[..n]);
            let r = TarLikeEngine::new(&a, Format::TarGz).test(None, &mut ctl());
            assert!(is_corrupt(&r), "gz cut {n}: {r:?}");
        }
    }

    #[test]
    fn tar_without_padding_beyond_two_zero_blocks_passes() {
        for terminator in [1usize, 2, 3, 20] {
            for f in [Format::Tar, Format::TarGz] {
                let t = tar_of(&[file("a.txt", b"x")], terminator);
                let dir = tempfile::tempdir().unwrap();
                let a = write(dir.path(), "a.tar", &pack(f, &t));
                let rep = TarLikeEngine::new(&a, f).test(None, &mut ctl()).unwrap();
                assert!(
                    rep.failed.is_empty(),
                    "{terminator} {f:?}: {:?}",
                    rep.failed
                );
                assert!(TarLikeEngine::new(&a, f).list(None).is_ok());
            }
        }
        // a file whose data ends in zero blocks and which has no terminator is still cut
        let t = tar_of(&[file("z.bin", &[0u8; 2048])], 0);
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "z.tar", &t);
        let r = TarLikeEngine::new(&a, Format::Tar).test(None, &mut ctl());
        assert!(is_corrupt(&r), "{r:?}");
    }

    #[test]
    fn bad_header_keeps_earlier_entries_and_reports() {
        let mut t = tar_of(&[file("a.txt", b"first"), file("b.txt", b"second")], 2);
        t[1024 + 10] ^= 0x7F; // damage b.txt's header name, checksum now wrong
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.tar", &t);
        let out = dir.path().join("out");
        let r = TarLikeEngine::new(&a, Format::Tar).extract(None, &req(&out), &mut ctl());
        assert!(is_corrupt(&r), "{r:?}");
        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"first");
    }

    #[test]
    fn cancel_mid_file() {
        let big = vec![9u8; 4 * 1024 * 1024];
        let tar = tar_of(&[file("big.bin", &big), file("z.txt", b"z")], 2);
        for f in [Format::Tar, Format::TarGz, Format::Gz] {
            for keep in [false, true] {
                let dir = tempfile::tempdir().unwrap();
                let body = if f == Format::Gz {
                    gz(&big)
                } else {
                    pack(f, &tar)
                };
                let a = write(dir.path(), "a.bin.gz", &body);
                let out = dir.path().join("out");
                let mut rq = req(&out);
                rq.keep_broken = keep;
                let mut c = ctl();
                c.cancel_after = Some(2);
                let r = TarLikeEngine::new(&a, f).extract(None, &rq, &mut c);
                assert!(matches!(r, Err(ArchiveError::Cancelled)), "{f:?} {r:?}");
                assert!(
                    leftovers(&out).is_empty(),
                    "{f:?} {keep}: {:?}",
                    leftovers(&out)
                );
            }
            let dir = tempfile::tempdir().unwrap();
            let body = if f == Format::Gz {
                gz(&big)
            } else {
                pack(f, &tar)
            };
            let a = write(dir.path(), "a.bin.gz", &body);
            let mut c = ctl();
            c.cancel_after = Some(2);
            let r = TarLikeEngine::new(&a, f).test(None, &mut c);
            assert!(matches!(r, Err(ArchiveError::Cancelled)), "{f:?} {r:?}");
        }
    }

    #[test]
    fn duplicates_follow_overwrite_policy() {
        let t = tar_of(&[file("a.txt", b"one"), file("a.txt", b"two")], 2);
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.tar", &t);
        let run = |policy, name: &str| {
            let out = dir.path().join(name);
            let mut rq = req(&out);
            rq.overwrite = policy;
            let rep = TarLikeEngine::new(&a, Format::Tar)
                .extract(None, &rq, &mut ctl())
                .unwrap();
            (out, rep)
        };
        let (out, rep) = run(OverwritePolicy::Overwrite, "o1");
        assert_eq!((rep.ok, rep.skipped), (2, 0));
        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"two");
        let (out, rep) = run(OverwritePolicy::Skip, "o2");
        assert_eq!((rep.ok, rep.skipped), (1, 1));
        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"one");
        let (out, rep) = run(OverwritePolicy::Rename, "o3");
        assert_eq!(rep.ok, 2);
        assert_eq!(fs::read(out.join("a.txt")).unwrap(), b"one");
        assert_eq!(fs::read(out.join("a (1).txt")).unwrap(), b"two");
    }

    #[test]
    fn selection_and_flat_extract() {
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.tar", &std_tar());
        let out = dir.path().join("out");
        let mut rq = req(&out);
        rq.selection = vec!["d".into()];
        rq.keep_paths = false;
        let rep = TarLikeEngine::new(&a, Format::Tar)
            .extract(None, &rq, &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty());
        assert_eq!(leftovers(&out), ["b.txt"]);
    }
}
