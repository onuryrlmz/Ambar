//! TAR (plain, gz, bz2, xz) and single-stream (gz, bz2, xz) engine. Everything is
//! read as a forward stream. Entry names come from the raw header bytes and every
//! output path goes through `plan_entry`; links and special files are never created.
//! A stream that ends without the end-of-archive zero block, or whose decoder fails,
//! is reported as `Corrupt`: a truncated archive is never clean.

use super::plan::PlanOutcome;
use super::plan::{
    copy_stream, normalize_entry_path, plan_entry, skip_stream, write_entry, Exact, Selection,
};
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
    /// Bytes of regular files, known once the tar was listed (progress total only).
    file_total: u64,
}

/// What an entry is, as far as extraction is concerned.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Dir,
    /// Symlink, hard link, device, fifo ...: never created.
    Unsupported(&'static str),
}

struct Item {
    entry: Entry,
    kind: Kind,
}

type Visit<'a> = dyn FnMut(Item, &mut dyn Read, &mut dyn JobCtl) -> Result<Flow, ArchiveError> + 'a;

enum Flow {
    Next,
    /// The data stream failed inside this entry; nothing after it can be read.
    Broken,
}

/// Largest GNU long-name/link or PAX record that is read into memory.
const MAX_RECORD: u64 = 1024 * 1024;

/// A `JobCtl` that never cancels, for listing.
struct NoCtl;

impl JobCtl for NoCtl {
    fn progress(&mut self, _: &str, _: u64, _: u64) {}
    fn cancelled(&self) -> bool {
        false
    }
    fn resolve_conflict(&mut self, _: &Path) -> super::ConflictChoice {
        super::ConflictChoice::Cancel
    }
}

/// Reads up to `buf.len()` bytes, stopping early only at the end of input.
fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Looks up `key` in a PAX record block (`<len> key=value\n` repeated).
fn pax_lookup(data: &[u8], key: &str) -> Result<Option<Vec<u8>>, String> {
    let mut found = None;
    // NUL padding after the last record is tolerated.
    let end = data.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let mut rest = &data[..end];
    while !rest.is_empty() {
        let sp = rest
            .iter()
            .position(|&b| b == b' ')
            .ok_or("bad pax record")?;
        let len: usize = std::str::from_utf8(&rest[..sp])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or("bad pax record length")?;
        if len <= sp + 1 || len > rest.len() {
            return Err("bad pax record length".into());
        }
        let rec = rest[sp + 1..len]
            .strip_suffix(b"\n")
            .ok_or("bad pax record (no newline)")?;
        let eq = rec
            .iter()
            .position(|&b| b == b'=')
            .ok_or("bad pax record (no '=')")?;
        if &rec[..eq] == key.as_bytes() {
            found = Some(rec[eq + 1..].to_vec());
        }
        rest = &rest[len..];
    }
    Ok(found)
}

fn header_checksum_ok(block: &[u8; 512], h: &tar::Header) -> bool {
    let sum: u32 = block[..148]
        .iter()
        .chain(&block[156..])
        .map(|&b| b as u32)
        .sum::<u32>()
        + 8 * 32;
    h.cksum().is_ok_and(|c| c == sum)
}

impl TarLikeEngine {
    pub fn new(path: &Path, format: Format) -> TarLikeEngine {
        TarLikeEngine {
            path: path.to_path_buf(),
            format,
            cache: None,
            file_total: 0,
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
    /// The headers are parsed here (not by the `tar` crate's iterator) so that long-name
    /// and PAX records stay bounded and sparse entries are measured by their on-disk size.
    /// Data the visitor leaves unread is read and dropped, polling cancel.
    /// `Ok(Some(msg))` means the stream itself is damaged (bad header, decoder
    /// failure, cut off, no end-of-archive marker).
    fn stream(
        &self,
        ctl: &mut dyn JobCtl,
        visit: &mut Visit,
    ) -> Result<Option<String>, ArchiveError> {
        let mut probe = self.decoder()?;
        let r: &mut dyn Read = &mut probe;
        macro_rules! bail {
            ($e:expr) => {
                return Ok(Some($e.to_string()))
            };
        }
        let mut long_name: Option<Vec<u8>> = None;
        let mut long_link = false;
        let mut pax: Option<Vec<u8>> = None;
        let mut block = [0u8; BLOCK as usize];
        loop {
            if ctl.cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let n = match read_full(r, &mut block) {
                Ok(n) => n,
                Err(e) => bail!(e),
            };
            if n == 0 {
                bail!("archive is truncated (no end-of-archive marker)");
            }
            if n < block.len() {
                bail!("archive is truncated (partial header)");
            }
            if block.iter().all(|&b| b == 0) {
                break;
            }
            let h = tar::Header::from_byte_slice(&block);
            if !header_checksum_ok(&block, h) {
                bail!("archive header checksum mismatch");
            }
            let ty = h.entry_type();
            let mut size = match h.entry_size() {
                Ok(s) => s,
                Err(e) => bail!(e),
            };
            if matches!(
                ty,
                EntryType::GNULongName | EntryType::GNULongLink | EntryType::XHeader
            ) {
                if size > MAX_RECORD {
                    bail!("oversized long-name or extended header record");
                }
                let mut data = vec![0u8; size as usize];
                let pad = (BLOCK - size % BLOCK) % BLOCK;
                if let Err(e) = r.read_exact(&mut data) {
                    bail!(e);
                }
                let mut padding = Exact {
                    inner: r,
                    left: pad,
                };
                if let Err(e) = skip_stream(&mut padding, "", ctl) {
                    match e {
                        ArchiveError::Cancelled => return Err(e),
                        e => bail!(e),
                    }
                }
                match ty {
                    EntryType::GNULongName => {
                        if long_name.is_some() {
                            bail!("two long name entries describing the same member");
                        }
                        while data.last() == Some(&0) {
                            data.pop();
                        }
                        long_name = Some(data);
                    }
                    EntryType::GNULongLink => {
                        if long_link {
                            bail!("two long link entries describing the same member");
                        }
                        long_link = true;
                    }
                    _ => {
                        if pax.is_some() {
                            bail!("two pax extension entries describing the same member");
                        }
                        pax = Some(data);
                    }
                }
                continue;
            }
            // Sparse entries carry extension blocks before their data.
            let sparse = ty.is_gnu_sparse();
            let mut shown_size = size;
            if sparse {
                if let Some(g) = h.as_gnu() {
                    shown_size = g.real_size().unwrap_or(size);
                    let mut ext = g.is_extended();
                    while ext {
                        if ctl.cancelled() {
                            return Err(ArchiveError::Cancelled);
                        }
                        let mut eb = [0u8; BLOCK as usize];
                        match read_full(r, &mut eb) {
                            Ok(n) if n == eb.len() => {}
                            Ok(_) => bail!("archive is truncated (sparse header)"),
                            Err(e) => bail!(e),
                        }
                        ext = eb[504] == 1;
                    }
                }
            }
            let raw_name = pax.as_deref().map(|p| pax_lookup(p, "path")).transpose();
            let raw_name = match raw_name {
                Ok(v) => v.flatten(),
                Err(e) => bail!(e),
            };
            if let Some(p) = pax.as_deref() {
                // A PAX size overrides the header size.
                match pax_lookup(p, "size") {
                    // An empty value means "unset".
                    Ok(Some(v)) if v.is_empty() => {}
                    Ok(Some(v)) => {
                        match std::str::from_utf8(&v).ok().and_then(|s| s.parse().ok()) {
                            Some(s) => size = s,
                            None => bail!("bad pax size"),
                        }
                    }
                    Ok(None) => {}
                    Err(e) => bail!(e),
                }
            }
            let name_bytes = raw_name
                .or_else(|| long_name.take())
                .unwrap_or_else(|| h.path_bytes().into_owned());
            long_name = None;
            long_link = false;
            pax = None;
            let dir_by_name = ty == EntryType::Regular && size == 0 && name_bytes.ends_with(b"/");
            let kind = match ty {
                EntryType::Directory => Kind::Dir,
                _ if dir_by_name => Kind::Dir,
                EntryType::Regular | EntryType::Continuous => Kind::File,
                EntryType::Symlink | EntryType::Link => Kind::Unsupported("link"),
                EntryType::GNUSparse => Kind::Unsupported("sparse file"),
                EntryType::Char | EntryType::Block => Kind::Unsupported("device file"),
                EntryType::Fifo => Kind::Unsupported("fifo"),
                EntryType::XGlobalHeader => Kind::Unsupported("global header"),
                _ => Kind::Unsupported("special entry"),
            };
            let path = normalize_entry_path(&String::from_utf8_lossy(&name_bytes))
                .trim_end_matches('/')
                .to_string();
            let global = ty == EntryType::XGlobalHeader;
            let item = Item {
                entry: Entry {
                    path,
                    is_dir: kind == Kind::Dir,
                    size: match kind {
                        Kind::File => size,
                        Kind::Unsupported(_) if sparse => shown_size,
                        _ => 0,
                    },
                    packed: None,
                    modified: h.mtime().ok().map(|t| t as i64),
                    crc: None,
                    encrypted: false,
                },
                kind,
            };
            let mut data = Exact {
                inner: r,
                left: size,
            };
            if !global {
                match visit(item, &mut data, ctl)? {
                    Flow::Next => {}
                    Flow::Broken => return Ok(None),
                }
            }
            // Whatever the visitor left, and the padding up to the next header.
            match skip_stream(&mut data, "", ctl) {
                Ok(()) => {}
                Err(ArchiveError::Cancelled) => return Err(ArchiveError::Cancelled),
                Err(e) => bail!(e),
            }
            let mut padding = Exact {
                inner: r,
                left: (BLOCK - size % BLOCK) % BLOCK,
            };
            match skip_stream(&mut padding, "", ctl) {
                Ok(()) => {}
                Err(ArchiveError::Cancelled) => return Err(ArchiveError::Cancelled),
                Err(e) => bail!(e),
            }
        }
        if long_name.is_some() || long_link || pax.is_some() {
            bail!("members found describing a future member but no future member found");
        }
        // Read the rest so that a decoder checksum or a cut trailer is noticed.
        match skip_stream(r, "", ctl) {
            Ok(()) => Ok(None),
            Err(ArchiveError::Cancelled) => Err(ArchiveError::Cancelled),
            Err(e) => Ok(Some(e.to_string())),
        }
    }

    fn list_tar(&self) -> Result<(Vec<Entry>, u64), ArchiveError> {
        let mut out = Vec::new();
        let mut total = 0u64;
        let bad = self.stream(&mut NoCtl, &mut |item, _, _| {
            if item.kind == Kind::File {
                total = total.saturating_add(item.entry.size);
            }
            if !item.entry.path.is_empty() {
                out.push(item.entry);
            }
            Ok(Flow::Next)
        })?;
        match bad {
            Some(msg) => Err(ArchiveError::Corrupt(msg)),
            None => Ok((out, total)),
        }
    }

    fn process_tar(
        &self,
        req: Option<&ExtractRequest>,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError> {
        let total = self.file_total;
        let mut report = OpReport::default();
        let mut done = 0u64;
        let selection = Selection::of(req);
        let bad = self.stream(ctl, &mut |item, data, ctl| {
            if ctl.cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let entry = &item.entry;
            if entry.path.is_empty() || !selection.has(&entry.path) {
                return Ok(Flow::Next);
            }
            let mut fail = |error: ArchiveError| {
                report.failed.push(FileFailure {
                    path: entry.path.clone(),
                    error,
                });
            };
            if let Kind::Unsupported(what) = item.kind {
                if req.is_some() {
                    fail(ArchiveError::Unsupported(what.into()));
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
                        PlanOutcome::Nothing => return Ok(Flow::Next),
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
                    done = done.saturating_add(entry.size);
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
        if !Selection::of(req).has(&entry.path) {
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
                Ok(PlanOutcome::Nothing) => return Ok(report),
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
            let (entries, total) = self.list_tar()?;
            self.file_total = total;
            entries
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

    fn header_bytes(ty: u8, name: &str, size: u64) -> Vec<u8> {
        let mut h = ::tar::Header::new_gnu();
        {
            let o = h.as_old_mut();
            o.name[..name.len()].copy_from_slice(name.as_bytes());
        }
        h.set_mode(0o644);
        h.set_size(size);
        h.set_mtime(1);
        h.set_entry_type(::tar::EntryType::new(ty));
        h.set_cksum();
        h.as_bytes().to_vec()
    }

    fn padded(mut v: Vec<u8>) -> Vec<u8> {
        v.resize(v.len().div_ceil(512) * 512, 0);
        v
    }

    fn long_name(name: &str) -> Vec<u8> {
        let mut data = name.as_bytes().to_vec();
        data.push(0);
        let mut t = header_bytes(b'L', "././@LongLink", data.len() as u64);
        t.extend(padded(data));
        t
    }

    fn pax(records: &str) -> Vec<u8> {
        let mut t = header_bytes(b'x', "PaxHeaders/x", records.len() as u64);
        t.extend(padded(records.as_bytes().to_vec()));
        t
    }

    fn pax_record(key: &str, value: &str) -> String {
        let body = format!(" {key}={value}\n");
        let mut len = body.len() + 1;
        while len.to_string().len() + body.len() != len {
            len = len.to_string().len() + body.len();
        }
        format!("{len}{body}")
    }

    #[test]
    fn long_name_records_are_honoured_and_cannot_traverse() {
        let long = format!("{}/deep.txt", "d".repeat(150));
        let t = [
            long_name("../long-evil.txt"),
            file("short1", b"LONG"),
            pax(&pax_record("path", "../pax-evil.txt")),
            file("short2", b"PAX"),
            long_name(&long),
            file("short3", b"OK"),
            pax(&pax_record("path", "viapax/ok.txt")),
            file("short4", b"OK2"),
            file("fine.txt", b"fine"),
        ]
        .concat();
        let t = tar_of(&[t], 2);
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.tar", &t);
        let es = TarLikeEngine::new(&a, Format::Tar).list(None).unwrap().1;
        let names: Vec<_> = es.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(names[0], "../long-evil.txt");
        assert_eq!(names[1], "../pax-evil.txt");
        assert_eq!(names[2], long);
        assert_eq!(names[3], "viapax/ok.txt");
        let out = dir.path().join("x/y/out");
        let rep = TarLikeEngine::new(&a, Format::Tar)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        let bad: Vec<_> = rep.failed.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(bad, ["../long-evil.txt", "../pax-evil.txt"]);
        assert!(rep.failed.iter().all(|f| f.error.code() == "unsafe_path"));
        assert_eq!(rep.ok, 3);
        assert_eq!(fs::read(out.join(&long)).unwrap(), b"OK");
        assert_eq!(fs::read(out.join("viapax/ok.txt")).unwrap(), b"OK2");
        assert!(!dir.path().join("x/y/long-evil.txt").exists());
        assert!(!dir.path().join("x/y/pax-evil.txt").exists());
    }

    #[test]
    fn oversized_long_name_or_pax_record_is_rejected() {
        let over = 1024 * 1024 + 1;
        for ty in *b"LKx" {
            let mut t = header_bytes(ty, "x", over);
            t.extend(padded(vec![b'a'; over as usize]));
            t.extend(file("a.txt", b"x"));
            let t = tar_of(&[t], 2);
            let dir = tempfile::tempdir().unwrap();
            let a = write(dir.path(), "a.tar", &t);
            let l = TarLikeEngine::new(&a, Format::Tar).list(None);
            assert!(
                matches!(l, Err(ArchiveError::Corrupt(_))),
                "{}: {l:?}",
                ty as char
            );
            let r = TarLikeEngine::new(&a, Format::Tar).test(None, &mut ctl());
            assert!(is_corrupt(&r), "{}: {r:?}", ty as char);
        }
        // a declared size far beyond the file is rejected the same way
        let mut t = header_bytes(b'L', "x", 1 << 40);
        t.extend(file("a.txt", b"x"));
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "b.tar", &tar_of(&[t], 2));
        assert!(matches!(
            TarLikeEngine::new(&a, Format::Tar).list(None),
            Err(ArchiveError::Corrupt(_))
        ));
    }

    fn sparse_entry(extended: bool) -> Vec<u8> {
        let mut h = ::tar::Header::new_gnu();
        h.set_path("sparse.bin").unwrap();
        h.set_mode(0o644);
        h.set_entry_type(::tar::EntryType::GNUSparse);
        h.set_size(512);
        {
            let g = h.as_gnu_mut().unwrap();
            g.realsize.copy_from_slice(b"00000010000\0");
            g.sparse[0].offset.copy_from_slice(b"00000000000\0");
            g.sparse[0].numbytes.copy_from_slice(b"00000001000\0");
            if extended {
                g.isextended[0] = 1;
            }
        }
        h.set_cksum();
        let mut s = h.as_bytes().to_vec();
        if extended {
            s.extend(vec![0u8; 512]);
        }
        s.extend(vec![b's'; 512]);
        s
    }

    #[test]
    fn sparse_entry_is_not_a_truncation() {
        for extended in [false, true] {
            for last in [true, false] {
                let (a1, s) = (file("a.txt", b"a"), sparse_entry(extended));
                let t = if last { [a1, s] } else { [s, a1] };
                let t = tar_of(&t, 2);
                let dir = tempfile::tempdir().unwrap();
                let a = write(dir.path(), "a.tar", &t);
                let name = format!("ext={extended} last={last}");
                let es = TarLikeEngine::new(&a, Format::Tar).list(None);
                assert_eq!(es.map(|x| x.1.len()).ok(), Some(2), "{name}");
                let rep = TarLikeEngine::new(&a, Format::Tar)
                    .test(None, &mut ctl())
                    .unwrap();
                assert!(rep.failed.is_empty(), "{name}: {:?}", rep.failed);
                let out = dir.path().join("out");
                let rep = TarLikeEngine::new(&a, Format::Tar)
                    .extract(None, &req(&out), &mut ctl())
                    .unwrap();
                assert_eq!(rep.ok, 1, "{name}");
                assert_eq!(rep.failed.len(), 1, "{name}");
                assert_eq!(rep.failed[0].path, "sparse.bin");
                assert!(
                    matches!(&rep.failed[0].error, ArchiveError::Unsupported(m) if m.contains("sparse"))
                );
                assert_eq!(leftovers(&out), ["a.txt"]);
            }
        }
    }

    #[test]
    fn unsupported_reason_is_accurate() {
        let t = tar_of(
            &[
                raw_entry("l", Kind::Symlink, b"", "x"),
                header_bytes(b'6', "fifo", 0),
                header_bytes(b'3', "dev", 0),
                header_bytes(b'V', "label", 0),
            ],
            2,
        );
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.tar", &t);
        let rep = TarLikeEngine::new(&a, Format::Tar)
            .extract(None, &req(&dir.path().join("o")), &mut ctl())
            .unwrap();
        let msg = |p: &str| match &rep.failed.iter().find(|f| f.path == p).unwrap().error {
            ArchiveError::Unsupported(m) => m.clone(),
            e => panic!("{e:?}"),
        };
        assert_eq!(msg("l"), "link");
        assert!(msg("fifo").contains("fifo"));
        assert!(msg("dev").contains("device"));
        assert!(!msg("label").contains("link"));
    }

    #[test]
    fn cancel_is_polled_while_skipping_unselected_data() {
        use crate::engine::PollCtl;
        let big = vec![9u8; 8 * 1024 * 1024];
        let tar = tar_of(&[file("big.bin", &big), file("z.txt", b"z")], 2);
        for f in [Format::Tar, Format::TarGz] {
            let dir = tempfile::tempdir().unwrap();
            let a = write(dir.path(), "a.tar", &pack(f, &tar));
            let mut rq = req(&dir.path().join("o"));
            rq.selection = vec!["z.txt".into()];
            // runs to the end: many polls while the 8 MiB entry is skipped
            let mut c = PollCtl {
                calls: std::cell::Cell::new(0),
                cancel_at: u64::MAX,
            };
            let rep = TarLikeEngine::new(&a, f)
                .extract(None, &rq, &mut c)
                .unwrap();
            assert!(rep.failed.is_empty());
            assert!(c.calls.get() >= 100, "{f:?}: {}", c.calls.get());
            // and cancel stops it inside the skipped entry
            let mut c = PollCtl {
                calls: std::cell::Cell::new(0),
                cancel_at: 20,
            };
            let r = TarLikeEngine::new(&a, f).extract(None, &rq, &mut c);
            assert!(matches!(r, Err(ArchiveError::Cancelled)), "{f:?}: {r:?}");
            // the drain after the terminator polls too
            let t = tar_of(&[file("z.txt", b"z")], 2 + 16384);
            let a = write(dir.path(), "t.tar", &pack(f, &t));
            let mut c = PollCtl {
                calls: std::cell::Cell::new(0),
                cancel_at: 20,
            };
            let r = TarLikeEngine::new(&a, f).test(None, &mut c);
            assert!(
                matches!(r, Err(ArchiveError::Cancelled)),
                "{f:?} drain: {r:?}"
            );
        }
    }

    fn huge_sparse(name: &str) -> Vec<u8> {
        let mut h = ::tar::Header::new_gnu();
        h.set_path(name).unwrap();
        h.set_mode(0o644);
        h.set_entry_type(::tar::EntryType::GNUSparse);
        h.set_size(0);
        {
            let g = h.as_gnu_mut().unwrap();
            // base-256 number: 2^63 - 1
            let mut v = [0xFFu8; 12];
            v[0] = 0x80;
            v[1] = 0;
            v[2] = 0;
            v[3] = 0;
            v[4] = 0x7F;
            g.realsize.copy_from_slice(&v);
        }
        h.set_cksum();
        h.as_bytes().to_vec()
    }

    struct Totals(Vec<u64>);
    impl JobCtl for Totals {
        fn progress(&mut self, _: &str, _: u64, total: u64) {
            self.0.push(total);
        }
        fn cancelled(&self) -> bool {
            false
        }
        fn resolve_conflict(&mut self, _: &Path) -> crate::engine::ConflictChoice {
            crate::engine::ConflictChoice::Overwrite
        }
    }

    #[test]
    fn untrusted_sparse_sizes_do_not_overflow_totals() {
        let t = tar_of(
            &[
                huge_sparse("s1"),
                huge_sparse("s2"),
                file("a.txt", b"12345"),
            ],
            2,
        );
        let dir = tempfile::tempdir().unwrap();
        let a = write(dir.path(), "a.tar", &t);
        let mut e = TarLikeEngine::new(&a, Format::Tar);
        let es = e.list(None).unwrap().1;
        assert_eq!(es.len(), 3);
        let mut tot = Totals(vec![]);
        let rep = e
            .extract(None, &req(&dir.path().join("o")), &mut tot)
            .unwrap();
        assert_eq!(rep.ok, 1);
        assert_eq!(rep.failed.len(), 2);
        // the progress total counts only what is extracted
        assert_eq!(tot.0, [5]);
        let rep = TarLikeEngine::new(&a, Format::Tar)
            .test(None, &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty());
    }

    #[test]
    fn pax_size_empty_is_unset_and_nul_padding_is_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let recs = format!("{}{}", pax_record("size", ""), pax_record("path", "p.txt"));
        let t = tar_of(&[pax(&recs), file("short", b"data!")], 2);
        let a = write(dir.path(), "a.tar", &t);
        let es = TarLikeEngine::new(&a, Format::Tar).list(None).unwrap().1;
        assert_eq!((es[0].path.as_str(), es[0].size), ("p.txt", 5));
        let out = dir.path().join("o");
        let rep = TarLikeEngine::new(&a, Format::Tar)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        assert!(rep.failed.is_empty(), "{:?}", rep.failed);
        assert_eq!(fs::read(out.join("p.txt")).unwrap(), b"data!");
        // NUL padding inside the record block
        let mut data = pax_record("path", "n.txt").into_bytes();
        data.extend([0u8; 40]);
        let mut x = header_bytes(b'x', "PaxHeaders/x", data.len() as u64);
        x.extend(padded(data));
        let t = tar_of(&[x, file("short", b"nn")], 2);
        let a = write(dir.path(), "b.tar", &t);
        let es = TarLikeEngine::new(&a, Format::Tar).list(None).unwrap().1;
        assert_eq!(es[0].path, "n.txt");
        // other malformed records stay corrupt
        for bad in ["size=abc\n", "5 pa\n", "99 path=x\n"] {
            let mut data = format!("{} ", bad.len() + 3).into_bytes();
            data.extend(bad.as_bytes());
            if bad.starts_with('5') || bad.starts_with('9') {
                data = bad.as_bytes().to_vec();
            }
            let mut x = header_bytes(b'x', "PaxHeaders/x", data.len() as u64);
            x.extend(padded(data));
            let t = tar_of(&[x, file("short", b"nn")], 2);
            let a = write(dir.path(), "c.tar", &t);
            let l = TarLikeEngine::new(&a, Format::Tar).list(None);
            assert!(matches!(l, Err(ArchiveError::Corrupt(_))), "{bad}: {l:?}");
        }
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
        let mut cuts: Vec<usize> = (0..full - 512).step_by(512).collect();
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
