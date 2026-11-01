//! ZIP engine. Names are decoded here (UTF-8, else CP857 for legacy Turkish
//! archives) and extraction never uses the crate's own `extract`/`enclosed_name`:
//! every output path goes through `plan_entry`, and files are written to a temp
//! file via `write_via_temp`.

use super::plan::{is_selected, normalize_entry_path, plan_entry, write_via_temp, PlanOutcome};
use super::{
    ArchiveEngine, ArchiveInfo, Entry, ExtractRequest, FileFailure, Format, JobCtl, OpReport,
    Password,
};
use crate::error::ArchiveError;
use std::fs::File;
use std::io::{BufReader, ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use zip::result::ZipError;
use zip::ZipArchive;

const CHUNK: usize = 64 * 1024;
const PROGRESS_STEP: u64 = 1024 * 1024;
const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;

pub struct ZipEngine {
    path: PathBuf,
}

struct Meta {
    entry: Entry,
    link: bool,
}

impl ZipEngine {
    pub fn new(path: &Path) -> ZipEngine {
        ZipEngine {
            path: path.to_path_buf(),
        }
    }

    fn open(&self) -> Result<ZipArchive<BufReader<File>>, ArchiveError> {
        let f = File::open(&self.path)?;
        ZipArchive::new(BufReader::new(f)).map_err(map_zip)
    }

    fn metas(ar: &mut ZipArchive<BufReader<File>>) -> Result<Vec<Meta>, ArchiveError> {
        let mut out = Vec::with_capacity(ar.len());
        for i in 0..ar.len() {
            let f = ar.by_index_raw(i).map_err(map_zip)?;
            let name = decode_name(f.name_raw());
            let path = normalize_entry_path(&name)
                .trim_end_matches('/')
                .to_string();
            out.push(Meta {
                entry: Entry {
                    path,
                    is_dir: f.is_dir(),
                    size: f.size(),
                    packed: Some(f.compressed_size()),
                    modified: f.last_modified().map(|d| {
                        let days =
                            days_from_civil(d.year() as i64, d.month() as i64, d.day() as i64);
                        days * 86400
                            + d.hour() as i64 * 3600
                            + d.minute() as i64 * 60
                            + d.second() as i64
                    }),
                    crc: Some(f.crc32()),
                    encrypted: f.encrypted(),
                },
                link: f.unix_mode().is_some_and(|m| m & S_IFMT == S_IFLNK),
            });
        }
        Ok(out)
    }

    fn process(
        &self,
        pw: Option<&Password>,
        req: Option<&ExtractRequest>,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError> {
        let mut ar = self.open()?;
        let metas = Self::metas(&mut ar)?;
        let wanted = |path: &str| req.is_none_or(|r| is_selected(path, &r.selection));
        let total: u64 = metas
            .iter()
            .filter(|m| !m.entry.is_dir && wanted(&m.entry.path))
            .map(|m| m.entry.size)
            .sum();
        let mut report = OpReport::default();
        let mut done = 0u64;

        for (i, m) in metas.iter().enumerate() {
            if ctl.cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let entry = &m.entry;
            if !wanted(&entry.path) {
                continue;
            }
            let fail = |report: &mut OpReport, error: ArchiveError| {
                report.failed.push(FileFailure {
                    path: entry.path.clone(),
                    error,
                });
            };
            if req.is_some() && m.link {
                fail(&mut report, ArchiveError::Unsupported("link".into()));
                continue;
            }
            ctl.progress(&entry.path, done, total);

            let result: Result<(), ArchiveError> = match req {
                None if entry.is_dir => continue,
                None => match open_file(&mut ar, i, entry, pw) {
                    Err(e @ (ArchiveError::WrongPassword | ArchiveError::PasswordRequired)) => {
                        return Err(e)
                    }
                    Err(e) => Err(e),
                    Ok(mut f) => copy(&mut f, None, entry, ctl, done, total).map(|_| ()),
                },
                Some(req) => {
                    let plan = match plan_entry(entry, req, ctl) {
                        Ok(p) => p,
                        Err(e @ (ArchiveError::UnsafePath(_) | ArchiveError::DestConflict(_))) => {
                            fail(&mut report, e);
                            continue;
                        }
                        Err(e) => return Err(e),
                    };
                    match plan {
                        PlanOutcome::Skip => {
                            report.skipped += 1;
                            continue;
                        }
                        PlanOutcome::Write(dest) if entry.is_dir => {
                            std::fs::create_dir_all(&dest)?;
                            report.ok += 1;
                            continue;
                        }
                        PlanOutcome::Write(dest) => match open_file(&mut ar, i, entry, pw) {
                            Err(
                                e @ (ArchiveError::WrongPassword | ArchiveError::PasswordRequired),
                            ) => return Err(e),
                            Err(e) => Err(e),
                            Ok(mut f) => write_via_temp(&dest, req.keep_broken, |tmp| {
                                let r = std::fs::OpenOptions::new()
                                    .write(true)
                                    .create_new(true)
                                    .open(tmp)
                                    .map_err(ArchiveError::from)
                                    .and_then(|mut out| {
                                        copy(&mut f, Some(&mut out), entry, ctl, done, total)
                                            .map(|_| ())
                                    });
                                match r {
                                    Err(ArchiveError::Corrupt(_)) if entry.encrypted => {
                                        // Garbage from a wrong key is not worth keeping.
                                        let _ = std::fs::remove_file(tmp);
                                        Err(ArchiveError::WrongPassword)
                                    }
                                    Err(e @ ArchiveError::Corrupt(_)) => Err(e),
                                    Err(e) => {
                                        // Cancel, disk and permission errors are not corruption.
                                        let _ = std::fs::remove_file(tmp);
                                        Err(e)
                                    }
                                    Ok(()) => Ok(()),
                                }
                            }),
                        },
                    }
                }
            };
            match result {
                Ok(()) => {
                    report.ok += 1;
                    done += entry.size;
                }
                // A wrong key that slipped past the header check fails per entry (a key
                // rejected at open time already ended the run above).
                Err(
                    e @ (ArchiveError::Corrupt(_)
                    | ArchiveError::Unsupported(_)
                    | ArchiveError::WrongPassword),
                ) => fail(&mut report, e),
                Err(e) => return Err(e),
            }
        }
        Ok(report)
    }
}

fn open_file<'a>(
    ar: &'a mut ZipArchive<BufReader<File>>,
    index: usize,
    entry: &Entry,
    pw: Option<&Password>,
) -> Result<zip::read::ZipFile<'a, BufReader<File>>, ArchiveError> {
    if entry.encrypted {
        let pw = pw.ok_or(ArchiveError::PasswordRequired)?;
        ar.by_index_decrypt(index, pw.as_bytes()).map_err(map_zip)
    } else {
        ar.by_index(index).map_err(map_zip)
    }
}

/// Streams `src` to `out` (or nowhere). Checks cancel on every chunk and reports
/// progress about once per MiB. The zip reader verifies the CRC at end of data.
fn copy(
    src: &mut impl Read,
    mut out: Option<&mut File>,
    entry: &Entry,
    ctl: &mut dyn JobCtl,
    done: u64,
    total: u64,
) -> Result<u64, ArchiveError> {
    let mut buf = vec![0u8; CHUNK];
    let mut bytes = 0u64;
    let mut reported = 0u64;
    loop {
        if ctl.cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let n = match src.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(match e.kind() {
                    ErrorKind::InvalidData | ErrorKind::UnexpectedEof => {
                        ArchiveError::Corrupt(format!("{}: {e}", entry.path))
                    }
                    _ => e.into(),
                })
            }
        };
        if let Some(out) = out.as_deref_mut() {
            out.write_all(&buf[..n])?;
        }
        bytes += n as u64;
        if bytes - reported >= PROGRESS_STEP {
            reported = bytes;
            ctl.progress(&entry.path, done + bytes, total);
        }
    }
    if let Some(out) = out {
        out.flush()?;
    }
    Ok(bytes)
}

fn map_zip(e: ZipError) -> ArchiveError {
    match e {
        ZipError::Io(e) => e.into(),
        ZipError::InvalidPassword => ArchiveError::WrongPassword,
        ZipError::UnsupportedArchive(s) if s == ZipError::PASSWORD_REQUIRED => {
            ArchiveError::PasswordRequired
        }
        ZipError::UnsupportedArchive(s) => ArchiveError::Unsupported(s.to_string()),
        ZipError::CompressionMethodNotSupported(m) => {
            ArchiveError::Unsupported(format!("compression method {m}"))
        }
        other => ArchiveError::Corrupt(other.to_string()),
    }
}

/// Valid UTF-8 stays as is; anything else is taken as CP857 (legacy Turkish tools).
fn decode_name(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => oem_cp::decode_string_incomplete_table_lossy(
            raw,
            &oem_cp::code_table::DECODING_TABLE_CP857,
        ),
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (DOS times are local; taken as UTC).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

impl ArchiveEngine for ZipEngine {
    fn list(&mut self, _pw: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
        let mut ar = self.open()?;
        let comment = ar.comment();
        let comment = (!comment.is_empty()).then(|| decode_name(comment));
        let entries = Self::metas(&mut ar)?.into_iter().map(|m| m.entry).collect();
        let info = ArchiveInfo {
            format: Format::Zip,
            file_size: std::fs::metadata(&self.path)?.len(),
            encrypted_headers: false,
            solid: false,
            multi_volume: false,
            has_recovery: false,
            locked: false,
            comment,
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
    use ::zip::write::SimpleFileOptions;
    use ::zip::{CompressionMethod, ZipWriter};
    use std::fs;
    use std::fs::File;
    use std::path::PathBuf;

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

    fn stored() -> SimpleFileOptions {
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored)
    }

    fn make(path: &Path, files: &[(&str, &[u8])]) {
        let mut w = ZipWriter::new(File::create(path).unwrap());
        for (n, d) in files {
            w.start_file(*n, stored()).unwrap();
            w.write_all(d).unwrap();
        }
        w.finish().unwrap();
    }

    fn patch_all(bytes: &mut [u8], from: &[u8], to: &[u8]) {
        let mut i = 0;
        let mut hits = 0;
        while i + from.len() <= bytes.len() {
            if &bytes[i..i + from.len()] == from {
                bytes[i..i + to.len()].copy_from_slice(to);
                i += from.len();
                hits += 1;
            } else {
                i += 1;
            }
        }
        assert!(hits > 0);
    }

    fn clear_utf8_flag(bytes: &mut [u8]) {
        for i in 0..bytes.len().saturating_sub(10) {
            if bytes[i..i + 4] == [0x50, 0x4b, 3, 4] {
                bytes[i + 7] &= !0x08;
            } else if bytes[i..i + 4] == [0x50, 0x4b, 1, 2] {
                bytes[i + 9] &= !0x08;
            }
        }
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

    fn list(path: &Path) -> Vec<Entry> {
        ZipEngine::new(path).list(None).unwrap().1
    }

    // --- hand-built ZipCrypto archive (the zip writer cannot produce one) ---
    fn crc_update(crc: u32, b: u8) -> u32 {
        let mut c = crc ^ b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB88320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        c
    }

    fn crc32(data: &[u8]) -> u32 {
        !data.iter().fold(!0u32, |c, &b| crc_update(c, b))
    }

    fn zipcrypto_zip(name: &str, data: &[u8], password: &str) -> Vec<u8> {
        let mut k = [305419896u32, 591751049, 878082192];
        let upd = |k: &mut [u32; 3], c: u8| {
            k[0] = crc_update(k[0], c);
            k[1] = k[1]
                .wrapping_add(k[0] & 0xff)
                .wrapping_mul(134775813)
                .wrapping_add(1);
            k[2] = crc_update(k[2], (k[1] >> 24) as u8);
        };
        for b in password.bytes() {
            upd(&mut k, b);
        }
        let crc = crc32(data);
        let mut plain = vec![7u8; 11];
        plain.push((crc >> 24) as u8);
        plain.extend_from_slice(data);
        let enc: Vec<u8> = plain
            .iter()
            .map(|&p| {
                let t = (k[2] | 2) as u16;
                let c = p ^ ((t.wrapping_mul(t ^ 1)) >> 8) as u8;
                upd(&mut k, p);
                c
            })
            .collect();
        let n = name.as_bytes();
        let mut out = vec![];
        let le16 = |v: &mut Vec<u8>, x: u16| v.extend_from_slice(&x.to_le_bytes());
        let le32 = |v: &mut Vec<u8>, x: u32| v.extend_from_slice(&x.to_le_bytes());
        le32(&mut out, 0x04034b50);
        for x in [20u16, 1, 0, 0, 0x21] {
            le16(&mut out, x);
        }
        le32(&mut out, crc);
        le32(&mut out, enc.len() as u32);
        le32(&mut out, data.len() as u32);
        le16(&mut out, n.len() as u16);
        le16(&mut out, 0);
        out.extend_from_slice(n);
        out.extend_from_slice(&enc);
        let cd = out.len() as u32;
        le32(&mut out, 0x02014b50);
        for x in [20u16, 20, 1, 0, 0, 0x21] {
            le16(&mut out, x);
        }
        le32(&mut out, crc);
        le32(&mut out, enc.len() as u32);
        le32(&mut out, data.len() as u32);
        le16(&mut out, n.len() as u16);
        for _ in 0..4 {
            le16(&mut out, 0);
        }
        le32(&mut out, 0);
        le32(&mut out, 0);
        out.extend_from_slice(n);
        let cdsize = out.len() as u32 - cd;
        le32(&mut out, 0x06054b50);
        le16(&mut out, 0);
        le16(&mut out, 0);
        le16(&mut out, 1);
        le16(&mut out, 1);
        le32(&mut out, cdsize);
        le32(&mut out, cd);
        le16(&mut out, 0);
        out
    }

    #[test]
    fn lists_and_extracts_utf8_names() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        make(
            &z,
            &[("belgeler/öğrenci.txt", b"merhaba"), ("kök.txt", b"x")],
        );
        let (info, entries) = ZipEngine::new(&z).list(None).unwrap();
        assert_eq!(info.format, Format::Zip);
        let paths: Vec<_> = entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["belgeler/öğrenci.txt", "kök.txt"]);
        assert_eq!(entries[0].size, 7);
        assert!(entries[0].modified.is_some());
        let out = d.path().join("out");
        let r = ZipEngine::new(&z)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        assert_eq!((r.ok, r.failed.len()), (2, 0));
        assert_eq!(
            fs::read(out.join("belgeler/öğrenci.txt")).unwrap(),
            b"merhaba"
        );
        assert_eq!(leftovers(&out).len(), 2);
    }

    #[test]
    fn cp857_name_without_utf8_flag_decodes_turkish() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        make(&z, &[("kurulu_.txt", b"x")]);
        let mut b = fs::read(&z).unwrap();
        patch_all(&mut b, b"kurulu_.txt", b"kurulu\x9f.txt");
        fs::write(&z, b).unwrap();
        assert_eq!(list(&z)[0].path, "kuruluş.txt");
        let out = d.path().join("out");
        ZipEngine::new(&z)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        assert!(out.join("kuruluş.txt").exists());
    }

    #[test]
    fn valid_utf8_without_flag_stays_utf8() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        make(&z, &[("şeker.txt", b"x")]);
        let mut b = fs::read(&z).unwrap();
        clear_utf8_flag(&mut b);
        fs::write(&z, b).unwrap();
        assert_eq!(list(&z)[0].path, "şeker.txt");
    }

    #[test]
    fn traversal_entry_is_failed_not_written() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        make(
            &z,
            &[("ok1.txt", b"1"), ("../evil.txt", b"e"), ("ok2.txt", b"2")],
        );
        let out = d.path().join("out");
        let r = ZipEngine::new(&z)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        assert_eq!(r.ok, 2);
        assert_eq!(r.failed.len(), 1);
        assert_eq!(r.failed[0].path, "../evil.txt");
        assert!(matches!(r.failed[0].error, ArchiveError::UnsafePath(_)));
        assert!(!d.path().join("evil.txt").exists());
        assert_eq!(leftovers(&out), ["ok1.txt", "ok2.txt"]);
    }

    #[test]
    fn aes_encrypted_needs_password() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        let mut w = ZipWriter::new(File::create(&z).unwrap());
        w.start_file(
            "gizli.txt",
            stored().with_aes_encryption(::zip::AesMode::Aes256, "doğru"),
        )
        .unwrap();
        w.write_all(b"secret content").unwrap();
        w.finish().unwrap();
        let out = d.path().join("out");
        assert!(list(&z)[0].encrypted);
        let mut e = ZipEngine::new(&z);
        assert!(matches!(
            e.extract(None, &req(&out), &mut ctl()),
            Err(ArchiveError::PasswordRequired)
        ));
        let bad = pw("yanlış");
        assert!(matches!(
            e.extract(Some(&bad), &req(&out), &mut ctl()),
            Err(ArchiveError::WrongPassword)
        ));
        assert!(!out.exists() || leftovers(&out).is_empty());
        let good = pw("doğru");
        let r = e.extract(Some(&good), &req(&out), &mut ctl()).unwrap();
        assert_eq!((r.ok, r.failed.len()), (1, 0));
        assert_eq!(fs::read(out.join("gizli.txt")).unwrap(), b"secret content");
    }

    #[test]
    fn zipcrypto_wrong_password_detected() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        let content = b"zipcrypto protected payload, long enough to matter";
        fs::write(&z, zipcrypto_zip("c.txt", content, "right")).unwrap();
        let mut e = ZipEngine::new(&z);
        let out = d.path().join("good");
        let r = e
            .extract(Some(&pw("right")), &req(&out), &mut ctl())
            .unwrap();
        assert_eq!((r.ok, r.failed.len()), (1, 0));
        assert_eq!(fs::read(out.join("c.txt")).unwrap(), content);
        assert!(matches!(
            e.extract(None, &req(&out), &mut ctl()),
            Err(ArchiveError::PasswordRequired)
        ));
        for wrong in ["wrong", "Right", "rigth", "x", "right "] {
            let out = d.path().join(format!("w-{wrong}"));
            match e.extract(Some(&pw(wrong)), &req(&out), &mut ctl()) {
                Err(ArchiveError::WrongPassword) => {}
                Ok(r) => {
                    assert_eq!(r.ok, 0, "{wrong}");
                    assert_eq!(r.failed.len(), 1, "{wrong}");
                }
                Err(other) => panic!("{wrong}: {other:?}"),
            }
            assert!(!out.exists() || leftovers(&out).is_empty(), "{wrong}");
        }
    }

    const PAYLOAD: &[u8] = b"PAYLOAD-0123456789-ABCDEFGHIJ";

    fn corrupt_zip(dir: &Path) -> PathBuf {
        let z = dir.join("c.zip");
        make(&z, &[("a.txt", PAYLOAD), ("b.txt", b"fine")]);
        let mut b = fs::read(&z).unwrap();
        let at = b.windows(PAYLOAD.len()).position(|w| w == PAYLOAD).unwrap();
        b[at + 3] ^= 0xff;
        fs::write(&z, b).unwrap();
        z
    }

    #[test]
    fn corrupt_crc_is_reported() {
        let d = tempfile::tempdir().unwrap();
        let z = corrupt_zip(d.path());
        let out = d.path().join("out");
        let r = ZipEngine::new(&z)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        assert_eq!(r.failed.len(), 1);
        assert_eq!(r.failed[0].path, "a.txt");
        assert!(matches!(r.failed[0].error, ArchiveError::Corrupt(_)));
        assert_eq!(r.ok, 1);
        assert_eq!(leftovers(&out), ["b.txt"]);
        let t = ZipEngine::new(&z).test(None, &mut ctl()).unwrap();
        assert_eq!(t.failed.len(), 1);
    }

    #[test]
    fn corrupt_keep_broken_keeps_file() {
        let d = tempfile::tempdir().unwrap();
        let z = corrupt_zip(d.path());
        let out = d.path().join("out");
        let mut rq = req(&out);
        rq.keep_broken = true;
        let r = ZipEngine::new(&z).extract(None, &rq, &mut ctl()).unwrap();
        assert_eq!(r.failed.len(), 1);
        assert_eq!(leftovers(&out), ["a.txt", "b.txt"]);
        assert_eq!(fs::read(out.join("a.txt")).unwrap().len(), PAYLOAD.len());
    }

    #[test]
    fn directories_only_entries_created() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        let mut w = ZipWriter::new(File::create(&z).unwrap());
        w.add_directory("bos/", stored()).unwrap();
        w.add_directory("bos/alt/", stored()).unwrap();
        w.finish().unwrap();
        let es = list(&z);
        assert!(es.iter().all(|e| e.is_dir && !e.path.ends_with('/')));
        assert_eq!(es[0].path, "bos");
        let out = d.path().join("out");
        let r = ZipEngine::new(&z)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        assert_eq!((r.ok, r.failed.len()), (2, 0));
        assert!(out.join("bos/alt").is_dir());
    }

    #[test]
    fn cancel_stops_extract() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        make(&z, &[("1.txt", b"1"), ("2.txt", b"2"), ("3.txt", b"3")]);
        let out = d.path().join("out");
        let mut c = ctl();
        c.cancel_after = Some(1);
        let r = ZipEngine::new(&z).extract(None, &req(&out), &mut c);
        assert!(matches!(r, Err(ArchiveError::Cancelled)));
        assert!(leftovers(&out).len() < 3);
    }

    #[test]
    fn cancel_mid_file_removes_partial_output() {
        let big = vec![0xA5u8; 4 * 1024 * 1024];
        for keep in [false, true] {
            let d = tempfile::tempdir().unwrap();
            let z = d.path().join("a.zip");
            make(&z, &[("big.bin", &big)]);
            let out = d.path().join("out");
            let mut rq = req(&out);
            rq.keep_broken = keep;
            let mut c = ctl();
            // 1 = the entry-start progress, 2 = the first MiB chunk, then cancel fires.
            c.cancel_after = Some(2);
            let r = ZipEngine::new(&z).extract(None, &rq, &mut c);
            assert!(matches!(r, Err(ArchiveError::Cancelled)), "keep={keep}");
            assert!(leftovers(&out).is_empty(), "keep={keep}");
            assert!(c.seen.len() < 6);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_entry_is_failed_not_created() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        let mut w = ZipWriter::new(File::create(&z).unwrap());
        w.add_symlink("link", "/etc/passwd", SimpleFileOptions::default())
            .unwrap();
        w.start_file("after.txt", stored()).unwrap();
        w.write_all(b"x").unwrap();
        w.finish().unwrap();
        let out = d.path().join("out");
        let r = ZipEngine::new(&z)
            .extract(None, &req(&out), &mut ctl())
            .unwrap();
        assert_eq!(r.ok, 1);
        assert_eq!(r.failed.len(), 1);
        assert_eq!(r.failed[0].path, "link");
        assert!(matches!(r.failed[0].error, ArchiveError::Unsupported(_)));
        assert!(fs::symlink_metadata(out.join("link")).is_err());
        assert_eq!(leftovers(&out), ["after.txt"]);
    }

    #[test]
    fn truncated_zip_is_never_clean() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        make(&z, &[("a.txt", &[1u8; 5000]), ("b.txt", &[2u8; 5000])]);
        let full = fs::read(&z).unwrap();
        for cut in [full.len() - 10, full.len() / 2, 30] {
            fs::write(&z, &full[..cut]).unwrap();
            let out = d.path().join(format!("o{cut}"));
            let mut e = ZipEngine::new(&z);
            assert!(
                matches!(e.list(None), Err(ArchiveError::Corrupt(_))),
                "{cut}"
            );
            assert!(
                matches!(
                    e.extract(None, &req(&out), &mut ctl()),
                    Err(ArchiveError::Corrupt(_))
                ),
                "{cut}"
            );
            assert!(!out.exists(), "{cut}");
        }
    }

    #[test]
    fn open_engine_dispatches_zip() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        make(&z, &[("a.txt", b"x")]);
        let mut e = crate::engine::open_engine(&z).unwrap();
        assert_eq!(e.list(None).unwrap().1.len(), 1);
    }
}
