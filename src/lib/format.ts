import { getLang } from "./i18n/index.ts";

const UNITS = ["B", "KB", "MB", "GB", "TB"];

/** 2516582 -> "2,4 MB" (Turkish decimal comma; "1 KB" not "1,0 KB"). */
export function formatBytes(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return "0 B";
  let i = 0;
  let v = n;
  // Compare the rounded value so 1048575 becomes "1 MB", not "1024 KB".
  while (Math.round(v * 10) / 10 >= 1024 && i < UNITS.length - 1) {
    v /= 1024;
    i++;
  }
  if (i === 0) return `${Math.round(v)} B`;
  const s = (Math.round(v * 10) / 10).toFixed(1).replace(".", ",").replace(/,0$/, "");
  return `${s} ${UNITS[i]}`;
}

export function formatRatio(packed: number | null, size: number): string {
  if (packed === null || !(size > 0)) return "—";
  return `%${Math.round((packed / size) * 100)}`;
}

/** Takes milliseconds (Entry.modified is seconds: multiply by 1000 first). */
export function formatDate(ms: number | null): string {
  if (ms === null || !Number.isFinite(ms)) return "—";
  const d = new Date(ms);
  if (Number.isNaN(d.getTime())) return "—";
  return new Intl.DateTimeFormat(getLang() === "en" ? "en-GB" : "tr-TR", {
    day: "2-digit", month: "2-digit", year: "numeric",
    hour: "2-digit", minute: "2-digit", hour12: false,
  }).format(d);
}

export function formatCrc(crc: number | null): string {
  if (crc === null) return "—";
  return crc.toString(16).toUpperCase().padStart(8, "0");
}

/** Item counts with the language's digit grouping: 100000 -> "100.000". */
export function formatCount(n: number): string {
  return n.toLocaleString(getLang() === "en" ? "en-GB" : "tr-TR");
}
