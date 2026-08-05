# medium — ravel-app / ravel-ui（シェル・パネル・状態管理）

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

**関連**: [HIGH-15](../closed/HIGH-15-settrack-resamples-on-prep-thread.md)（エンジン側の同種問題）

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
（[CRIT-02](../closed/CRIT-02-save-failure-invisible-and-swallows-quit.md)）と組み合わせると、
クラッシュ時に最後の手動保存以降の作業がすべて失われる。

**修正方針**: `DocumentStore` のコミットにジャーナル書き込みを配線し、起動時に復旧プロンプトを出す。
より安価な暫定策としてオートセーブを先に入れる。

**関連**: [medium/core-evaluator.md](core-evaluator.md) の MED-CORE-08（core 側から見た同じ問題と設計上の障害）

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

**引受先**: `docs/implementation/viewer-tool-extensions-plan.md` の `TOOLX-1`
（実装する方を採る）。`docs/implementation/done/pointer-feedback-plan.md` は
この 2 ツールのカーソルを意図的に見送っており、`TOOLX-1` がカーソルも同時に付ける
（機能が無いものに UI の約束をしないため）。

---

## MED-APP-17 | bug | カーブエディタの縦ズームが未実装で、Fit ボタンが何もしない

**該当**: `crates/ravel-app/src/panels/timeline.rs:241`, `:345`, `:948-951`, `:2800-2802`

縦方向の手動レンジを持つフィールドがあるが、**`Some(..)` を代入するコードが
1 行も存在しない**。

| 行 | 内容 |
| --- | --- |
| `:241` | `curve_value_range: Option<(f64, f64)>` の宣言 |
| `:345` | `None` で初期化 |
| `:949` | `fit_curve_values` が `None` を代入 |
| `:2801` | 読み出し（`.or(self.curve_value_range)`） |

帰結が 2 つ:

1. **縦ズーム・縦パンが存在しない**。縦の表示範囲は常に
   `curve_value_bounds(&resolved)` の自動 bounds に固定される
2. **Fit ボタンが何もしない**。`fit_curve_values` は `None` に `None` を
   代入して `cx.notify()` するだけ。既に auto なので見た目が変わらない

ツールバーとコンテキストメニューの両方から到達できる（`:2162`, `:3917`）が、
どちらも無反応。

**修正方針**: 縦ズーム（ホイール / ピンチ / ドラッグ）を実装して
`curve_value_range` を書く経路を作る。その時点で `fit_curve_values` が
「手動レンジを捨てて自動に戻す」という意味を持つ。

**現状（`PARAM-5` 実施後）**: 置き場所は済んでいる。`curve_value_range` は
`crates/ravel-app/src/widgets/curve_view.rs` の `CurveValueRange` になり、
`fit_curve_values` はその `fit()`（= データ追従に戻す）を呼ぶ。Properties の
カーブエディタは同じ型をホイールと数値入力から書いている。**残っているのは
Timeline に書き込み操作を足すこと**（ホイールを縦ズームに割り当てると既存の
スクロール挙動が変わるため、`PARAM-5` では足していない）。それまで Timeline
の Fit は自動範囲に自動範囲を代入するので見た目が変わらない。

**検証**: ホイール / ピンチで縦方向にズームでき、Fit で自動範囲へ戻るテスト。

---

## MED-APP-19 | bug | `Channel4` パラメータが常に Color として描画される

**該当**: `crates/ravel-ui/src/properties/node.rs:141`

ノードパラメータ → Properties フィールドの写像で、`Channel4` が
`PropertyField::Color` に決め打ちされている。`Channel2` / `Channel3` は
`PropertyField::Vector` になる（`:121`, `:131`）のに、4 成分だけ色扱い。

色ではない Vec4 パラメータが色スウォッチと `(r, g, b)` テキストで表示され、
成分を個別に編集できない。**実例**: `attribute.set` の `type = "vec4"`
（`vector-field-plan.md` 単位 5 で `value` が型駆動の 1 パラメータになった）。
同じノードの `type = "color"` は色なので現状の描画が正しく、両者を
テンプレート側の宣言で区別する必要がある。

**wire 型の側は解決済み**（単位 5）。4 成分パラメータポートは `COLOR` と
`VEC4` の両方を受けるので（`ParameterValue::port_accepted_types`）、
`vector.construct.vec4` から駆動できる。残るのは Properties の描画だけ。

**修正方針**: 色かどうかをレジストリのテンプレート側で宣言する
（`viewer-overlay-manipulator-plan.md` が導入する `ParamRole` と同じ層に
`Color` の区別を置くのが素直）。宣言が無い `Channel4` は `Vector` として
4 成分表示にする。

**検証**: 色として宣言されていない `Channel4` が 4 成分の Vector 行になるテスト。
`constant.color` の `color` が従来どおり ColorPicker になるテスト。

---

## MED-APP-20 | debt | Vector フィールドに成分ラベルとリンクトグルが無い

**該当**: `crates/ravel-app/src/panels/properties.rs:274-309`

`PropertyField::Vector` は成分ごとの `ScrubInput` を横並びで描画する
（`:294-299` の `div().flex().gap_1()`、各 `min_w(56px)`）。C4D / Houdini と
同じ行レイアウトだが、

- 各フィールドに**成分ラベル（X / Y / Z）が無い**。成分の区別が位置だけ
- **リンクトグル（均一スケール）が無い**
- キーフレームダイヤはフィールド単位（押すと全成分に打つ）。AE と同じ挙動なので
  仕様として妥当だが、成分別に打つ手段が無い

**修正方針**: 成分ラベルを `ScrubInput` の接頭辞として描く。リンクトグルは
`ParamRole::Size` を宣言したパラメータにのみ出す。

なお**この問題が表面化するのは組み込みノードが Vec を `Channel2` /
`Channel3` で宣言してから**。現状は `center_x` / `center_y` のように
Float 2 本に分解されており（`crates/ravel-core/src/registry/builtin.rs:566-582`
他）、Vector 行にほとんど到達しない。統合は
`docs/implementation/vector-field-plan.md` 単位 5 が担当する。

**検証**: 成分ラベルが型のアリティに応じて X / Y / Z / W になるテスト。

---

## MED-APP-21 | debt | Viewer の bbox が `type_key` の固定 match でパラメータから再構成される

**該当**: `crates/ravel-app/src/panels/viewer.rs:2388-2423`, `:453`, `:527`

`shape_node_bounds` はジオメトリを評価せず、`type_key` の match で
パラメータ名を直読みして矩形を作る。

```rust
"shape.rect"    => (width * 0.5, height * 0.5)
"shape.ellipse" => (radius_x, radius_y)
"shape.polygon" => (radius, radius)
"shape.star"    => (outer_radius, outer_radius)
```

帰結が 3 つ:

1. shape ノードを追加するたびにこの match を編集しないと bbox が出ない
   （`geometry-ops-plan.md` 単位 11 の `shape.line` / `shape.grid` が該当）
2. `geometry.transform` や `scatter.*` を経た**実際の形状が反映されない**
3. `docs/specifications/procedural-geometry.md` の設計原則 1
   「固定機能のリピーターを作らない」に対する既存の例外

ドラッグ経路（`:453`, `:527`）も同じ関数に依存している。

**修正方針**: 評価済み Geometry から bbox を出す。設計と実装単位は
`docs/implementation/viewer-overlay-manipulator-plan.md` 単位 3
（`shape_node_bounds` の廃止を含む）。**推測値と実測値を並存させない** —
並存させると評価前後で bbox が飛ぶ。

**検証**: `type_key` を知らないノードで bbox が描かれるテスト。
`geometry.transform` を経た形状の bbox が変換後になるテスト。
