#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SOURCE_ICON="$ROOT_DIR/assets/icons/org.rscribus.RScribus.svg"
OUTPUT_ROOT="${1:-$ROOT_DIR/build/icons}"
ICON_NAME="org.rscribus.RScribus"
SIZES=(16 32 48 64 128 256 512)

if ! command -v rsvg-convert >/dev/null 2>&1; then
    echo "rsvg-convert is required to render PNG icons." >&2
    exit 1
fi

if [[ ! -f "$SOURCE_ICON" ]]; then
    echo "Source icon not found: $SOURCE_ICON" >&2
    exit 1
fi

for size in "${SIZES[@]}"; do
    dir="$OUTPUT_ROOT/hicolor/${size}x${size}/apps"
    mkdir -p "$dir"
    rsvg-convert \
        --width="$size" \
        --height="$size" \
        "$SOURCE_ICON" \
        --output="$dir/$ICON_NAME.png"
done

scalable_dir="$OUTPUT_ROOT/hicolor/scalable/apps"
mkdir -p "$scalable_dir"
cp "$SOURCE_ICON" "$scalable_dir/$ICON_NAME.svg"

echo "Rendered icons under $OUTPUT_ROOT"
