# Sumi

軽量な手書きノートアプリ for Linux。GTK4 + libadwaita + Poppler でネイティブ実装しています。

## 機能

- **無限キャンバス** — ページの制約なしで手書き
- **ページノート** — A4 縦のページを追加しながら手書き
- **PDF ノート** — PDF を取り込み、ページの上に手書き注釈
- ペン / マーカー / 消しゴム / 選択 / 手のひらツール
- 選択ツールでストロークを範囲選択・移動・コピー / カット / ペースト / 削除
- 筆圧対応（スタイラス）
- スタイラスのペン先ボタン / 消しゴム（ペン尻）対応
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

バージョンとビルド元のコミットを確認する場合:

```sh
./target/release/sumi --version
# sumi 0.1.0 (84e1cf9)
```

### Linux ARM (aarch64)

GTK / Poppler をシステムライブラリとしてリンクするため、aarch64 マシン上でネイティブにビルドするのが簡単です。

```sh
# Arch Linux ARM の例
sudo pacman -S --needed base-devel rust gtk4 libadwaita poppler-glib cairo pkgconf
cargo build --release
```

クロスコンパイルする場合は aarch64 用の sysroot が必要です（例: `cross` + `aarch64-unknown-linux-gnu` 用の GTK/Poppler 開発パッケージ）。手元の x86_64 バイナリはそのまま ARM では動きません。

## 更新

```sh
./build.sh
```

`git pull`、リリースビルド、バージョン確認、`install.sh` をまとめて実行します。

## インストール

```sh
./install.sh
```

`~/.local/bin/sumi` にバイナリ、`~/.local/share/applications/` にデスクトップエントリ、
`~/.local/share/icons/hicolor/scalable/apps/` にアイコンを配置します。

## ショートカット

| キー | 動作 |
| --- | --- |
| `1` / `2` / `3` / `4` / `5` | ペン / マーカー / 消しゴム / 手 / 選択 |
| `Ctrl+Z` | 元に戻す |
| `Ctrl+Shift+Z`, `Ctrl+Y` | やり直す |
| `Ctrl+C` / `Ctrl+X` / `Ctrl+V` | コピー / カット / ペースト |
| `Ctrl+A` | 現在のページのストロークを全選択（選択ツール時） |
| `Delete` / `BackSpace` | 選択したストロークを削除 |
| `Esc` | 選択を解除 / ツールホイールを閉じる |
| `Ctrl++` / `Ctrl+-` / `Ctrl+0` | ズームイン / アウト / リセット |
| `F` | 全体を表示 |
| `W` | ツールホイールを表示 |
| `Space`（押している間） | 一時的にパン |
| `PageUp` / `PageDown` | 前 / 次のページ |
| スタイラスの第1ボタン（押しながら） | 一時的に消しゴム |
| スタイラスの第2ボタン（押しながら） | 一時的に手のひら（移動） |
| スタイラスを裏返す | 一時的に消しゴム |
| Lenovo Tab Pen Plus のボタン（短押し） | 消しゴム / 元のツールを切り替え |
| Lenovo Tab Pen Plus のボタン（ダブルクリック） | ツールホイールを表示 |
| Lenovo Tab Pen Plus のボタン（長押し） | ツールホイールを表示 |

Lenovo Tab Pen Plus のボタンは Linux 上では `tbpen-buttons` ブリッジ経由で
`Ctrl+1` / `Ctrl+4` / `Ctrl+5` / `Ctrl+6` のキーとして届きます。
Sumi は `Ctrl+1` / `Ctrl+4` を「短押し（消しゴム ⇄ 直前のツール）」、
`Ctrl+5` をダブルクリック（ツールホイール）、`Ctrl+6` を長押し（ツールホイール）として扱います。
1回の短押しで複数のキーが届いても、500ms 以内の重複は1回の操作としてまとめます。

ツールホイールは `src/app.rs` の `ToolWheel::new()` 内の `entries` に項目を追加すると
自動で円形に配置されます。中央のボタン、外側のクリック、または `Esc` で閉じます。

ダブルクリックと長押しの判別はペン本体のファームウェアが行います。
長押し成立は約2.7秒です。

## 選択・コピー・移動

選択ツール（`5`）でストロークをタップすると選択できます。何もない場所から
ドラッグすると範囲選択になります（`Shift` で追加選択）。選択したストロークを
ドラッグすると移動でき、`Ctrl+C` / `Ctrl+X` / `Ctrl+V` でコピー・カット・
ペーストできます。ペーストしたストロークは選択状態になり、そのまま移動できます。

カット・削除・移動・ペーストはすべてアンドゥできます。

### ペンのボタンが反応しないとき

環境変数 `SUMI_DEBUG_INPUT=1` を付けるか、`--input-debug` を付けて起動すると、
ペン / キーボードの入力イベントを記録します。

```sh
SUMI_DEBUG_INPUT=1 ./target/release/sumi
# または
./target/release/sumi --input-debug
```

記録先:

- 端末の標準エラー出力
- `~/.local/share/sumi/input-debug.log`

ペンのボタンを押したときの `type` / `source` / `device` / `tool` / `button` を確認すると、
ボタンが Linux 側に届いているか、どんなボタン番号で届いているか分かります。

## データ

- ノート: `~/.local/share/sumi/sumi.db`（SQLite, WAL）
- 取り込んだ PDF: `~/.local/share/sumi/pdfs/`
- 設定は保存しません（テーマはシステム設定に追従）

バックアップは `~/.local/share/sumi/` ディレクトリごとコピーしてください。

## ライセンス

MIT
