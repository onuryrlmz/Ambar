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
use serde::Serialize;
use unicode_normalization::UnicodeNormalization;
use std::path::{Path, PathBuf};

/// One run of a batch, and the folder `--separate` gives it, if its name allows one.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedArchive {
    pub path: PathBuf,
    pub folder: Option<String>,
}

/// A further volume of a set: it has no run of its own, and is still reported.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinedPart {
    pub part: PathBuf,
    pub first: PathBuf,
}

#[derive(Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchPlan {
    pub archives: Vec<PlannedArchive>,
    pub joined: Vec<JoinedPart>,
    /// Canonical paths of every source file, RAR volumes included.
    pub protected: Vec<PathBuf>,
}

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

/// Name order with numbers read as numbers, so part 2 precedes part 10. A file name can
/// come in either Unicode form, so both compare as NFC. Case only breaks ties, and two
/// different names never compare equal.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (na, nb): (String, String) = (a.nfc().collect(), b.nfc().collect());
    let (mut x, mut y) = (na.as_str(), nb.as_str());
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

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Only a RAR's own header can say it is a volume. A name proves nothing: `yedek.2.rar` and
/// `rapor.2024.rar` are usually archives of their own.
fn volume_of(path: &Path, pw: Option<&Password>) -> Option<rar::VolumeInfo> {
    match detect_format(path) {
        Ok(Format::Rar) => rar::volume_info(path, pw),
        _ => None,
    }
}

/// What identifies one run of `path`: all the volumes of a set share it, since opening any of
/// them works on the whole set. None while a password is needed to read the file's header.
pub fn run_key(path: &Path, pw: Option<&Password>) -> Option<PathBuf> {
    match detect_format(path) {
        Ok(Format::Rar) => rar::volume_info(path, pw).map(|v| canonical(&v.first)),
        _ => Some(canonical(path)),
    }
}

/// Files the run must not write over when it reads `path`: itself and its set.
pub fn source_files(path: &Path, pw: Option<&Password>) -> Vec<PathBuf> {
    let mut files = vec![canonical(path)];
    if let Some(info) = volume_of(path, pw).filter(|v| v.is_volume) {
        files.extend(rar::set_files(&info.first).iter().map(|p| canonical(p)));
    }
    files
}

/// The folder each archive gets under `--separate`, in run order. A name already taken gets
/// a number, compared as a case-insensitive disk would, so same names never merge.
fn assign_folders(runs: Vec<PathBuf>) -> Vec<PlannedArchive> {
    let fold = |name: &str| name.nfc().collect::<String>().to_lowercase();
    let mut taken = HashSet::new();
    runs.into_iter()
        .map(|path| {
            let folder = archive_folder_name(&path).ok().map(|stem| {
                let mut name = stem.clone();
                let mut n = 0;
                while !taken.insert(fold(&name)) {
                    n += 1;
                    name = format!("{stem} ({n})");
                }
                name
            });
            PlannedArchive { path, folder }
        })
        .collect()
}

/// The archives of a batch in the order they run. A repeated file drops out. A later volume
/// of a RAR set whose header says so has no run of its own and is listed in `joined`, so it
/// never vanishes unmentioned. Nothing else drops: a file that cannot be read or recognized
/// stays, and fails with its own error when its turn comes.
pub fn plan_batch(mut paths: Vec<PathBuf>, pw: Option<&Password>) -> BatchPlan {
    paths.sort_by(|a, b| natural_cmp(&name_of(a), &name_of(b)).then_with(|| a.cmp(b)));
    let mut plan = BatchPlan::default();
    let mut protected = HashSet::new();
    let infos: Vec<(PathBuf, PathBuf)> = paths
        .into_iter()
        .map(|path| {
            let first = volume_of(&path, pw).map_or_else(|| path.clone(), |v| v.first);
            for file in source_files(&path, pw) {
                if protected.insert(file.clone()) {
                    plan.protected.push(file);
                }
            }
            (path, first)
        })
        .collect();
    // Which file runs for each key: the first volume if given, or else the earliest part.
    let mut runner: std::collections::HashMap<PathBuf, &Path> = std::collections::HashMap::new();
    for (path, first) in &infos {
        let key = canonical(first);
        let is_first = canonical(path) == key;
        let slot = runner.entry(key).or_insert(path);
        if is_first {
            *slot = path;
        }
    }
    let mut seen = HashSet::new();
    let mut runs = Vec::new();
    for (path, first) in &infos {
        let (key, here) = (canonical(first), canonical(path));
        if !seen.insert(here.clone()) {
            continue; // the same file again
        }
        if runner[&key] == path.as_path() {
            runs.push(path.clone());
        } else if here != canonical(runner[&key]) {
            plan.joined.push(JoinedPart { part: path.clone(), first: first.clone() });
        }
    }
    plan.archives = assign_folders(runs);
    plan
}

/// Removes the empty folders under `dir`, then `dir` itself if that leaves it empty. Links
/// are never followed, and removing a folder with anything in it just fails.
pub fn prune_empty_tree(dir: &Path) {
    if let Ok(items) = std::fs::read_dir(dir) {
        for item in items.flatten() {
            if item.file_type().is_ok_and(|t| t.is_dir()) {
                prune_empty_tree(&item.path());
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

fn is_plain_component(name: &str) -> bool {
    !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\', '\0'])
}

/// The archive's name without its extension and leading dots, as one plain path component.
/// A file name is outside our control: anything that would not stay one component, or
/// nothing at all, is refused.
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
    let stem = stem.trim_start_matches('.');
    if !is_plain_component(stem) {
        return Err(ArchiveError::UnsafePath(name));
    }
    Ok(stem.to_string())
}

/// Makes the archive's own folder directly under `dest`, or takes the one already there.
/// True when this call made it. A file or a link at that name is never touched.
pub fn ensure_archive_folder(dest: &Path, name: &str) -> Result<(PathBuf, bool), ArchiveError> {
    let name = name.to_string();
    if !is_plain_component(&name) {
        return Err(ArchiveError::UnsafePath(name));
    }
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
pub fn prepare_own_folder(
    req: &mut ExtractRequest,
    archive: &Path,
    folder: Option<&str>,
) -> Result<Option<PathBuf>, ArchiveError> {
    detect_format(archive)?;
    let name = match folder {
        Some(name) => name.to_string(),
        None => archive_folder_name(archive)?,
    };
    let (dir, created) = ensure_archive_folder(&req.dest, &name)?;
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

/// When an archive fails, takes back the empty folders it left in the own folder this run
/// made: everything in there is this run's. `created` is None for a folder that was there.
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
        let failed = result.as_ref().map_or(true, |r| !r.failed.is_empty());
        if let (true, Some(dir)) = (failed, &self.created) {
            prune_empty_tree(dir);
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
        assert_eq!(paths(&plan_batch(vec![a10.clone(), a2.clone(), again], None)), vec![a2, a10]);
    }

    #[test]
    fn plan_runs_a_rar_set_once() {
        let p = plan_batch(vec![fixture("multi.part2.rar"), fixture("plain5.rar"), fixture("multi.part1.rar")], None);
        assert_eq!(paths(&p), vec![fixture("multi.part1.rar"), fixture("plain5.rar")]);
        assert_eq!(p.joined, vec![JoinedPart { part: fixture("multi.part2.rar"), first: fixture("multi.part1.rar") }]);
    }

    /// Copies a fixture to a new name in `d`.
    fn named(d: &Path, fx: &str, name: &str) -> PathBuf {
        let p = d.join(name);
        fs::copy(fixture(fx), &p).unwrap();
        p
    }

    #[test]
    fn rar_files_that_only_look_numbered_all_run() {
        let d = tempfile::tempdir().unwrap();
        let (a, b) = (named(d.path(), "plain5.rar", "yedek.1.rar"), named(d.path(), "stored.rar", "yedek.2.rar"));
        let (c, e) = (named(d.path(), "plain5.rar", "a.part1.rar"), named(d.path(), "stored.rar", "a.part2.rar"));
        let p = plan_batch(vec![a.clone(), b.clone(), c.clone(), e.clone()], None);
        assert_eq!(paths(&p), vec![c, e, a, b]);
        assert!(p.joined.is_empty());
    }

    #[test]
    fn an_old_style_set_runs_once_from_its_rar() {
        let p = plan_batch(
            ["old.r00", "old.r01", "old.rar"].iter().map(|n| fixture(n)).collect(),
            None,
        );
        assert_eq!(paths(&p), vec![fixture("old.rar")]);
        assert_eq!(p.joined.len(), 2);
        assert!(p.joined.iter().all(|j| j.first == fixture("old.rar")));
    }

    #[test]
    fn a_set_with_encrypted_headers_collapses_only_once_the_password_opens_it() {
        let all = || vec![fixture("envol.part1.rar"), fixture("envol.part2.rar")];
        assert_eq!(paths(&plan_batch(all(), None)).len(), 2, "without it the header cannot say");
        let wrong = Password::new("Yanlis".into());
        assert_eq!(paths(&plan_batch(all(), Some(&wrong))).len(), 2);
        let right = Password::new("Test123".into());
        let p = plan_batch(all(), Some(&right));
        assert_eq!(paths(&p), vec![fixture("envol.part1.rar")]);
        assert_eq!(p.joined.len(), 1);
    }

    #[test]
    fn the_key_of_a_part_is_its_first_volume_when_the_header_says_so() {
        let canon = |n: &str| fs::canonicalize(fixture(n)).unwrap();
        assert_eq!(run_key(&fixture("multi.part2.rar"), None), Some(canon("multi.part1.rar")));
        assert_eq!(run_key(&fixture("old.r01"), None), Some(canon("old.rar")));
        assert_eq!(run_key(&fixture("plain5.rar"), None), Some(canon("plain5.rar")));
        assert_eq!(run_key(&fixture("envol.part2.rar"), None), None);
        let right = Password::new("Test123".into());
        assert_eq!(run_key(&fixture("envol.part2.rar"), Some(&right)), Some(canon("envol.part1.rar")));
    }

    #[test]
    fn nfd_and_nfc_names_sort_together() {
        assert_eq!(sorted(vec!["C\u{327}anta-5", "Çanta-2", "Çanta-10"]), vec!["Çanta-2", "C\u{327}anta-5", "Çanta-10"]);
    }

    #[test]
    fn same_names_get_a_numbered_folder_in_plan_order() {
        let d = tempfile::tempdir().unwrap();
        let (x, y) = (d.path().join("2023"), d.path().join("2024"));
        fs::create_dir(&x).unwrap();
        fs::create_dir(&y).unwrap();
        let zips = [x.join("foto.zip"), y.join("foto.zip"), x.join("same.zip"), x.join("Same.tar.gz"), x.join("same (1).zip")];
        for z in &zips {
            zip(z, &[("x", b"x")]);
        }
        let p = plan_batch(zips.to_vec(), None);
        let by = |path: &Path| p.archives.iter().find(|a| a.path == path).unwrap().folder.clone().unwrap();
        assert_eq!(by(&zips[0]), "foto");
        assert_eq!(by(&zips[1]), "foto (1)");
        let same = [by(&zips[2]), by(&zips[3]), by(&zips[4])];
        let fold: HashSet<String> = same.iter().map(|s| s.to_lowercase()).collect();
        assert_eq!(fold.len(), 3, "a case-insensitive disk must not merge them: {same:?}");
    }

    #[test]
    fn a_name_without_a_safe_folder_is_none_and_still_runs() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("...zip");
        zip(&z, &[("x", b"x")]);
        let p = plan_batch(vec![z.clone()], None);
        assert_eq!(p.archives, vec![PlannedArchive { path: z, folder: None }]);
    }

    #[test]
    fn the_plan_protects_every_source_and_all_volumes() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        zip(&z, &[("x", b"x")]);
        let p = plan_batch(vec![z.clone(), fixture("multi.part1.rar"), fixture("old.rar")], None);
        for path in [z, fixture("multi.part1.rar"), fixture("multi.part2.rar"), fixture("old.rar"), fixture("old.r00"), fixture("old.r01")] {
            assert!(p.protected.contains(&fs::canonicalize(&path).unwrap()), "{path:?}");
        }
    }

    #[test]
    fn prune_removes_empty_folders_bottom_up_and_nothing_else() {
        let d = tempfile::tempdir().unwrap();
        let root = d.path().join("own");
        fs::create_dir_all(root.join("a/b/c")).unwrap();
        fs::create_dir_all(root.join("k")).unwrap();
        fs::write(root.join("k/dosya"), "x").unwrap();
        prune_empty_tree(&root);
        assert!(!root.join("a").exists());
        assert!(root.join("k/dosya").exists());
        let all_empty = d.path().join("bos");
        fs::create_dir_all(all_empty.join("a/b")).unwrap();
        prune_empty_tree(&all_empty);
        assert!(!all_empty.exists());
    }

    #[cfg(unix)]
    #[test]
    fn prune_never_follows_a_link() {
        let d = tempfile::tempdir().unwrap();
        let outside = d.path().join("dis");
        fs::create_dir_all(outside.join("bos")).unwrap();
        let root = d.path().join("own");
        fs::create_dir(&root).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("l")).unwrap();
        prune_empty_tree(&root);
        assert!(outside.join("bos").exists());
        assert!(root.join("l").exists());
    }

    #[test]
    fn plan_keeps_files_it_cannot_read_or_recognize() {
        let d = tempfile::tempdir().unwrap();
        let text = d.path().join("x.part2.rar");
        fs::write(&text, "merhaba").unwrap();
        let gone = d.path().join("yok.part1.rar");
        assert_eq!(paths(&plan_batch(vec![text.clone(), gone.clone()], None)), vec![text, gone]);
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
            (".ssh.zip", "ssh"),
            (".zip", "zip"),
            (".tar.gz", "tar"),
            ("..x.rar", "x"),
        ] {
            assert_eq!(archive_folder_name(Path::new(file)).unwrap(), want, "{file}");
        }
    }

    #[test]
    fn folder_name_refuses_anything_but_one_plain_component() {
        let mut names = vec!["..zip", "...zip", "x\0y.zip"];
        if cfg!(unix) {
            names.extend(["a\\b.zip", "..\\..\\x.zip"]); // on Windows these are paths, not names
        }
        for file in names {
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
        let (dir, created) = ensure_archive_folder(d.path(), "a-1").unwrap();
        assert_eq!(dir, d.path().join("a-1"));
        assert!(created && dir.is_dir());
        // A second archive of that name, or a re-run, uses it and says it did not make it.
        let (again, created) = ensure_archive_folder(d.path(), "a-1").unwrap();
        assert_eq!(again, dir);
        assert!(!created);
    }

    #[test]
    fn own_folder_never_replaces_a_file() {
        let d = tempfile::tempdir().unwrap();
        fs::write(d.path().join("a"), "mine").unwrap();
        assert!(matches!(ensure_archive_folder(d.path(), "a"), Err(ArchiveError::DestConflict(_))));
        assert_eq!(fs::read_to_string(d.path().join("a")).unwrap(), "mine");
    }

    #[cfg(unix)]
    #[test]
    fn own_folder_never_goes_through_a_symlink() {
        let d = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), d.path().join("a")).unwrap();
        assert!(matches!(ensure_archive_folder(d.path(), "a"), Err(ArchiveError::UnsafePath(_))));
    }

    fn request(dest: &Path) -> ExtractRequest {
        ExtractRequest {
            dest: dest.to_path_buf(),
            selection: vec![],
            base: String::new(),
            keep_paths: true,
            overwrite: OverwritePolicy::Skip,
            keep_broken: false,
            protected: vec![],
        }
    }

    fn paths(p: &BatchPlan) -> Vec<PathBuf> {
        p.archives.iter().map(|a| a.path.clone()).collect()
    }

    #[test]
    fn a_non_archive_gets_no_folder() {
        let d = tempfile::tempdir().unwrap();
        let text = d.path().join("not.txt");
        fs::write(&text, "merhaba").unwrap();
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        let mut req = request(&out);
        assert!(matches!(prepare_own_folder(&mut req, &text, None), Err(ArchiveError::NotAnArchive)));
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
        let made = prepare_own_folder(&mut req, &z, Some("a-1")).unwrap();
        assert_eq!(req.dest, out.join("a-1"));
        assert_eq!(made, Some(out.join("a-1")));
        assert_eq!(prepare_own_folder(&mut request(&out), &z, None).unwrap(), None);
    }

    #[test]
    fn a_folder_name_from_the_window_stays_one_component() {
        let d = tempfile::tempdir().unwrap();
        let z = d.path().join("a.zip");
        zip(&z, &[("x", b"x")]);
        let out = d.path().join("out");
        fs::create_dir(&out).unwrap();
        for bad in ["../dis", "a/b", "a\\b", "..", ".", ""] {
            let mut req = request(&out);
            assert!(prepare_own_folder(&mut req, &z, Some(bad)).is_err(), "{bad:?}");
            assert_eq!(req.dest, out);
        }
        assert_eq!(fs::read_dir(&out).unwrap().count(), 0);
        assert!(!d.path().join("dis").exists());
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
            Ok(OpReport { ok: 1, skipped: 0, failed: Vec::<FileFailure>::new(), decrypted: 0 })
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

    /// Makes nested folders, as an archive does on the way to a file that then fails.
    struct NestedThenFail;
    impl ArchiveEngine for NestedThenFail {
        fn list(&mut self, _: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError> {
            Err(ArchiveError::NotAnArchive)
        }
        fn extract(&mut self, _: Option<&Password>, req: &ExtractRequest, _: &mut dyn JobCtl) -> Result<OpReport, ArchiveError> {
            fs::create_dir_all(req.dest.join("alt/ic"))?;
            Ok(OpReport {
                ok: 0,
                skipped: 0,
                failed: vec![FileFailure { path: "alt/ic/f".into(), error: ArchiveError::Corrupt("x".into()) }],
                decrypted: 0,
            })
        }
        fn test(&mut self, _: Option<&Password>, _: &mut dyn JobCtl) -> Result<OpReport, ArchiveError> {
            Ok(OpReport::default())
        }
    }

    #[test]
    fn a_failed_archive_takes_back_the_empty_folders_it_made() {
        let d = tempfile::tempdir().unwrap();
        let own = d.path().join("own");
        fs::create_dir(&own).unwrap();
        let mut e = OwnFolderEngine { inner: Box::new(NestedThenFail), created: Some(own.clone()) };
        let _ = e.extract(None, &request(&own), &mut quiet());
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
