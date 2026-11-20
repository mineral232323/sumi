#!/bin/sh
set -eu

cd "$(dirname "$0")"

BIN_DIR="${HOME}/.local/bin"
APP_DIR="${HOME}/.local/share/applications"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"

if [ ! -x target/release/sumi ]; then
    echo "target/release/sumi がありません。先に 'cargo build --release' を実行してください。" >&2
    exit 1
fi

install -d "$BIN_DIR" "$APP_DIR" "$ICON_DIR"
install -m 755 target/release/sumi "$BIN_DIR/sumi"
install -m 644 assets/com.ryouta.Sumi.desktop "$APP_DIR/com.ryouta.Sumi.desktop"
install -m 644 assets/icons/apps/com.ryouta.Sumi.svg "$ICON_DIR/com.ryouta.Sumi.svg"
# Remove the old icon name used by earlier releases.
rm -f "$ICON_DIR/sumi-app.svg"

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q "${HOME}/.local/share/icons/hicolor" 2>/dev/null || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q "$APP_DIR" 2>/dev/null || true
fi

echo "Sumi をインストールしました: $BIN_DIR/sumi"
echo "PATH に $BIN_DIR が含まれている場合は 'sumi' で起動できます。"
