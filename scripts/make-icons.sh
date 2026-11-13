#!/usr/bin/env bash
# Build the app icons from one master PNG.
#
#   scripts/make-icons.sh [master.png]
#
# Reads docs/images/icon-original.png by default (square, at least 512x512,
# transparent background) and writes:
#   crates/rsyncstat-app/assets/icon.png   256 px, embedded in the window icon
#   docs/images/rsyncstat-logo.png         512 px, shown in the README
#   crates/rsyncstat-app/assets/icon.icns  macOS (needs macOS: iconutil)
#   crates/rsyncstat-app/assets/icon.ico   Windows, 16 to 256 px
#
# Needs python3. Resizing uses sips (macOS) or ImageMagick (magick/convert).
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
master="${1:-$root/docs/images/icon-original.png}"
assets="$root/crates/rsyncstat-app/assets"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

[ -f "$master" ] || { echo "error: master PNG not found: $master" >&2; exit 1; }
command -v python3 >/dev/null || { echo "error: python3 is required" >&2; exit 1; }

# resize <px> <out.png>: square resize that keeps the alpha channel.
if command -v sips >/dev/null; then
  resize() { sips -z "$1" "$1" "$master" --out "$2" >/dev/null; }
elif command -v magick >/dev/null; then
  resize() { magick "$master" -resize "${1}x${1}" "$2"; }
elif command -v convert >/dev/null; then
  resize() { convert "$master" -resize "${1}x${1}" "$2"; }
else
  echo "error: need sips (macOS) or ImageMagick (magick/convert)" >&2
  exit 1
fi

size="$(python3 - "$master" <<'PY'
import struct, sys
with open(sys.argv[1], "rb") as f:
    head = f.read(24)
if head[:8] != b"\x89PNG\r\n\x1a\n":
    sys.exit("not a PNG file")
w, h = struct.unpack(">II", head[16:24])
print(f"{w} {h}")
PY
)"
read -r w h <<<"$size"
if [ "$w" != "$h" ] || [ "$w" -lt 512 ]; then
  echo "error: master must be square and at least 512x512 (got ${w}x${h})" >&2
  exit 1
fi

mkdir -p "$assets"

echo "PNG  icon.png (256) and rsyncstat-logo.png (512)"
resize 256 "$assets/icon.png"
resize 512 "$root/docs/images/rsyncstat-logo.png"

echo "ICO  icon.ico"
for px in 16 24 32 48 64 128 256; do
  resize "$px" "$work/ico_$px.png"
done
python3 - "$work" "$assets/icon.ico" <<'PY'
# An ICO is a small header plus directory pointing at PNG images.
import struct, sys
work, out = sys.argv[1], sys.argv[2]
sizes = [16, 24, 32, 48, 64, 128, 256]
images = [open(f"{work}/ico_{s}.png", "rb").read() for s in sizes]
header = struct.pack("<HHH", 0, 1, len(sizes))
offset = 6 + 16 * len(sizes)
entries = b""
for s, data in zip(sizes, images):
    dim = 0 if s == 256 else s  # 0 means 256 in the ICO format
    entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
    offset += len(data)
with open(out, "wb") as f:
    f.write(header + entries + b"".join(images))
PY

if command -v iconutil >/dev/null; then
  echo "ICNS icon.icns"
  set="$work/AppIcon.iconset"
  mkdir -p "$set"
  for base in 16 32 128 256 512; do
    resize "$base" "$set/icon_${base}x${base}.png"
    resize "$((base * 2))" "$set/icon_${base}x${base}@2x.png"
  done
  iconutil -c icns "$set" -o "$assets/icon.icns"
else
  echo "ICNS skipped: iconutil is only available on macOS"
fi

echo "done"
