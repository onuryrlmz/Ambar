//! Guards for opening extracted files with their default app: a list of file types that
//! launch code, and post-extraction hardening (no exec bits, download-origin marks).

use crate::error::ArchiveError;
use std::path::Path;

const MAC: &[&str] = &[
    "command", "terminal", "tool", "jar", "pkg", "mpkg", "scpt", "scptd", "applescript",
    "workflow", "webloc", "fileloc", "inetloc", "app", "dmg", "sh",
];
const WINDOWS: &[&str] = &[
    "exe", "com", "bat", "cmd", "msi", "scr", "lnk", "ps1", "vbs", "js", "jse", "wsf", "hta",
    "reg", "url", "jar",
];
const LINUX: &[&str] = &["desktop", "sh", "jar", "appimage", "run"];

fn platform_list() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        MAC
    } else if cfg!(windows) {
        WINDOWS
    } else {
        LINUX
    }
}

/// Final extension, lowercased, ignoring trailing dots and spaces.
fn final_extension(name: &str) -> Option<String> {
    let name = name.trim_end_matches(['.', ' ']);
    let base = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let (stem, ext) = base.rsplit_once('.')?;
    (!stem.is_empty() || !ext.is_empty()).then(|| ext.to_lowercase())
}

fn risky_in(list: &[&str], name: &str) -> Option<String> {
    final_extension(name).filter(|e| list.contains(&e.as_str()))
}

/// The extension when `name` would launch code on this platform.
pub fn risky_extension(name: &str) -> Option<String> {
    risky_in(platform_list(), name)
}

/// Fails with `RiskyOpen` unless the user explicitly allowed it.
pub fn check_risky(name: &str, allow_risky: bool) -> Result<(), ArchiveError> {
    match risky_extension(name) {
        Some(ext) if !allow_risky => Err(ArchiveError::RiskyOpen(ext)),
        _ => Ok(()),
    }
}

/// Whether a file from the local pane may go to its default app. A link is judged by its
/// target as well: `masum.txt` pointing at a script launches the script.
pub fn check_local_open(path: &Path, allow_risky: bool) -> Result<(), ArchiveError> {
    if !path.is_file() {
        return Err(ArchiveError::Io("not a file".into()));
    }
    let real = std::fs::canonicalize(path)?;
    for p in [path, real.as_path()] {
        check_risky(&p.file_name().unwrap_or_default().to_string_lossy(), allow_risky)?;
    }
    Ok(())
}

/// Removes execute permission bits.
#[cfg(unix)]
pub fn strip_exec_bits(file: &Path) -> Result<(), ArchiveError> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::symlink_metadata(file)?.permissions().mode();
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(mode & !0o111))?;
    Ok(())
}

#[cfg(not(unix))]
pub fn strip_exec_bits(_file: &Path) -> Result<(), ArchiveError> {
    Ok(())
}

#[cfg(target_os = "macos")]
mod mac {
    use std::ffi::{c_char, c_int, c_void};
    extern "C" {
        pub fn setxattr(
            path: *const c_char,
            name: *const c_char,
            value: *const c_void,
            size: usize,
            position: u32,
            options: c_int,
        ) -> c_int;
        #[cfg(test)]
        pub fn getxattr(
            path: *const c_char,
            name: *const c_char,
            value: *mut c_void,
            size: usize,
            position: u32,
            options: c_int,
        ) -> isize;
    }
    pub const XATTR_NOFOLLOW: c_int = 1;
}

/// Marks the file as downloaded so the OS asks before launching it.
#[cfg(target_os = "macos")]
pub fn mark_downloaded(file: &Path) -> Result<(), ArchiveError> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let value = format!("0081;{secs:x};Ambar;");
    let path = CString::new(file.as_os_str().as_bytes())
        .map_err(|_| ArchiveError::Io("quarantine mark failed".into()))?;
    let name = CString::new("com.apple.quarantine").unwrap();
    let rc = unsafe {
        mac::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            mac::XATTR_NOFOLLOW,
        )
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(ArchiveError::Io("quarantine mark failed".into()))
    }
}

#[cfg(windows)]
pub fn mark_downloaded(file: &Path) -> Result<(), ArchiveError> {
    let mut stream = file.as_os_str().to_os_string();
    stream.push(":Zone.Identifier");
    std::fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n")
        .map_err(|_| ArchiveError::Io("zone mark failed".into()))
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn mark_downloaded(_file: &Path) -> Result<(), ArchiveError> {
    Ok(())
}

/// Prepares an extracted file for opening. A failed mark means: do not open.
pub fn harden(file: &Path, mark: bool) -> Result<(), ArchiveError> {
    strip_exec_bits(file)?;
    if mark {
        mark_downloaded(file)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_open_refuses_a_folder_and_a_missing_file() {
        let d = tempfile::tempdir().unwrap();
        assert!(matches!(check_local_open(d.path(), true), Err(ArchiveError::Io(_))));
        assert!(matches!(check_local_open(&d.path().join("yok.txt"), true), Err(ArchiveError::Io(_))));
    }

    #[test]
    fn local_open_guards_risky_types_unless_allowed() {
        let d = tempfile::tempdir().unwrap();
        let plain = d.path().join("belge.txt");
        std::fs::write(&plain, b"x").unwrap();
        check_local_open(&plain, false).unwrap();
        let name = if cfg!(windows) { "a.exe" } else if cfg!(target_os = "macos") { "a.command" } else { "a.sh" };
        let risky = d.path().join(name);
        std::fs::write(&risky, b"x").unwrap();
        assert!(matches!(check_local_open(&risky, false), Err(ArchiveError::RiskyOpen(_))));
        check_local_open(&risky, true).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn local_open_judges_a_link_by_its_target_too() {
        let d = tempfile::tempdir().unwrap();
        let name = if cfg!(target_os = "macos") { "a.command" } else { "a.sh" };
        let risky = d.path().join(name);
        std::fs::write(&risky, b"x").unwrap();
        let link = d.path().join("masum.txt");
        std::os::unix::fs::symlink(&risky, &link).unwrap();
        assert!(matches!(check_local_open(&link, false), Err(ArchiveError::RiskyOpen(_))));
    }

    #[test]
    fn risky_lists_are_case_insensitive_and_use_final_extension() {
        assert_eq!(risky_in(WINDOWS, "Setup.EXE").as_deref(), Some("exe"));
        assert_eq!(risky_in(WINDOWS, "a.exe. ").as_deref(), Some("exe"));
        assert_eq!(risky_in(WINDOWS, "a.exe.txt"), None);
        assert_eq!(risky_in(WINDOWS, "a.txt.js").as_deref(), Some("js"));
        assert_eq!(risky_in(WINDOWS, "dir/run.cmd").as_deref(), Some("cmd"));
        assert_eq!(risky_in(WINDOWS, "exe"), None);
        assert_eq!(risky_in(MAC, "X.App").as_deref(), Some("app"));
        assert_eq!(risky_in(MAC, "x.webloc").as_deref(), Some("webloc"));
        assert_eq!(risky_in(MAC, "x.pdf"), None);
        assert_eq!(risky_in(LINUX, "x.AppImage").as_deref(), Some("appimage"));
        assert_eq!(risky_in(LINUX, "x.desktop").as_deref(), Some("desktop"));
        assert_eq!(risky_in(LINUX, "x.exe"), None);
    }

    #[test]
    fn check_risky_needs_allow_flag() {
        let name = if cfg!(windows) { "a.bat" } else { "a.sh" };
        assert_eq!(check_risky(name, false).unwrap_err().code(), "risky_open");
        assert!(check_risky(name, true).is_ok());
        assert!(check_risky("a.txt", false).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn exec_bits_are_stripped() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x");
        std::fs::write(&f, b"x").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        strip_exec_bits(&f).unwrap();
        assert_eq!(std::fs::metadata(&f).unwrap().permissions().mode() & 0o777, 0o644);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn quarantine_attribute_is_set() {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x.txt");
        std::fs::write(&f, b"x").unwrap();
        harden(&f, true).unwrap();
        let path = CString::new(f.as_os_str().as_bytes()).unwrap();
        let name = CString::new("com.apple.quarantine").unwrap();
        let mut buf = [0u8; 128];
        let n = unsafe {
            mac::getxattr(path.as_ptr(), name.as_ptr(), buf.as_mut_ptr().cast(), buf.len(), 0, 0)
        };
        assert!(n > 0);
        let v = String::from_utf8_lossy(&buf[..n as usize]).into_owned();
        assert!(v.starts_with("0081;") && v.ends_with(";Ambar;"), "{v}");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn failed_quarantine_is_an_error() {
        assert!(harden(Path::new("/nonexistent/zz"), true).is_err());
    }
}
