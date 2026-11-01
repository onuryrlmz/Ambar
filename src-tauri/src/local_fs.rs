//! Local folder listing for the file panels. Sorting is left to the frontend.

use crate::error::ArchiveError;
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalEntry {
    pub name: String,
    pub path: String,
    /// Follows symlinks, so linked folders can be entered. A broken link is `false`.
    pub is_dir: bool,
    pub is_symlink: bool,
    /// Name-based: starts with a dot.
    pub hidden: bool,
    pub size: u64,
    /// Seconds since the Unix epoch.
    pub modified: Option<i64>,
}

pub fn list_local(dir: &Path) -> Result<Vec<LocalEntry>, ArchiveError> {
    let mut out = Vec::new();
    for item in std::fs::read_dir(dir)? {
        let Ok(item) = item else { continue };
        let path = item.path();
        let name = item.file_name().to_string_lossy().into_owned();
        let Ok(link_meta) = std::fs::symlink_metadata(&path) else { continue };
        let is_symlink = link_meta.file_type().is_symlink();
        // A broken link has no target metadata: list it as a plain, non-folder entry.
        let meta = if is_symlink {
            std::fs::metadata(&path).unwrap_or(link_meta)
        } else {
            link_meta
        };
        let modified = meta.modified().ok().map(|t| match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => i64::try_from(d.as_secs()).unwrap_or(i64::MAX),
            Err(e) => -i64::try_from(e.duration().as_secs()).unwrap_or(i64::MAX),
        });
        let is_dir = meta.is_dir();
        out.push(LocalEntry {
            hidden: name.starts_with('.'),
            name,
            path: path.to_string_lossy().into_owned(),
            is_dir,
            is_symlink,
            size: if is_dir { 0 } else { meta.len() },
            modified,
        });
    }
    Ok(out)
}

pub fn parent_dir(path: &Path) -> Option<String> {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_with_dirs_and_sizes() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("sub")).unwrap();
        std::fs::write(d.path().join("a.txt"), b"12345").unwrap();
        std::fs::write(d.path().join(".hid"), b"").unwrap();
        let mut v = list_local(d.path()).unwrap();
        v.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(v.len(), 3);
        assert_eq!(v[0].name, ".hid");
        assert!(v[0].hidden);
        let a = v.iter().find(|e| e.name == "a.txt").unwrap();
        assert_eq!((a.is_dir, a.size, a.hidden), (false, 5, false));
        assert!(a.modified.is_some());
        let s = v.iter().find(|e| e.name == "sub").unwrap();
        assert!(s.is_dir);
        let j = serde_json::to_value(a).unwrap();
        assert_eq!(j["isSymlink"], false);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_reported_and_broken_ones_do_not_fail() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join("real")).unwrap();
        std::os::unix::fs::symlink(d.path().join("real"), d.path().join("lnk")).unwrap();
        std::os::unix::fs::symlink(d.path().join("gone"), d.path().join("broken")).unwrap();
        let v = list_local(d.path()).unwrap();
        let l = v.iter().find(|e| e.name == "lnk").unwrap();
        assert!(l.is_dir && l.is_symlink);
        let b = v.iter().find(|e| e.name == "broken").unwrap();
        assert!(!b.is_dir && b.is_symlink);
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_dir_returns_permission_error() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("locked");
        std::fs::create_dir(&p).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
        let r = list_local(&p);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(r.unwrap_err().code(), "permission");
    }

    #[test]
    fn parent_dir_works() {
        assert_eq!(parent_dir(Path::new("/a/b")).as_deref(), Some("/a"));
        assert_eq!(parent_dir(Path::new("/")), None);
    }
}
