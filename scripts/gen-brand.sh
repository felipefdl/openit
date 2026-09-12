#!/usr/bin/env bash
# Render assets/brand and assets/app-icons from assets/brand/openit.svg.
#
# openit.svg is the logo: the lens glyph on a black rounded tile. Everything
# else is derived from it:
#   openit-glyph.svg  the glyph alone, strokes in currentColor (tinted in-app)
#   openit-mark.png   the logo at 2048px (README)
#   app-icons/        icon.png (1024), 128x128@2x, 128x128, 32x32,
#                     icon.icns (iconutil), icon.ico (magick)
#
# Requires magick, resvg, python3, and iconutil (macOS).
#
# Usage:
#   just brand
#   scripts/gen-brand.sh

set -euo pipefail

MARK_SIZE=2048
ICON_SIZE=1024

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd "$script_dir/.." && pwd)
brand_dir="$repo_root/assets/brand"
icons_dir="$repo_root/assets/app-icons"
logo="$brand_dir/openit.svg"
glyph="$brand_dir/openit-glyph.svg"

if [[ $# -ne 0 ]]; then
  echo "usage: $0" >&2
  exit 2
fi

need() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "gen-brand: missing $1 (brew install imagemagick resvg)" >&2
    exit 1
  fi
}

need magick
need resvg
need python3
need iconutil

if [[ ! -f "$logo" ]]; then
  echo "gen-brand: missing $logo" >&2
  exit 1
fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

# Glyph: drop the tile rect, tint the strokes with currentColor.
python3 - "$logo" "$glyph" <<'PY'
import re
import sys
from pathlib import Path

src = Path(sys.argv[1]).read_text()
body, rects = re.subn(r'\s*<rect\b[^>]*/>', "", src, count=1)
if rects != 1:
  raise SystemExit("gen-brand: openit.svg has no tile <rect>")
body = re.sub(r'(stroke|fill)="#[Ff]{6}"', r'\1="currentColor"', body)
Path(sys.argv[2]).write_text(body)
PY

resvg -w "$MARK_SIZE" -h "$MARK_SIZE" "$logo" "$brand_dir/openit-mark.png"

mkdir -p "$icons_dir" "$work/openit.iconset"
tile="$work/icon-tile.png"
resvg -w "$ICON_SIZE" -h "$ICON_SIZE" "$logo" "$tile"
magick "$tile" PNG32:"$icons_dir/icon.png"
magick "$tile" -resize 256x256 PNG32:"$icons_dir/128x128@2x.png"
magick "$tile" -resize 128x128 PNG32:"$icons_dir/128x128.png"
magick "$tile" -resize 32x32 PNG32:"$icons_dir/32x32.png"
for size in 16 32 128 256 512; do
  double=$((size * 2))
  magick "$tile" -resize "${size}x${size}" PNG32:"$work/openit.iconset/icon_${size}x${size}.png"
  magick "$tile" -resize "${double}x${double}" PNG32:"$work/openit.iconset/icon_${size}x${size}@2x.png"
done
iconutil -c icns "$work/openit.iconset" -o "$icons_dir/icon.icns"
magick "$tile" -define icon:auto-resize=256,64,48,32,24,16 "$icons_dir/icon.ico"

echo "wrote $glyph"
echo "wrote $brand_dir/openit-mark.png"
echo "wrote $icons_dir/icon.png icon.icns icon.ico 128x128@2x.png 128x128.png 32x32.png"
