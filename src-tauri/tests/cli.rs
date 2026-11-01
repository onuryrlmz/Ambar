//! Runs the built binary against generated archives.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use zip::write::SimpleFileOptions;

const BIN: &str = env!("CARGO_BIN_EXE_ambar");

fn zip_file(path: &Path, files: &[(&str, &[u8])]) {
    let mut w = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let o = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in files {
        w.start_file(*name, o).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap();
}

fn aes_zip(path: &Path, pw: &str) {
    let mut w = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let o = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    w.start_file("gizli.txt", o.with_aes_encryption(zip::AesMode::Aes256, pw)).unwrap();
    w.write_all(b"secret").unwrap();
    w.finish().unwrap();
}

/// Flips a stored file's first data byte: the archive opens, the CRC then fails.
fn damage_data(path: &Path, data: &[u8]) {
    let mut b = fs::read(path).unwrap();
    let at = b.windows(data.len()).position(|w| w == data).unwrap();
    b[at] ^= 0xff;
    fs::write(path, b).unwrap();
}

struct Env {
    dir: tempfile::TempDir,
}

impl Env {
    fn new() -> Self {
        Env { dir: tempfile::tempdir().unwrap() }
    }
    fn p(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
    fn zip(&self, name: &str, files: &[(&str, &[u8])]) -> PathBuf {
        let p = self.p(name);
        zip_file(&p, files);
        p
    }
    fn dest(&self) -> PathBuf {
        self.p("data")
    }
}

fn ambar(args: &[&std::ffi::OsStr]) -> Output {
    Command::new(BIN).args(args).stdin(Stdio::null()).output().unwrap()
}

fn extract(archives: &[&Path], dest: &Path, flags: &[&str]) -> Output {
    let mut args: Vec<&std::ffi::OsStr> = vec!["extract".as_ref()];
    args.extend(archives.iter().map(|a| a.as_os_str()));
    args.push("--dest".as_ref());
    args.push(dest.as_os_str());
    args.extend(flags.iter().map(|f| std::ffi::OsStr::new(*f)));
    ambar(&args)
}

fn with_stdin(args: &[&std::ffi::OsStr], stdin: &[u8]) -> Output {
    let mut c = Command::new(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(stdin).unwrap();
    c.wait_with_output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}
fn code(o: &Output) -> i32 {
    o.status.code().unwrap_or(-1)
}
fn last_line(o: &Output) -> String {
    out(o).lines().last().unwrap_or("").to_string()
}

#[test]
fn merges_many_archives_into_one_folder_and_creates_it() {
    let e = Env::new();
    let a = e.zip("t-1.zip", &[("Takeout/a.txt", b"aa")]);
    let b = e.zip("t-2.zip", &[("Takeout/b.txt", b"bb")]);
    let dest = e.p("yeni/alt/data");
    let o = extract(&[&a, &b], &dest, &[]);
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(fs::read(dest.join("Takeout/a.txt")).unwrap(), b"aa");
    assert_eq!(fs::read(dest.join("Takeout/b.txt")).unwrap(), b"bb");
    assert!(out(&o).contains("t-1.zip: ok 1, skipped 0, failed 0"), "{}", out(&o));
    let total = last_line(&o);
    assert!(total.starts_with("Total:") && total.contains("ok 2, skipped 0, failed 0"), "{total}");
    assert!(total.contains("overwrite policy: skip"), "{total}");
}

#[test]
fn archives_run_in_numeric_name_order_whatever_the_arguments_say() {
    let e = Env::new();
    let (p10, p2, p1) = (e.zip("p-10.zip", &[("c", b"c")]), e.zip("p-2.zip", &[("b", b"b")]), e.zip("p-1.zip", &[("a", b"a")]));
    let o = extract(&[&p10, &p2, &p1], &e.dest(), &[]);
    let s = out(&o);
    let (i1, i2, i10) = (s.find("p-1.zip:").unwrap(), s.find("p-2.zip:").unwrap(), s.find("p-10.zip:").unwrap());
    assert!(i1 < i2 && i2 < i10, "{s}");
}

#[test]
fn a_later_part_overwrites_an_earlier_one_only_in_numeric_order() {
    let e = Env::new();
    let (p10, p2) = (e.zip("p-10.zip", &[("same", b"ten")]), e.zip("p-2.zip", &[("same", b"two")]));
    assert_eq!(code(&extract(&[&p10, &p2], &e.dest(), &["--overwrite", "overwrite"])), 0);
    assert_eq!(fs::read(e.dest().join("same")).unwrap(), b"ten");
}

#[test]
fn separate_gives_each_archive_its_own_folder_under_dest() {
    let e = Env::new();
    let a = e.zip("yedek-1.zip", &[("a.txt", b"a")]);
    let b = e.zip("yedek-2.zip", &[("a.txt", b"b")]);
    let o = extract(&[&a, &b], &e.dest(), &["--separate"]);
    assert_eq!(code(&o), 0, "{}", out(&o));
    assert_eq!(fs::read(e.dest().join("yedek-1/a.txt")).unwrap(), b"a");
    assert_eq!(fs::read(e.dest().join("yedek-2/a.txt")).unwrap(), b"b");
    assert_eq!(fs::read_dir(e.dest()).unwrap().count(), 2);
}

#[test]
fn a_corrupt_archive_among_good_ones_fails_alone_with_exit_1() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("a.txt", b"a")]);
    let bad = e.p("b.zip");
    fs::write(&bad, b"PK\x03\x04this is not really a zip at all").unwrap();
    let c = e.zip("c.zip", &[("c.txt", b"c")]);
    let o = extract(&[&a, &bad, &c], &e.dest(), &[]);
    assert_eq!(code(&o), 1, "{}", out(&o));
    assert!(e.dest().join("a.txt").exists() && e.dest().join("c.txt").exists());
    let s = out(&o);
    assert!(s.contains("b.zip: ok 0, skipped 0, failed 1"), "{s}");
    assert!(s.contains("failed: b.zip [corrupt]"), "{s}");
    assert!(last_line(&o).contains("failed 1"), "{}", last_line(&o));
}

#[test]
fn a_damaged_entry_is_listed_with_path_and_code_and_no_output_stays() {
    let e = Env::new();
    let data: &[u8] = b"bu dosyanin verisi birazdan bozulacak";
    let z = e.zip("a.zip", &[("iyi.txt", b"iyi"), ("alt/bozuk.txt", data)]);
    damage_data(&z, data);
    let o = extract(&[&z], &e.dest(), &[]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).contains("a.zip: ok 1, skipped 0, failed 1"), "{}", out(&o));
    assert!(out(&o).contains("failed: alt/bozuk.txt [corrupt]"), "{}", out(&o));
    assert!(e.dest().join("iyi.txt").exists());
    assert!(!e.dest().join("alt/bozuk.txt").exists());
    // keep-broken keeps it, and the run still fails.
    let dest = e.p("kept");
    let o = extract(&[&z], &dest, &["--keep-broken"]);
    assert_eq!(code(&o), 1);
    assert!(dest.join("alt/bozuk.txt").exists());
}

#[test]
fn a_non_archive_fails_with_its_code_and_gets_no_folder() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("a.txt", b"a")]);
    let text = e.p("notlar.txt");
    fs::write(&text, "merhaba").unwrap();
    let o = extract(&[&text, &a], &e.dest(), &["--separate"]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).contains("failed: notlar.txt [not_archive]"), "{}", out(&o));
    assert!(e.dest().join("a/a.txt").exists());
    assert!(!e.dest().join("notlar").exists());
}

#[test]
fn a_missing_file_fails_alone() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("a.txt", b"a")]);
    let o = extract(&[&e.p("yok.zip"), &a], &e.dest(), &[]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).contains("yok.zip: ok 0, skipped 0, failed 1"), "{}", out(&o));
    assert!(e.dest().join("a.txt").exists());
}

#[test]
fn an_encrypted_zip_without_password_fails_clearly_and_others_continue() {
    let e = Env::new();
    let enc = e.p("a-enc.zip");
    aes_zip(&enc, "Dogru-Parola-7");
    let plain = e.zip("b.zip", &[("b.txt", b"b")]);
    let o = extract(&[&enc, &plain], &e.dest(), &[]);
    assert_eq!(code(&o), 1);
    let s = out(&o);
    assert!(s.contains("[password_required]") && s.contains("--password-stdin"), "{s}");
    assert!(e.dest().join("b.txt").exists());
    assert!(!e.dest().join("gizli.txt").exists());
}

#[test]
fn password_stdin_opens_every_archive_that_needs_it_and_leaves_no_trace() {
    let e = Env::new();
    let (x, y) = (e.p("x.zip"), e.p("y.zip"));
    aes_zip(&x, "Dogru-Parola-7");
    aes_zip(&y, "Dogru-Parola-7");
    let plain = e.zip("z.zip", &[("z.txt", b"z")]);
    let dest = e.dest();
    let o = with_stdin(
        &["extract".as_ref(), x.as_os_str(), y.as_os_str(), plain.as_os_str(), "--dest".as_ref(), dest.as_os_str(), "--separate".as_ref(), "--password-stdin".as_ref()],
        b"Dogru-Parola-7\n",
    );
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert_eq!(fs::read(dest.join("x/gizli.txt")).unwrap(), b"secret");
    assert_eq!(fs::read(dest.join("y/gizli.txt")).unwrap(), b"secret");
    assert!(dest.join("z/z.txt").exists());
    assert!(!out(&o).contains("Dogru-Parola-7") && !err(&o).contains("Dogru-Parola-7"));
}

#[test]
fn a_wrong_password_fails_that_archive_and_writes_nothing() {
    let e = Env::new();
    let x = e.p("x.zip");
    aes_zip(&x, "Dogru-Parola-7");
    let dest = e.dest();
    let o = with_stdin(
        &["extract".as_ref(), x.as_os_str(), "--dest".as_ref(), dest.as_os_str(), "--password-stdin".as_ref()],
        b"Yanlis-Parola-1\r\n",
    );
    assert_eq!(code(&o), 1);
    assert!(out(&o).contains("[wrong_password]"), "{}", out(&o));
    assert!(!dest.join("gizli.txt").exists());
    assert!(!out(&o).contains("Yanlis-Parola-1") && !err(&o).contains("Yanlis-Parola-1"));
}

#[test]
fn empty_stdin_for_a_password_is_a_usage_error() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("a.txt", b"a")]);
    let dest = e.dest();
    let o = with_stdin(&["extract".as_ref(), a.as_os_str(), "--dest".as_ref(), dest.as_os_str(), "--password-stdin".as_ref()], b"");
    assert_eq!(code(&o), 2, "{}", err(&o));
    assert!(!dest.exists());
}

#[test]
fn a_rerun_skips_by_default_touches_nothing_and_succeeds() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("a.txt", b"arsiv")]);
    assert_eq!(code(&extract(&[&a], &e.dest(), &[])), 0);
    fs::write(e.dest().join("a.txt"), "elimle degistirdim").unwrap();
    let o = extract(&[&a], &e.dest(), &[]);
    assert_eq!(code(&o), 0, "skipped alone is success: {}", out(&o));
    assert!(out(&o).contains("a.zip: ok 0, skipped 1, failed 0"), "{}", out(&o));
    assert!(last_line(&o).contains("skipped 1") && last_line(&o).contains("overwrite policy: skip"));
    assert_eq!(fs::read_to_string(e.dest().join("a.txt")).unwrap(), "elimle degistirdim");
}

#[test]
fn overwrite_and_rename_policies_do_what_they_say() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("a.txt", b"arsiv")]);
    fs::create_dir(e.dest()).unwrap();
    fs::write(e.dest().join("a.txt"), "eski").unwrap();
    let o = extract(&[&a], &e.dest(), &["--overwrite", "rename"]);
    assert_eq!(code(&o), 0);
    assert_eq!(fs::read_to_string(e.dest().join("a.txt")).unwrap(), "eski");
    assert_eq!(fs::read(e.dest().join("a (1).txt")).unwrap(), b"arsiv");
    assert!(last_line(&o).contains("overwrite policy: rename"));
    assert_eq!(code(&extract(&[&a], &e.dest(), &["--overwrite", "overwrite"])), 0);
    assert_eq!(fs::read(e.dest().join("a.txt")).unwrap(), b"arsiv");
}

#[test]
fn no_paths_flattens() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("x/y/z.txt", b"z")]);
    assert_eq!(code(&extract(&[&a], &e.dest(), &["--no-paths"])), 0);
    assert!(e.dest().join("z.txt").exists());
    assert!(!e.dest().join("x").exists());
}

#[test]
fn an_entry_cannot_escape_the_destination() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("../kacak.txt", b"x"), ("iyi.txt", b"y")]);
    let o = extract(&[&a], &e.dest(), &[]);
    assert_eq!(code(&o), 1, "{}", out(&o));
    assert!(out(&o).contains("[unsafe_path]"), "{}", out(&o));
    assert!(!e.p("kacak.txt").exists());
    assert!(e.dest().join("iyi.txt").exists());
}

#[test]
fn a_rar_set_given_as_all_its_parts_runs_once() {
    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let e = Env::new();
    let o = extract(&[&fx.join("multi.part2.rar"), &fx.join("multi.part1.rar")], &e.dest(), &["--overwrite", "rename"]);
    assert_eq!(code(&o), 0, "{}", out(&o));
    assert!(e.dest().join("buyuk.bin").exists());
    assert!(!e.dest().join("buyuk (1).bin").exists());
    assert_eq!(out(&o).matches("multi.part").count(), 1, "{}", out(&o));
}

#[test]
fn a_relative_dest_lands_under_the_working_folder() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("a.txt", b"a")]);
    let o = Command::new(BIN).current_dir(e.dir.path()).args(["extract", "a.zip", "--dest", "goreli/data"]).output().unwrap();
    assert_eq!(code(&o), 0, "{}{}", out(&o), err(&o));
    assert!(a.exists() && e.p("goreli/data/a.txt").exists());
}

#[test]
fn a_dest_that_cannot_be_made_is_exit_1() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("a.txt", b"a")]);
    let file = e.p("dosya");
    fs::write(&file, "x").unwrap();
    let o = extract(&[&a], &file, &[]);
    assert_eq!(code(&o), 1, "{}", err(&o));
    assert!(err(&o).contains("destination"), "{}", err(&o));
    assert_eq!(fs::read_to_string(&file).unwrap(), "x");
}

#[test]
fn test_reports_good_and_bad_archives() {
    let e = Env::new();
    let good = e.zip("iyi.zip", &[("a.txt", b"a")]);
    let data: &[u8] = b"bu dosyanin verisi birazdan bozulacak";
    let bad = e.zip("kotu.zip", &[("b.txt", data)]);
    damage_data(&bad, data);
    let o = ambar(&["test".as_ref(), good.as_os_str()]);
    assert_eq!(code(&o), 0, "{}", out(&o));
    assert!(out(&o).contains("iyi.zip: ok 1, skipped 0, failed 0"), "{}", out(&o));
    let o = ambar(&["test".as_ref(), bad.as_os_str(), good.as_os_str()]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).contains("failed: b.txt [corrupt]"), "{}", out(&o));
    assert!(!last_line(&o).contains("overwrite policy"));
}

#[test]
fn test_uses_the_password_too() {
    let e = Env::new();
    let x = e.p("x.zip");
    aes_zip(&x, "Dogru-Parola-7");
    let o = ambar(&["test".as_ref(), x.as_os_str()]);
    assert_eq!(code(&o), 1);
    assert!(out(&o).contains("[password_required]"), "{}", out(&o));
    let o = with_stdin(&["test".as_ref(), x.as_os_str(), "--password-stdin".as_ref()], b"Dogru-Parola-7\n");
    assert_eq!(code(&o), 0, "{}", out(&o));
}

#[test]
fn usage_errors_are_exit_2() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("a.txt", b"a")]);
    let s = a.to_str().unwrap();
    let d = e.dest();
    let d = d.to_str().unwrap();
    for args in [
        vec!["extract"],
        vec!["extract", s],
        vec!["extract", "--dest", d],
        vec!["extract", s, "--dest"],
        vec!["extract", s, "--dest", d, "--overwrite", "ask"],
        vec!["extract", s, "--dest", d, "--overwrite"],
        vec!["extract", s, "--dest", d, "--dest", d],
        vec!["extract", s, "--dest", d, "--nonsense"],
        vec!["test"],
        vec!["test", s, "--dest", d],
        vec!["test", s, "--separate"],
    ] {
        let o = Command::new(BIN).args(&args).output().unwrap();
        assert_eq!(code(&o), 2, "{args:?}: {}", err(&o));
        assert!(!err(&o).is_empty(), "{args:?}");
        assert!(out(&o).is_empty(), "{args:?}");
    }
    assert!(!e.dest().exists());
}

#[test]
fn a_password_on_the_command_line_is_refused_and_never_echoed() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("a.txt", b"a")]);
    let s = a.to_str().unwrap();
    let d = e.dest();
    let d = d.to_str().unwrap();
    for pair in [vec!["--password", "Gizli-9"], vec!["--password=Gizli-9"], vec!["-p", "Gizli-9"], vec!["-pGizli-9"]] {
        let mut args = vec!["extract", s, "--dest", d];
        args.extend(&pair);
        let o = Command::new(BIN).args(&args).output().unwrap();
        assert_eq!(code(&o), 2, "{pair:?}");
        assert!(err(&o).contains("--password-stdin"), "{pair:?}: {}", err(&o));
        assert!(!err(&o).contains("Gizli-9") && !out(&o).contains("Gizli-9"), "{pair:?}: {}", err(&o));
    }
}

#[test]
fn help_and_version_are_exit_0_on_stdout() {
    for flag in ["--help", "-h"] {
        let o = Command::new(BIN).arg(flag).output().unwrap();
        assert_eq!(code(&o), 0);
        assert!(out(&o).contains("ambar extract") && out(&o).contains("--password-stdin"), "{}", out(&o));
    }
    let o = Command::new(BIN).arg("--version").output().unwrap();
    assert_eq!(code(&o), 0);
    assert_eq!(out(&o).trim(), format!("ambar {}", env!("CARGO_PKG_VERSION")));
    let o = Command::new(BIN).args(["extract", "--help"]).output().unwrap();
    assert_eq!(code(&o), 0);
}

#[test]
fn piped_stderr_gets_no_progress() {
    let e = Env::new();
    let a = e.zip("a.zip", &[("a.txt", b"a")]);
    let o = extract(&[&a], &e.dest(), &[]);
    assert_eq!(err(&o), "");
}

#[cfg(unix)]
#[test]
fn ctrl_c_stops_the_run_removes_partial_output_and_exits_130() {
    let e = Env::new();
    let big = e.p("a.zip");
    {
        let mut w = zip::ZipWriter::new(fs::File::create(&big).unwrap());
        let o = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        w.start_file("buyuk.bin", o).unwrap();
        let block = vec![7u8; 1024 * 1024];
        for _ in 0..600 {
            w.write_all(&block).unwrap();
        }
        w.finish().unwrap();
    }
    let later = e.zip("b.zip", &[("sonra.txt", b"x")]);
    let dest = e.dest();
    let mut child = Command::new(BIN)
        .args(["extract".as_ref(), big.as_os_str(), later.as_os_str(), "--dest".as_ref(), dest.as_os_str()])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let part = dest.join("buyuk.bin.ambar-part");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !part.exists() {
        assert!(std::time::Instant::now() < deadline, "the part file never appeared");
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    unsafe { libc::kill(child.id() as i32, libc::SIGINT) };
    let o = {
        let _ = &mut child;
        child.wait_with_output().unwrap()
    };
    assert_eq!(code(&o), 130, "{}{}", out(&o), err(&o));
    assert!(!part.exists(), "partial output stayed");
    assert!(!dest.join("buyuk.bin").exists());
    assert!(!dest.join("sonra.txt").exists(), "the rest must not run");
    assert!(last_line(&o).starts_with("Total:"), "{}", out(&o));
}
