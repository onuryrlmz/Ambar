#!/usr/bin/env bash
# Regenerates src-tauri/tests/fixtures/*.rar with the `rar` command line tool.
# The tool is downloaded into tests/.tools (gitignored) and is never shipped.
# Developer machine only (macOS arm64).
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TOOLS="$ROOT/tests/.tools"
OUT="$ROOT/src-tauri/tests/fixtures"
PW="Test123"

mkdir -p "$TOOLS" "$OUT"
if [ ! -x "$TOOLS/rar/rar" ]; then
  curl -sSfL -o "$TOOLS/rar.tar.gz" https://www.rarlab.com/rar/rarmacos-arm-723.tar.gz
  tar -xzf "$TOOLS/rar.tar.gz" -C "$TOOLS"
fi
RAR="$TOOLS/rar/rar"
# RAR 7.x can no longer create RAR4 (-ma4); plain4.rar needs the last 6.x release.
if [ ! -x "$TOOLS/rar6/rar/rar" ]; then
  mkdir -p "$TOOLS/rar6"
  curl -sSfL -o "$TOOLS/rar6.tar.gz" https://www.rarlab.com/rar/rarmacos-arm-624.tar.gz
  tar -xzf "$TOOLS/rar6.tar.gz" -C "$TOOLS/rar6"
fi
RAR4="$TOOLS/rar6/rar/rar"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
SRC="$WORK/src"
mkdir -p "$SRC/belgeler/alt" "$SRC/türkçe ğüşıöç"
printf 'rapor icerigi\n' > "$SRC/belgeler/rapor.txt"
printf 'notlar\n' > "$SRC/belgeler/alt/notlar.txt"
printf 'emoji\n' > "$SRC/türkçe ğüşıöç/😀.txt"
printf 'kok\n' > "$SRC/kok.txt"

# Fixed pseudo-random 20 KB (AES-CTR keystream with a constant key), so the bytes
# are identical on every run. The tests compare against this stored copy.
KEY=000102030405060708090a0b0c0d0e0f
openssl enc -aes-128-ctr -K "$KEY" -iv 00000000000000000000000000000000 \
  < <(head -c 20480 /dev/zero) > "$ROOT/src-tauri/tests/buyuk.bin"

rm -f "$OUT"/*.rar
cd "$SRC"
Q="-idq -y"

"$RAR" a $Q -ma5 "$OUT/plain5.rar" belgeler "türkçe ğüşıöç" kok.txt
"$RAR4" a $Q -ma4 "$OUT/plain4.rar" belgeler "türkçe ğüşıöç" kok.txt
"$RAR" a $Q -ma5 "-p$PW" "$OUT/enc.rar" belgeler "türkçe ğüşıöç" kok.txt
"$RAR" a $Q -ma5 "-hp$PW" "$OUT/enc-headers.rar" belgeler "türkçe ğüşıöç" kok.txt
"$RAR" a $Q -ma5 -s "$OUT/solid.rar" belgeler "türkçe ğüşıöç" kok.txt
printf 'yorum' > "$WORK/comment.txt"
"$RAR" a $Q -ma5 "-z$WORK/comment.txt" "$OUT/comment.rar" belgeler "türkçe ğüşıöç" kok.txt
"$RAR" a $Q -ma5 -rr5 "$OUT/recovery.rar" belgeler "türkçe ğüşıöç" kok.txt
"$RAR" a $Q -ma5 "$OUT/locked.rar" belgeler "türkçe ğüşıöç" kok.txt
"$RAR" k $Q "$OUT/locked.rar"

# multi-volume: 20 KB random file plus kok.txt; -v12k gives exactly two volumes (-v5k gives five)
cp "$ROOT/src-tauri/tests/buyuk.bin" "$SRC/buyuk.bin"
"$RAR" a $Q -ma5 -v12k "$OUT/multi.rar" buyuk.bin kok.txt
rm "$SRC/buyuk.bin"

# symlink: l -> /tmp stored as a link entry
LNK="$WORK/lnk"
mkdir -p "$LNK"
printf 'kok\n' > "$LNK/kok.txt"
ln -s /tmp "$LNK/l"
( cd "$LNK" && "$RAR" a $Q -ma5 -ol "$OUT/symlink.rar" kok.txt l )

# corrupt: plain5.rar with one byte flipped inside a file's payload (headers intact),
# so extraction fails the CRC check. Small files are stored, so the text is findable.
cp "$OUT/plain5.rar" "$OUT/corrupt.rar"
python3 -I - "$OUT/corrupt.rar" <<'PY'
import sys
p = sys.argv[1]
b = bytearray(open(p, "rb").read())
i = bytes(b).find(b"rapor icerigi\n")
assert i > 0, "payload not found"
b[i + 2] ^= 0xFF
open(p, "wb").write(b)
PY

ls "$OUT"
