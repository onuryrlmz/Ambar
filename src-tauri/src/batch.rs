//! What a batch of archives needs on top of a single job, shared by the window and the
//! command line: the order, one run per RAR set, the own-folder name and the password check.

use crate::engine::plan::safe_dest;
use crate::engine::{
    detect_format, open_engine, rar, ArchiveEngine, ArchiveInfo, Entry, ExtractRequest, Format,
    JobCtl, OpReport, Password,
};
use crate::error::ArchiveError;
use std::cmp::Ordering;
use std::collections::HashSet;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};

/// Splits off a leading run of digits, or of anything else.
fn next_run(s: &str) -> (&str, &str) {
    let digits = s.starts_with(|c: char| c.is_ascii_digit());
    let end = s
        .find(|c: char| c.is_ascii_digit() != digits)
        .unwrap_or(s.len());
    s.split_at(end)
}

/// By value, so any number of digits fits: fewer significant digits is the smaller number.
fn cmp_digits(a: &str, b: &str) -> Ordering {
    let (x, y) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    x.len().cmp(&y.len()).then_with(|| x.cmp(y))
}

/// Name order with numbers read as numbers, so part 2 precedes part 10. Case only breaks
/// ties, and two different names never compare equal.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut x, mut y) = (a, b);
    while !x.is_empty() && !y.is_empty() {
        let (rx, tx) = next_run(x);
        let (ry, ty) = next_run(y);
        let digits = rx.starts_with(|c: char| c.is_ascii_digit())
            && ry.starts_with(|c: char| c.is_ascii_digit());
        let order = if digits {
            cmp_digits(rx, ry)
        } else {
            rx.to_lowercase().cmp(&ry.to_lowercase())
        };
        if order != Ordering::Equal {
            return order;
        }
        (x, y) = (tx, ty);
    }
    x.len().cmp(&y.len()).then_with(|| a.cmp(b))
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Opening any part of a RAR set works on the whole set, so all the parts are one run.
fn run_key(path: &Path) -> PathBuf {
    let start = match detect_format(path) {
        Ok(Format::Rar) => rar::first_volume(path),
        _ => path.to_path_buf(),
    };
    std::fs::canonicalize(&start).unwrap_or(start)
}

/// The archives of a batch in the order they run. A repeated file or a further part of a
/// RAR set drops out, or it would be extracted twice. Nothing else does: a file that cannot
/// be read or recognized stays, and fails with its own error when its turn comes.
pub fn plan_batch(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort_by(|a, b| natural_cmp(&name_of(a), &name_of(b)).then_with(|| a.cmp(b)));
    let mut seen = HashSet::new();
    paths.retain(|p| seen.insert(run_key(p)));
    paths
}

/// The archive's name without its extension, as one plain path component. A file name is
/// outside our control: anything that would not stay one component is refused.
pub fn archive_folder_name(archive: &Path) -> Result<String, ArchiveError> {
    let name = archive
        .file_name()
        .ok_or_else(|| ArchiveError::UnsafePath(archive.to_string_lossy().into_owned()))?
        .to_string_lossy()
        .into_owned();
    let lower = name.to_ascii_lowercase();
    let compound = [".tar.gz", ".tar.bz2", ".tar.xz"]
        .iter()
        .find(|e| lower.len() > e.len() && lower.ends_with(**e));
    let stem = match compound {
        Some(e) => &name[..name.len() - e.len()],
        None => match name.rfind('.') {
            Some(i) if i > 0 => &name[..i],
            _ => name.as_str(),
        },
    };
    if stem.is_empty() || stem == "." || stem == ".." || stem.contains(['/', '\\', '\0']) {
        return Err(ArchiveError::UnsafePath(name));
    }
    Ok(stem.to_string())
}

/// Makes the archive's own folder directly under `dest`, or takes the one already there.
/// True when this call made it. A file or a link at that name is never touched.
pub fn ensure_archive_folder(dest: &Path, archive: &Path) -> Result<(PathBuf, bool), ArchiveError> {
    let name = archive_folder_name(archive)?;
    let dir = safe_dest(dest, &name)?;
    match std::fs::create_dir(&dir) {
        Ok(()) => Ok((dir, true)),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = std::fs::symlink_metadata(&dir)?;
            if meta.file_type().is_symlink() {
                Err(ArchiveError::UnsafePath(name))
            } else if meta.is_dir() {
                Ok((dir, false))
            } else {
                Err(ArchiveError::DestConflict(name))
            }
        }
        Err(e) => Err(e.into()),
    }
}

/// Points the request at the archive's own folder and returns that folder if this call
/// made it. A file that is no archive fails first, so it leaves no folder behind.
pub fn prepare_own_folder(req: &mut ExtractRequest, archive: &Path) -> Result<Option<PathBuf>, ArchiveError> {
    detect_format(archive)?;
    let (dir, created) = ensure_archive_folder(&req.dest, archive)?;
    req.dest = dir.clone();
    Ok(created.then_some(dir))
}

/// True when the archive holds something a password opens. Asked before the job starts, so
/// no plain file is written by a run that then stops for the password and is run again,
/// which the rename policy would turn into duplicates. Any other trouble is the job's to report.
pub fn needs_password(archive: &Path) -> bool {
    if !matches!(detect_format(archive), Ok(Format::Rar | Format::Zip | Format::SevenZ)) {
        return false; // The TAR family has no encryption, and listing it reads it all.
    }
    let listed = catch_unwind(AssertUnwindSafe(|| open_engine(archive)?.list(None)));
    match listed {
        Ok(Ok((_, entries))) => entries.iter().any(|e| e.encrypted && !e.is_dir),
        Ok(Err(ArchiveError::PasswordRequired | ArchiveError::WrongPassword)) => true,
        _ => false,
    }
}

/// Takes back an own folder that stayed empty: an archive that failed leaves no trace.
/// `created` is only what this run made, and removing a folder with anything in it fails.
pub struct OwnFolderEngine {
    pub inner: Box<dyn ArchiveEngine>,
    pub created: Option<PathBuf>,
}

impl ArchiveEngine for OwnFolderEngine {
    fn list(&mut self, pw: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
        self.inner.list(pw)
    }

    fn extract(
        &mut self,
        pw: Option<&Password>,
        req: &ExtractRequest,
        ctl: &mut dyn JobCtl,
    ) -> Result<OpReport, ArchiveError> {
        let result = self.inner.extract(pw, req, ctl);
        if let Some(dir) = &self.created {
            let _ = std::fs::remove_dir(dir);
        }
        result
    }

    fn test(&mut self, pw: Option<&Password>, ctl: &mut dyn JobCtl) -> Result<OpReport, ArchiveError> {
        self.inner.test(pw, ctl)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ConflictChoice, FileFailure, OverwritePolicy, TestCtl};
    use std::fs;
    use std::io::Write;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    fn zip(path: &Path, files: &[(&str, &[u8])]) {
        use ::zip::write::SimpleFileOptions;
        let mut w = ::zip::ZipWriter::new(fs::File::create(path).unwrap());
        for (name, data) in files {
            w.start_file(*name, SimpleFileOptions::default()).unwrap();
            w.write_all(data).unwrap();
        }
        w.finish().unwrap();
    }

    fn aes_zip(path: &Path, pw: &str) {
        use ::zip::write::SimpleFileOptions;
        let mut w = ::zip::ZipWriter::new(fs::File::create(path).unwrap());
        let o = SimpleFileOptions::default();
        w.start_file("gizli.txt", o.with_aes_encryption(::zip::AesMode::Aes256, pw)).unwrap();
        w.write_all(b"secret").unwrap();
        w.finish().unwrap();
    }

    fn sorted(mut v: Vec<&str>) -> Vec<&str> {
        v.sort_by(|a, b| natural_cmp(a, b));
        v
    }

    #[test]
    fn part_2_precedes_part_10() {
        assert_eq!(
            sorted(vec!["p-10.zip", "p-2.zip", "p-1.zip", "p-100.zip"]),
            vec!["p-1.zip", "p-2.zip", "p-10.zip", "p-100.zip"]
        );
    }

    #[test]
    fn takeout_names_sort_by_part_number() {
        let names: Vec<String> = (1..=31).rev().map(|i| format!("takeout-20260101T101010Z-{i}.zip")).collect();
        let mut v: Vec<&str> = names.iter().map(String::as_str).collect();
        v.sort_by(|a, b| natural_cmp(a, b));
        assert!(v[0].ends_with("-1.zip") && v[1].ends_with("-2.zip") && v[30].ends_with("-31.zip"), "{v:?}");
    }

    #[test]
    fn natural_order_is_total_and_copes_with_any_digits() {
        assert_eq!(natural_cmp("a", "a"), Ordering::Equal);
        assert_ne!(natural_cmp("a01", "a1"), Ordering::Equal, "different names never tie");
        assert_eq!(natural_cmp("a01", "a1"), natural_cmp("a1", "a01").reverse());
        assert_eq!(natural_cmp("A2", "a10"), Ordering::Less, "case does not outrank the number");
        let huge = "9".repeat(400);
        assert_eq!(natural_cmp(&format!("a{huge}"), &format!("a1{huge}")), Ordering::Less);
        assert_eq!(sorted(vec!["b", "a2", "a"]), vec!["a", "a2", "b"]);
    }

    #[test]
    fn plan_orders_by_name_and_drops_repeats() {
        let d = tempfile::tempdir().unwrap();
        let (a2, a10) = (d.path().join("a-2.zip"), d.path().join("a-10.zip"));
        zip(&a2, &[("x", b"x")]);
        zip(&a10, &[("y", b"y")]);
        let again = d.path().join(".").join("a-2.zip");
        assert_eq!(plan_batch(vec![a10.clone(), a2.clone(), again]), vec![a2, a10]);
    }

    #[test]
    fn plan_runs_a_rar_set_once() {
        let v = plan_batch(vec![fixture("multi.part2.rar"), fixture("plain5.rar"), fixture("multi.part1.rar")]);
        assert_eq!(v, vec![fixture("multi.part1.rar"), fixture("plain5.rar")]);
    }

    #[test]
    fn plan_keeps_files_it_cannot_read_or_recognize() {
        let d = tempfile::tempdir().unwrap();
        let text = d.path().join("x.part2.rar");
        fs::write(&text, "merhaba").unwrap();
        let gone = d.path().join("yok.part1.rar");
        assert_eq!(plan_batch(vec![text.clone(), gone.clone()]), vec![text, gone]);
    }

    #[test]
    fn folder_name_drops_the_extension() {
        for (file, want) in [
            ("takeout-001.zip", "takeout-001"),
            ("a.b.rar", "a.b"),
            ("yedek.tar.gz", "yedek"),
            ("yedek.TAR.XZ", "yedek"),
            ("yedek.tgz", "yedek"),
            ("çalışma şifreli.7z", "çalışma şifreli"),
            ("uzantısız", "uzantısız"),
            (".zip", ".zip"),
            (".tar.gz", ".tar"),
        ] {
            assert_eq!(archive_folder_name(Path::new(file)).unwrap(), want, "{file}");
        }
    }

    #[test]
    fn folder_name_refuses_anything_but_one_plain_component() {
        for file in ["..zip", "...zip", "a\\b.zip", "..\\..\\x.zip", "x\0y.zip"] {
            assert!(
                matches!(archive_folder_name(Path::new(file)), Err(ArchiveError::UnsafePath(_))),
                "{file:?}"
            );
        }
        assert!(archive_folder_name(Path::new("/")).is_err());
        assert!(archive_folder_name(Path::new("..")).is_err());
    }

    #[test]
    fn own_folder_is_made_under_dest_only() {
        let d = tempfile::tempdir().unwrap();
        let (dir, created) = ensure_archive_folder(d.path(), Path::new("/uzak/yer/a-1.zip")).unwrap();
        assert_eq!(dir, d.path().join("a-1"));
        assert!(created && dir.is_dir());
        // A second archive of that name, or a re-run, uses it and says it did not make it.
        let (again, created) = ensure_archive_folder(d.path(), Path::new("a-1.rar")).unwrap();
        assert_eq!(again, dir);
        assert!(!created);
    }

    #[test]
    fn own_folder_never_replaces_a_file() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a"), "mine").unwrap();
        assert!(matches!(ensure_archive_folder(d.path(), Path::new("a.zip")), Err(ArchiveError::DestConflict(_))));
        assert_eq!(fs::read_to_string(d.path().join("a")).unwrap(), "mine");
    }

    #[cfg(unix)]
    #[test]
    fn own_folder_never_goes_through_a_symlink() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("a")).unwrap();
        assert!(matches!(ensure_archive_folder(d.path(), Path::new("a.zip")), Err(ArchiveError::UnsafePath(_))));
    }

    fn request(dest: &Path) -> ExtractRequest {
        ExtractRequest {
            dest: dest.to_path_buf(),
            selection: vec![],
            base: String::new(),
            keep_paths: true,
            overwrite: OverwritePolicy::Skip,
            keep_broken: false,
        }
    }

    #[test]
    fn a_non_archive_gets_no_folder() {
        let d = tempfile::tempdir().unwrap();
        let text = d.path().join("not.txt");
        fs::write(&text, "merhaba").unwrap();
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        let mut req = request(&out);
        assert!(matches!(prepare_own_folder(&mut req, &text), Err(ArchiveError::NotAnArchive)));
        assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
        assert_eq!(req.dest, out);
    }

    #[test]
    fn prepare_points_the_request_at_the_own_folder() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a-1.zip");
        zip(&z, &[("x.txt", b"x")]);
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        let mut req = request(&out);
        let made = prepare_own_folder(&mut req, &z).unwrap();
        assert_eq!(req.dest, out.join("a-1"));
        assert_eq!(made, Some(out.join("a-1")));
        assert_eq!(prepare_own_folder(&mut request(&out), &z).unwrap(), None);
    }

    struct Scripted {
        fail: bool,
    }
    impl ArchiveEngine for Scripted {
        fn list(&mut self, _: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
            Err(ArchiveError::NotAnArchive)
        }
        fn extract(&mut self, _: Option<&Password>, req: &ExtractRequest, _: &mut dyn JobCtl) -> Result<OpReport, ArchiveError> {
            if self.fail {
                return Err(ArchiveError::Corrupt("x".into()));
            }
            fs::write(req.dest.join("f"), "x")?;
            Ok(OpReport { ok: 1, skipped: 0, failed: Vec::<FileFailure>::new() })
        }
        fn test(&mut self, _: Option<&Password>, _: &mut dyn JobCtl) -> Result<OpReport, ArchiveError> {
            Ok(OpReport::default())
        }
    }

    fn quiet() -> TestCtl {
        TestCtl { cancel_after: None, choice: ConflictChoice::Skip, seen: vec![] }
    }

    #[test]
    fn a_failed_archive_leaves_no_empty_folder_of_its_own() {
        let d = tempfile::tempdir().unwrap();
        let own = d.path().join("own");
        fs::create_dir(&own).unwrap();
        let mut e = OwnFolderEngine { inner: Box::new(Scripted { fail: true }), created: Some(own.clone()) };
        assert!(e.extract(None, &request(&own), &mut quiet()).is_err());
        assert!(!own.exists());
    }

    #[test]
    fn a_folder_with_output_or_made_earlier_stays() {
        let d = tempfile::tempdir().unwrap();
        let own = d.path().join("own");
        fs::create_dir(&own).unwrap();
        let mut e = OwnFolderEngine { inner: Box::new(Scripted { fail: false }), created: Some(own.clone()) };
        e.extract(None, &request(&own), &mut quiet()).unwrap();
        assert!(own.join("f").exists());
        let earlier = d.path().join("earlier");
        fs::create_dir(&earlier).unwrap();
        let mut e = OwnFolderEngine { inner: Box::new(Scripted { fail: true }), created: None };
        assert!(e.extract(None, &request(&earlier), &mut quiet()).is_err());
        assert!(earlier.exists());
    }

    #[test]
    fn password_check_tells_encrypted_from_plain() {
        let d = tempfile::tempdir().unwrap();
        let (enc, plain) = (d.path().join("e.zip"), d.path().join("p.zip"));
        aes_zip(&enc, "doğru");
        zip(&plain, &[("x", b"x")]);
        assert!(needs_password(&enc));
        assert!(!needs_password(&plain));
        assert!(needs_password(&fixture("enc.rar")));
        assert!(needs_password(&fixture("enc-headers.rar")));
        assert!(!needs_password(&fixture("plain5.rar")));
        assert!(!needs_password(&d.path().join("yok.zip")));
        let text = d.path().join("t.txt");
        fs::write(&text, "merhaba").unwrap();
        assert!(!needs_password(&text));
    }
}
