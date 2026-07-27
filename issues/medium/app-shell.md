# medium — ravel-app / ravel-ui（シェル・パネル・状態管理）

---

## MED-APP-01 | bug | 分離パネルの OS ウィンドウを閉じるとシェルが desync、`reattach_window` は dead API

**該当**: `crates/ravel-app/src/workspace.rs:566-605`, `crates/ravel-ui/src/shell.rs:132-145`

`AppShell::reattach_window` は「分離 OS ウィンドウがユーザーに閉じられたときホストが呼ぶ」と
文書化されているが呼び出し元がゼロ。`open_detached` はクローズハンドラを登録しない。

タイトルバーで分離ウィンドウを閉じると、そのパネルはどこにも表示されなくなり
（メインドック内では hidden、ウィンドウは消滅）、`DetachedWindowHandles` に stale ハンドルが残り、
シェルは分離状態のままになる。復帰手段は Cmd+Shift+R（「最後に分離したパネル」へのフォールバック）だけ。

**修正方針**: 分離ウィンドウに `on_window_should_close` を登録し、
`shell.reattach_window(id)` を呼んでパネルをドックへ復帰させる。

---

## MED-APP-02 | bug | タイムライン終端の自動一時停止が publish されない（再生ボタンが戻らず、音声も止まらない）

**該当**: `crates/ravel-app/src/playback.rs:220-236`, `:437-472`

通常のティック間隔では最終フレームが `playing=true` で publish される。
次のティックで `frame_from` 内部が自動一時停止するが、フレームが変わらないため
`tick_with` が `None` を返し、`publish` / `forward_transport(false)` が走らない。
再生 / 一時停止アイコンは「再生中」のまま（notify されない）、
音声エンジンには Pause が送られない。
（一時停止が publish されるのはフレームがまだ動く late-tick 経路のみ。）

**修正方針**: フレーム移動が無くても `is_playing()` が false に遷移した時点で
更新を emit する（またはティックループで明示的に publish / forward する）。

---

## MED-APP-03 | bug | ノードエディタのドラッグが `pressed_button` を確認せず、Escape / ボタン喪失の復帰もない

**該当**: `crates/ravel-app/src/panels/node_editor.rs:1868-1954`

`DragMode::Pan/MoveNodes/Connect/SelectBox` がボタン状態に関係なく全マウス移動で適用される。
キャンバス外でマウスアップするとドラッグが armed のまま残り、
ボタンを押していない状態で再入するとパン / 移動 / ラバーバンドが続く。
Viewer と Timeline は同じ問題に対する防御を持つ
（`viewer.rs:1741-1747`, `timeline.rs:3464-3467`）。
ノードエディタにはどのドラッグにも Escape キャンセルが無い。

**修正方針**: `event.pressed_button != Some(Left)` のとき `drag = DragMode::None` にリセット。
`node_origins` を復元する Escape キャンセルを追加。

---

## MED-APP-04 | bug | Timeline のレイヤーヘッダクリックが stale なキーフレーム選択を残し、Delete を横取りする

**該当**: `crates/ravel-app/src/panels/timeline.rs:3084-3101`（対比 `:3977-3983`, `:958-967`）

バークリックは「Delete がレイヤーを対象にし続けるように」`selected_keyframes` をクリアするが、
ヘッダクリックはしない。
レイヤー A のキーフレームを選択 → レイヤー B のヘッダをクリック → Delete で、
レイヤー B ではなく A のキーフレーム（折りたたまれた行にあり不可視の可能性）が削除される。

**修正方針**: ヘッダ選択経路でも `selected_keyframes` をクリアする。

---

## MED-APP-05 | bug | Viewer が `SelectedPropertiesTarget` を無条件に上書きし、自分の所有でない Layer ターゲットを消す

**該当**: `crates/ravel-app/src/panels/viewer.rs:371-387`, `:673-687`

`NodeEditorPanel::notify_properties_selection` はターゲット所有権を尊重する
（ノード選択が空のとき自分の `Nodes` ターゲットのみ取り下げる）が、
Viewer の `publish_selection` は無条件に `Empty` を設定する。
Timeline でレイヤーを選択 → Select ツールで空キャンバスをクリックすると、
レイヤーはまだ選択されているのに Layer プロパティが空になる。

**修正方針**: ノードエディタと同じガードを適用。
2パネルが分岐したコピーを持っているので、共有の publish ヘルパーに抽出する。

---

## MED-APP-06 | bug | `prune_media_selection` が無関係な対象から Properties ターゲットを奪う

**該当**: `crates/ravel-app/src/panels/mod.rs:182-210`

`set_media_selection` が `SelectedPropertiesTarget` を無条件に上書きし、
`prune_media_selection` はドキュメント変更ごとに走る。
レイヤーを検査中に、以前選択したメディアアセットを削除する undo が入ると、
Properties パネルが強制的に `Empty` / `MediaAsset` にリセットされる。
レイヤー側の prune 経路には明示的な所有権ガードがある
（`properties_shows_layer_selection`, `mod.rs:416-453`）が、メディア側に相当物が無い。

**修正方針**: 選択グローバルを直接 prune し、
現在のターゲットが既に `MediaAsset` の場合のみターゲットを再 publish する。

---

## MED-APP-07 | bug | Timeline のバードラッグが no-op の undo ステップを記録する

**該当**: `crates/ravel-app/src/panels/timeline.rs:1323-1428`, `:1655-1698`

`MoveKeyframe` / `GraphKeyframes` はデルタ 0 で早期 return するが、
MoveBar / TrimIn / TrimOut / Reorder はしない。
バー上のクリック + 1px のぶれで `changed: true` になり `drag_ended` が無条件にコミットする
（`UndoStack::push` は重複排除しない）。
Ctrl+Z が見た目上何も起こさなくなり、ゴミステップが 200 件上限から実履歴を追い出す。

**修正方針**: キーフレーム側のガードを踏襲する
（フレームデルタ 0 の間は apply をスキップ。Reorder は最終インデックスを起点と比較）。

---

## MED-APP-08 | bug | MediaBin のサムネイルがアセット ID キーのため File ▸ Open を越えて stale になる

**該当**: `crates/ravel-app/src/panels/media_bin.rs:176-178`, `:214-218`

`thumb_images` はアセット ID キーでプロジェクト差し替えを越えて生存する。
ID はファイル名 stem 由来なので、同名アセット（`clip`）を含む別プロジェクトを開くと
前プロジェクトのサムネイルが永久に表示される。
`AudioService` はこの ID 再利用ケースを generation カウンタで防いでいるが、
サムネイルマップには無い。`ThumbnailCache::invalidate` は production 呼び出し元がゼロ。

**修正方針**: ドキュメント差し替え時に `thumb_images` をクリアする
（`AudioService::on_document_replaced` と同じフックを使う）。
または解決済みパスでキーにする。

---

## MED-APP-09 | bug | 音声トラック構築がライブ編集ごとに UI スレッドで無制限の作業を行う

**該当**: `crates/ravel-app/src/audio/mixdown.rs:213-219`, `:255-272`,
`crates/ravel-app/src/audio/mod.rs:188-295`

`AudioService::sync` はドキュメント observer から UI スレッド上で、
ライブジェスチャーを含む全編集で走る。
トリムハンドルのドラッグはマウス移動ごとにビルドキーを変えるため、
移動1回あたり最大約 128MiB のサンプル memcpy と、サンプル単位のゲインカーブ評価
（48kHz の数分 = 数百万回のチャンネル評価）が発生する。
リポジトリ自身の「UI スレッド外で行う」ルール違反。

**修正方針**: `build_track` を generation ガード付きでバックグラウンドエグゼキュータへ移す。
またはコミット時のみ再構築し、ジェスチャー中はデバウンスする。

**関連**: [HIGH-15](../high/HIGH-15-settrack-resamples-on-prep-thread.md)（エンジン側の同種問題）

---

## MED-APP-10 | debt | 設定レイヤー全体が永続化されるが一切適用されない — 日本語ロケールが到達不能

**該当**: `crates/ravel-app/src/project/settings.rs`,
`crates/ravel-app/src/project/mod.rs:319-333`, `crates/ravel-app/src/main.rs:49`

`settings.toml`（ロケール、OCIO カラー設定、プロキシ再生、オートセーブ有効 / 間隔）は
モデル化・マージ・全プロジェクトへの書き出しまで実装されているが、
`resolved_settings` に production 呼び出し元が無い。
オートセーブタスクは存在せず、OCIO / プロキシの消費側も存在せず、
`ravel_i18n::set_locale` はどこからも呼ばれない。
アプリは `init(dir, "en")` をハードコードしているため、
完全にメンテされている `ja.toml`（235キー）をユーザー操作で有効化する手段が無い。

**修正方針**: 解決済み設定を配線する（ユーザー価値のある locale とオートセーブから着手）。
または消費側ができるまで dead フィールドを削る。

---

## MED-APP-11 | debt | クラッシュ復旧ジャーナルが core に存在するが完全に未配線

**該当**: `crates/ravel-core/src/undo/{journal,recovery}.rs`（`crates/ravel-app` に呼び出し元なし）

ジャーナルの writer / reader と `recover()` のリプレイ機構は実装・テスト済みだが、
アプリは編集時にジャーナルを書かず、起動時に復旧も試みない。
オートセーブ無し（MED-APP-10）+ 保存失敗が不可視
（[CRIT-02](../critical/CRIT-02-save-failure-invisible-and-swallows-quit.md)）と組み合わせると、
クラッシュ時に最後の手動保存以降の作業がすべて失われる。

**修正方針**: `DocumentStore` のコミットにジャーナル書き込みを配線し、起動時に復旧プロンプトを出す。
より安価な暫定策としてオートセーブを先に入れる。

**関連**: [medium/core-evaluator.md](core-evaluator.md) の MED-CORE-08（core 側から見た同じ問題と設計上の障害）

---

## MED-APP-12 | bug | GPU コンテキスト初期化が起動時に panic、エラーダイアログ無し

**該当**: `crates/ravel-app/src/project_state.rs:184`

`GpuContext::new_blocking().expect("GPU context initialization failed")` —
wgpu がアダプタを得られないマシン / ドライバでは毎回の起動でクラッシュする。
同ファイルのメインウィンドウ失敗経路（`main.rs:101-105` はログ出力して正常終了）と不整合。

**修正方針**: エラーを伝播させて致命的エラーダイアログを表示する
（またはウィンドウ経路と同様に log-and-quit）。

---

## MED-APP-13 | debt | Timeline の行レイアウト走査が4箇所に手動で複製され、チャンネル数のソースが2種類ある

**該当**: `crates/ravel-app/src/panels/timeline.rs:1744`, `:2501`, `:2535`, `:2585-2723`

`keyframes_in_rect`、`row_at_content_y_in`、`total_layer_height`、描画コードが
それぞれ行 / チャンネルの y レイアウトを再導出している。
2つは `row.channel_names.len()`、2つは `row_channels(...)` を使う。
現在一致しているのは ravel-ui `keyframes.rs` の構築の仕方に依存した偶然であり、
乖離すればその行以下すべてでヒットテストと描画が無言でずれる。

**修正方針**: 描画・ヒットテスト・ラバーバンド・高さ計算を駆動する
単一の `(RowHit, y_range)` イテレータを抽出する。

---

## MED-APP-14 | debt | NodeEditorPanel がプロジェクトのレジストリではなく自前の `NodeRegistry` を作る

**該当**: `crates/ravel-app/src/panels/node_editor.rs:474`, `:497-498`

パネルは自分のレジストリに `register_builtins` するが、
authoritative なレジストリは `ProjectState` が所有している（Viewer は `project.registry()` を使う）。
プロジェクトレジストリにのみ登録されたものは Add Node メニュー、`param_range` のクランプ、
カテゴリ色から欠落する。2つが無言で乖離しうる。

**修正方針**: レジストリを `ProjectState` から解決し、プロジェクトが無い場合のみ builtins にフォールバック。

---

## MED-APP-15 | debt | Hand / Zoom ツールが機能しない dead UI

**該当**: `crates/ravel-app/src/panels/viewer.rs:1242-1249`, `:1795-1815`

ツールバーは Hand / Zoom を提供し 'H' 押下で Hand に切り替わるが、
Hand の左ドラッグパンも Zoom のクリックズームもハンドラが存在しない
（中ボタンドラッグのみがパンし、それはどのツールでも動く）。
選択すると左ボタン編集が無効化されるだけ。

**修正方針**: Hand は左ドラッグをパン経路へ、Zoom はクリック / alt+クリックを `zoom_toward` へ
ルーティングする。または実装まではツールを外す。
