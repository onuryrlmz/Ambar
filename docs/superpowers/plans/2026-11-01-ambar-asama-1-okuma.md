# Ambar Stage 1 (Reading) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A dual-pane desktop app that opens RAR, ZIP, 7z, TAR, GZ, BZ2 and XZ archives, browses them, extracts and tests them, and handles passwords, with no `rar` tool installed.

**Architecture:** Rust engines sit behind one `ArchiveEngine` trait and run on background threads managed by a job manager. Every extraction path goes through one shared safety function. The Svelte frontend holds a flat entry list in memory, derives directory views with pure functions, and renders them through a virtual list.

**Tech Stack:** Tauri 2, Rust 2021, Svelte 5 + SvelteKit (adapter-static), TypeScript, `unrar-ng` 0.7, `zip` 9, `sevenz-rust2` 0.24, `tar` 0.4, `flate2` 1, `bzip2` 0.6, `liblzma` 0.4, `oem_cp` 2, `zeroize` 1, `thiserror` 2, `tempfile` 3.

**Spec:** `docs/superpowers/specs/2026-11-01-ambar-design.md`. Stages 2 to 5 get their own plans.

## Global Constraints

- Identifier `com.onuryrlmz.ambar`, product name `Ambar`, author `onuryrlmz`, crate `ambar` / `ambar_lib`. No code is shared with or imported from Liman.
- Default language is Turkish; all UI strings live in `src/lib/i18n/tr.ts` and `en.ts`, never inline.
- About dialog shows "by onuryrlmz" and "© 2026 onuryrlmz".
- Theme follows the system (light and dark) via `prefers-color-scheme`.
- Passwords are never written to disk, logs, events or process arguments. In Rust they are held in `Zeroizing<String>`.
- No file is ever written outside the chosen destination.
- Never overwrite without following the chosen overwrite policy.
- A failed operation is never reported as success.
- Do not use the WinRAR name or logo. Do not bundle or download `rar` in this stage. `rar` is only used by `scripts/make-fixtures.sh` on the developer machine.
- Liman's `bundle.macOS.signingIdentity: "-"` applies.
- Commits are local. Do not create a GitHub repository or push.
- End every commit message with: `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`

## Review Focus

- A 100,000-entry archive opens and scrolls smoothly: tree indexing stays fast and only visible rows render (Task 7 test).
- A ZIP made on Turkish Windows without the UTF-8 flag shows `kuruluş.txt`, not garbage (Task 4 test).
- An archive with the wrong extension opens by its real format, and a non-archive named `.rar` gives "not an archive" (Task 1 test).
- A file entry colliding with an existing directory, or a destination that fails mid-write, gives an error and leaves no partial file and no deleted directory (Task 2 and Task 6 tests).
- A Windows-illegal name in an archive (`CON`, `a:b`, trailing dot) extracts under a safe name on Windows and is not a crash (Task 2 test).

---

## File Structure

```
src-tauri/src/lib.rs               app setup, command registration, native menu
src-tauri/src/error.rs             ArchiveError
src-tauri/src/engine/mod.rs        types, ArchiveEngine, JobCtl, open_engine
src-tauri/src/engine/detect.rs     format detection by magic bytes
src-tauri/src/engine/plan.rs       safe_dest, plan_entry, selection matching
src-tauri/src/engine/rar.rs        RAR engine
src-tauri/src/engine/zip.rs        ZIP engine
src-tauri/src/engine/sevenz.rs     7z engine
src-tauri/src/engine/tarlike.rs    TAR, TAR.*, GZ, BZ2, XZ engine
src-tauri/src/jobs.rs              background jobs, progress, cancel, conflict answers
src-tauri/src/session.rs           in-memory password cache, temp files
src-tauri/src/commands.rs          Tauri commands
src-tauri/src/local_fs.rs          local folder listing
src-tauri/tests/fixtures/          committed RAR fixtures
scripts/make-fixtures.sh           regenerates RAR fixtures with the real rar tool
src/lib/api.ts, types.ts           typed invoke wrappers and shared types
src/lib/i18n/{tr,en,index}.ts      strings
src/lib/tree.ts, sort.ts, virtual.ts, format.ts   pure logic
src/lib/VirtualList.svelte, LocalPane.svelte, ArchivePane.svelte, Toolbar.svelte, StatusBar.svelte
src/lib/dialogs/{Modal,Password,Extract,Conflict,Progress,Result,Info,About}.svelte
src/routes/+page.svelte, +layout.ts
tests/*.test.mjs                   node --test on pure TS logic
```

---

### Task 1: Project setup and format detection

**Files:**
- Create: project scaffold (`package.json`, `svelte.config.js`, `vite.config.js`, `tsconfig.json`, `.gitignore`, `src-tauri/Cargo.toml`, `src-tauri/tauri.conf.json`, `src-tauri/capabilities/default.json`, `src-tauri/build.rs`, `src-tauri/src/main.rs`, `src-tauri/src/lib.rs`, `src/app.html`, `src/routes/+layout.ts`, `src/routes/+page.svelte`)
- Create: `src-tauri/src/error.rs`, `src-tauri/src/engine/mod.rs`, `src-tauri/src/engine/detect.rs`
- Test: inline `#[cfg(test)]` in `detect.rs`

**Interfaces:**
- Produces: `enum Format { Rar, Zip, SevenZ, Tar, TarGz, TarBz2, TarXz, Gz, Bz2, Xz }` (serde camelCase); `fn detect_format(path: &Path) -> Result<Format, ArchiveError>`.
- Produces: `enum ArchiveError` with `thiserror` and a serde shape `{ code: string, detail: string }`. Variants and codes: `NotAnArchive` `not_archive`, `PasswordRequired` `password_required`, `WrongPassword` `wrong_password`, `MissingVolume(String)` `missing_volume`, `Corrupt(String)` `corrupt`, `DiskFull` `disk_full`, `PermissionDenied(String)` `permission`, `UnsafePath(String)` `unsafe_path`, `DestConflict(String)` `dest_conflict`, `Unsupported(String)` `unsupported`, `Cancelled` `cancelled`, `Io(String)` `io`. `impl From<std::io::Error>` maps `StorageFull` to `DiskFull` and `PermissionDenied` to `PermissionDenied`.

- [ ] **Step 1: Scaffold.** Run `git init` in `~/Desktop/OnurGithub/Ambar`, then copy the shape of Liman's `package.json` scripts, `svelte.config.js` and `tauri.conf.json` (window 1280x800, min 760x480, `signingIdentity "-"`) with Ambar's names. Do not add updater, serial, SSH or xterm dependencies. Set `"test": "node --test \"tests/**/*.test.mjs\""`. Add plugins `tauri-plugin-dialog` and `tauri-plugin-opener`. Add `.gitignore` for `node_modules`, `build`, `.svelte-kit`, `src-tauri/target`, `tests/.tools`, `.superpowers`. Generate icons with `npx tauri icon` from a simple crate-shaped placeholder PNG in `assets/`.

- [ ] **Step 2: Verify the scaffold builds.**

Run: `npm install && npm run build && cargo check --manifest-path src-tauri/Cargo.toml`
Expected: all exit 0.

- [ ] **Step 3: Write failing tests in `detect.rs`.**

```rust
#[test] fn rar5_magic_is_rar_even_with_zip_extension() // bytes 52 61 72 21 1A 07 01 00 in x.zip -> Format::Rar
#[test] fn rar4_magic_is_rar()                         // 52 61 72 21 1A 07 00
#[test] fn zip_magic_with_rar_extension_is_zip()       // 50 4B 03 04 in x.rar -> Format::Zip
#[test] fn empty_zip_is_zip()                          // 50 4B 05 06
#[test] fn sevenz_magic()                              // 37 7A BC AF 27 1C
#[test] fn gz_with_tar_gz_name_is_targz()              // 1F 8B + name a.tar.gz or a.tgz -> TarGz; b.gz -> Gz
#[test] fn bz2_and_xz_magic_with_names()               // 42 5A 68, FD 37 7A 58 5A 00; .tar.bz2/.tbz2 -> TarBz2; .tar.xz/.txz -> TarXz
#[test] fn tar_ustar_at_offset_257()                   // "ustar" at 257 -> Tar
#[test] fn text_file_named_rar_is_not_archive()        // "merhaba" in x.rar -> Err(NotAnArchive)
#[test] fn sfx_rar_is_found_after_stub()               // 4096 junk bytes then RAR5 magic -> Rar; scan first 1 MiB
```

- [ ] **Step 4: Run tests to verify they fail.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml detect`
Expected: FAIL, `detect_format` not defined.

- [ ] **Step 5: Implement `detect_format` and `ArchiveError`.** Check fixed offsets first. Scan the first 1 MiB for the RAR signature only when no other format matched.

- [ ] **Step 6: Run tests to verify they pass.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml detect`
Expected: PASS, 10 tests.

- [ ] **Step 7: Commit.** `git add -A && git commit -m "Scaffold Ambar and detect archive formats by content"`

---

### Task 2: Engine contract and safe extraction paths

**Files:**
- Modify: `src-tauri/src/engine/mod.rs`
- Create: `src-tauri/src/engine/plan.rs`
- Test: inline in `plan.rs`

**Interfaces:**
- Consumes: `Format`, `ArchiveError`.
- Produces, in `engine/mod.rs`, all serde camelCase:

```rust
pub struct Entry { pub path: String, pub is_dir: bool, pub size: u64, pub packed: Option<u64>,
    pub modified: Option<i64>, pub crc: Option<u32>, pub encrypted: bool }
pub struct ArchiveInfo { pub format: Format, pub file_size: u64, pub encrypted_headers: bool,
    pub solid: bool, pub multi_volume: bool, pub has_recovery: bool, pub locked: bool, pub comment: Option<String> }
pub enum OverwritePolicy { Ask, Overwrite, Skip, Rename }
pub enum ConflictChoice { Overwrite, Skip, Rename, Cancel }
pub struct ExtractRequest { pub dest: PathBuf, pub selection: Vec<String>, pub base: String,
    pub keep_paths: bool, pub overwrite: OverwritePolicy, pub keep_broken: bool }
pub struct FileFailure { pub path: String, pub error: ArchiveError }
pub struct OpReport { pub ok: u64, pub skipped: u64, pub failed: Vec<FileFailure> }
pub trait JobCtl: Send {
    fn progress(&mut self, current: &str, done_bytes: u64, total_bytes: u64);
    fn cancelled(&self) -> bool;
    fn resolve_conflict(&mut self, path: &Path) -> ConflictChoice;
}
pub type Password = zeroize::Zeroizing<String>;
pub trait ArchiveEngine: Send {
    fn list(&mut self, pw: Option<&Password>) -> Result<(ArchiveInfo, Vec<Entry>), ArchiveError>;
    fn extract(&mut self, pw: Option<&Password>, req: &ExtractRequest, ctl: &mut dyn JobCtl) -> Result<OpReport, ArchiveError>;
    fn test(&mut self, pw: Option<&Password>, ctl: &mut dyn JobCtl) -> Result<OpReport, ArchiveError>;
}
pub fn open_engine(path: &Path) -> Result<Box<dyn ArchiveEngine>, ArchiveError>;
```

`Entry.path` always uses `/`, has no leading `/` and no trailing `/`. `packed` is `None` for RAR, because the `unrar-ng` header does not expose it.

- Produces, in `engine/plan.rs`:
  - `fn safe_dest(dest_root: &Path, rel: &str) -> Result<PathBuf, ArchiveError>`
  - `fn is_selected(entry_path: &str, selection: &[String]) -> bool`
  - `fn output_rel(entry_path: &str, req: &ExtractRequest) -> Option<String>`
  - `enum PlanOutcome { Write(PathBuf), Skip }`
  - `fn plan_entry(entry: &Entry, req: &ExtractRequest, ctl: &mut dyn JobCtl) -> Result<PlanOutcome, ArchiveError>`
  - `fn normalize_entry_path(raw: &str) -> String`, which converts `\` to `/`, strips leading `/` and `./`, and applies NFC.
  - `fn write_via_temp(target: &Path, keep_broken: bool, write: impl FnOnce(&Path) -> Result<(), ArchiveError>) -> Result<(), ArchiveError>`: writes to `<target>.ambar-part`, renames on success, deletes on failure unless `keep_broken`, in which case it renames to target.

- [ ] **Step 1: Write failing tests in `plan.rs`.**

```rust
#[test] fn plain_relative_path_joins()               // safe_dest("/d","a/b.txt") == "/d/a/b.txt"
#[test] fn parent_traversal_rejected()               // "../x", "a/../../x" -> Err(UnsafePath)
#[test] fn inner_parent_within_root_rejected_too()   // "a/../x" -> Err(UnsafePath); no normalization, any ".." fails
#[test] fn absolute_and_drive_paths_rejected()       // "/etc/x", "C:\\x", "\\\\srv\\x" -> Err(UnsafePath)
#[test] fn symlink_in_existing_parent_rejected()     // real symlink dest/link -> outside; "link/x" -> Err(UnsafePath)
#[test] fn nfd_name_becomes_nfc()                    // normalize_entry_path("s\u{327}.txt") == "ş.txt"
#[cfg(windows)] #[test] fn windows_reserved_names_sanitized() // "CON.txt"->"_CON.txt", "a:b"->"a_b", "x."->"x_"
#[test] fn selection_matches_file_and_children()     // selection ["a"] matches "a", "a/b"; not "ab"
#[test] fn empty_selection_matches_all()
#[test] fn output_rel_strips_base_when_keep_paths()  // base "belgeler", "belgeler/x/y.txt" -> "x/y.txt"
#[test] fn output_rel_flattens_when_not_keep_paths() // "belgeler/x/y.txt" -> "y.txt"
#[test] fn plan_overwrite_policy_rename_picks_free_name() // existing a.txt -> "a (1).txt"; a (1).txt also -> "a (2).txt"
#[test] fn plan_skip_policy_returns_skip()
#[test] fn plan_ask_cancel_returns_cancelled()       // ctl returns Cancel -> Err(Cancelled)
#[test] fn file_over_existing_directory_is_conflict_error() // dest/a is dir, entry file a, even with Overwrite -> Err(DestConflict); dir still exists
#[test] fn write_via_temp_failure_leaves_no_files()
#[test] fn write_via_temp_failure_keep_broken_keeps_file()
#[test] fn write_via_temp_success_replaces_target_only_at_end()
```

- [ ] **Step 2: Run tests to verify they fail.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml plan`
Expected: FAIL, functions not defined.

- [ ] **Step 3: Implement `plan.rs` and the types.** `safe_dest` rejects any `..` component. It then walks existing components under `dest_root` with `symlink_metadata` and rejects any symlink it finds. NFC needs `unicode-normalization`. All engines create parent directories only after `plan_entry` returns `Write`.

- [ ] **Step 4: Run tests to verify they pass.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml plan`
Expected: PASS.

- [ ] **Step 5: Commit.** `git add -A && git commit -m "Add the engine contract and safe extraction paths"`

---

### Task 3: RAR engine with real fixtures

**Files:**
- Create: `scripts/make-fixtures.sh`, `src-tauri/tests/fixtures/*.rar`, `src-tauri/src/engine/rar.rs`
- Test: inline in `rar.rs`

**Interfaces:**
- Consumes: Task 2 types; `plan_entry`, `normalize_entry_path`.
- Produces: `RarEngine::new(path: &Path) -> RarEngine`, implementing `ArchiveEngine`. Wired into `open_engine`.
- Also produces a shared test helper in `engine/mod.rs` under `#[cfg(test)]`: `struct TestCtl { cancel_after: Option<usize>, choice: ConflictChoice, seen: Vec<String> }`, implementing `JobCtl`.

- [ ] **Step 1: Write `scripts/make-fixtures.sh`.** It downloads `https://www.rarlab.com/rar/rarmacos-arm-723.tar.gz` with `curl` to the gitignored `tests/.tools/`, builds this tree from a fixed source and writes the output to `src-tauri/tests/fixtures/`.

Tree: `belgeler/rapor.txt` ("rapor icerigi\n"), `belgeler/alt/notlar.txt` ("notlar\n"), `türkçe ğüşıöç/😀.txt` ("emoji\n"), `kok.txt` ("kok\n").

Fixtures, using password `Test123`:
`plain5.rar` (`-ma5`), `plain4.rar` (`-ma4`), `enc.rar` (`-pTest123`), `enc-headers.rar` (`-hpTest123`), `solid.rar` (`-s`), `multi.part1.rar`+`multi.part2.rar` (`-v5k`, with 20 KB of random bytes added as `buyuk.bin`), `comment.rar` (`-z` with "yorum"), `locked.rar` (`k`), `recovery.rar` (`-rr5`), `corrupt.rar` (`plain5.rar` copied with bytes at the payload flipped so CRC fails).

- [ ] **Step 2: Generate and commit the fixtures.**

Run: `bash scripts/make-fixtures.sh && ls src-tauri/tests/fixtures | wc -l`
Expected: 11 files.

- [ ] **Step 3: Write failing tests in `rar.rs`.**

```rust
#[test] fn lists_rar5_with_turkish_and_emoji_names() // contains "türkçe ğüşıöç/😀.txt", "belgeler/alt/notlar.txt"; sizes correct; dirs is_dir
#[test] fn lists_rar4()
#[test] fn listing_header_encrypted_without_password_requires_it() // Err(PasswordRequired)
#[test] fn listing_header_encrypted_with_wrong_password()          // Err(WrongPassword)
#[test] fn listing_header_encrypted_with_password_works()          // info.encrypted_headers == true
#[test] fn enc_lists_without_password_marks_encrypted()            // entries encrypted == true
#[test] fn extract_enc_wrong_password_errors_no_partial_files()    // failed or Err(WrongPassword); dest has no files and no .ambar-part
#[test] fn extract_all_content_matches()                           // reads files back and compares
#[test] fn extract_selection_only_with_base()                      // selection ["belgeler/alt"], base "belgeler" -> only dest/alt/notlar.txt
#[test] fn extract_solid_selected_late_file_correct()
#[test] fn extract_from_part1_and_part2_reassembles()              // buyuk.bin byte-equal
#[test] fn opening_part2_uses_first_part()
#[test] fn missing_part2_reports_missing_volume()                  // copy only part1 -> Err(MissingVolume("multi.part2.rar")) on extract
#[test] fn test_ok_on_plain()                                      // failed empty
#[test] fn test_reports_crc_failure_on_corrupt()                   // failed non-empty or Err(Corrupt)
#[test] fn info_flags()                                            // comment, locked, recovery, solid
#[test] fn cancel_stops_extract()                                  // cancel_after Some(1) -> Err(Cancelled)
```

- [ ] **Step 4: Run tests to verify they fail.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml engine::rar`
Expected: FAIL.

- [ ] **Step 5: Implement `RarEngine`.** Use the per-entry API, `open_for_listing` then `open_for_processing`, with `extract_to` on the path from `plan_entry`. Never use `extract_all`, because it would bypass `safe_dest`. Use `skip()` for unselected entries. Call `as_first_part()` on open. Map `Code`: `MissingPassword`→`PasswordRequired`, `BadPassword`→`WrongPassword`, `BadData`/`BadArchive`→`Corrupt`, `UnknownFormat`→`NotAnArchive`. For `EOpen` on a multipart archive, check with `nth_part` which part is absent and return `MissingVolume`. Before each entry check `ctl.cancelled()`. Treat `file_time` as a Unix or DOS timestamp according to the crate's documented format.

- [ ] **Step 6: Run tests to verify they pass.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml engine::rar`
Expected: PASS, 17 tests.

- [ ] **Step 7: Commit.** `git add -A && git commit -m "Add the RAR engine with real rar fixtures"`

---

### Task 4: ZIP engine

**Files:**
- Create: `src-tauri/src/engine/zip.rs`
- Test: inline; fixtures are built at runtime with the `zip` writer in a `tempfile::TempDir`.

**Interfaces:**
- Consumes: Task 2 types and `plan_entry`, `write_via_temp`.
- Produces: `ZipEngine::new(path: &Path) -> ZipEngine`, implementing `ArchiveEngine`. Wired into `open_engine`.
- Enable the `zip` crate's AES and deflate/bzip2/zstd features.

- [ ] **Step 1: Write failing tests.**

```rust
#[test] fn lists_and_extracts_utf8_names()
#[test] fn cp857_name_without_utf8_flag_decodes_turkish()
// write ASCII name "kurulu_.txt", patch '_' byte to 0x9F in local and central headers -> entry "kuruluş.txt"
#[test] fn valid_utf8_without_flag_stays_utf8()
#[test] fn traversal_entry_is_failed_not_written()     // "../evil.txt" -> in report.failed with UnsafePath; other entries still extracted; nothing outside dest
#[test] fn aes_encrypted_needs_password()              // no pw -> PasswordRequired; wrong -> WrongPassword, no leftover; right -> content equal
#[test] fn zipcrypto_wrong_password_detected()         // may fail on CRC; expect WrongPassword or file in failed, never success
#[test] fn corrupt_crc_is_reported()                   // flip payload byte -> failed non-empty; no partial without keep_broken
#[test] fn corrupt_keep_broken_keeps_file()
#[test] fn directories_only_entries_created()
#[test] fn cancel_stops_extract()
```

- [ ] **Step 2: Run tests to verify they fail.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml engine::zip`
Expected: FAIL.

- [ ] **Step 3: Implement `ZipEngine`.** Read `name_raw()`. When the bytes are valid UTF-8 keep them, otherwise decode with `oem_cp` code page 857. Do not use the crate's own `enclosed_name` or `extract`. Stream through `write_via_temp`, reporting progress every 1 MiB. Use `info.comment` for the archive comment.

- [ ] **Step 4: Run tests to verify they pass.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml engine::zip`
Expected: PASS, 10 tests.

- [ ] **Step 5: Commit.** `git add -A && git commit -m "Add the ZIP engine with Turkish legacy names"`

---

### Task 5: 7z, TAR and single-stream engines

**Files:**
- Create: `src-tauri/src/engine/sevenz.rs`, `src-tauri/src/engine/tarlike.rs`
- Test: inline; fixtures are built at runtime with each crate's writer.

**Interfaces:**
- Consumes: Task 2 types; `plan_entry`, `write_via_temp`.
- Produces: `SevenZEngine::new(path)`, `TarLikeEngine::new(path, format: Format)`. Both wired into `open_engine`, so every `Format` variant now resolves.
- Single-stream formats (`Gz`, `Bz2`, `Xz`) show one entry named after the file without its last extension, with `size` 0 until decompressed. `Bz2`/`Xz` `test` reads the full stream.

- [ ] **Step 1: Write failing tests.**

```rust
// sevenz.rs
#[test] fn lists_and_extracts()
#[test] fn encrypted_needs_password()     // no pw -> PasswordRequired; wrong -> WrongPassword, no leftover; right OK
#[test] fn selected_entry_only()
#[test] fn corrupt_is_reported()
#[test] fn cancel_stops_extract()
// tarlike.rs
#[test] fn tar_gz_bz2_xz_list_and_extract()
#[test] fn tar_traversal_entry_is_failed()      // hand-written ustar header named "../evil.txt"
#[test] fn tar_symlink_entry_cannot_escape()    // symlink entry "l -> /tmp" then "l/x": nothing written outside dest; symlink entries are reported in `failed` with Unsupported, never created
#[test] fn gz_single_stream_extracts_named_output() // a.txt.gz -> a.txt
#[test] fn truncated_gz_is_corrupt()            // no leftover file
```

- [ ] **Step 2: Run tests to verify they fail.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- engine::sevenz engine::tarlike`
Expected: FAIL.

- [ ] **Step 3: Implement both engines.** Enable `sevenz-rust2`'s AES feature and `liblzma`'s static feature so no system library is required. Tar listing streams the whole file, and the result is cached in the engine.

- [ ] **Step 4: Run all engine tests.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml engine`
Expected: PASS.

- [ ] **Step 5: Commit.** `git add -A && git commit -m "Add 7z, TAR and single-stream engines"`

---

### Task 6: Jobs, sessions and Tauri commands

**Files:**
- Create: `src-tauri/src/jobs.rs`, `src-tauri/src/session.rs`, `src-tauri/src/local_fs.rs`, `src-tauri/src/commands.rs`
- Modify: `src-tauri/src/lib.rs`
- Test: inline in `jobs.rs`, `session.rs`, `local_fs.rs`

**Interfaces:**
- Consumes: `open_engine`, Task 2 types.
- Produces these Tauri commands, in camelCase JSON:
  - `open_archive(path: String, password: Option<String>) -> { info: ArchiveInfo, entries: Vec<Entry> }`, run on `spawn_blocking`. A given password is cached only after success.
  - `start_extract(path: String, password: Option<String>, req: ExtractRequest) -> String` returns job id.
  - `start_test(path: String, password: Option<String>) -> String`
  - `cancel_job(id: String)`
  - `answer_conflict(id: String, choice: ConflictChoice)`
  - `forget_password(path: String)`
  - `list_local(path: String) -> Vec<LocalEntry>`, where `LocalEntry { name, path, is_dir, size, modified }`; hidden files are included with a name-based `hidden: bool`
  - `home_dir() -> String`
  - `parent_dir(path: String) -> Option<String>`
  - `open_entry(path: String, entry: String, password: Option<String>, quick_look: bool) -> ()`: extracts into a session temp folder, then opens with `tauri-plugin-opener`, or `qlmanage -p` on Mac when `quick_look` is true.
- Produces events: `job-progress` `{ id, current, doneBytes, totalBytes }`, `job-conflict` `{ id, path }`, `job-done` `{ id, report: OpReport | null, error: ArchiveError | null }`.
- `password` falls back to the cache when `None`. Commands never include the password in events, errors or logs.

- [ ] **Step 1: Write failing tests.**

```rust
// jobs.rs, with a fake engine and an event sink trait so no Tauri runtime is needed
#[test] fn job_emits_done_with_report()
#[test] fn cancel_flag_reaches_engine_and_done_has_cancelled_error()
#[test] fn conflict_blocks_until_answer()     // resolve_conflict waits on channel; answer_conflict releases it
#[test] fn cancel_releases_pending_conflict()  // cancel while waiting -> ConflictChoice::Cancel
#[test] fn failed_entries_are_not_success()   // report.failed non-empty is delivered intact
#[test] fn disk_error_surfaces_as_disk_full() // engine returns ArchiveError::from(StorageFull) -> job-done error code "disk_full"
// session.rs
#[test] fn password_cached_per_path_and_forgotten()
#[test] fn serialized_events_contain_no_password() // JSON of all events for a job with password "Sifre-Gizli-9" does not contain it
#[test] fn temp_dir_removed_on_cleanup()
// local_fs.rs
#[test] fn lists_with_dirs_and_sizes()
#[test] fn unreadable_dir_returns_permission_error() // #[cfg(unix)], chmod 000
```

- [ ] **Step 2: Run tests to verify they fail.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml -- jobs session local_fs`
Expected: FAIL.

- [ ] **Step 3: Implement.** Jobs run on `std::thread`. `struct Jobs` is managed state holding `HashMap<String, JobHandle>` with an `AtomicBool` cancel and an `mpsc::Sender<ConflictChoice>`. Throttle progress to at most 20 events per second. Call temp cleanup on the window's `Destroyed` event and at startup for stale folders under the app cache directory.

- [ ] **Step 4: Run all Rust tests.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml`
Expected: PASS.

- [ ] **Step 5: Commit.** `git add -A && git commit -m "Add background jobs, session passwords and commands"`

---

### Task 7: Frontend logic and virtual list

**Files:**
- Create: `src/lib/types.ts`, `src/lib/api.ts`, `src/lib/i18n/{tr,en,index}.ts`, `src/lib/tree.ts`, `src/lib/sort.ts`, `src/lib/virtual.ts`, `src/lib/format.ts`, `src/lib/VirtualList.svelte`
- Test: `tests/tree.test.mjs`, `tests/sort.test.mjs`, `tests/virtual.test.mjs`, `tests/format.test.mjs`

**Interfaces:**
- Consumes: Task 6 commands and events; `types.ts` mirrors the Rust types.
- Produces:
  - `buildIndex(entries: Entry[]): ArchiveIndex`
  - `childrenOf(index: ArchiveIndex, dir: string): TreeNode[]`, where `TreeNode = { name, path, isDir, size, packed, modified, encrypted, crc }`. Implicit parent folders that have no entry of their own are synthesized, with `size` as the sum of descendants.
  - `descendantPaths(index, paths: string[]): string[]`
  - `sortNodes(nodes, key: SortKey, dir: "asc"|"desc"): TreeNode[]`. Folders come first, and names compare with `Intl.Collator("tr", { numeric: true })`.
  - `visibleRange(scrollTop, viewportH, rowH, total, overscan): { start, end }`
  - `formatBytes(n)`, using Turkish decimals (`2,4 MB`); `formatRatio(packed, size)`; `formatDate(ms)`.
  - `t(key, params?)`, with the language from `localStorage` key `ambar.lang`, defaulting to `tr`; every failure code from Task 1 has a string `errors.<code>`.
  - `VirtualList.svelte` with props `items`, `rowHeight`, a `row` snippet, and `onScroll`.

- [ ] **Step 1: Write failing tests.**

```js
test("synthesizes implicit folders and sums sizes")
test("root children contain only top level")
test("descendantPaths expands folder")
test("100k entries index under 500 ms and childrenOf under 50 ms")
test("Turkish sorting: c < ç < d, ı < i, folders first")
test("numeric sort: 2.txt before 10.txt")
test("visibleRange clamps at ends and adds overscan")
test("visibleRange renders under 100 rows for 100k items at 800px")
test("formatBytes 2516582 -> 2,4 MB; 0 -> 0 B")
test("formatRatio null packed -> em dash")
```

- [ ] **Step 2: Run tests to verify they fail.**

Run: `npm test`
Expected: FAIL.

- [ ] **Step 3: Implement.** The index is `Map<dir, TreeNode[]>`, built once in one pass over entries.

- [ ] **Step 4: Run tests to verify they pass.**

Run: `npm test && npm run check`
Expected: PASS, and svelte-check reports 0 errors.

- [ ] **Step 5: Commit.** `git add -A && git commit -m "Add archive tree, sorting, virtual list and strings"`

---

### Task 8: Dual-pane window

**Files:**
- Create: `src/lib/LocalPane.svelte`, `src/lib/ArchivePane.svelte`, `src/lib/Toolbar.svelte`, `src/lib/StatusBar.svelte`, `src/lib/tabs.svelte.ts`
- Modify: `src/routes/+page.svelte`, `src-tauri/src/lib.rs` (native menu)
- Test: `tests/tabs.test.mjs` for pure tab logic; manual check via Tauri dev.

**Interfaces:**
- Consumes: Task 7 functions; `api.ts`.
- Produces:
  - `tabs.svelte.ts`: `openTab(path)`, `closeTab(id)`, `activeTab`, and on each tab: `{ id, path, info, index, cwd, selection, sortKey, sortDir, locked }`. Opening a path that is already open focuses its tab.
  - Toolbar buttons Ekle, Çıkart, Sına, Göster, Sil, Onar, Bilgi. In this stage Ekle, Sil and Onar are disabled with a tooltip `t("toolbar.stage2")`.
  - Native menu with Dosya, Düzen, Görünüm, Yardım, and Mac app menu. Menu items emit `menu` events with an id.

- [ ] **Step 1: Write failing tests in `tests/tabs.test.mjs`.**

```js
test("opening the same path twice focuses the existing tab")
test("closing active tab focuses a neighbor")
test("navigate into folder and back restores parent selection")
```

Put the pure logic in `src/lib/tabs-core.ts` so Node can test it. `tabs.svelte.ts` wraps it with runes.

- [ ] **Step 2: Run tests to verify they fail.**

Run: `npm test`
Expected: FAIL.

- [ ] **Step 3: Implement the layout.** Follow option C in `docs/superpowers/specs/2026-11-01-ana-pencere-taslaklari.html`: toolbar, left local pane, draggable splitter, right archive pane with tabs and breadcrumb, status bar.
  - **Splitter:** ratio saved in `localStorage` key `ambar.split`, wrapped in try/catch.
  - **Empty right pane:** a drop zone with `t("archive.dropHint")`.
  - **Keyboard in both panes:** up/down, Enter (open), Backspace (up), Space (selection on Windows/Linux, Quick Look on Mac), Cmd/Ctrl+A, Cmd/Ctrl+W (close tab), Tab (switch pane).
  - **Columns:** sortable; packed and ratio columns show an em dash when `packed` is null.

- [ ] **Step 4: Run tests, then see it in the real window.**

Run: `npm test && npm run tauri dev`
Expected: tests pass. In the window, the left pane shows the home folder, double-clicking `src-tauri/tests/fixtures/plain5.rar` in the left pane opens it on the right, folders are enterable, and the Turkish and emoji names display correctly in light and dark.

- [ ] **Step 5: Commit.** `git add -A && git commit -m "Add the dual-pane window with tabs and native menu"`

---

### Task 9: Dialogs and extraction flow

**Files:**
- Create: `src/lib/dialogs/{Modal,Password,Extract,Conflict,Progress,Result,Info,About}.svelte`, `src/lib/flows.ts`
- Modify: `src/routes/+page.svelte`, `ArchivePane.svelte`, `LocalPane.svelte`
- Test: `tests/flows.test.mjs`

**Interfaces:**
- Consumes: Task 6 commands and events, Task 8 panes.
- Produces in `flows.ts`, with injected dependencies so it runs under Node:
  - `openWithPassword(path, deps)`: on `password_required` or `wrong_password`, prompts and retries until cancel.
  - `summarizeReport(report): { kind: "ok"|"partial"|"failed", ... }`

- [ ] **Step 1: Write failing tests.**

```js
test("openWithPassword prompts on password_required and retries with answer")
test("wrong password shows wrong-password message and prompts again")
test("cancel at prompt stops without error")
test("password is not kept in the returned tab state")
test("report with failed entries is partial, never ok")
test("report with zero ok and failures is failed")
test("cancelled job shows cancelled, not error, not ok")
```

- [ ] **Step 2: Run tests to verify they fail.**

Run: `npm test`
Expected: FAIL.

- [ ] **Step 3: Implement the flows.**
  - **Extract dialog:** destination (default: left pane's folder), overwrite policy, keep-paths, keep-broken, selected or all.
  - **Conflict dialog:** Üzerine yaz, Atla, Yeniden adlandır, İptal, with "tümüne uygula", which is a frontend answer repeated for later conflicts.
  - **Progress dialog:** current file, percentage and İptal. Duraklat and remaining time come in stage 2.
  - **Result dialog:** ok, partial with per-file failures, or failed, with a Ayrıntılar section.
  - **Info dialog:** `ArchiveInfo`.
  - **About:** Global Constraints text.
  - **Drag:** archive pane rows dragged to the left pane start extraction with the left pane's current folder and the Ask policy. Local pane items dropped on the right pane do nothing in this stage except archive files, which open.
  - **OS drop:** a file dropped on the window from the OS opens if it is an archive. Non-archive drops show `t("errors.not_archive")`.
  - **Entry open:** Göster and double-click on an entry call `open_entry`; Space on Mac calls it with `quick_look: true`.

- [ ] **Step 4: Run tests and check in the real window.**

Run: `npm test && npm run tauri dev`
Expected: tests pass. In the window, `enc-headers.rar` prompts, rejects a wrong password, accepts `Test123`, and extracts by dragging to the left pane. `corrupt.rar` tested via Sına shows a failure. Quick Look opens `kok.txt`.

- [ ] **Step 5: Commit.** `git add -A && git commit -m "Add password, extract, progress and result flows"`

---

### Task 10: End-to-end verification on a real build

**Files:**
- Create: `README.md`
- Modify: whichever files the checks reveal

**Interfaces:**
- Consumes: everything. Produces no new interface.

- [ ] **Step 1: Run all checks.**

Run: `cargo test --manifest-path src-tauri/Cargo.toml && npm test && npm run check`
Expected: all pass.

- [ ] **Step 2: Build the release app.** Tauri's own dmg step fails on this Mac, so build only the app.

Run: `npm run tauri build -- --bundles app`
Expected: `src-tauri/target/release/bundle/macos/Ambar.app` exists. Open it with `open`.

- [ ] **Step 3: Performance check with a generated large archive.** Create 100,000 small files in a scratch directory, archive them with the fixtures' `rar` tool into a scratch `big.rar`, and open it in the app.
Expected: the listing appears without freezing the window, scrolling is smooth, and the window stays responsive during a full extraction with working İptal.

- [ ] **Step 4: Manual scenario list in the release app.** Open each fixture; extract whole, selected and without paths; overwrite with each policy; use a read-only destination (expect a permission error); check Turkish and emoji names; try a file named `sahte.rar` containing text; use the multipart with part 2 removed; try light and dark; switch to English.
Expected: every result matches Global Constraints. Fix anything that fails, adding a test first when the failure is in testable logic.

- [ ] **Step 5: Write `README.md`.** Cover what stage 1 does, what comes next, how to build, how to regenerate fixtures, and the UnRAR licence note: "UnRAR source code is used to extract RAR archives, and may not be used to recreate the RAR compression algorithm."

- [ ] **Step 6: Commit.** `git add -A && git commit -m "Verify stage 1 on a release build and add the README"`
