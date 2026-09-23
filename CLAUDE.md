# CLAUDE.md

> このファイルはプロジェクトの進行に合わせて Claude Code 自身が更新していく「生きたドキュメント」です。
> 新規プロジェクト開始時はほぼ空の状態で構いません。実装が進むにつれて、下記のルールに従って自動的に育てていってください。

---

## Claude Codeへの指示（自己更新ルール）

あなたはこのプロジェクトで作業するたびに、以下のルールに従って本ファイル（CLAUDE.md）を更新してください。

### 更新すべきタイミング
- 新しいファイル/モジュール/ディレクトリを作成したとき
- 既存のアーキテクチャや設計方針を変更したとき（例: ライブラリの乗り換え、レイヤー構成の変更）
- 「今後も同じ説明をユーザーにさせそうだ」と感じたとき
- ユーザーから明示的に「CLAUDE.mdに書いて」と言われたとき

### 更新時の振る舞い
- 該当する章（下記「## 構成」参照）に、簡潔な箇条書きで追記する。長文の説明文は書かない。
- 「Critical Paths（ファイル所在表）」は特に優先して最新化する。何かを追加/変更したら、まずここを疑う。
- 迷ったら、ユーザーに聞かずにいったん追記し、後で「CLAUDE.mdのここを追記しました」と一言報告する。
- 冗長な履歴は残さない。古い情報は書き換える（変更履歴セクション以外は追記ではなく上書き優先）。
- コード規約・設計判断の「理由」も一言添える（例: 「認証は express-session を採用（JWT不要な単一サーバー構成のため）」）。

### 書かないこと
- 実装の詳細なロジック説明（コード自体やdocstringに書くべきもの）
- 一時的なタスクの進捗（TODOリストや作業ログはここに書かない）
- 憶測・未確定の設計（決まったことだけを書く）

---

## 構成

### 1. プロジェクト概要

- 目的: Windows 11 と Manjaro KDE (Linux) で同じ挙動をする、単一ウィンドウの画像ビュワー。
- 想定ユーザー: ファイル関連付けから画像を次々に見る個人ユーザー（単一ユーザー・単一マシン）。
- 仕様の原典は `imageviewer_instructions.md`。仕様変更はまずそちらを直す。

### 2. 技術スタック

- 言語: Rust 2021 edition / rust-version 1.90（`std::fs::File::try_lock` を使うため 1.89 以上が必須）
- GUI: `gpui-kit 0.6`。gpui 本体・`gpui-component`・既定アイコンを 1 クレートで再エクスポートする公式の入口で、
  `gpui` と `gpui-component` のバージョン不整合が起きないため採用。`use gpui_kit::*;` が gpui、
  `gpui_kit::component` が gpui-component。
- 画像デコード: `image 0.25`（`png` / `jpeg` フィーチャのみ）。gpui 自身と同じメジャー版なので
  `image::Frame` が `gpui::RenderImage` にそのまま渡せる。
- 設定: `toml 0.9` + `serde`（読み込みのみ。書き出しは手書き。理由は §4）
- ログ: `log 0.4` + 自前のファイルロガー + `chrono`（タイムスタンプ）
- IPC: Linux はロックファイル + Unix ドメインソケット（std のみ）、Windows は名前付き Mutex +
  名前付きパイプ（`windows-sys 0.60`）
- スレッド間の受け渡し: `async-channel 2`（IPC スレッド → gpui のフォアグラウンドタスク）
- DB: なし

### 3. ディレクトリ構造 / Critical Paths（最重要）

| やりたいこと | 場所 |
|---|---|
| 起動処理・引数処理・ウィンドウ生成・起動時の表示内容の決定 | `src/main.rs` |
| キー操作の追加／画面の描画／画像の切り替えロジック | `src/viewer.rs` |
| キーバインドの変更 | `src/viewer.rs` の `bind_keys()` |
| 対応画像フォーマットの追加 | `src/media/mod.rs` の `ImageFormat` に列挙子を追加し、`src/media/<形式>.rs` を作る |
| 画像のデコード処理そのもの | `src/media/png.rs` / `src/media/jpeg.rs`（共通部は `mod.rs` の `decode_as`） |
| フォルダ走査・次/前の画像・兄弟フォルダ探索 | `src/library.rs` |
| ファイル名の並び順 | `src/natural_sort.rs` |
| `config.toml` の項目追加・書式変更 | `src/config.rs` |
| ログの出力先・書式・フィルタ | `src/logging.rs` |
| `config.toml` / `viewer.log` の置き場所 | `src/app_paths.rs` |
| エラーをユーザーに見せる（メッセージボックス・パニックフック） | `src/report.rs` |
| 多重起動制御・既存ウィンドウへのパス受け渡し | `src/ipc/mod.rs`（共通 API）、`src/ipc/unix.rs`、`src/ipc/windows.rs` |
| テスト用の一時ディレクトリ・テスト画像の生成 | `src/test_support.rs`（`#[cfg(test)]` のみ） |
| ファイル関連付けの手順書 | `docs/file-association.md` |

### 4. 設計方針・規約

- **OS 差はユーザーから見えないこと。** 多重起動制御は OS ごとに別実装だが、`src/ipc/mod.rs` の
  `Instance` / `Primary` / `Secondary` という 1 つの API に隠す。`#[cfg_attr(unix, path = ...)]` で
  実装を差し替えており、呼び出し側に `cfg` を書かない。
- **画像フォーマットは enum + trait。** `ImageFormat::decoder()` の `match` が網羅チェックになるので、
  列挙子を足すとコンパイルエラーで対応漏れが分かる。拡張子だけで形式を決め、ファイル内容は
  スニッフィングしない（`.png` と名乗る JPEG はエラーにする）。
- **ディスクの内容は毎回読み直す。** ビュワーが開いている間にファイルが消える前提で、
  次/前の操作のたびにフォルダを列挙し直す。
- **デコード失敗 = そのファイルは無い。** 壊れた画像は握りつぶして次へ進む（仕様 §8）。
  エラーは必ずログに残す。
- **`config.toml` の書き出しは手書き。** 仕様 §10 が `verbose` をコメントアウト済みのサンプルとして
  同梱することを求めており、シリアライザはコメントを出力できないため。読み込みは serde。
- **ログのフィルタ。** Error / Warn は全クレートから通すが、Debug 以下は `imageviewer` から出たものだけ。
  gpui の詳細ログで `viewer.log` が埋まるのを防ぐため。
- **テストモジュールで `use super::*` を書かない（`src/viewer.rs` など gpui を使うファイル）。**
  `gpui_kit::*` に gpui 独自の `test` マクロが含まれており、Rust の `#[test]` を隠して
  「recursion limit reached while expanding `#[test]`」になる。必要な項目だけ個別に import する。
- 命名は「何であるか」を綴る。`cx` は gpui のもの。
- 未実装機能（ズーム等）のための抽象化は先回りして作らない。`ViewerView::render_image()` に
  後から手を入れれば済む構造にとどめてある。

### 5. よく使うコマンド

```bash
cargo build                      # デバッグビルド（詳細ログが常に出る）
cargo build --release
cargo test
cargo clippy --all-targets
cargo run -- /path/to/image.png
```

### 6. 既知の制約・注意点

- **Windows は debug / release とも GUI サブシステム（コンソールなし）。** 標準出力は捨てられるので、
  ユーザーに見せるエラーは `src/report.rs`（ログ + メッセージボックス）経由で出す。
- **Windows の「プログラムから開く」は exe のファイル名だけで登録される。**
  `HKCU\Software\Classes\Applications\imageviewer.exe` のパスは別の場所の同名 exe を選んでも更新されない。
  「関連付けで開くと何も起きない」ときはまずここを疑う（手順は `docs/file-association.md`）。
- **X11 は `window_bounds()` がフルスクリーン中に復元サイズを返さない。** Wayland は
  `WindowBounds::Fullscreen(復元サイズ)` を返すが、X11 は常に `Windowed(現在のサイズ)` を返す。
  そのため `ViewerView` がウィンドウモード時の矩形を自分で覚えている（`windowed_bounds`）。
  ここを消すとフルスクリーンのまま終了したときに画面サイズが保存されてしまう。
- **終了時に gpui が `window not found` を ERROR で 1 行出す。** gpui 内部の
  `on_visibility_change` コールバックがウィンドウ破棄後に発火するためで、こちらの不具合ではない。
  `viewer.log` に毎回 1 行残る。
- 画像は 1 枚ずつ同期デコードする。巨大な画像では切り替え時に一瞬固まる。先読みはしていない。
- `config.toml` の書き出しは終了時（ウィンドウを閉じる／Esc／Ctrl+Q）のみ。強制終了では保存されない。
- 複数ファイルの同時オープンには対応しない。`.desktop` の `Exec` は `%F` ではなく `%f`。

### 7. 変更履歴

- 2026-09-23: 初回実装。`imageviewer_instructions.md` の全項目を実装。
- 2026-09-23: Windows 版を実機で確認。コンソール非表示を debug にも適用、関連付けの落とし穴を文書化。

---

**運用メモ（ユーザー向け）**
- ある程度育ってきたら、章ごとに `docs/` 配下へ分割してこのファイルからリンクする構成に移行することを検討してください（tududiのCLAUDE.mdのような形）。
- 定期的に「CLAUDE.mdを見直して、実態と合っていない箇所を修正して」とClaude Codeに依頼すると、陳腐化を防げます。
