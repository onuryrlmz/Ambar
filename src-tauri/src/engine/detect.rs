use crate::error::ArchiveError;
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Format {
    Rar,
    Zip,
    SevenZ,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    Gz,
    Bz2,
    Xz,
}

const SCAN_LIMIT: u64 = 1024 * 1024;
const RAR4: &[u8] = &[0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x00];
const RAR5: &[u8] = &[0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x01, 0x00];

fn has_ext(name: &str, exts: &[&str]) -> bool {
    let n = name.to_lowercase();
    exts.iter().any(|e| n.ends_with(e))
}

/// Detects the archive format from file content; the name only disambiguates
/// compressed tarballs from plain compressed files.
pub fn detect_format(path: &Path) -> Result<Format, ArchiveError> {
    if path.is_dir() {
        return Err(ArchiveError::NotAnArchive);
    }
    let mut buf = Vec::new();
    std::fs::File::open(path)?
        .take(SCAN_LIMIT)
        .read_to_end(&mut buf)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    if buf.starts_with(RAR5) || buf.starts_with(RAR4) {
        return Ok(Format::Rar);
    }
    if buf.starts_with(&[0x50, 0x4B, 0x03, 0x04])
        || buf.starts_with(&[0x50, 0x4B, 0x05, 0x06])
        || buf.starts_with(&[0x50, 0x4B, 0x07, 0x08])
    {
        return Ok(Format::Zip);
    }
    if buf.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return Ok(Format::SevenZ);
    }
    if buf.starts_with(&[0x1F, 0x8B]) {
        return Ok(if has_ext(&name, &[".tar.gz", ".tgz"]) {
            Format::TarGz
        } else {
            Format::Gz
        });
    }
    if buf.starts_with(&[0x42, 0x5A, 0x68]) {
        return Ok(if has_ext(&name, &[".tar.bz2", ".tbz2", ".tbz"]) {
            Format::TarBz2
        } else {
            Format::Bz2
        });
    }
    if buf.starts_with(&[0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00]) {
        return Ok(if has_ext(&name, &[".tar.xz", ".txz"]) {
            Format::TarXz
        } else {
            Format::Xz
        });
    }
    if buf.len() >= 262 && &buf[257..262] == b"ustar" {
        return Ok(Format::Tar);
    }
    // Self-extracting RAR: signature after an executable stub.
    if buf.windows(RAR5.len()).any(|w| w == RAR5) || buf.windows(RAR4.len()).any(|w| w == RAR4) {
        return Ok(Format::Rar);
    }
    Err(ArchiveError::NotAnArchive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn make(name: &str, bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        std::fs::File::create(&p).unwrap().write_all(bytes).unwrap();
        (dir, p)
    }

    #[test]
    fn a_folder_is_not_an_archive() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(detect_format(d.path()), Err(ArchiveError::NotAnArchive)));
    }

    #[test]
    fn rar5_magic_is_rar_even_with_zip_extension() {
        let (_d, p) = make("x.zip", &[0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x01, 0x00]);
        assert_eq!(detect_format(&p).unwrap(), Format::Rar);
    }
    #[test]
    fn rar4_magic_is_rar() {
        let (_d, p) = make("x.rar", &[0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x00]);
        assert_eq!(detect_format(&p).unwrap(), Format::Rar);
    }
    #[test]
    fn zip_magic_with_rar_extension_is_zip() {
        let (_d, p) = make("x.rar", &[0x50, 0x4B, 0x03, 0x04, 0, 0]);
        assert_eq!(detect_format(&p).unwrap(), Format::Zip);
    }
    #[test]
    fn empty_zip_is_zip() {
        let (_d, p) = make("x.zip", &[0x50, 0x4B, 0x05, 0x06, 0, 0]);
        assert_eq!(detect_format(&p).unwrap(), Format::Zip);
    }
    #[test]
    fn sevenz_magic() {
        let (_d, p) = make("x.7z", &[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C, 0, 4]);
        assert_eq!(detect_format(&p).unwrap(), Format::SevenZ);
    }
    #[test]
    fn gz_with_tar_gz_name_is_targz() {
        let gz = [0x1F, 0x8B, 0x08, 0];
        let (_d, p) = make("a.tar.gz", &gz);
        assert_eq!(detect_format(&p).unwrap(), Format::TarGz);
        let (_d, p) = make("a.tgz", &gz);
        assert_eq!(detect_format(&p).unwrap(), Format::TarGz);
        let (_d, p) = make("b.gz", &gz);
        assert_eq!(detect_format(&p).unwrap(), Format::Gz);
    }
    #[test]
    fn bz2_and_xz_magic_with_names() {
        let bz = [0x42, 0x5A, 0x68, 0x39];
        let xz = [0xFD, 0x37, 0x7A, 0x58, 0x5A, 0x00];
        for n in ["a.tar.bz2", "a.tbz2"] {
            let (_d, p) = make(n, &bz);
            assert_eq!(detect_format(&p).unwrap(), Format::TarBz2);
        }
        let (_d, p) = make("a.bz2", &bz);
        assert_eq!(detect_format(&p).unwrap(), Format::Bz2);
        for n in ["a.tar.xz", "a.txz"] {
            let (_d, p) = make(n, &xz);
            assert_eq!(detect_format(&p).unwrap(), Format::TarXz);
        }
        let (_d, p) = make("a.xz", &xz);
        assert_eq!(detect_format(&p).unwrap(), Format::Xz);
    }
    #[test]
    fn tar_ustar_at_offset_257() {
        let mut b = vec![0u8; 512];
        b[257..262].copy_from_slice(b"ustar");
        let (_d, p) = make("a.tar", &b);
        assert_eq!(detect_format(&p).unwrap(), Format::Tar);
    }
    #[test]
    fn text_file_named_rar_is_not_archive() {
        let (_d, p) = make("x.rar", b"merhaba");
        assert!(matches!(detect_format(&p), Err(ArchiveError::NotAnArchive)));
    }
    #[test]
    fn sfx_rar_is_found_after_stub() {
        let mut b = vec![0x90u8; 4096];
        b.extend_from_slice(&[0x52, 0x61, 0x72, 0x21, 0x1A, 0x07, 0x01, 0x00]);
        let (_d, p) = make("setup.exe", &b);
        assert_eq!(detect_format(&p).unwrap(), Format::Rar);
    }
}
