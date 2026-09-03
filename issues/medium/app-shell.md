# medium — ravel-app / ravel-ui / ravel-cli（シェル・パネル・状態管理・ヘッドレス経路）

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

**該当**: `crates/ravel-project/src/settings.rs`,
`crates/ravel-project/src/lib.rs:319-333`, `crates/ravel-app/src/main.rs:49`

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
（`done/viewer-overlay-manipulator-plan.md` が導入する `ParamRole` と同じ層に
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

## MED-APP-29 | bug / debt | `layer.ref` のレイヤー指定が数値スクラブで、参照ポートを変えても出力型が変わらない

**該当**: `crates/ravel-core/src/registry/builtin.rs:529-540`（`layer_ref`）

```rust
.with_output(OutputPort { name: "output".into(), data_type: DataTypeId::FRAME_BUFFER })
.with_param(int_parameter("layer", -1))
.with_param(string_parameter("port", "frame"))
.with_param_range("layer", -1.0..=16_777_215.0, -1.0..=1000.0)
```

2 つある。

1. **`layer` が Int パラメータ**なので、Properties には −1〜16,777,215 の
   数値スクラブが出る。ユーザーはレイヤー ID を知らないし、スクラブすると
   存在しないレイヤーを指す。`port` も自由文字列
2. **出力ポートの型が `FRAME_BUFFER` 固定**。`port` を変えても
   出力の型が追随しないので、フレーム以外を参照した瞬間に型が嘘になる

**修正方針は計画書へ移した**（2026-08-09）。調べたところ、足りないのは
`layer.ref` の書き方ではなく**文脈から候補と型が決まる機構**そのものだった:
`Registry::param_options` はテンプレート静的、`SHELL-5` の Parent
ドロップダウンはレイヤーフィールドの別経路、パラメータ → 出力ポート型の追随は
どこにも無い（`set_params` が retype するのはパラメータポートだけ）。
複数クレートに跨るので Design gate に当たる。
→ [`contextual-parameter-options-plan.md`](../../docs/implementation/contextual-parameter-options-plan.md)
の `CPO-1`〜`CPO-7`。この issue はその単位が入った時点で閉じる。

---

## MED-APP-30 | bug | Timeline のキーフレーム行の成分名が arity だけで決まる

**該当**: `crates/ravel-ui/src/keyframes.rs:869-874`

```rust
let names = match components.len() {
    1 => vec![CHANNEL_VALUE],
    2 => vec!["X", "Y"],
    3 => vec!["R", "G", "B"],      // ← Vec3 でも RGB
    _ => vec!["R", "G", "B", "A"], // ← Vec4 でも RGBA
};
```

2 成分だけ X / Y で、**3 成分以上は無条件に色扱い**。Vec3 パラメータに
キーフレームを打つと、Timeline の子行が `R` / `G` / `B` と表示される。

**再現**: `constant.vec3`（`vector-field-plan.md` 単位 6、#402）の値に
キーフレームを打つ。

**既存の票は覆っていない**:

| 票 | 覆っている範囲 |
| --- | --- |
| `MED-APP-19` | Properties の描画。`Channel4` が `PropertyField::Color` 決め打ち（**4 成分の話で 3 成分に触れていない**） |
| `MED-APP-20` | Properties の Vector 行に成分ラベルが**無い**（**間違っている**話ではない） |
| 本票 | Timeline のキーフレーム行の成分名 |

**修正方針**: 根は 3 票とも同じで、「このパラメータは色か、ベクタか」が
テンプレート側で宣言されていないこと。`MED-APP-19` が挙げている方針
（`done/viewer-overlay-manipulator-plan.md` の `ParamRole` と同じ層に `Color` の
区別を置く）に相乗りさせ、宣言が無い 3 / 4 成分は `X` / `Y` / `Z` / `W` に
する。**3 票まとめて片付ける**のが素直。

**検証**: 色として宣言されていない `Channel3` のキーフレーム行が
`X` / `Y` / `Z` になるテスト。`constant.color` が従来どおり `R` / `G` / `B` /
`A` のままであるテスト。

## MED-APP-37 | bug | 評価結果が「届いた時点のコンポジション」と対で扱われ、切替中の結果が別コンプの寸法で解釈される

**該当**: `crates/ravel-app/src/project_state.rs:2160-2178`（`ViewerOutput::Frame` /
`ViewerOutput::Gpu` の組み立て）

届いた評価結果に `composition_resolution` を付けるとき、**その結果がどのコンプの
ものかではなく「今アクティブなコンプ」**を読んでいる。コンプ A の評価が飛んでいる
最中に B へ切り替えると、A の絵が **B の解像度**で解釈される。

- 症状 1（従来から）: オーバーレイのコンプ座標変換がずれる。bbox やマニピュレータが
  絵と合わない
- 症状 2（`INSP-3` で増えた）: ピクセル読み取りが**別の画素の値**を報告する
  （`comp_to_buffer_index` がコンプ寸法とバッファ寸法の比を使うため）

`ViewerUpdate` は自分がどのコンプを評価したかを持たないので、**受け取り側では
判定できない**のが根本。`load_project_from` の `load_request` / `revision` ガードと
同じ形（要求時の識別子を結果に添えて、届いたときに突き合わせる）が要る。

→ `ViewerUpdate`（`ravel-core` の `ViewerResult`）に評価したコンプ id を載せ、
アクティブなコンプと一致しない結果は捨てる。捨てるだけで良いのは、切替時には
必ず新しい要求が出ているため。

**検証**: 遅い結果を A のまま作り、B へ切り替えてから配達して、
`ViewerFrame` が更新されない（または A の寸法で解釈される）ことを落とすテスト。

---

## MED-APP-39 | bug | プレビュー解像度を切り替えても、キャッシュ帯が前の係数のまま残ることがある

**該当**: `crates/ravel-app/src/project_state.rs:2229-2252`（`publish_cache_band`）、
`:1767-1774`（`set_viewer_resolution`）

`publish_cache_band` は**フレームキャッシュの version だけ**を見て早期 return する。
帯そのものは `viewer_eval_context`（実効係数を含む）で計算するので、係数が変われば
帯も変わるべきだが、**`set_viewer_resolution` は `published_band_version` を
落とさない**。

新しい係数のフレームがまだキャッシュに無ければ、続く評価で version が上がるので
1 回分の遅れで収まる。**問題は両方がキャッシュに載っている場合** —
`Full` → `1/2` → `Full` と往復すると version が動かないので、帯は
**別の係数で計算したまま**残る。Timeline は「スクラブがタダで済む」と言い、
`INSP-4` の Viewer 右上は同じ帯から割合を出すので、両方が同じだけ嘘をつく。

→ `set_viewer_resolution` で `clear_cache_band`（`published_band_version = None`）
を通す。表示チャンネル・ピクセル読み取りの setter はキャッシュ自体を捨てるので
version が動き、この穴には当たらない。

**検証**: 2 つの係数のフレームを両方キャッシュに入れてから係数を戻し、帯が
戻した係数のものになることを落とすテスト。

---

## MED-APP-38 | bug | 表示設定の切り替えが「飛んでいる評価」を締め出さないので、古い設定のフレームがキャッシュに戻る

**該当**: `crates/ravel-app/src/project_state.rs:1780-1795`（`set_display_channel`）、
`:1840-1855`（`set_pixel_readout`）、`crates/ravel-core/src/runtime/eval_service.rs:836`

どちらの setter も**出力段フレームキャッシュを捨ててから**再要求する
（`INSP-2` / `INSP-3`）。しかし**捨てた時点で走っているワーカーの評価**（先読み
`CACHE-9` を含む）は古い設定で finalize を終え、`clear()` の**後に**キャッシュへ
入りうる。

- チャンネル: 古いモードの表示バイト列が入る → 次のヒットで前のモードの絵が返る
- 読み取り: リニアフレームを持たない（または持ったままの）エントリが入る →
  読み取りが空のまま / off にしたのに f32 を運び続ける

いずれも**次の無効化まで残る**（一過性ではない）。

→ 設定の世代（`u64`）を要求と結果に載せ、**世代が古い結果はキャッシュへ入れない**。
`AudioService` の `generation` と `finish_pending_generation`（#472）が同じ形の前例。

**検証**: 古い世代の結果を配達して、キャッシュに入らないことを落とすテスト。

---

## MED-APP-40 | debt | macOS では GPUI 自身の Metal device の喪失を問う口が fork に無い

**該当**: `crates/ravel-app/src/workspace.rs`（`host_gpu_context` / capability 判定の
`#[cfg(target_os = "macos")]` 腕）、`crates/ravel-app/src/panels/viewer.rs`
（`host_device_loss` を `false` に固定する `cfg(not(...))` 腕）、
gpui fork の `crates/gpui/src/platform.rs`（`gpu_device_lost`）

fork の `PlatformWindow::gpu_device_lost()` と `gpu_context_full()` は
`#[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]` の
中にしか無く、`gpui_macos`（Metal ネイティブ）はどちらも実装していない。したがって
macOS では**問う口自体が無い** — `None` が返るのではなく呼べない。

結果として Ravel が macOS で検出できるのは**自前 wgpu device の喪失だけ**である
（`GPULOSS-1` で登録した loss callback）。GPUI の Metal renderer / command queue 側の
喪失と再生成は検出されない。`GPULOSS-4` はこれを安全側で確定させた: 自前 device が
死んだら zero-copy を永久に切って CPU フレームで描き続ける（復旧ではない）。

**実害**: 大きくない。Ravel と GPUI が同じ Metal device を共有している（`ZC-2` の
`native_device_matches` がそれを確認している）ので、GPUI 側の device が死ぬ状況は
Ravel 自前 context の callback も撃つ可能性が高く、そのときは検出できる。「GPUI が自分の renderer を作り直して**別の** device に移った」場合は
**検出できる** — `with_surface_texture` は renderer の現在の device ハンドルを
毎フレーム受け取って `native_device_matches` を通すので、identity が変われば
描かずに `false` を返し、`surface_lost` の経路が capability を落として CPU
フレームを要求する。**残る穴は「identity が変わらないまま device が失われた」
場合だけ** — Ravel 自前 context の callback は撃たれず（別の device なので）、
GPUI 側にそれを問う口が無いので、zero-copy を試み続けて毎 paint が失敗する。
不正なサンプリングは起きない（照合が弾く）ので、症状は「絵が出ない / 出るのが
遅い」に留まる。

**修正方針**: fork の `PlatformWindow` に「callback を上書きせず native device の
identity / loss status を読む口」を macOS でも足せるか調べる（`gpui_macos` の
`MetalRenderer` は `MTLDevice` を保持しているので identity は返せる。喪失は Metal に
`MTLDeviceWasRemoved` 相当の通知があるかに依る）。足せるなら macOS も
`GPULOSS-3` と同じ recovery coordinator に載せられる。足せないなら
`GPULOSS-4` の安全側確定が macOS の最終形になる。

**severity の根拠**: bug ではなく debt。安全側の fallback が既に入っていて、
絵は出続け、不正なサンプリングも起きない。high でないのはデータ損失もクラッシュも
無いため、low でないのは 1 プラットフォームが device 喪失検出を持たない状態が
`GPULOSS-5`（macOS の実機確認）と macOS の recovery 実装をそのまま塞ぐため。

**検証**: fork 側の調査が先。`gpui_macos` に口が付いたら、Linux / Windows と同じ
identity 照合と loss polling のテストを macOS 腕に足す。

---

## MED-APP-41 | debt | zero-copy の可否が session 全体で 1 個なので、別 GPU の 2 枚目の window が main window の zero-copy も落とす

**該当**: `crates/ravel-app/src/project_state.rs`（`configure_viewer_surface` と
`viewer_surface_enabled: Arc<AtomicBool>`）、`crates/ravel-app/src/panels/viewer.rs`
（paint 側の capability 判定）

`viewer_surface_enabled` は `ProjectState` が 1 本だけ持つ共有 atomic で、評価
worker の `DisplayTransform` がそれを読んで「GPU テクスチャを出すか、CPU フレームを
出すか」を決める。**出力の形が session に 1 つしかない**ので、window ごとに
別の答えを持てない。

結果、`done/gpu-device-loss-recovery-plan.md` の `GPULOSS-5` が完了条件に書いた
「device mismatch ならその window だけ CPU fallback」は、実装では
**session 全体が CPU fallback になる**。別 GPU に載った 2 枚目の window を開くと、
main window の zero-copy も一緒に落ちる。

**実害**: 小さい。絵は出続け（CPU 経路）、不正なサンプリングも起きない。マルチ GPU
機で分離 window を使ったときに main window のプレビューが遅くなるだけである。

**修正方針**: worker が 2 つの表現を同時に作るか、capability を window ごとに持って
paint 側が選ぶ。どちらも「zero-copy の可否の権威を 2 つにしない」という
`ZC-8` 以来の方針に触るので、per-window が実際に要る状況（マルチ GPU 機での分離
window）が出てから決める。

**severity の根拠**: bug ではなく debt。安全側の劣化であり、完了条件の記述が
アーキテクチャより広かった。low でないのは、計画書の完了条件と実装が食い違って
いる状態そのものが次の単位の設計を誤らせるため。

## MED-APP-42 | bug | macOS の自前 device 喪失では、退役したフレームが Global に残り続けることがある

**該当**: `crates/ravel-app/src/project_state.rs`（`report_gpu_device_loss`）、
`crates/ravel-app/src/panels/mod.rs`（`ViewerFrame` global）

`GPULOSS-5` は device epoch の交換（`restart_eval_worker`）と session の release で
`ViewerFrame` を blank にした。しかし **macOS の自前 device 喪失の経路
（`report_gpu_device_loss`）は blank しない** — zero-copy を切って CPU フレームを
1 枚要求するだけである。

要求が通れば次のフレームが上書きするので自己解消する。**要求が失敗する経路**
（評価がエラーで返る、worker が既に居ない）では、死んだ device のテクスチャを
運ぶ `GpuFrame` が global に残り続ける。paint 側の guard は identity 照合なので
不正なサンプリングは起きないが、**退役した pool への lease が解放されない**。

**実害**: 中程度。喪失時に一度だけ、そのフレーム分の VRAM が返らない。
`GPULOSS-4` のテストが `ViewerFrame::Frame` の publish を期待しているので、
blank を足すならそのテストの意図（「CPU フレームで描き続ける」）と両立させる形に
する必要がある。

**修正方針**: `report_gpu_device_loss` でも fence 付きで blank し、CPU フレームの
要求が成功したときにそれが上書きされる形にする。`GPULOSS-4` のテストは
「blank → CPU フレーム」の順を見るように書き換える。

**severity の根拠**: bug。ただしクラッシュも不正な絵も起こさず、喪失という
既に劣化した状態でしか踏めないので high ではない。low でないのは、GPU
リソースが返らない経路を残すため。

