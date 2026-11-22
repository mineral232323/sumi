#!/bin/sh
set -eu

cd "$(dirname "$0")"

if [ "$(id -u)" -eq 0 ] && [ -n "${SUDO_USER:-}" ]; then
    echo "sudo ではなく、GNOME にログインしている通常ユーザーで実行してください。" >&2
    echo "sudo で入れると /root/.local に入り、アプリ一覧に出ません。" >&2
    exit 1
fi

DATA_HOME="${XDG_DATA_HOME:-${HOME}/.local/share}"
BIN_DIR="${HOME}/.local/bin"
APP_DIR="$DATA_HOME/applications"
ICON_ROOT="$DATA_HOME/icons/hicolor"
ICON_DIR="$ICON_ROOT/scalable/apps"

if [ ! -x target/release/sumi ]; then
    echo "target/release/sumi がありません。先に 'cargo build --release' を実行してください。" >&2
    exit 1
fi

install -d "$BIN_DIR" "$APP_DIR" "$ICON_DIR"
install -m 755 target/release/sumi "$BIN_DIR/sumi"

# Use an absolute Exec path so the entry still launches when the GNOME
# session does not have ~/.local/bin in PATH.
tmp_desktop="$(mktemp)"
trap 'rm -f "$tmp_desktop"' EXIT INT TERM
sed "s|^Exec=sumi$|Exec=\"$BIN_DIR/sumi\"|" assets/com.ryouta.Sumi.desktop > "$tmp_desktop"
install -m 644 "$tmp_desktop" "$APP_DIR/com.ryouta.Sumi.desktop"

install -m 644 assets/icons/apps/com.ryouta.Sumi.svg "$ICON_DIR/com.ryouta.Sumi.svg"
for size in 16 24 32 48 64 128 256 512; do
    size_dir="$ICON_ROOT/${size}x${size}/apps"
    install -d "$size_dir"
    install -m 644 "assets/icons/apps/png/com.ryouta.Sumi-$size.png" \
        "$size_dir/com.ryouta.Sumi.png"
    # Remove the old icon name used by earlier releases.
    rm -f "$size_dir/sumi-app.png"
done
rm -f "$ICON_DIR/sumi-app.svg"

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q "$ICON_ROOT" 2>/dev/null || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q "$APP_DIR" 2>/dev/null || true
fi
if command -v xdg-desktop-menu >/dev/null 2>&1; then
    xdg-desktop-menu forceupdate --mode user 2>/dev/null || true
fi

echo "Sumi をインストールしました: $BIN_DIR/sumi"
echo "デスクトップエントリ: $APP_DIR/com.ryouta.Sumi.desktop"
echo "見つからない場合は一度ログアウトして再ログインしてください。"
