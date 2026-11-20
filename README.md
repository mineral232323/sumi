# Sumi

軽量な手書きノートアプリ for Linux。GTK4 + libadwaita + Poppler でネイティブ実装しています。

## 機能

- **無限キャンバス** — ページの制約なしで手書き
- **ページノート** — A4 縦のページを追加しながら手書き
- **PDF ノート** — PDF を取り込み、ページの上に手書き注釈
- ペン / マーカー / 消しゴム / 手のひらツール
- 筆圧対応（スタイラス）
- 色パレット、ツール別サイズ、アンドゥ / リドゥ
- ピンチズーム、ホイールズーム、ページ移動、全体表示
- ライト / ダークテーマ追従
- SQLite にローカル保存（`~/.local/share/sumi/sumi.db`）

## 必要なもの

Rust 1.92 以上と、以下のシステムライブラリ:

| ディストリ | パッケージ |
| --- | --- |
| Arch / CachyOS | `gtk4 libadwaita poppler-glib cairo pkgconf` |
| Debian / Ubuntu | `libgtk-4-dev libadwaita-1-dev libpoppler-glib-dev libcairo2-dev pkg-config` |
| Fedora | `gtk4-devel libadwaita-devel poppler-glib-devel cairo-devel pkgconf` |

## ビルド

```sh
cargo build --release
./target/release/sumi
```

### Linux ARM (aarch64)

GTK / Poppler をシステムライブラリとしてリンクするため、aarch64 マシン上でネイティブにビルドするのが簡単です。

```sh
# Arch Linux ARM の例
sudo pacman -S --needed base-devel rust gtk4 libadwaita poppler-glib cairo pkgconf
cargo build --release
```

クロスコンパイルする場合は aarch64 用の sysroot が必要です（例: `cross` + `aarch64-unknown-linux-gnu` 用の GTK/Poppler 開発パッケージ）。手元の x86_64 バイナリはそのまま ARM では動きません。

## インストール

```sh
./install.sh
```

`~/.local/bin/sumi` にバイナリ、`~/.local/share/applications/` にデスクトップエントリ、
`~/.local/share/icons/hicolor/scalable/apps/` にアイコンを配置します。

## ショートカット

| キー | 動作 |
| --- | --- |
| `1` / `2` / `3` / `4` | ペン / マーカー / 消しゴム / 手 |
| `Ctrl+Z` | 元に戻す |
| `Ctrl+Shift+Z`, `Ctrl+Y` | やり直す |
| `Ctrl++` / `Ctrl+-` / `Ctrl+0` | ズームイン / アウト / リセット |
| `F` | 全体を表示 |
| `Space`（押している間） | 一時的にパン |
| `PageUp` / `PageDown` | 前 / 次のページ |

## データ

- ノート: `~/.local/share/sumi/sumi.db`（SQLite, WAL）
- 取り込んだ PDF: `~/.local/share/sumi/pdfs/`
- 設定は保存しません（テーマはシステム設定に追従）

バックアップは `~/.local/share/sumi/` ディレクトリごとコピーしてください。

## ライセンス

MIT
