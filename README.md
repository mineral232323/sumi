# Sumi

軽量な手書きノートアプリ for Linux。GTK4 + libadwaita + Poppler でネイティブ実装しています。

## 機能

- **無限キャンバス** — ページの制約なしで手書き
- **ページノート** — A4 縦のページを追加しながら手書き
- **PDF ノート** — PDF を取り込み、ページの上に手書き注釈
- ペン / マーカー / 消しゴム / 選択 / 手のひらツール
- **図形ツール** — 直線 / 四角形 / 円 / 三角形をドラッグで描画（色と太さはペンの設定、アンドゥ対応）
- 選択ツールでストロークを範囲選択・移動・コピー / カット / ペースト / 削除
- 選択したストロークの色をパレットから変更（ペン / マーカー別）
- 筆圧対応（スタイラス）
- スタイラスのペン先ボタン / 消しゴム（ペン尻）対応
- 色パレット、ツール別サイズ（全体設定として保存され、次のノート・ページ・起動でも維持）、アンドゥ / リドゥ
- ピンチズーム、ホイールズーム、ページ移動、全体表示
- ライト / ダークテーマ追従
- SQLite にページ単位でローカル保存（60秒ごとの自動保存 + `Ctrl+S`）

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

### リポジトリから

```sh
./install.sh
```

`~/.local/bin/sumi` にバイナリ、`~/.local/share/applications/` にデスクトップエントリ、
`~/.local/share/icons/hicolor/scalable/apps/` にアイコンを配置します。

### curl で一発

Rust と GTK / Poppler の開発パッケージが入っていれば、リポジトリを一時ディレクトリに取得して
ビルド・インストールまで行えます。

```sh
curl -fsSL https://raw.githubusercontent.com/mineral232323/sumi/main/get.sh | sh
```

別のブランチやタグを使う場合:

```sh
curl -fsSL https://raw.githubusercontent.com/mineral232323/sumi/main/get.sh \
  | SUMI_REF=v0.1.0 sh
```

### GNOME のアプリ一覧に出ないとき

- `sudo ./install.sh` で入れていないか確認してください。`sudo` だと `/root/.local` に入り、GNOME のアプリ一覧には出ません
- `XDG_DATA_HOME` を設定している場合は、その配下にデスクトップエントリとアイコンが入ります
- インストール後に一度ログアウトして再ログインしてください（GNOME Shell のアプリ一覧キャッシュが残ることがあります）
- 次のコマンドで起動できるか確認できます

```sh
ls ~/.local/share/applications/com.ryouta.Sumi.desktop
desktop-file-validate ~/.local/share/applications/com.ryouta.Sumi.desktop
gtk-launch com.ryouta.Sumi
```

## ショートカット

| キー | 動作 |
| --- | --- |
| `1` / `2` / `3` / `4` / `5` / `6` | ペン / マーカー / 消しゴム / 手 / 選択 / 図形 |
| `Ctrl+Z` | 元に戻す |
| `Ctrl+Shift+Z`, `Ctrl+Y` | やり直す |
| `Ctrl+S` | いますぐ保存 |
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
- 保存: 変更は60秒ごとに自動保存。`Ctrl+S`、ヘッダーの保存ボタン、またはメニューの「保存」で即時保存
- 保存はページ単位の差分のみ。ページをまたぐ変更でも、変更されたページだけを書き込みます
- 強制終了や電源断では、最大60秒分の変更が失われる可能性があります（ノート切替・ウィンドウを閉じる時は即時保存）
- 取り込んだ PDF: `~/.local/share/sumi/pdfs/`
- 設定は保存しません（テーマはシステム設定に追従）

バックアップは `~/.local/share/sumi/` ディレクトリごとコピーしてください。

## ライセンス

MIT
