import test from "node:test";
import assert from "node:assert/strict";
import { tr } from "../src/lib/i18n/tr.ts";
import { en } from "../src/lib/i18n/en.ts";
import { t, setLang } from "../src/lib/i18n/index.ts";

// Keep in sync with ArchiveError::code() in src-tauri/src/error.rs.
const CODES = [
  "not_archive", "password_required", "wrong_password", "missing_volume", "corrupt",
  "disk_full", "permission", "unsafe_path", "dest_conflict", "unsupported",
  "cancelled", "risky_open", "io",
];

test("tr and en have identical key sets", () => {
  assert.deepEqual(Object.keys(tr).sort(), Object.keys(en).sort());
});

test("every error code has a string in both languages", () => {
  for (const c of CODES) {
    assert.ok(tr[`errors.${c}`], `tr ${c}`);
    assert.ok(en[`errors.${c}`], `en ${c}`);
  }
});

test("t() defaults to tr without localStorage, falls back to key, interpolates", () => {
  assert.equal(t("errors.io"), tr["errors.io"]);
  assert.equal(t("no.such.key"), "no.such.key");
  assert.equal(t("progress.files", { n: 3 }), tr["progress.files"].replace("{n}", "3"));
});

test("setLang switches language", () => {
  setLang("en");
  assert.equal(t("errors.io"), en["errors.io"]);
  setLang("tr");
});
