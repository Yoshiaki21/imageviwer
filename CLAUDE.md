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
- 乱数: `fastrand 2`（R キー・右クリックのランダム表示用。gpui の依存に既に含まれていて追加のビルドが要らないため）
- ゴミ箱: `trash 5`（Windows のごみ箱と freedesktop の Trash を同じ API で扱えるため。
  Windows では gpui が初期化済みの STA に合わせる既定の `coinit_apartmentthreaded` のまま使う）
- マウスの OS 依存値（ダブルクリック間隔・ホイール1ノッチの行数）: Windows は `windows-sys` の
  `GetDoubleClickTime` / `SPI_GETWHEELSCROLLLINES`、Linux は gpui の固定値（400ms / 3行）に合わせる
- IPC: Linux はロックファイル + Unix ドメインソケット（std のみ）、Windows は名前付き Mutex +
  名前付きパイプ（`windows-sys 0.60`）
- スレッド間の受け渡し: `async-channel 2`（IPC スレッド → gpui のフォアグラウンドタスク）
- 画像の別スレッドデコード: gpui の `background_executor().spawn()`（スレッドプールを自前で持たないため。
  結果は `spawn_in` + `update_in` で UI スレッドへ戻す）
- DB: なし

### 3. ディレクトリ構造 / Critical Paths（最重要）

| やりたいこと | 場所 |
|---|---|
| 起動処理・引数処理・ウィンドウ生成・起動時の表示内容の決定 | `src/main.rs` |
| キー操作の追加／画面の描画／画像の切り替えロジック | `src/viewer.rs` |
| キーバインドの変更 | `src/viewer.rs` の `bind_keys()` |
| ←→ のループ（折り返し）・Delete でゴミ箱へ移動・R（と 2・3 の右クリック）でランダム表示 | `src/viewer.rs` の `step_image()` / `on_delete_image()` / `show_random_image()`（巡回順は `wrapping_indices()`、ランダム順は `shuffled_others()`） |
| 同じフォルダ内の移動（候補順・待ち合わせ・欠損時の読み直し）／F5 | `src/viewer.rs` の `start_seek()` / `resolve()` / `on_refresh_listing()`（候補の順番は `candidates()` と `Pick`、欠損の判定は `media::is_missing()`） |
| 先読み（何を・どの順で・いくつ並行してデコードし、いつ捨てるか） | `src/prefetch.rs` の `Prefetcher` / `window_indices()`。デコードの起動と結果の受け取りは `viewer.rs` の `schedule_decodes()` / `on_decoded()` |
| ↑↓ で戻ったときの表示位置（フォルダごとの閲覧位置の記憶・上限・忘れる順番） | `src/folder_history.rs` の `FolderHistory`（`[folder_history]`）。記録は `viewer.rs` の `replace_content()` と `save_state()`、使うのは `step_folder()` と `open_file()`（`load_remembered()`） |
| 先読みの枚数・メモリ上限の設定 | `config.rs` の `PrefetchConfig`（`[prefetch]`） |
| GPU テクスチャの解放 | `viewer.rs` の `release_textures()`（`Prefetcher::take_released()` と `orphaned`） |
| マウス操作（4分割クリック・右クリック・ダブルクリック・ホイール）の割り当て | `src/viewer.rs` の `on_mouse_down()` / `click_area()` / `on_right_mouse_down()` / `on_scroll_wheel()` |
| クリック位置→エリアの判定、ダブルクリック間隔、ホイールのノッチ換算 | `src/pointer.rs` |
| フォルダ名・ファイル名の表示／「前（次）のフォルダはありません」等のお知らせ | `src/viewer.rs` の `show_caption()` / `display_caption()` / `render_caption()`（種類は `CaptionText`）、表示時間は `config.rs` の `OverlayConfig` |
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
- **一覧はフォルダに入ったときだけ読み、食い違いに気付いたら読み直す。** 同じフォルダ内の移動
  （←→・ホイール・クリック・R）は `Gallery::entries` を使い回す。毎回列挙するとファイル数に比例して遅くなり、
  キーの押しっぱなしで描画が止まったため（2,000 枚で 1 回約 50ms）。一覧のファイルが開けず
  NotFound だったら一覧を読み直してやり直すので、削除・移動は毎回読み直したのと同じに見える。
  壊れたファイルでは読み直さない（読み直しても変わらないため）。追加されたファイルは F5 まで見えない（仕様）。
- **同じフォルダ内の移動はデコードを待たない。** 移動は「候補の列（`Seek`）」として持ち、`resolve()` が
  デコード済みなら表示、未完了なら待つ（完了時に `on_decoded()` から再開）。UI スレッドでデコードすると
  1 枚がキーリピート間隔（約 33ms）を超えた時点で描画が止まるため。gpui（Windows）は溜まった入力を
  全部処理してから描画する。
- **カーソルと表示中の画像は別物。** `Gallery::index` は最後に移動した先、`Gallery::shown` は画面の画像。
  次の移動はカーソル基準（押しっぱなしで最新の移動先へ向かうため）、Delete と保存は `shown` 基準
  （見ている画像を消す・覚えるため）。
- **デコードは途中で止めない。** `image` クレートに中断手段が無いため。不要になった結果は `Prefetcher` が
  捨て、並行数の枠も完了まで確保したままにする（取り消すと実際の並行数が上限を超えるため）。
- **デコード済みの画像は表示直前にファイルのサイズ・更新日時を確認する（`FileStamp`）。** キャッシュから
  出すとファイルを開かないので、削除・上書きに気付けないため。
- **フォルダごとの閲覧位置はファイル名で覚え、無ければ先頭から。** 番号で覚えると F5・Delete で
  別の画像を指すため。起動時の復元（`[last_opened]` の番号＋最も近い画像）とは別の仕組みのまま（仕様 §8 を変えないため）。
  フォルダの比較は Windows だけ大文字・小文字を区別しない（関連付けと ↑↓ でパスの綴りが違いうるため）。
- **GPU テクスチャは、画面にもキャッシュにも無くなった画像だけ解放する。** キャッシュにある画像の
  テクスチャを残すと、戻ったときに再転送が要らない。
- **フォルダ列挙では `Path::is_file()` / `is_dir()` を使わない。** `DirEntry::file_type()` を使う
  （`library.rs` の `is_image()` / `is_folder()`）。前者はファイルごとに OS へ問い合わせるため、Windows で約 20 倍遅い。
- **デコード失敗 = そのファイルは無い。** 壊れた画像は握りつぶして次へ進む（仕様 §8）。
  エラーは必ずログに残す。
- **`config.toml` の書き出しは手書き。** 仕様 §10 が `verbose` をコメントアウト済みのサンプルとして
  同梱することを求めており、シリアライザはコメントを出力できないため。読み込みは serde。
- **ログのフィルタ。** Error / Warn は全クレートから通すが、Debug 以下は `imageviewer` から出たものだけ。
  gpui の詳細ログで `viewer.log` が埋まるのを防ぐため。
- **テストモジュールで `use super::*` を書かない（`src/viewer.rs` など gpui を使うファイル）。**
  `gpui_kit::*` に gpui 独自の `test` マクロが含まれており、Rust の `#[test]` を隠して
  「recursion limit reached while expanding `#[test]`」になる。必要な項目だけ個別に import する。
- **右クリックは待たずに即実行する。** 右ボタンにはダブルクリックの動作が無いため。
- **シングルクリックはダブルクリック間隔だけ遅らせて実行する。** ダブルクリックでフルスクリーンにしたとき
  画像まで移動しないため。待機中のクリックは `pending_click` の `Task` で持ち、drop でキャンセルする。
- **タイマーは gpui の `Task` をフィールドに保持し、上書き（drop）でキャンセルする。** 表示の消去も同じ方式。
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
- **Delete の失敗は `report::error` 経由。** Linux ではメッセージボックスが無く stderr にしか出ないため、
  KDE ではログ（`viewer.log`）でしか気付けない。
- 起動時・関連付け・↑↓ の最初の 1 枚は UI スレッドで同期デコードする（黒い画面を出さないため）。
  巨大な画像ではその瞬間だけ固まる。↑↓ を押しっぱなしにした場合も同様。
- 先読みのメモリは 1 枚＝幅×高さ×4 バイト。既定（先 4・前 2）で 2304×1792 なら約 115MB、
  上限は `[prefetch] memory_mb`。
- `config.toml` の書き出しは終了時（ウィンドウを閉じる／Esc／Ctrl+Q）のみ。強制終了では保存されない。
- 複数ファイルの同時オープンには対応しない。`.desktop` の `Exec` は `%F` ではなく `%f`。

### 7. 変更履歴

- 2026-09-23: 初回実装。`imageviewer_instructions.md` の全項目を実装。
- 2026-09-23: Windows 版を実機で確認。コンソール非表示を debug にも適用、関連付けの落とし穴を文書化。
- 2026-09-23: ←→ のループと Delete（ゴミ箱へ移動）を追加。
- 2026-09-23: マウス操作（4分割クリック・ダブルクリック・ホイール）とフォルダ名・ファイル名の一時表示を追加。
- 2026-09-23: 兄弟フォルダが無いときのお知らせ表示を追加。以上の追加機能を Windows 版の実機で確認。
- 2026-09-23: R キーと右クリック（2・3）でのランダム表示を追加。KDE と Windows 版の実機で確認。
- 2026-09-28: 大量のファイルがあるフォルダで押しっぱなしの表示が止まる問題を修正。フォルダ列挙の高速化、
  一覧の使い回し（欠損時のみ読み直し）、F5 での一覧の読み直しを追加。
- 2026-09-28: 大きい画像で押しっぱなしの表示が止まる問題を修正。同じフォルダ内の移動のデコードを
  別スレッド化し、前後の先読みとデコード済み画像のキャッシュ（`src/prefetch.rs`、`[prefetch]`）を追加。
  以上を Windows 版と KDE 版の実機で確認。
- 2026-10-10: ↑↓ で戻ったフォルダを前回見ていた画像から表示するよう変更（`src/folder_history.rs`、
  `[folder_history]`）。仕様 §4 の「移動先は先頭の画像」を改めた。KDE 版と Windows 版の実機で確認。

---

**運用メモ（ユーザー向け）**
- ある程度育ってきたら、章ごとに `docs/` 配下へ分割してこのファイルからリンクする構成に移行することを検討してください（tududiのCLAUDE.mdのような形）。
- 定期的に「CLAUDE.mdを見直して、実態と合っていない箇所を修正して」とClaude Codeに依頼すると、陳腐化を防げます。
