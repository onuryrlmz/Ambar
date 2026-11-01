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

# Fixed modification times (2020-01-02 03:04:05 UTC = 1577934245) so tests can check them.
TZ=UTC find "$SRC" -exec touch -t 202001020304.05 {} +

# Fixed pseudo-random 20 KB (AES-CTR keystream with a constant key), so the bytes
# are identical on every run. The tests compare against this stored copy.
KEY=000102030405060708090a0b0c0d0e0f
openssl enc -aes-128-ctr -K "$KEY" -iv 00000000000000000000000000000000 \
  < <(head -c 20480 /dev/zero) > "$ROOT/src-tauri/tests/buyuk.bin"

rm -f "$OUT"/*.rar "$OUT"/*.r[0-9][0-9]
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
# old-style names (old.rar, old.r00, ...), and a set whose headers are encrypted
"$RAR4" a $Q -ma4 -m0 -v8k -vn "$OUT/old.rar" buyuk.bin kok.txt
"$RAR" a $Q -ma5 -v12k "-hp$PW" "$OUT/envol.rar" buyuk.bin kok.txt
rm "$SRC/buyuk.bin"

# plain entry first, then an encrypted one: a wrong password stops the run half way
printf 'acik\n' > "$SRC/acik.txt"
printf 'gizli\n' > "$SRC/gizli.txt"
"$RAR" a $Q -ma5 "$OUT/mixed.rar" acik.txt
"$RAR" a $Q -ma5 "-p$PW" "$OUT/mixed.rar" gizli.txt
rm "$SRC/acik.txt" "$SRC/gizli.txt"

# stored (-m0) incompressible file first, then a small one: for cancel-inside-a-file tests
ST="$WORK/st"
mkdir -p "$ST"
cp "$ROOT/src-tauri/tests/buyuk.bin" "$ST/buyuk.bin"
printf 'kok\n' > "$ST/kok.txt"
touch -t 202001020304.05 "$ST/buyuk.bin" "$ST/kok.txt"
( cd "$ST" && "$RAR" a $Q -ma5 -m0 "$OUT/stored.rar" buyuk.bin kok.txt )

# symlink: l -> /tmp stored as a link entry
LNK="$WORK/lnk"
mkdir -p "$LNK"
printf 'kok\n' > "$LNK/kok.txt"
ln -s /tmp "$LNK/l"
( cd "$LNK" && "$RAR" a $Q -ma5 -ol "$OUT/symlink.rar" kok.txt l )

# redirect entries that UnRAR would resolve against the process working directory:
# a hard link (-oh) and a file reference (-oi). The names are unique so the tests can
# plant a colliding file in the working directory.
HL="$WORK/hl"
mkdir -p "$HL"
printf 'hard\n' > "$HL/ambar-hl-src.txt"
ln "$HL/ambar-hl-src.txt" "$HL/ambar-hl-link.txt"
printf 'after\n' > "$HL/ambar-hl-after.txt"
( cd "$HL" && "$RAR" a $Q -ma5 -oh "$OUT/hardlink.rar" ambar-hl-src.txt ambar-hl-link.txt ambar-hl-after.txt )
RF="$WORK/rf"
mkdir -p "$RF"
printf 'identical content for a file reference\n' > "$RF/ambar-ref-a.txt"
cp "$RF/ambar-ref-a.txt" "$RF/ambar-ref-b.txt"
printf 'after\n' > "$RF/ambar-ref-after.txt"
( cd "$RF" && "$RAR" a $Q -ma5 -oi1:1 "$OUT/fileref.rar" ambar-ref-a.txt ambar-ref-b.txt ambar-ref-after.txt )

# disguised symlink: a/b/l -> ../../ESCAPED, with the mode bits patched to look like a
# regular file (header CRC fixed). Only the redirect record still says "symlink".
DG="$WORK/dg"
mkdir -p "$DG/a/b"
printf 'kok\n' > "$DG/kok.txt"
ln -s ../../ESCAPED "$DG/a/b/l"
( cd "$DG" && "$RAR" a $Q -ma5 -ol "$WORK/dg.rar" kok.txt a/b/l )
python3 -I - "$WORK/dg.rar" "$OUT/symlink-disguised.rar" <<'PY'
import sys, zlib
src, dst = sys.argv[1:3]
b = bytearray(open(src, "rb").read())
def vint(i):
    v = 0; s = 0
    while True:
        c = b[i]; v |= (c & 0x7f) << s; s += 7; i += 1
        if not c & 0x80:
            return v, i
patched = 0
i = 8
while i < len(b):
    crcpos = i
    size, p = vint(i + 4)
    hstart, hend = i + 4, p + size
    typ, q = vint(p)
    fl, q = vint(q)
    if fl & 1:
        _, q = vint(q)
    data = 0
    if fl & 2:
        data, q = vint(q)
    if typ == 2:
        _, q = vint(q)       # file flags
        _, q = vint(q)       # unpacked size
        a0 = q
        attr, q = vint(q)
        if attr & 0o170000 == 0o120000:
            width = q - a0
            v = (attr & 0o7777) | 0o100000
            enc = []
            for k in range(width):
                x = v & 0x7f; v >>= 7
                enc.append(x | (0x80 if k < width - 1 else 0))
            b[a0:q] = bytes(enc)
            b[crcpos:crcpos + 4] = zlib.crc32(bytes(b[hstart:hend])).to_bytes(4, "little")
            patched += 1
    if typ == 5:
        break
    i = hend + data
assert patched == 1, patched
open(dst, "wb").write(b)
PY

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
