#!/bin/sh
set -eu

REPO="https://github.com/mineral232323/sumi.git"
REF="${SUMI_REF:-main}"

die() {
    echo "エラー: $*" >&2
    exit 1
}

command -v git >/dev/null 2>&1 || die "git が必要です"
command -v cargo >/dev/null 2>&1 || die "Rust (cargo) が必要です。https://rustup.rs を参照してください"
command -v pkg-config >/dev/null 2>&1 || die "pkg-config が必要です"

missing=""
for package in gtk4 libadwaita-1 poppler-glib; do
    if ! pkg-config --exists "$package"; then
        missing="$missing $package"
    fi
done
if [ -n "$missing" ]; then
    cat >&2 <<EOF
開発パッケージが見つかりません:$missing

Arch / CachyOS:
  sudo pacman -S --needed base-devel rust gtk4 libadwaita poppler-glib cairo pkgconf

Debian / Ubuntu:
  sudo apt install build-essential rustc cargo libgtk-4-dev libadwaita-1-dev libpoppler-glib-dev libcairo2-dev pkg-config

Fedora:
  sudo dnf install gcc make rust cargo gtk4-devel libadwaita-devel poppler-glib-devel cairo-devel pkgconf
EOF
    exit 1
fi

tmp="$(mktemp -d "${TMPDIR:-/tmp}/sumi.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT INT TERM

echo "==> Sumi を取得します ($REF)"
git clone --depth 1 --branch "$REF" "$REPO" "$tmp/sumi"

cd "$tmp/sumi"
echo "==> リリースビルド"
cargo build --release

echo "==> インストール"
./install.sh
