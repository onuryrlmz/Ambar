import test from "node:test";
import assert from "node:assert/strict";
import {
  localFileGoesToItsApp, describeOpenFailures, extractTarget, goUpFlow, openEntryWithPrompts, openLocalWithPrompt, openWithPassword, pathsEqual,
  progressView, runJob, safeName, safeText, summarizeDone, summarizeReport, toArchiveError,
} from "../src/lib/flows.ts";
import { readFileSync } from "node:fs";
import { emptyTabs, openTab, openedTab } from "../src/lib/tabs-core.ts";

const err = (code, detail = "") => ({ code, detail });
const INFO = {
  format: "rar", fileSize: 1, encryptedHeaders: true, solid: false, multiVolume: false,
  hasRecovery: false, locked: false, comment: null,
};
const RESULT = { info: INFO, entries: [] };

/** An `open` that fails until it sees `right`, recording every password it was given. */
function lockedOpen(right) {
  const seen = [];
  return {
    seen,
    open: async (_path, password) => {
      seen.push(password);
      if (password === null) throw err("password_required");
      if (password !== right) throw err("wrong_password");
      return RESULT;
    },
  };
}

function asker(...answers) {
  const calls = [];
  return {
    calls,
    ask: async (...args) => {
      calls.push(args);
      return answers.shift() ?? null;
    },
  };
}

test("openWithPassword prompts on password_required and retries with answer", async () => {
  const o = lockedOpen("Test123");
  const a = asker("Test123");
  const r = await openWithPassword("/a.rar", { open: o.open, askPassword: a.ask });
  assert.equal(r.kind, "opened");
  assert.deepEqual(o.seen, [null, "Test123"], "the first try sends null so the session cache is used");
  assert.deepEqual(a.calls, [["/a.rar", false]]);
});

test("wrong password shows wrong-password message and prompts again", async () => {
  const o = lockedOpen("Test123");
  const a = asker("yanlis", "Test123");
  const r = await openWithPassword("/a.rar", { open: o.open, askPassword: a.ask });
  assert.equal(r.kind, "opened");
  assert.deepEqual(o.seen, [null, "yanlis", "Test123"]);
  assert.deepEqual(a.calls, [["/a.rar", false], ["/a.rar", true]]);
});

test("cancel at prompt stops without error", async () => {
  const o = lockedOpen("Test123");
  const a = asker(null);
  const r = await openWithPassword("/a.rar", { open: o.open, askPassword: a.ask });
  assert.deepEqual(r, { kind: "cancelled" });
  assert.deepEqual(o.seen, [null]);
});

test("another open error is a failure and does not prompt", async () => {
  const a = asker("x");
  const r = await openWithPassword("/a.bin", {
    open: async () => { throw err("not_archive"); },
    askPassword: a.ask,
  });
  assert.equal(r.kind, "failed");
  assert.equal(r.error.code, "not_archive");
  assert.equal(a.calls.length, 0);
});

test("password is not kept in the returned tab state", async () => {
  const o = lockedOpen("Gizli-Parola-9");
  const a = asker("Gizli-Parola-9");
  const r = await openWithPassword("/a.rar", { open: o.open, askPassword: a.ask });
  assert.ok(!JSON.stringify(r).includes("Gizli-Parola-9"));
  let s = openTab(emptyTabs(), "/a.rar");
  s = openedTab(s, s.tabs[0].id, r.result);
  const flat = JSON.stringify(s, (_k, v) => (v instanceof Map || v instanceof Set ? [...v] : v));
  assert.ok(!flat.includes("Gizli-Parola-9"));
  assert.equal(s.tabs[0].locked, true);
});

test("report with failed entries is partial, never ok", () => {
  const s = summarizeReport({ ok: 3, skipped: 1, failed: [{ path: "a", error: err("corrupt") }] });
  assert.equal(s.kind, "partial");
  assert.equal(s.ok, 3);
  assert.equal(s.skipped, 1);
  assert.equal(s.failures.length, 1);
});

test("report with zero ok and failures is failed", () => {
  const s = summarizeReport({ ok: 0, skipped: 2, failed: [{ path: "a", error: err("corrupt") }] });
  assert.equal(s.kind, "failed");
});

test("a clean report is ok, even when everything was skipped", () => {
  assert.equal(summarizeReport({ ok: 2, skipped: 0, failed: [] }).kind, "ok");
  assert.equal(summarizeReport({ ok: 0, skipped: 2, failed: [] }).kind, "ok");
});

test("cancelled job shows cancelled, not error, not ok", () => {
  const s = summarizeDone({ id: "j", report: null, error: err("cancelled") });
  assert.equal(s.kind, "cancelled");
  assert.equal(s.error, null);
});

test("a job error is failed and keeps its code and detail", () => {
  const s = summarizeDone({ id: "j", report: null, error: err("disk_full", "x") });
  assert.equal(s.kind, "failed");
  assert.deepEqual(s.error, err("disk_full", "x"));
});

test("a done event with neither report nor error is never success", () => {
  assert.equal(summarizeDone({ id: "j", report: null, error: null }).kind, "failed");
});

/** A fake backend: `script` holds one function per run, called with that run's controls. */
function fakeJobs(script, overrides = {}) {
  const log = { ids: [], passwords: [], answers: [], cancels: [], watched: [], unwatched: [], order: [] };
  const handlers = new Map();
  let n = 0;
  const deps = {
    newId: () => `job-${++n}`,
    watch: async (id, h) => {
      log.order.push(`watch:${id}`);
      log.watched.push(id);
      handlers.set(id, h);
      return () => {
        log.unwatched.push(id);
        handlers.delete(id);
      };
    },
    start: async (id, password) => {
      log.order.push(`start:${id}`);
      log.ids.push(id);
      log.passwords.push(password);
      const run = script.shift();
      queueMicrotask(() => run?.(handlers.get(id), id));
    },
    cancel: async (id) => {
      log.cancels.push(id);
    },
    answer: async (id, choice) => {
      log.answers.push([id, choice]);
    },
    askPassword: async () => null,
    askConflict: async () => ({ choice: "skip", applyAll: false }),
    ...overrides,
  };
  return { deps, log };
}

const doneOk = (h, id) => h.onDone({ id, report: { ok: 1, skipped: 0, failed: [] }, error: null });
const doneErr = (code) => (h, id) => h.onDone({ id, report: null, error: err(code) });

test("runJob registers its handlers before it starts the job", async () => {
  const { deps, log } = fakeJobs([doneOk]);
  const s = await runJob(deps);
  assert.equal(s.kind, "ok");
  assert.deepEqual(log.order, ["watch:job-1", "start:job-1"]);
  assert.deepEqual(log.passwords, [null], "null first: the session cache is the backend's");
});

test("runJob prompts on a password error and re-runs under a new id", async () => {
  const asked = [];
  const { deps, log } = fakeJobs([doneErr("password_required"), doneErr("wrong_password"), doneOk], {
    askPassword: async (wrong) => {
      asked.push(wrong);
      return wrong ? "Test123" : "yanlis";
    },
  });
  const s = await runJob(deps);
  assert.equal(s.kind, "ok");
  assert.deepEqual(asked, [false, true]);
  assert.deepEqual(log.passwords, [null, "yanlis", "Test123"]);
  assert.equal(new Set(log.ids).size, 3);
});

test("runJob: cancel at the password prompt is cancelled", async () => {
  const { deps } = fakeJobs([doneErr("password_required")]);
  assert.equal((await runJob(deps)).kind, "cancelled");
});

test("runJob: a password failure inside a report prompts too", async () => {
  const wrong = (h, id) =>
    h.onDone({ id, report: { ok: 0, skipped: 0, failed: [{ path: "a", error: err("wrong_password") }] }, error: null });
  const { deps, log } = fakeJobs([wrong, doneOk], { askPassword: async () => "Test123" });
  assert.equal((await runJob(deps)).kind, "ok");
  assert.deepEqual(log.passwords, [null, "Test123"]);
});

test("runJob: cancelling the prompt after a partial run reports that run, not cancelled", async () => {
  const mixed = (h, id) =>
    h.onDone({ id, report: { ok: 2, skipped: 0, failed: [{ path: "a", error: err("password_required") }] }, error: null });
  const { deps } = fakeJobs([mixed]);
  const s = await runJob(deps);
  assert.equal(s.kind, "partial");
  assert.equal(s.ok, 2);
});

test("runJob: a start rejection is failed and drops the handlers", async () => {
  const { deps, log } = fakeJobs([], {
    start: async () => { throw err("io", "destination must be an existing absolute folder"); },
  });
  const s = await runJob(deps);
  assert.equal(s.kind, "failed");
  assert.equal(s.error.code, "io");
  assert.deepEqual(log.unwatched, ["job-1"]);
});

test("runJob forwards progress", async () => {
  const seen = [];
  const run = (h, id) => {
    h.onProgress({ id, current: "a", doneBytes: 1, totalBytes: 2 });
    doneOk(h, id);
  };
  const { deps } = fakeJobs([run], { onProgress: (ev) => seen.push(ev.current) });
  await runJob(deps);
  assert.deepEqual(seen, ["a"]);
});

const settle = async () => {
  for (let i = 0; i < 20; i++) await new Promise((r) => setImmediate(r));
};

test("tümüne uygula answers later conflicts of that job without asking", async () => {
  let asked = 0;
  const run = async (h, id) => {
    for (const path of ["/d/a", "/d/b", "/d/c"]) {
      h.onConflict({ id, path });
      await settle();
    }
    doneOk(h, id);
  };
  const { deps, log } = fakeJobs([run], {
    askConflict: async () => {
      asked++;
      return { choice: "overwrite", applyAll: true };
    },
  });
  await runJob(deps);
  assert.equal(asked, 1);
  assert.deepEqual(log.answers.map((a) => a[1]), ["overwrite", "overwrite", "overwrite"]);
});

test("without tümüne uygula every conflict asks", async () => {
  const paths = [];
  const run = async (h, id) => {
    for (const path of ["/d/a", "/d/b"]) {
      h.onConflict({ id, path });
      await settle();
    }
    doneOk(h, id);
  };
  const { deps, log } = fakeJobs([run], {
    askConflict: async (path) => {
      paths.push(path);
      return { choice: "rename", applyAll: false };
    },
  });
  await runJob(deps);
  assert.deepEqual(paths, ["/d/a", "/d/b"]);
  assert.equal(log.answers.length, 2);
});

test("cancel at a conflict is never remembered as an answer for all", async () => {
  let asked = 0;
  const run = async (h, id) => {
    h.onConflict({ id, path: "/d/a" });
    await settle();
    h.onConflict({ id, path: "/d/b" });
    await settle();
    doneErr("cancelled")(h, id);
  };
  const { deps, log } = fakeJobs([run], {
    askConflict: async () => {
      asked++;
      return { choice: "cancel", applyAll: true };
    },
  });
  assert.equal((await runJob(deps)).kind, "cancelled");
  assert.equal(asked, 2);
  assert.deepEqual(log.answers.map((a) => a[1]), ["cancel", "cancel"]);
});

test("the cancel control cancels the running job", async () => {
  let control;
  const run = async (h, id) => {
    control.cancel();
    await settle();
    doneErr("cancelled")(h, id);
  };
  const { deps, log } = fakeJobs([run], { onControl: (c) => (control = c) });
  assert.equal((await runJob(deps)).kind, "cancelled");
  assert.ok(log.cancels.length > 0 && log.cancels.every((id) => id === "job-1"));
});

test("a cancel pressed before start returns is sent again once the job exists", async () => {
  let control;
  let release;
  const { deps, log } = fakeJobs([], {
    onControl: (c) => (control = c),
    start: async () => {
      control.cancel();
      await new Promise((r) => (release = r));
    },
  });
  // The start never produces a done here: the cancel is what the test looks at.
  void runJob(deps);
  await settle();
  assert.deepEqual(log.cancels, ["job-1"]);
  release();
  await settle();
  assert.deepEqual(log.cancels, ["job-1", "job-1"]);
});

test("after cancel, a late conflict is answered cancel without a dialog", async () => {
  let control;
  let asked = 0;
  const run = async (h, id) => {
    control.cancel();
    h.onConflict({ id, path: "/d/a" });
    await settle();
    doneErr("cancelled")(h, id);
  };
  const { deps, log } = fakeJobs([run], {
    onControl: (c) => (control = c),
    askConflict: async () => {
      asked++;
      return { choice: "overwrite", applyAll: false };
    },
  });
  await runJob(deps);
  assert.equal(asked, 0);
  assert.deepEqual(log.answers.map((a) => a[1]), ["cancel"]);
});

test("the entry flow confirms a risky open and only then allows it", async () => {
  const calls = [];
  const open = async (password, allowRisky) => {
    calls.push([password, allowRisky]);
    if (!allowRisky) throw err("risky_open", "sh");
  };
  let confirms = 0;
  const r = await openEntryWithPrompts({
    open,
    askPassword: async () => null,
    confirmRisky: async () => {
      confirms++;
      return true;
    },
  });
  assert.equal(r.kind, "opened");
  assert.equal(confirms, 1);
  assert.deepEqual(calls, [[null, false], [null, true]]);
});

test("the entry flow stops when the risky open is refused", async () => {
  const calls = [];
  const r = await openEntryWithPrompts({
    open: async (_p, allow) => {
      calls.push(allow);
      throw err("risky_open");
    },
    askPassword: async () => null,
    confirmRisky: async () => false,
  });
  assert.deepEqual(r, { kind: "cancelled" });
  assert.deepEqual(calls, [false]);
});

test("the entry flow prompts for a password and keeps the risky answer across retries", async () => {
  const calls = [];
  const open = async (password, allow) => {
    calls.push([password, allow]);
    if (!allow) throw err("risky_open");
    if (password === null) throw err("password_required");
    if (password !== "ok") throw err("wrong_password");
  };
  const wrongs = [];
  const answers = ["no", "ok"];
  let confirms = 0;
  const r = await openEntryWithPrompts({
    open,
    askPassword: async (wrong) => {
      wrongs.push(wrong);
      return answers.shift();
    },
    confirmRisky: async () => {
      confirms++;
      return true;
    },
  });
  assert.equal(r.kind, "opened");
  assert.equal(confirms, 1);
  assert.deepEqual(wrongs, [false, true]);
  assert.deepEqual(calls.at(-1), ["ok", true]);
});

test("the entry flow reports other errors", async () => {
  const r = await openEntryWithPrompts({
    open: async () => { throw err("corrupt", "x"); },
    askPassword: async () => null,
    confirmRisky: async () => true,
  });
  assert.equal(r.kind, "failed");
  assert.equal(r.error.code, "corrupt");
});

test("a local file open confirms only a risky type", async () => {
  const calls = [];
  const open = async (allow) => {
    calls.push(allow);
    if (!allow) throw err("risky_open");
  };
  assert.equal((await openLocalWithPrompt({ open, confirmRisky: async () => true })).kind, "opened");
  assert.deepEqual(calls, [false, true]);
  assert.equal((await openLocalWithPrompt({ open, confirmRisky: async () => false })).kind, "cancelled");
});

test("extractTarget: a selection goes as plain paths, based at the current folder", () => {
  assert.deepEqual(extractTarget("belgeler", new Set(["f:belgeler/a.txt", "d:belgeler/alt"]), "selected"), {
    selection: ["belgeler/a.txt", "belgeler/alt"],
    base: "belgeler",
  });
});

test("extractTarget: all of a subfolder is that folder, all of the root is everything", () => {
  assert.deepEqual(extractTarget("belgeler", new Set(), "folder"), { selection: ["belgeler"], base: "belgeler" });
  assert.deepEqual(extractTarget("", new Set(), "folder"), { selection: [], base: "" });
  assert.deepEqual(extractTarget("belgeler", new Set(["f:belgeler/a"]), "folder"), {
    selection: ["belgeler"],
    base: "belgeler",
  });
});

test("extractTarget: 'selected' with nothing selected means the folder, never an empty whole-archive surprise", () => {
  assert.deepEqual(extractTarget("belgeler", new Set(), "selected"), { selection: ["belgeler"], base: "belgeler" });
});

test("progressView guards a zero total with an indeterminate state", () => {
  assert.equal(progressView(null).percent, null);
  assert.equal(progressView({ id: "j", current: "a", doneBytes: 0, totalBytes: 0 }).percent, null);
  assert.equal(progressView({ id: "j", current: "a", doneBytes: 5, totalBytes: 0 }).percent, null);
});

test("progressView floors and clamps", () => {
  assert.equal(progressView({ id: "j", current: "a", doneBytes: 1, totalBytes: 3 }).percent, 33);
  assert.equal(progressView({ id: "j", current: "a", doneBytes: 999, totalBytes: 1000 }).percent, 99);
  assert.equal(progressView({ id: "j", current: "a", doneBytes: 9, totalBytes: 3 }).percent, 100);
});

test("safeName shows control and bidi characters as a marker", () => {
  assert.equal(safeName("a\u202Etxt.sh"), "a\uFFFDtxt.sh");
  assert.equal(safeName("satır\nyeni"), "satır\uFFFDyeni");
  assert.equal(safeName("türkçe 🙂.txt"), "türkçe 🙂.txt");
});

test("pathsEqual ignores trailing separators only", () => {
  assert.ok(pathsEqual("/a/b/", "/a/b"));
  assert.ok(pathsEqual("/", "/"));
  assert.ok(!pathsEqual("/a/b", "/a/c"));
});

test("describeOpenFailures: one, several of one code, several of mixed codes", () => {
  assert.deepEqual(describeOpenFailures([]), null);
  assert.deepEqual(describeOpenFailures([{ name: "a", code: "corrupt" }]), { kind: "one", name: "a", code: "corrupt" });
  assert.deepEqual(
    describeOpenFailures([{ name: "a", code: "not_archive" }, { name: "b", code: "not_archive" }]),
    { kind: "many", n: 2, names: "a, b", code: "not_archive" },
  );
  assert.deepEqual(
    describeOpenFailures([{ name: "a", code: "not_archive" }, { name: "b", code: "corrupt" }]),
    { kind: "many", n: 2, names: "a, b", code: null },
  );
  const six = ["a", "b", "c", "d", "e", "f"].map((name) => ({ name, code: "not_archive" }));
  assert.equal(describeOpenFailures(six).names, "a, b, c, d, …");
});

test("toArchiveError keeps code and detail and wraps anything else", () => {
  assert.deepEqual(toArchiveError(err("corrupt", "x")), err("corrupt", "x"));
  assert.equal(toArchiveError(new Error("boom")).code, "io");
});

test("cancelling a job's password prompt is silent when nothing ran to completion", async () => {
  const { deps } = fakeJobs([doneErr("password_required")]);
  const s = await runJob(deps);
  assert.equal(s.kind, "cancelled");
  assert.equal(s.dismissed, true);
  assert.equal(s.mayHaveWritten, false);
});

test("a wrong-password run that wrote nothing, then a cancelled prompt, is silent too", async () => {
  const run = (h, id) => {
    h.onProgress({ id, current: "a", doneBytes: 0, totalBytes: 5 });
    h.onDone({ id, report: { ok: 0, skipped: 0, failed: [{ path: "a", error: err("wrong_password") }] }, error: null });
  };
  const s = await runJob(fakeJobs([run]).deps);
  assert.equal(s.dismissed, true);
  assert.equal(s.mayHaveWritten, false);
});

test("a cancelled prompt keeps a report from any earlier run that got files out", async () => {
  const mixed = (h, id) =>
    h.onDone({ id, report: { ok: 2, skipped: 0, failed: [{ path: "a", error: err("password_required") }] }, error: null });
  const answers = ["yanlis", null];
  const { deps } = fakeJobs([mixed, doneErr("wrong_password")], { askPassword: async () => answers.shift() });
  const s = await runJob(deps);
  assert.equal(s.kind, "partial");
  assert.equal(s.ok, 2);
  assert.ok(!s.dismissed);
  assert.equal(s.mayHaveWritten, true);
});

test("İptal before any progress may not have written; after progress it may have", async () => {
  let control;
  const early = async (h, id) => {
    control.cancel();
    doneErr("cancelled")(h, id);
  };
  const a = await runJob(fakeJobs([early], { onControl: (c) => (control = c) }).deps);
  assert.equal(a.kind, "cancelled");
  assert.ok(!a.dismissed, "the user asked for it mid-job: the dialog says so");
  assert.equal(a.mayHaveWritten, false);

  const late = async (h, id) => {
    h.onProgress({ id, current: "a", doneBytes: 1, totalBytes: 5 });
    control.cancel();
    doneErr("cancelled")(h, id);
  };
  const b = await runJob(fakeJobs([late], { onControl: (c) => (control = c) }).deps);
  assert.equal(b.mayHaveWritten, true);
});

test("a finished report counts as maybe written only when something came out", async () => {
  assert.equal(summarizeReport({ ok: 1, skipped: 0, failed: [] }).mayHaveWritten, true);
  assert.equal(summarizeReport({ ok: 0, skipped: 1, failed: [] }).mayHaveWritten, false);
});

test("safeName covers every invisible character, and the source spells them as escapes", () => {
  for (const c of ["\u0000", "\u001f", "\u007f", "\u009f", "\u061C", "\u200E", "\u200F", "\u202A", "\u202E", "\u2066", "\u2069"]) {
    assert.equal(safeName(`a${c}b`), "a\uFFFDb");
  }
  const src = readFileSync(new URL("../src/lib/names.ts", import.meta.url), "utf8");
  const invisible = /[\u0000-\u0009\u000b-\u001f\u007f-\u009f\u061C\u200B-\u200F\u202A-\u202E\u2060-\u2069\uFFFD]/;
  assert.ok(!invisible.test(src), "names.ts holds a raw invisible character");
});

test("safeText keeps line breaks and tabs, and still marks the rest", () => {
  assert.equal(safeText("bir\nki\tuc\r\nx"), "bir\nki\tuc\r\nx");
  assert.equal(safeText("a\u202Eb\u0000c"), "a\uFFFDb\uFFFDc");
});

test("describeOpenFailures shows sanitized names", () => {
  assert.equal(describeOpenFailures([{ name: "a\u202Etxt.sh", code: "corrupt" }]).name, "a\uFFFDtxt.sh");
  assert.equal(
    describeOpenFailures([{ name: "a\u202Eb", code: "corrupt" }, { name: "c", code: "corrupt" }]).names,
    "a\uFFFDb, c",
  );
});

/** The sequence `local.svelte.ts` keeps: each navigation takes the next number. */
function navSeq() {
  let latest = 0;
  return { current: () => latest, take: () => ++latest };
}

test("goUp shows the parent and takes a number for it", async () => {
  const seq = navSeq();
  const shown = [];
  await goUpFlow("/a/b", seq, {
    parent: async () => "/a",
    show: async (mine, parent, cameFrom) => void shown.push([mine, parent, cameFrom]),
    fail: () => assert.fail("no failure expected"),
  });
  assert.deepEqual(shown, [[1, "/a", "/a/b"]]);
});

test("goUp with no parent takes no number, so a navigation in flight survives", async () => {
  const seq = navSeq();
  const inFlight = seq.take();
  await goUpFlow("/", seq, {
    parent: async () => null,
    show: async () => assert.fail("nowhere to go"),
    fail: () => assert.fail("no failure expected"),
  });
  assert.equal(seq.current(), inFlight);
});

test("goUp gives way to a navigation that started while it waited", async () => {
  const seq = navSeq();
  await goUpFlow("/a/b", seq, {
    parent: async () => {
      seq.take();
      return "/a";
    },
    show: async () => assert.fail("a newer navigation owns the pane"),
    fail: () => assert.fail("no failure expected"),
  });
  assert.equal(seq.current(), 1);
});

test("goUp reports a failed lookup, unless a newer navigation took over", async () => {
  const seq = navSeq();
  const fails = [];
  const deps = (later) => ({
    parent: async () => {
      if (later) seq.take();
      throw err("io", "x");
    },
    show: async () => assert.fail("no parent"),
    fail: (e) => fails.push(e.code),
  });
  await goUpFlow("/a", seq, deps(false));
  assert.deepEqual(fails, ["io"]);
  await goUpFlow("/a", seq, deps(true));
  assert.deepEqual(fails, ["io"]);
});

test("a file named like an archive is never handed to its app when it is not one", () => {
  assert.equal(localFileGoesToItsApp("sahte.rar", "not_archive"), false);
  assert.equal(localFileGoesToItsApp("SAHTE.ZIP", "not_archive"), false);
  assert.equal(localFileGoesToItsApp("belge.txt", "not_archive"), true);
  assert.equal(localFileGoesToItsApp("bozuk.rar", "corrupt"), false);
  assert.equal(localFileGoesToItsApp("belge.txt", undefined), false);
});
