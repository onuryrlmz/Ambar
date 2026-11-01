import { tr } from "./tr.ts";
import { en } from "./en.ts";

export type Lang = "tr" | "en";
const KEY = "ambar.lang";
const tables: Record<Lang, Record<string, string>> = { tr, en };
let override: Lang | null = null;

export function getLang(): Lang {
  try {
    const v = globalThis.localStorage?.getItem(KEY);
    if (v === "tr" || v === "en") return v;
  } catch {
    // no storage (tests, private mode): fall through
  }
  return override ?? "tr";
}

export function setLang(l: Lang): void {
  override = l;
  try {
    globalThis.localStorage?.setItem(KEY, l);
  } catch {
    // ignore
  }
}

/** Looks the key up in the current language; a missing key falls back to the key. */
export function t(key: string, params?: Record<string, string | number>): string {
  let s = tables[getLang()][key] ?? key;
  if (params) {
    for (const [k, v] of Object.entries(params)) s = s.split(`{${k}}`).join(String(v));
  }
  return s;
}

/** The user-facing text for a backend error; an unknown code reads as a generic I/O error. */
export function errorText(code: string): string {
  const key = `errors.${code}`;
  return key in tables[getLang()] ? t(key) : t("errors.io");
}
