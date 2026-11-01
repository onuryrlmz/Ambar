//! Path-safety and overwrite planning. This is the extraction security boundary:
//! every engine must route each output path through `plan_entry`/`safe_dest`.

use super::{ConflictChoice, Entry, ExtractRequest, JobCtl, OverwritePolicy};
use crate::error::ArchiveError;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, PartialEq, Eq)]
pub enum PlanOutcome {
    Write(PathBuf),
    Skip,
}

/// Converts `\` to `/`, strips leading `/` and `./`, applies NFC.
pub fn normalize_entry_path(raw: &str) -> String {
    let s = raw.replace('\\', "/");
    let mut s = s.as_str();
    loop {
        if let Some(r) = s.strip_prefix('/') {
            s = r;
        } else if let Some(r) = s.strip_prefix("./") {
            s = r;
        } else {
            break;
        }
    }
    s.nfc().collect()
}

#[cfg_attr(not(windows), allow(dead_code))]
fn sanitize_component(c: &str) -> String {
    let mut s: String = c
        .chars()
        .map(|ch| {
            if matches!(ch, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (ch as u32) < 32 {
                '_'
            } else {
                ch
            }
        })
        .collect();
    let kept = s.trim_end_matches(['.', ' ']).len();
    let trailing = s.len() - kept;
    s.truncate(kept);
    s.push_str(&"_".repeat(trailing));
    let stem = s.split('.').next().unwrap_or("").to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        s.insert(0, '_');
    }
    s
}

/// Joins `rel` under `dest_root`, refusing anything that could escape it.
pub fn safe_dest(dest_root: &Path, rel: &str) -> Result<PathBuf, ArchiveError> {
    let bad = || ArchiveError::UnsafePath(rel.to_string());
    if rel.contains('\0') || rel.starts_with('/') || rel.starts_with('\\') {
        return Err(bad());
    }
    let b = rel.as_bytes();
    if b.len() >= 2
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && (b.len() == 2 || b[2] == b'/' || b[2] == b'\\')
    {
        return Err(bad());
    }
    let mut out = dest_root.to_path_buf();
    let mut count = 0;
    for comp in rel.split(['/', '\\']) {
        match comp {
            "" | "." => continue,
            ".." => return Err(bad()),
            c => {
                if cfg!(windows) {
                    out.push(sanitize_component(c));
                } else {
                    out.push(c);
                }
                count += 1;
            }
        }
    }
    if count == 0 {
        return Err(bad());
    }
    // Refuse to write through any symlink that already exists under the root.
    let mut cur = dest_root.to_path_buf();
    let tail = out.strip_prefix(dest_root).map_err(|_| bad())?;
    for c in tail.components() {
        cur.push(c);
        match std::fs::symlink_metadata(&cur) {
            Ok(m) if m.file_type().is_symlink() => return Err(bad()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => break,
            Err(e) => return Err(e.into()),
        }
    }
    Ok(out)
}

pub fn is_selected(entry_path: &str, selection: &[String]) -> bool {
    if selection.is_empty() {
        return true;
    }
    selection.iter().any(|s| {
        let s = s.trim_end_matches('/');
        entry_path == s
            || entry_path
                .strip_prefix(s)
                .is_some_and(|rest| rest.starts_with('/'))
    })
}

/// Path of the entry relative to the destination, or `None` if nothing to write.
pub fn output_rel(entry_path: &str, req: &ExtractRequest) -> Option<String> {
    let rel = if req.keep_paths {
        let base = req.base.trim_matches('/');
        if base.is_empty() {
            entry_path
        } else if entry_path == base {
            return None;
        } else {
            entry_path
                .strip_prefix(base)
                .and_then(|r| r.strip_prefix('/'))
                .unwrap_or(entry_path)
        }
    } else {
        entry_path.rsplit('/').next().unwrap_or(entry_path)
    };
    if rel.is_empty() {
        None
    } else {
        Some(rel.to_string())
    }
}

fn free_name(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name.as_str(), ""),
    };
    let mut n = 1;
    loop {
        let cand = target.with_file_name(format!("{stem} ({n}){ext}"));
        if std::fs::symlink_metadata(&cand).is_err() {
            return cand;
        }
        n += 1;
    }
}

pub fn plan_entry(
    entry: &Entry,
    req: &ExtractRequest,
    ctl: &mut dyn JobCtl,
) -> Result<PlanOutcome, ArchiveError> {
    let Some(rel) = output_rel(&entry.path, req) else {
        return Ok(PlanOutcome::Skip);
    };
    if entry.is_dir && !req.keep_paths {
        return Ok(PlanOutcome::Skip);
    }
    let dest = safe_dest(&req.dest, &rel)?;
    let existing = match std::fs::symlink_metadata(&dest) {
        Ok(m) => Some(m),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.into()),
    };
    let Some(meta) = existing else {
        return Ok(PlanOutcome::Write(dest));
    };
    if entry.is_dir {
        return if meta.is_dir() {
            Ok(PlanOutcome::Write(dest))
        } else {
            Err(ArchiveError::DestConflict(rel))
        };
    }
    if meta.is_dir() {
        return Err(ArchiveError::DestConflict(rel));
    }
    let choice = match req.overwrite {
        OverwritePolicy::Overwrite => ConflictChoice::Overwrite,
        OverwritePolicy::Skip => ConflictChoice::Skip,
        OverwritePolicy::Rename => ConflictChoice::Rename,
        OverwritePolicy::Ask => ctl.resolve_conflict(&dest),
    };
    match choice {
        ConflictChoice::Overwrite => Ok(PlanOutcome::Write(dest)),
        ConflictChoice::Skip => Ok(PlanOutcome::Skip),
        ConflictChoice::Rename => Ok(PlanOutcome::Write(free_name(&dest))),
        ConflictChoice::Cancel => Err(ArchiveError::Cancelled),
    }
}

/// Writes to `<target>.ambar-part` and renames on success. On failure the part
/// file is deleted, or kept under the target name when `keep_broken`.
pub fn write_via_temp(
    target: &Path,
    keep_broken: bool,
    write: impl FnOnce(&Path) -> Result<(), ArchiveError>,
) -> Result<(), ArchiveError> {
    let mut name: OsString = target.as_os_str().to_owned();
    name.push(".ambar-part");
    let temp = PathBuf::from(name);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    match write(&temp) {
        Ok(()) => {
            std::fs::rename(&temp, target)?;
            Ok(())
        }
        Err(e) => {
            if keep_broken && temp.exists() {
                let _ = std::fs::rename(&temp, target);
            } else {
                let _ = std::fs::remove_file(&temp);
            }
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::TestCtl;
    use std::fs;

    fn ctl(choice: ConflictChoice) -> TestCtl {
        TestCtl {
            cancel_after: None,
            choice,
            seen: vec![],
        }
    }

    fn req(dest: &Path, policy: OverwritePolicy) -> ExtractRequest {
        ExtractRequest {
            dest: dest.to_path_buf(),
            selection: vec![],
            base: String::new(),
            keep_paths: true,
            overwrite: policy,
            keep_broken: false,
        }
    }

    fn file(path: &str) -> Entry {
        Entry {
            path: path.into(),
            is_dir: false,
            size: 1,
            packed: None,
            modified: None,
            crc: None,
            encrypted: false,
        }
    }

    fn unsafe_path(r: Result<PathBuf, ArchiveError>) -> bool {
        matches!(r, Err(ArchiveError::UnsafePath(_)))
    }

    #[test]
    fn plain_relative_path_joins() {
        assert_eq!(
            safe_dest(Path::new("/d"), "a/b.txt").unwrap(),
            Path::new("/d/a/b.txt")
        );
    }

    #[test]
    fn parent_traversal_rejected() {
        assert!(unsafe_path(safe_dest(Path::new("/d"), "../x")));
        assert!(unsafe_path(safe_dest(Path::new("/d"), "a/../../x")));
    }

    #[test]
    fn inner_parent_within_root_rejected_too() {
        assert!(unsafe_path(safe_dest(Path::new("/d"), "a/../x")));
    }

    #[test]
    fn absolute_and_drive_paths_rejected() {
        for p in ["/etc/x", "C:\\x", "\\\\srv\\x", "", "a\0b"] {
            assert!(unsafe_path(safe_dest(Path::new("/d"), p)), "{p:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_in_existing_parent_rejected() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dest = root.path().join("dest");
        fs::create_dir(&dest).unwrap();
        std::os::unix::fs::symlink(outside.path(), dest.join("link")).unwrap();
        assert!(unsafe_path(safe_dest(&dest, "link/x")));
        assert!(unsafe_path(safe_dest(&dest, "link")));
    }

    #[test]
    fn nfd_name_becomes_nfc() {
        assert_eq!(normalize_entry_path("s\u{327}.txt"), "ş.txt");
        assert_eq!(normalize_entry_path("\\./a\\b"), "a/b");
    }

    #[cfg(windows)]
    #[test]
    fn windows_reserved_names_sanitized() {
        let r = Path::new("C:\\d");
        assert_eq!(safe_dest(r, "CON.txt").unwrap(), r.join("_CON.txt"));
        assert_eq!(safe_dest(r, "a:b").unwrap(), r.join("a_b"));
        assert_eq!(safe_dest(r, "x.").unwrap(), r.join("x_"));
        assert_eq!(sanitize_component("a:b"), "a_b");
        assert_eq!(sanitize_component("x."), "x_");
    }

    #[test]
    fn selection_matches_file_and_children() {
        let sel = vec!["a".to_string()];
        assert!(is_selected("a", &sel));
        assert!(is_selected("a/b", &sel));
        assert!(!is_selected("ab", &sel));
    }

    #[test]
    fn empty_selection_matches_all() {
        assert!(is_selected("anything/at/all", &[]));
    }

    #[test]
    fn output_rel_strips_base_when_keep_paths() {
        let mut r = req(Path::new("/d"), OverwritePolicy::Ask);
        r.base = "belgeler".into();
        assert_eq!(
            output_rel("belgeler/x/y.txt", &r).as_deref(),
            Some("x/y.txt")
        );
    }

    #[test]
    fn output_rel_flattens_when_not_keep_paths() {
        let mut r = req(Path::new("/d"), OverwritePolicy::Ask);
        r.keep_paths = false;
        assert_eq!(output_rel("belgeler/x/y.txt", &r).as_deref(), Some("y.txt"));
    }

    #[test]
    fn plan_overwrite_policy_rename_picks_free_name() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.txt"), "x").unwrap();
        let r = req(d.path(), OverwritePolicy::Rename);
        let mut c = ctl(ConflictChoice::Cancel);
        assert_eq!(
            plan_entry(&file("a.txt"), &r, &mut c).unwrap(),
            PlanOutcome::Write(d.path().join("a (1).txt"))
        );
        fs::write(d.path().join("a (1).txt"), "x").unwrap();
        assert_eq!(
            plan_entry(&file("a.txt"), &r, &mut c).unwrap(),
            PlanOutcome::Write(d.path().join("a (2).txt"))
        );
    }

    #[test]
    fn plan_skip_policy_returns_skip() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.txt"), "x").unwrap();
        let r = req(d.path(), OverwritePolicy::Skip);
        let mut c = ctl(ConflictChoice::Overwrite);
        assert_eq!(
            plan_entry(&file("a.txt"), &r, &mut c).unwrap(),
            PlanOutcome::Skip
        );
    }

    #[test]
    fn plan_ask_cancel_returns_cancelled() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a.txt"), "x").unwrap();
        let r = req(d.path(), OverwritePolicy::Ask);
        let mut c = ctl(ConflictChoice::Cancel);
        assert!(matches!(
            plan_entry(&file("a.txt"), &r, &mut c),
            Err(ArchiveError::Cancelled)
        ));
    }

    #[test]
    fn file_over_existing_directory_is_conflict_error() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir(d.path().join("a")).unwrap();
        let r = req(d.path(), OverwritePolicy::Overwrite);
        let mut c = ctl(ConflictChoice::Overwrite);
        assert!(matches!(
            plan_entry(&file("a"), &r, &mut c),
            Err(ArchiveError::DestConflict(_))
        ));
        assert!(d.path().join("a").is_dir());
    }

    #[test]
    fn write_via_temp_failure_leaves_no_files() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("sub/f.txt");
        let r = write_via_temp(&t, false, |p| {
            fs::write(p, "half")?;
            Err(ArchiveError::Corrupt("x".into()))
        });
        assert!(matches!(r, Err(ArchiveError::Corrupt(_))));
        assert!(!t.exists());
        assert_eq!(fs::read_dir(d.path().join("sub")).unwrap().count(), 0);
    }

    #[test]
    fn write_via_temp_failure_keep_broken_keeps_file() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("f.txt");
        let r = write_via_temp(&t, true, |p| {
            fs::write(p, "half")?;
            Err(ArchiveError::Corrupt("x".into()))
        });
        assert!(r.is_err());
        assert_eq!(fs::read_to_string(&t).unwrap(), "half");
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
    }

    #[test]
    fn write_via_temp_success_replaces_target_only_at_end() {
        let d = tempfile::tempdir().unwrap();
        let t = d.path().join("f.txt");
        fs::write(&t, "old").unwrap();
        write_via_temp(&t, false, |p| {
            assert_eq!(fs::read_to_string(&t).unwrap(), "old");
            fs::write(p, "new")?;
            Ok(())
        })
        .unwrap();
        assert_eq!(fs::read_to_string(&t).unwrap(), "new");
        assert_eq!(fs::read_dir(d.path()).unwrap().count(), 1);
    }
}
