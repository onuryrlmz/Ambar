import test from "node:test";
import assert from "node:assert/strict";
import { FAILURES_PER_ARCHIVE, runBatch } from "../src/lib/batch.ts";
import { selectedLocalFiles } from "../src/lib/tabs-core.ts";

const err = (code, detail = "") => ({ code, detail });
const report = (ok, skipped = 0, failed = [], decrypted = 0) => ({ report: { ok, skipped, failed, decrypted }, error: null });
const crash = (code) => ({ report: null, error: err(code) });

/**
 * A fake backend. `script` maps an archive path to what its job does, called with the
 * handlers, the password it got and the attempt number; returning a value finishes the job.
 */
function backend(script, over = {}) {
  const log = { started: [], running: 0, maxRunning: 0, passwords: {}, answers: [], cancels: [], prompts: [], archives: [] };
  const handlers = new Map();
  let n = 0;
  const attempts = {};
  const deps = {
    newId: () => `job-${++n}`,
    watch: async (id, h) => {
      handlers.set(id, h);
      return () => handlers.delete(id);
    },
    start: async (id, path, password) => {
      log.started.push(path);
      (log.passwords[path] ??= []).push(password);
      log.running++;
      log.maxRunning = Math.max(log.maxRunning, log.running);
      attempts[path] = (attempts[path] ?? 0) + 1;
      const finish = (done) => {
        log.running--;
        handlers.get(id).onDone({ id, ...done });
      };
      const out = await (script[path] ?? (() => report(1)))({
        id, handlers: handlers.get(id), password, attempt: attempts[path], finish,
      });
      if (out) setTimeout(() => finish(out), 0);
    },
    cancel: async (id) => void log.cancels.push(id),
    answer: async (id, choice) => {
      log.answers.push(choice);
      over.onAnswer?.(id, choice);
    },
    askConflict: async (path) => {
      log.prompts.push(`conflict:${path}`);
      return over.conflict?.(path) ?? { choice: "skip", applyAll: false };
    },
    needsPassword: async (path) => over.locked?.includes(path) ?? false,
    askPassword: async (name, wrong) => {
      log.prompts.push(`password:${name}:${wrong}`);
      return over.password ? over.password(name, wrong) : null;
    },
    onArchive: (i, total, name) => log.archives.push([i, total, name]),
    onControl: (c) => {
      log.control = c;
    },
  };
  return { deps, log };
}

test("runs one archive at a time, in the given order, naming each to the progress dialog", async () => {
  const { deps, log } = backend({});
  const s = await runBatch(["/d/a-1.zip", "/d/a-2.zip", "/d/a-10.zip"], deps);
  assert.deepEqual(log.started, ["/d/a-1.zip", "/d/a-2.zip", "/d/a-10.zip"]);
  assert.equal(log.maxRunning, 1);
  assert.deepEqual(log.archives, [[1, 3, "a-1.zip"], [2, 3, "a-2.zip"], [3, 3, "a-10.zip"]]);
  assert.equal(s.kind, "ok");
  assert.equal(s.total, 3);
  assert.equal(s.okArchives, 3);
  assert.equal(s.ok, 3);
  assert.deepEqual(s.rows, []);
});

test("one failed archive never stops the others, and is never reported as ok", async () => {
  const { deps, log } = backend({
    "/d/a.zip": () => report(2),
    "/d/b.txt": () => crash("not_archive"),
    "/d/c.zip": () => report(1, 0, [{ path: "x/bad.txt", error: err("corrupt") }]),
    "/d/d.zip": () => report(4, 1),
  });
  const s = await runBatch(["/d/a.zip", "/d/b.txt", "/d/c.zip", "/d/d.zip"], deps);
  assert.equal(log.started.length, 4);
  assert.equal(s.kind, "partial");
  assert.equal(s.okArchives, 2);
  assert.equal(s.failedArchives, 2);
  assert.equal(s.ok, 7);
  assert.equal(s.skipped, 1);
  assert.equal(s.failed, 2);
  assert.deepEqual(s.rows, [
    { archive: "b.txt", path: null, error: err("not_archive") },
    { archive: "c.zip", path: "x/bad.txt", error: err("corrupt") },
  ]);
});

test("everything failing is failed, not partial and never ok", async () => {
  const { deps } = backend({ "/a": () => crash("corrupt"), "/b": () => crash("not_archive") });
  const s = await runBatch(["/a", "/b"], deps);
  assert.equal(s.kind, "failed");
  assert.equal(s.failedArchives, 2);
  assert.equal(s.failed, 2);
});

test("skipped files alone are a clean run", async () => {
  const { deps } = backend({ "/a": () => report(0, 5) });
  const s = await runBatch(["/a"], deps);
  assert.equal(s.kind, "ok");
  assert.equal(s.okArchives, 1);
  assert.equal(s.skipped, 5);
});

test("an encrypted archive prompts before its job starts, once", async () => {
  const { deps, log } = backend({}, { locked: ["/a.zip"], password: () => "Parola-1" });
  await runBatch(["/a.zip"], deps);
  assert.deepEqual(log.prompts, ["password:a.zip:false"]);
  assert.deepEqual(log.passwords["/a.zip"], ["Parola-1"], "no passwordless first run to re-run");
});

test("a dismissed prompt skips that archive and continues, and is not ok", async () => {
  const { deps, log } = backend({}, { locked: ["/a.zip"] });
  const s = await runBatch(["/a.zip", "/b.zip"], deps);
  assert.deepEqual(log.started, ["/b.zip"], "the dismissed archive's job never starts");
  assert.equal(s.dismissedArchives, 1);
  assert.equal(s.okArchives, 1);
  assert.equal(s.kind, "partial");
  assert.deepEqual(s.rows, [{ archive: "a.zip", path: null, error: null, dismissed: true }]);
});

test("every prompt dismissed is failed, not ok", async () => {
  const { deps } = backend({}, { locked: ["/a.zip"] });
  const s = await runBatch(["/a.zip"], deps);
  assert.equal(s.kind, "failed");
  assert.equal(s.ok, 0);
});

test("a prompt dismissed after a wrong password skips too", async () => {
  let asked = 0;
  const { deps, log } = backend(
    { "/a.zip": () => crash("wrong_password"), "/b.zip": () => report(1) },
    { locked: ["/a.zip"], password: () => (++asked === 1 ? "yanlis" : null) },
  );
  const s = await runBatch(["/a.zip", "/b.zip"], deps);
  assert.deepEqual(log.started, ["/a.zip", "/b.zip"]);
  assert.equal(s.dismissedArchives, 1);
  assert.equal(s.okArchives, 1);
});

test("a password that worked is tried first on the next archive that needs one", async () => {
  const typed = [];
  const opened = () => report(1, 0, [], 1);
  const { deps, log } = backend(
    { "/a.zip": opened, "/b.zip": opened, "/c.zip": opened },
    { locked: ["/a.zip", "/b.zip", "/c.zip"], password: (name) => (typed.push(name), "Parola-1") },
  );
  const s = await runBatch(["/a.zip", "/b.zip", "/c.zip"], deps);
  assert.deepEqual(typed, ["a.zip"], "31 parts must not mean 31 prompts");
  assert.deepEqual(log.passwords["/c.zip"], ["Parola-1"]);
  assert.equal(s.okArchives, 3);
});

test("a tried password that fails prompts, and the new one is kept instead", async () => {
  const right = { "/a.zip": "bir", "/b.zip": "iki", "/c.zip": "iki" };
  const job = (path) => ({ password }) => (password === right[path] ? report(1, 0, [], 1) : crash("wrong_password"));
  const answers = ["bir", "iki"];
  const { deps, log } = backend(
    { "/a.zip": job("/a.zip"), "/b.zip": job("/b.zip"), "/c.zip": job("/c.zip") },
    { locked: ["/a.zip", "/b.zip", "/c.zip"], password: () => answers.shift() },
  );
  const s = await runBatch(["/a.zip", "/b.zip", "/c.zip"], deps);
  assert.deepEqual(log.passwords["/b.zip"], ["bir", "iki"]);
  assert.deepEqual(log.passwords["/c.zip"], ["iki"]);
  assert.deepEqual(log.prompts, ["password:a.zip:false", "password:b.zip:false"], "nobody typed a wrong one for b.zip");
  assert.equal(s.okArchives, 3);
});

test("a password that only skipped files proved nothing, so it is not remembered", async () => {
  const answers = ["YANLIS", "Parola-2"];
  const { deps, log } = backend(
    { "/a.zip": () => report(0, 4, [], 0), "/b.zip": () => report(1, 0, [], 1) },
    { locked: ["/a.zip", "/b.zip"], password: () => answers.shift() },
  );
  await runBatch(["/a.zip", "/b.zip"], deps);
  assert.deepEqual(log.passwords["/b.zip"], ["Parola-2"]);
  assert.deepEqual(log.prompts, ["password:a.zip:false", "password:b.zip:false"]);
});

test("a typed password that really fails says wrong on that archive's next prompt", async () => {
  const answers = ["yanlis", "dogru"];
  const { deps, log } = backend(
    { "/a.zip": ({ password }) => (password === "dogru" ? report(1, 0, [], 1) : crash("wrong_password")) },
    { locked: ["/a.zip"], password: () => answers.shift() },
  );
  await runBatch(["/a.zip"], deps);
  assert.deepEqual(log.prompts, ["password:a.zip:false", "password:a.zip:true"]);
});

test("joined volumes are listed and do not change the outcome", async () => {
  const { deps, log } = backend({});
  const s = await runBatch(["/d/s.part1.rar"], deps, [{ part: "/d/s.part2.rar", first: "/d/s.part1.rar" }]);
  assert.deepEqual(log.started, ["/d/s.part1.rar"]);
  assert.equal(s.kind, "ok");
  assert.equal(s.total, 1);
  assert.equal(s.joinedParts, 1);
  assert.deepEqual(s.rows, [{ archive: "s.part2.rar", path: null, error: null, volumeOf: "s.part1.rar" }]);
});

test("a later volume found only once the password opened its header is joined, not re-run", async () => {
  const opened = () => report(2, 0, [], 2);
  const { deps, log } = backend(
    { "/d/e.part1.rar": opened, "/d/e.part2.rar": opened, "/d/x.zip": () => report(1) },
    { locked: ["/d/e.part1.rar", "/d/e.part2.rar"], password: () => "Parola-1" },
  );
  deps.runKey = async (path, password) => {
    log.keys = [...(log.keys ?? []), [path, password]];
    return path.endsWith(".zip") ? path : password ? "/d/e.part1.rar" : null;
  };
  const s = await runBatch(["/d/e.part1.rar", "/d/e.part2.rar", "/d/x.zip"], deps);
  assert.deepEqual(log.started, ["/d/e.part1.rar", "/d/x.zip"]);
  assert.equal(s.total, 3);
  assert.equal(s.joinedParts, 1);
  assert.equal(s.okArchives, 2);
  assert.equal(s.notFinished, 0);
  assert.equal(s.kind, "ok");
  assert.deepEqual(s.rows, [{ archive: "e.part2.rar", path: null, error: null, volumeOf: "e.part1.rar" }]);
});

test("a plain archive gets no password, leaving the session cache to the backend", async () => {
  const { deps, log } = backend({}, { locked: ["/a.zip"], password: () => "Parola-1" });
  await runBatch(["/a.zip", "/p.zip"], deps);
  assert.deepEqual(log.passwords["/p.zip"], [null]);
});

test("a failed password check still runs the job, which reports the real error", async () => {
  const { deps, log } = backend({ "/a.zip": () => crash("corrupt") });
  deps.needsPassword = async () => {
    throw err("io");
  };
  const s = await runBatch(["/a.zip"], deps);
  assert.deepEqual(log.started, ["/a.zip"]);
  assert.equal(s.rows[0].error.code, "corrupt");
});

test("the summary never holds a password", async () => {
  const { deps } = backend({ "/b.zip": () => crash("corrupt") }, { locked: ["/a.zip"], password: () => "Gizli-Parola-9" });
  const s = await runBatch(["/a.zip", "/b.zip"], deps);
  assert.ok(!JSON.stringify(s).includes("Gizli-Parola-9"));
});

const conflicting = (paths) =>
  Object.fromEntries(paths.map((p) => [p, ({ handlers, id }) => {
    handlers.onConflict({ id, path: `${p}/same.txt` });
  }]));

test("tümüne uygula holds across the whole batch", async () => {
  const script = conflicting(["/a", "/b", "/c"]);
  const { deps, log } = backend(script, {
    conflict: () => ({ choice: "overwrite", applyAll: true }),
  });
  const finishers = [];
  for (const p of ["/a", "/b", "/c"]) {
    const inner = script[p];
    script[p] = (ctx) => {
      finishers.push(ctx);
      inner(ctx);
    };
  }
  deps.answer = async (id, choice) => {
    log.answers.push(choice);
    finishers.find((c) => c.id === id).finish(report(1));
  };
  const s = await runBatch(["/a", "/b", "/c"], deps);
  assert.deepEqual(log.prompts, ["conflict:/a/same.txt"], "asked once for three archives");
  assert.deepEqual(log.answers, ["overwrite", "overwrite", "overwrite"]);
  assert.equal(s.okArchives, 3);
});

test("a conflict answered without apply-all asks again in the next archive", async () => {
  const contexts = [];
  const job = (p) => (ctx) => {
    contexts.push(ctx);
    ctx.handlers.onConflict({ id: ctx.id, path: `${p}/x` });
  };
  const { deps, log } = backend({ "/a": job("/a"), "/b": job("/b") });
  deps.answer = async (id) => contexts.find((c) => c.id === id).finish(report(1));
  await runBatch(["/a", "/b"], deps);
  assert.deepEqual(log.prompts, ["conflict:/a/x", "conflict:/b/x"]);
});

test("the conflict's İptal stops the rest of the batch", async () => {
  const contexts = [];
  const { deps, log } = backend(
    { "/a": (ctx) => { contexts.push(ctx); ctx.handlers.onConflict({ id: ctx.id, path: "/a/x" }); } },
    { conflict: () => ({ choice: "cancel", applyAll: false }) },
  );
  deps.answer = async (id) => contexts[0].finish(crash("cancelled"));
  const s = await runBatch(["/a", "/b", "/c"], deps);
  assert.deepEqual(log.started, ["/a"]);
  assert.equal(s.kind, "cancelled");
  assert.equal(s.notFinished, 3);
});

test("İptal stops the current archive and the rest", async () => {
  const running = [];
  const { deps, log } = backend({
    "/b": (ctx) => {
      running.push(ctx);
      log.control.cancel();
    },
  });
  deps.cancel = async (id) => {
    log.cancels.push(id);
    running[0].finish(crash("cancelled"));
  };
  const s = await runBatch(["/a", "/b", "/c"], deps);
  assert.deepEqual(log.started, ["/a", "/b"]);
  assert.equal(log.cancels.length, 1, "the job under way hears about it");
  assert.equal(s.kind, "cancelled");
  assert.equal(s.okArchives, 1);
  assert.equal(s.notFinished, 2);
  assert.equal(s.ok, 1, "what got out before the cancel is still counted");
  assert.ok(s.mayHaveWritten);
});

test("İptal during the password check starts nothing more", async () => {
  const { deps, log } = backend({});
  deps.needsPassword = async () => {
    log.control.cancel();
    return false;
  };
  const s = await runBatch(["/a", "/b"], deps);
  assert.deepEqual(log.started, []);
  assert.equal(s.kind, "cancelled");
  assert.equal(s.notFinished, 2);
});

test("cancelled keeps the failures seen before it", async () => {
  const running = [];
  const { deps, log } = backend({
    "/a": () => crash("corrupt"),
    "/b": (ctx) => {
      running.push(ctx);
      log.control.cancel();
    },
  });
  deps.cancel = async () => running[0].finish(crash("cancelled"));
  const s = await runBatch(["/a", "/b"], deps);
  assert.equal(s.kind, "cancelled");
  assert.equal(s.failedArchives, 1);
  assert.equal(s.rows.length, 1);
});

test("a huge list of failures is capped per archive, and the count stays true", async () => {
  const many = Array.from({ length: FAILURES_PER_ARCHIVE + 50 }, (_, i) => ({ path: `f${i}`, error: err("corrupt") }));
  const { deps } = backend({ "/a": () => report(0, 0, many), "/b": () => crash("corrupt") });
  const s = await runBatch(["/a", "/b"], deps);
  assert.equal(s.failed, FAILURES_PER_ARCHIVE + 51);
  assert.equal(s.rows.length, FAILURES_PER_ARCHIVE + 2);
  assert.deepEqual(s.rows[FAILURES_PER_ARCHIVE], { archive: "a", path: null, error: null, more: 50 });
  assert.equal(s.rows[FAILURES_PER_ARCHIVE + 1].archive, "b");
});

test("start rejections count as that archive failing", async () => {
  const { deps } = backend({});
  const start = deps.start;
  deps.start = async (id, path, pw) => {
    if (path === "/a") throw err("dest_conflict", "a");
    return start(id, path, pw);
  };
  const s = await runBatch(["/a", "/b"], deps);
  assert.equal(s.rows[0].error.code, "dest_conflict");
  assert.equal(s.okArchives, 1);
});

test("an empty batch is nothing, not a success story", async () => {
  const { deps } = backend({});
  const s = await runBatch([], deps);
  assert.equal(s.total, 0);
  assert.equal(s.okArchives, 0);
});

test("selectedLocalFiles takes selected files in row order, never folders", () => {
  const rows = [
    { isDir: true, path: "/d/klasor" },
    { isDir: false, path: "/d/a.zip" },
    { isDir: false, path: "/d/b.zip" },
    { isDir: false, path: "/d/c.zip" },
  ];
  assert.deepEqual(selectedLocalFiles(rows, new Set(["f:/d/c.zip", "d:/d/klasor", "f:/d/a.zip"])), ["/d/a.zip", "/d/c.zip"]);
  assert.deepEqual(selectedLocalFiles(rows, new Set(["d:/d/klasor"])), []);
  assert.deepEqual(selectedLocalFiles(rows, new Set()), []);
});
