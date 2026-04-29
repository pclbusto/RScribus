#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ICON_NAME="org.rscribus.RScribus"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}"
ICON_THEME_DIR="$APP_DIR/icons/hicolor"
APPLICATIONS_DIR="$APP_DIR/applications"
BUILD_DIR="$ROOT_DIR/build/icons"
DESKTOP_TEMPLATE="$ROOT_DIR/dist/$ICON_NAME.desktop"
DESKTOP_TARGET="$APPLICATIONS_DIR/$ICON_NAME.desktop"
EXECUTABLE_PATH="${1:-RScribus}"
SIZES=(16 32 48 64 128 256 512)

"$ROOT_DIR/scripts/render-icons.sh" "$BUILD_DIR"

mkdir -p "$APPLICATIONS_DIR"

for size in "${SIZES[@]}"; do
    src="$BUILD_DIR/hicolor/${size}x${size}/apps/$ICON_NAME.png"
    dst_dir="$ICON_THEME_DIR/${size}x${size}/apps"
    mkdir -p "$dst_dir"
    cp "$src" "$dst_dir/$ICON_NAME.png"
done

mkdir -p "$ICON_THEME_DIR/scalable/apps"
cp "$BUILD_DIR/hicolor/scalable/apps/$ICON_NAME.svg" \
   "$ICON_THEME_DIR/scalable/apps/$ICON_NAME.svg"

sed "s|^Exec=.*$|Exec=$EXECUTABLE_PATH|" "$DESKTOP_TEMPLATE" > "$DESKTOP_TARGET"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$APPLICATIONS_DIR" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "$ICON_THEME_DIR" >/dev/null 2>&1 || true
fi

echo "Installed desktop entry: $DESKTOP_TARGET"
echo "Installed icons under: $ICON_THEME_DIR"
