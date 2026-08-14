# Viewer オーバーレイ機構とマニピュレータ 実装計画

> **Status**: In progress — 2026-07-29（候補の棚卸しと機構の次元を追記:
> 2026-07-30。単位 1 完了: 2026-08-01、PR #255）

対象: Viewer のオーバーレイを拡張可能な機構にし、評価結果を根拠にした
可視化（Field / Geometry）と、パラメータを直接掴めるマニピュレータを載せる。
関連要件: REQ-UI-011、REQ-UI-013、REQ-CORE-010、REQ-CORE-012。

## 問題

### 1. オーバーレイが canvas クロージャに直書きされている

現状 Viewer が描く重ね描きは 5 種類ある。4 種は 1 つの paint クロージャに
並んでおり（`crates/ravel-app/src/panels/viewer.rs:1598-1621`）、5 種目は
要素側にある。

| オーバーレイ | 描画箇所 |
|---|---|
| 比率グリッド | `:1606 paint_proportional_grid` |
| セーフエリア | `:1609 paint_safe_areas` |
| 選択 bbox（ノード / レイヤー） | `:1611-1618 paint_selection_bbox` |
| パス編集ハンドル | `:1619-1620 paint_path_overlay` |
| 評価エラー表示 | `:182` の `error` フィールドを `:1643` で要素として重ねる（`viewer.eval_error`） |

新しいオーバーレイを足すには、必要なデータを `render()` の先頭で組み立て
（`:1528-1580` に 3 本の即時クロージャがある）、paint クロージャに 1 行足し、
ヒットテストを `on_mouse_down` の分岐に割り込ませる、という 3 箇所の編集になる。
オーバーレイの種類が増えるほど `render()` と入力ハンドラが膨らむ。

### 2. bbox がジオメトリを評価していない

`shape_node_bounds`（`viewer.rs:2388-2423`）は `type_key` の match で
パラメータ名を直読みして矩形を再構成する。

```rust
"shape.rect"    => (width * 0.5, height * 0.5)
"shape.ellipse" => (radius_x, radius_y)
"shape.polygon" => (radius, radius)
"shape.star"    => (outer_radius, outer_radius)
```

帰結が 3 つ:

1. shape ノードを追加するたびにこの match を編集しないと bbox が出ない
2. `geometry.transform` や `scatter.*` を経た**実際の形状が反映されない**
3. `docs/specifications/procedural-geometry.md` の設計原則 1
   「固定機能のリピーターを作らない」に照らした既存の例外
   （`style-attributes-plan.md` が指摘する fill / stroke と同じ構図）

**Field オーバーレイをこの流儀では作れない。** フィールドは型キーから形が
決まらず、評価しないと値が分からない。

### 3. マニピュレータが custom_path 専用

ドラッグ可能なハンドルの機構は既に動いている
（`PathOverlay` は `viewer.rs:1896`、`PathHandleKind::{Point, InTangent, OutTangent}`
は `:155-166`、ヒットテストは `:2060-2085`）。ただし
`shape.custom_path` の `points` パラメータ専用。

`center_x` / `center_y` のような位置パラメータを掴む一般機構が無い。しかも
現状のパラメータ宣言は Vec を**別々の Float に分解**している
（`registry/builtin.rs:566-582` の `shape.rect`、`:450-467` の
`geometry.transform` 他）。この状態で一般化すると、`_x` / `_y` という
**名前の組を推測するヒューリスティクス**を Viewer が抱えることになる。

### 4. レイヤー殻を掴む手段が無く、bbox のハンドルは飾りになっている

`LayerTransform` は `anchor_point` / `position` / `scale` / `rotation` を
独立チャネルとして持つ（`crates/ravel-core/src/composition/mod.rs:87-91`）。
**このどれも Viewer から掴めない。**

選択 bbox は 8 個のハンドルを描いているが（`selection_handle_centers`
`viewer.rs:2720`、`paint_selection_handle` `:2736`）、**スケールも回転も
ジェスチャーが存在しない**。ドラッグで動くのは bbox の内側から始めた移動だけ
（`layer_move_mouse_down` `:492`）。`paint_selection_bbox` の doc コメント
（`:2751-2752`）は「レイヤー選択にハンドルを描かないのはレイヤー単位の
スケールジェスチャーが無いから」と書いており、ノード単位にはあるかのように
読めるが、ノード単位にも無い。

`ParamRole` によるマニピュレータ（下記単位 5）はノードのパラメータ宣言で
駆動するので、**殻には届かない**。殻の transform はノードパラメータではなく
`Layer` のフィールドであり、`ParamRole` を付ける先が無い。

### 5. どのオーバーレイを作るべきかの一覧が無かった

本計画は当初 6 種（既存 4 + Field + マニピュレータ）だけを並べていた。
2026-07-30 に Viewer の実装と各計画・要件を突き合わせた結果、**15 件の候補が
どの計画にも属していない**ことが分かった（下記「候補の棚卸し」）。そのうち
4 件は機構の前提そのものを変えるので、機構を作る前に扱いを決める必要がある。

## 決定事項

### 機構を先に作り、既存 5 種を載せ替える（挙動不変）

オーバーレイを trait + レジストリにして、既存の 5 種をそれに載せ替える。
このフェーズでは**見た目と操作を一切変えない**。
`backlog.md` の DOCK-1（レイアウトモデル v2 を旧レイアウトと等価に保って
入れる）と同じ「挙動不変で機構だけ差し替える」進め方を採る。

```rust
/// Viewer に重ねる 1 レイヤー。描画とヒットテストを 1 箇所に閉じる。
trait ViewerOverlay {
    /// 表示条件（選択状態・トグル・出力型）。false なら以降を呼ばない。
    fn is_active(&self, ctx: &OverlayContext) -> bool;

    /// 評価結果を要求する場合の pull 対象。None なら Document だけで描ける。
    fn eval_target(&self, ctx: &OverlayContext) -> Option<OverlayTarget>;

    /// コンプ空間で描く。window への paint はここだけ。
    fn paint(&self, ctx: &OverlayContext, painter: &mut OverlayPainter<'_>);

    /// 掴めるハンドル。空なら入力に関与しない。
    fn handles(&self, ctx: &OverlayContext) -> Vec<OverlayHandle>;

    /// ハンドルのドラッグを Document 変更に翻訳する。
    fn drag(&self, handle: &OverlayHandle, delta: CompVec2, ctx: &OverlayContext) -> OverlayEdit;
}
```

満たすべき性質:

- **座標系を 1 箇所に閉じる**。`OverlayPainter` はコンプ空間を受けて
  `frame_bounds` / `resolution` への変換を内部で行う。各オーバーレイが
  スケール計算を持たない（現状は `paint_*` 関数ごとに `frame_bounds` と
  `resolution` を渡し回している）
- **描画とヒットテストが同じ場所にある**。`handles()` が返す形が
  `paint()` が描く形と一致する。今の path overlay は描画
  （`:1620`）とヒットテスト（`:2060`）が離れており、ずれうる
- **`render()` は純粋なまま**。`.agents/rules/gpui.md` の render 純粋性を守る。
  評価要求は `eval_target()` の宣言を集めて 1 回発行し、結果は
  グローバル経由で届く（`NodeEvalTimings` / `ViewerFrame` と同じ扱い。
  `project_state.rs:935-940` の方針）
- **ハンドルのドラッグは 1 ジェスチャ 1 undo**。既存のスクラブと同じ
  （Change 中は undo を積まず、終了時に 1 スナップショット）
- **z 順と入力の優先順位を宣言で持つ**。オーバーレイごとに優先度を持ち、
  ハンドルのヒットテストは優先度の高い順に解決する。NodeEditor のポート
  ヒットテストが z 順を無視している問題（`issues/high/`）と同じ罠を
  最初から避ける

### 機構は 4 つの次元を最初から持つ

候補の棚卸しで、当初の trait が置いていた 4 つの前提（1 フレーム・コンプ空間・
マウス入力のみ・編集対象はノードパラメータ）をそれぞれ破る候補が見つかった。
**挙動不変のリファクタである単位 1 で入れておく。**後から足すと、載せ替えた
オーバーレイ全部を書き直すことになる。

| 次元 | 当初の前提 | 破る候補 | 単位 1 で用意するもの |
|---|---|---|---|
| 時間 | `eval_target()` は現在フレームの 1 点 | モーションパス（位置キーフレームの空間軌跡） | `OverlayContext` にフレーム範囲を持たせる余地を残す。**評価要求としては 1 点のまま**（軌跡はチャネル直読みで足りる。理由は「非対象」節） |
| 座標系 | `OverlayPainter` はコンプ空間のみ | ドラッグ中の数値 HUD、定規・ガイド、要素 index ラベル | **スクリーン空間の描画も `OverlayPainter` の API に入れる**。既に `SELECTION_HANDLE_PX`（`viewer.rs:2716`）と `paint_path_handle`（`:2705`）がズーム不変の固定 px で描いており、2 つの座標系は実質すでに混在している |
| 入力 | `handles()` と `drag()`（マウスのみ） | Viewer 上のテキスト編集（キャレット・IME） | trait には**追加しない**。フォーカスとキー入力を取るオーバーレイは別の設計判断なので `typography-plan.md` 側の課題として記録する（「非対象」節） |
| 編集対象 | `OverlayEdit` はノードパラメータの更新 | レイヤー殻の transform、親子付け替え | **`OverlayEdit` を「Document への変更」として定義する**。ノードパラメータ更新と殻チャネル更新の両方を表せる形にし、undo 規約（1 ジェスチャ 1 スナップショット）は共通にする |

### 殻トランスフォームは `ParamRole` とは別経路で扱う

`ParamRole`（単位 5）はノードのパラメータ宣言に付ける仕組みなので、
`Layer` のフィールドである殻 transform には使えない。殻のマニピュレータは
**`LayerTransform` の 4 チャネルを直接対象にする専用オーバーレイ**（単位 7）とし、
`ParamRole` の一般機構と混ぜない。混ぜると `Layer` に架空のパラメータ名を
生やすことになる。

なお殻マニピュレータは `vector-field-plan.md` 単位 A（Vec パラメータ正規化）に
**依存しない** — 殻は最初から `[AnimationChannel; 2]` で 1 つの概念として
まとまっており、`_x` / `_y` の名前推測問題が無い。したがって単位 5 より先に
実装できる。

### オーバーレイ用の評価は multi-target 評価に乗る

Field / Geometry の可視化は「コンプ出力とは別のノード出力を、同じフレームで
pull する」ことを要求する。これは
`attribute-spreadsheet-plan.md` 単位 1 が `EvalRequest` / `EvalUpdate` を
multi-target 化するのと**同じ機構**なので、独自の経路を作らず**そちらに乗る**。

`docs/implementation/README.md` が記録している `EvalRequest` 変更の衝突
（attribute-spreadsheet 単位 1 と stateful-eval 単位 3）に本計画も加わる。
着手順は 3 者で決める。

### パラメータの意味はレジストリが宣言する

マニピュレータは名前の推測ではなく宣言で駆動する。

```rust
// registry/builtin.rs
.with_param_role("center", ParamRole::Position)     // 位置ハンドル
.with_param_role("radius", ParamRole::Size)         // サイズハンドル
.with_param_role("direction", ParamRole::Direction) // 方向ハンドル
.with_param_role("rotation", ParamRole::Angle)      // 回転ハンドル
```

これは **Vec パラメータの正規化（`vector-field-plan.md` 単位 A）に依存する**。
`center_x` / `center_y` が別パラメータのままでは 1 つの `ParamRole` を
付けられない。`Channel2` へ統合されてはじめて宣言が成立する。

### `shape_node_bounds` の type_key match は廃止する

Geometry オーバーレイが実データから bbox を出せるようになった時点で、
`shape_node_bounds` の match を削除する。**両方を残さない** — 残すと
「評価前は推測値、評価後は実測値」で bbox が飛ぶ。

移行中の初回フレーム（評価結果が未着）は bbox を描かない。推測値で埋めない。

## 目標アーキテクチャ

```text
ViewerPanel::render()
  └ OverlayRegistry
       ├ GridOverlay          (Document のみ)
       ├ SafeAreaOverlay      (Document のみ)
       ├ SelectionBboxOverlay (Geometry 評価結果)
       ├ PathEditOverlay      (Document + ハンドル)
       ├ EvalErrorOverlay     (評価エラー。スクリーン空間)
       ├ FieldOverlay         (Field 評価結果)
       ├ GeometryAttrOverlay  (Geometry 評価結果 + 属性の矢印 / ラベル)
       ├ ParamManipulator     (ParamRole 宣言 + ハンドル)
       └ ShellManipulator     (LayerTransform の 4 チャネル + HUD)
             ↓ eval_target() を集約
       EvalRequest (multi-target)
             ↓ 結果はグローバル経由
       OverlayResults グローバル → paint / handles
             ↓ ハンドルのドラッグ
       OverlayEdit = Document への変更（ノードパラメータ | 殻チャネル）
```

## 実装単位

### 単位 1: オーバーレイ機構の抽出（挙動不変） — 完了（PR #255）

- `ViewerOverlay` trait、`OverlayContext`、`OverlayPainter`、`OverlayHandle`、
  `OverlayRegistry` を `crates/ravel-app/src/panels/viewer/overlay.rs` に置く
- 既存 5 種（グリッド / セーフエリア / 選択 bbox / パス編集 / 評価エラー表示）を
  載せ替える。`shape_node_bounds` はこの時点では触らず、そのまま bbox
  オーバーレイのデータ源にする
- `OverlayPainter` に**コンプ空間とスクリーン空間の両方**の描画 API を持たせる
  （既存のハンドルは既に固定 px で描かれており、この 2 系統は現状も存在する）
- `OverlayEdit` を「Document への変更」として定義する。ノードパラメータ更新と
  レイヤー殻チャネル更新の両方を表せる形にする（単位 5 と単位 7 が共有する）
- 入力側: ハンドルのヒットテストを優先度順の 1 経路に統合する

**完了条件**

- 5 種すべてについて、載せ替え前後で描画結果が一致するテスト
  （座標変換の性質テスト。ゴールデン画像は使わない）
- ハンドルのヒットテストが優先度順に解決されるテスト
- スクリーン空間で描く要素がズームに依存しないことのテスト
- `OverlayEdit` がノードパラメータと殻チャネルの両方を表せることを、
  型レベル（片方だけの形にならない）とテストの両方で示す
- `render()` に評価要求・フォーカス変更・状態変更が入っていないことの
  `ravel-review` 観点での確認

### 単位 2: オーバーレイ用の評価要求

- `eval_target()` の集約と、multi-target 化した `EvalRequest` への相乗り
- 結果を運ぶグローバルと、オーバーレイからの読み出し
- 結果未着のときはそのオーバーレイを描かない（推測で埋めない）

**完了条件**

- オーバーレイが 0 個アクティブなとき追加の評価要求が発行されないテスト
- 同じノードを 2 つのオーバーレイが要求したとき要求が 1 回に畳まれるテスト
- 結果未着時に描画が空になるテスト

### 単位 3: Geometry オーバーレイと `shape_node_bounds` の廃止

**前提 — 単位 2 の実装で判明した制約。着手前に解くこと。**

単位 2 が載せた相乗りは、**要求が評価するスコープと `OverlayTarget.network`
が一致するターゲットだけ**を対象にできる。`EvalRequest` は 1 リクエスト =
1 グラフ + 1 パスだからである。viewer の `EvalRequest` は `graph` にシェルの
コンパイル済みグラフ、`path` に空（root スコープ）を入れる。一方
`NetworkPath` は必ずコンプとレイヤーを名指すので `segments()` が空になること
はない。**したがって viewer のコンプ要求に相乗りできるターゲットは 1 つも
存在しない。**

判定を「要求のグラフにその `NodeId` が実在するか」で書いてはならない。
レイヤーネットワークは境界ノード経由で再帰評価されインライン展開されない
ので、シェルグラフにレイヤーネットワークのノードは決して居らず、ヒットする
のは **ID 衝突のときだけ**である。`deterministic_node_id` は
`comp << 32 | layer << 8 | role` なので、コンプ ID が 0 なら合成ノードの ID は
通常のノード ID の範囲に落ちる。その場合オーバーレイは**無関係な合成ノードの
結果**を描く。

ところが単位 3 が bbox を描きたい相手は、まさにノードエディタで選択中の
**レイヤーネットワーク内部のノード**である。したがって単位 3 は
`graph = resolve_network(document, path)` / `path = network.segments()` の
**ネットワークスコープの要求を別に出す**必要がある。

そのとき次とぶつかる:

- `ViewerUpdate::from_eval` は **`results[0]` を viewer フレームとして読む**
  （`project_state.rs`）。オーバーレイ専用の要求は target 0 がコンプ出力では
  ないので、そのまま同じ経路へ返すと `ViewerOutput::NotAFrame` になり
  **viewer が blank する**
- 単一の `EvalService` の世代（`published_generation`）を共有するので、
  2 種類の要求を素朴に交互発行すると互いを打ち消す

**採りうる方向**:

1. オーバーレイ専用要求を別チャネル（別の `EvalService` 消費経路）に分け、
   viewer フレームの世代と混ぜない
2. `EvalRequest` に「このターゲットは viewer フレームではない」という
   区別を持たせ、`from_eval` が target 0 を無条件にフレームと見なすのを
   やめる
3. レイヤーネットワークのノードをシェルグラフ側から到達可能にする
   （評価モデルに触るので重い）
4. **`EvalRequest` に「スコープ付きターゲット」を足す**（下記。**推奨**）

### 推奨は 4 — スコープ付きターゲットを 1 要求に同居させる

**`Evaluator::evaluate_at(path, graph, output, ctx)` が既にある**
（`crates/ravel-core/src/eval.rs:2485`）。呼び出しごとに `path` と `graph` を
받って `self.path` / `path_id` / `active_scopes` を張り替えるので、
**1 つの `Evaluator` で複数のスコープを順に評価できる。**

したがって `EvalRequest` に、既存の `nodes`（要求の `graph` / `path` で評価
する）とは別に、**ターゲットごとに `(path, graph, node)` を持つ列**を足せば
よい。ワーカーは同じ `Evaluator` で `evaluate_at` を続けて呼ぶ。

これが 1〜3 より良い理由:

- **target 0 はコンプ出力のまま。** `ViewerUpdate::from_eval` を触らずに
  済む（1 と 2 が解こうとしていた衝突が最初から起きない）
- **要求は 1 本のまま。** 世代も 1 本なので、2 種類の要求が互いを打ち消す
  問題（1 の動機）が消える
- **キャッシュを共有する。** `Evaluator` は `&mut self` で持ち回されるので、
  シェル評価がレイヤーネットワークを再帰評価した結果はキャッシュに載って
  いる。同じパスのノードを続けて引くのは**キャッシュヒット**になり、
  二重評価しない
- 評価モデルを変えない（3 の重さが無い）

**残る作業**は `EvalRequest` への列の追加、ワーカーループでの
`evaluate_at` 呼び出し、`EvalUpdate` がそれらの結果を
（`results` の位置規約を壊さずに）返すこと。`ravel-core` の公開 API 変更を
伴うので、単位 3 の冒頭でこれを入れる。

**この判断は単位 3 のスコープであって単位 2 では触らない。** 単位 2 が
入れた集約・スコープ判定・結果の受け渡しは、ネットワークスコープの要求に
対して正しく動くことをテストで固定してある。**コンプ要求の側では何も載らない
ことを（ID が衝突するターゲットを使って）テストで固定してあり、機構は単位 3
がネットワークスコープの要求を出すまでドーマントである。**

**この判断は単位 3 のスコープであって単位 2 では触らない。** 単位 2 が
入れた集約・スコープ判定・結果の受け渡しは、ネットワークスコープの要求に
対して正しく動くことをテストで固定してある。**コンプ要求の側では何も載らない
ことを（ID が衝突するターゲットを使って）テストで固定してあり、機構は単位 3
がネットワークスコープの要求を出すまでドーマントである。**

- 評価済み Geometry から bbox・点・パスを描く
- `viewer.rs:2388-2423` の type_key match を削除し、`:453` / `:527` の
  ドラッグ経路も評価済み bounds に切り替える
- 表示要素のトグル（bbox / 点 / パス / 属性値）を用意する

**完了条件**

- 新しい shape ノードを追加しても bbox が出ることのテスト
  （`type_key` を知らないノードで bbox が描かれる）
- `geometry.transform` を経た形状の bbox が変換後になるテスト
- `scatter.*` の全インスタンスが点として描かれるテスト

### 単位 4: Field オーバーレイ

- 選択ノードの FIELD 出力（`DataTypeId::FIELD`）をコンプ空間のグリッドで
  サンプルし、ヒートマップとして描く
- 表示モード: ヒートマップ / 等値線 / ベクトル矢印（ベクトルフィールドは
  `vector-field-plan.md` の VEC-1 が入ってから有効化）
- サンプル解像度は表示サイズから決め、上限を設ける。ズームしても
  サンプル数が発散しないようにする
- カラーマップと不透明度をパネルのトグルで調整する

**完了条件**

- スカラーフィールドがヒートマップとして描かれるテスト
  （既知の解析的フィールドで、特定座標の色が期待値になる）
- サンプル数が上限を超えないテスト
- FIELD 出力を持たないノードを選んでもオーバーレイが出ないテスト

### 単位 5: `ParamRole` とマニピュレータ

- `NodeTemplate` に `ParamRole`（`Position` / `Direction` / `Size` / `Angle`）を
  宣言する API を追加し、組み込みノードに付与する
- `ParamManipulator` オーバーレイ: 選択ノードの `ParamRole` 宣言から
  ハンドルを生成し、ドラッグをパラメータ更新に翻訳する
- キーフレーム付きパラメータのドラッグは平坦化せず現在フレームにキーを
  挿入・更新する（Properties のスクラブと同じ規約。`ui-impl-status.md`
  「アニメーションチャネル保持」）
- 殻の変換が掛かっているレイヤーでは、ハンドル位置に殻の行列を適用する
  （bbox が既にやっている `:1937-1941 transform_rect` と同じ扱い）

**依存**: `vector-field-plan.md` 単位 A（Vec パラメータ正規化）

**完了条件**

- `shape.rect` の `center` をドラッグして位置が変わるテスト
- ドラッグ 1 ジェスチャが 1 undo になるテスト
- キーフレーム付き `center` のドラッグがチャネルを平坦化しないテスト
- 殻の変換が非恒等なレイヤーでハンドルが図形に重なるテスト
- **`shape.line` / `shape.grid` が `ParamRole` を宣言していることのテスト。**
  `geometry-ops-plan.md` の単位 11 がこの条件を持っていたが、`ParamRole` は
  この単位が入れる型なので、宣言とテストはここで回収する

### 単位 7: レイヤー殻のマニピュレータとドラッグ HUD

bbox の 8 ハンドルを機能させ、殻 transform を Viewer で掴めるようにする。

- `ShellManipulator` オーバーレイ: `LayerTransform` の
  `scale`（角・辺のハンドル）、`rotation`（bbox 外側のドラッグ）、
  `anchor_point`（アンカーマーカーのドラッグ）、`position`（bbox 内側の
  ドラッグ。既存の移動を機構へ載せ替え）を対象にする
- Shift で縦横比固定、Alt でアンカー基準など修飾キーの規約は Timeline の
  トリム / Viewer のシェイプ描画と揃える
- ドラッグ中はスクリーン空間の HUD にデルタを出す（位置なら座標、スケールなら
  倍率、回転なら角度）。**HUD はこの単位で入れる**（掴めるようになって初めて
  数値が必要になる）
- キーフレーム付きチャネルのドラッグは平坦化せず現在フレームにキーを
  挿入・更新する（単位 5 と同じ規約）
- 親を持つレイヤーは親の変換込みで見た目に一致させる（bbox が既に
  `transform_rect` で行っている扱いと同じ）
- **親子リンクの線**をこの単位に含める: 子のアンカーから親のアンカーへ線を引く。
  親の設定 UI 自体は `layer-shell-wiring-plan.md` の `SHELL-5`（Properties の
  Parent ドロップダウン）が持つので、**線の表示だけを引き受ける**
- `paint_selection_bbox` の doc コメント（`viewer.rs:2751-2752`）が
  「レイヤー単位のスケールジェスチャーが無い」と書いている前提はこの単位で
  変わるので、コメントも同時に直す

**依存**: 単位 1（`OverlayEdit` の殻対応と スクリーン空間描画）。
**`vector-field-plan.md` 単位 A には依存しない**（殻は最初から
`[AnimationChannel; 2]`）。

**完了条件**

- 角ハンドルのドラッグで `scale` が変わり、Shift で縦横比が固定されるテスト
- bbox 外側のドラッグで `rotation` が変わるテスト
- アンカー移動が見た目の位置を動かさない（`position` を補正する）テスト
- 1 ドラッグ = 1 undo、Esc で revert のテスト
- キーフレーム付き `scale` のドラッグがチャネルを平坦化しないテスト
- 親を持つレイヤーでハンドルが見た目に一致するテスト
- **ハンドルの見た目が動作を約束するようになったこと**を確認したうえで、
  `done/pointer-feedback-plan.md` が保留した `Resize*` / 回転カーソルをこの単位で付ける

### 単位 9: モーションパス（軌跡とキー位置のドラッグ）

位置キーフレームの軌跡を Viewer に描き、キー位置を直接動かせるようにする。

- 軌跡は `position` の 2 チャネルを表示範囲でサンプルした折れ線として描く。
  **評価要求を出さない**（チャネルの直読みで足りる）
- キーが打たれているフレームの点を描き、ドラッグで両成分のキー値を書く
- 空間ベジェのハンドルは**持たない**（下記「非対象」のモーションパス項）
- 軌跡の表示範囲はレイヤーの表示区間 `[in, out)`。サンプル数に上限を設ける
- 殻の親を持つレイヤーは親の変換を掛けた位置で描く（単位 7 と同じ扱い）

**依存**: 単位 1、単位 7（殻チャネルを編集する `OverlayEdit`）

**完了条件**

- キー 2 点の線形補間で軌跡が直線になるテスト
- Bezier 補間のチャネルで軌跡が曲線としてサンプルされるテスト
- キー点のドラッグが `position` の両成分に同一フレームのキーを書くテスト
- ドラッグ 1 ジェスチャ 1 undo のテスト
- サンプル数が上限を超えないテスト
- 軌跡の描画が追加の評価要求を発行しないテスト

### 単位 8: ジオメトリ属性の空間可視化

単位 3 の Geometry オーバーレイ（bbox / 点 / パス）に、属性そのものの
可視化を足す。

- ベクトル属性の矢印（`N` / 接線 / `up` / 速度など、`Vec2` / `Vec3` の属性を
  選んで矢印で描く）
- 要素 index のラベル（スクリーン空間。表示数に上限を設ける）
- グループ（`evaluation-scope-plan.md` の group 規約）ごとの色分け
- 属性値のテキスト表示は**数値一覧ではない** — 空間上の 1 点に添える形に
  限る（一覧は `attribute-spreadsheet-plan.md` の担当）

`particle-plan.md` の速度ベクトル表示（同計画が非対象としたトレイルとは別）は
この単位に乗る。専用の描画経路を作らない。

**依存**: 単位 3

**完了条件**

- 指定した `Vec2` 属性が矢印として描かれるテスト（既知の値で向きと長さを検証）
- 要素数が上限を超えたときラベルが間引かれるテスト
- 属性を持たないジオメトリで何も描かれないテスト
- 表示のオン / オフがトグルで切り替わること

### 単位 6: レジストリ / ロケール / 文書

（ID は着手順ではない。文書更新は最後に行う。）

- オーバーレイのトグル項目とマニピュレータのロケール
- `docs/gpui-ui-guide.md` にオーバーレイの追加手順を記載
- `docs/ui-impl-status.md` の Viewer 表を更新（bbox ハンドルが**動作を持つ**
  ようになったことと、単位 7 / 8 の追加分）
- `docs/specifications/ui-spec.md` のビューア節にオーバーレイ機構と
  マニピュレータを追記

## 候補の棚卸し

2026-07-30 に Viewer の実装・全計画・REQ-UI を突き合わせて作った一覧。
**「Viewer に重ねるべきもの」の正はこの表**とし、新しい候補が出たらここに足す。

| 候補 | 引受先 |
|---|---|
| 比率グリッド / セーフエリア | 実装済み。単位 1 で載せ替え |
| 選択 bbox | 実装済み。単位 1 → 単位 3 で実データ化 |
| パス編集ハンドル | 実装済み。単位 1 で載せ替え |
| 評価エラー表示 | 実装済み。単位 1 で載せ替え |
| Field の可視化 | 単位 4 |
| ノードパラメータのマニピュレータ | 単位 5 |
| **レイヤー殻の scale / rotation / anchor** | **単位 7**（新規） |
| **ドラッグ中の数値 HUD** | **単位 7**（新規） |
| **ジオメトリ属性の矢印 / index ラベル / group 色分け** | **単位 8**（新規） |
| **モーションパス（軌跡表示＋キー位置のドラッグ）** | **単位 9**（新規）。空間ベジェは持たない |
| **親子リンクの線** | **単位 7** に同居（親の設定 UI は `layer-shell-wiring-plan.md` の `SHELL-5`） |
| パーティクルの速度ベクトル | 単位 8 に乗る（`particle-plan.md` は独自経路を作らない） |
| マスク / ロトのパス編集 | `effects-library-plan.md` FX-4。パス編集オーバーレイの拡張として設計する（1 レイヤーに複数パスが付くので選択粒度が増える） |
| テキストのキャレット / ベースライン / 字送りハンドル | `typography-plan.md`。**キー入力とフォーカスを取るオーバーレイ**という別種の設計判断を含むので本計画では扱わない |
| 3D のカメラ視錐台 / ライト / 軸ギズモ | `3d-scene-plan.md`。**同計画にオーバーレイ単位があるか未確認** — 無ければどちらにも属していないので、3D 着手時に本機構へ載せる形で単位を作る |
| ドラッグ中のスナップ / 整列ガイド | `viewer-snap-guides-plan.md` `SNAP-1`（吸着線の描画は本機構に乗る） |
| 定規とユーザーガイド | `viewer-snap-guides-plan.md` `SNAP-2`（`Composition` への追加フィールド、format v4 据え置き） |
| アルファのチェッカーボード | `viewer-inspection-plan.md` `INSP-1`。**本機構には乗らない** — 絵の下に敷くものなので背景の描画モード |
| チャンネル単独表示（R/G/B/A） | `viewer-inspection-plan.md` `INSP-2`。表示経路の変換であって重ね描きではない |
| カーソル下のピクセル値読み取り | `viewer-inspection-plan.md` `INSP-3`（表示は本機構のスクリーン空間描画に乗る） |
| ボックス選択（ラバーバンド）の枠 | `viewer-tool-extensions-plan.md` `TOOLX-2`（枠の描画は本機構に乗る） |
| プロキシ / 解像度低下 / ドロップフレーム / キャッシュ状態 | `viewer-inspection-plan.md` `INSP-4` |
| ROI（作業領域）枠 | **見送り**。部分レンダリングの機能が無いので枠だけ描いても使われない。`cache-plan.md` / `done/render-export-plan.md` が部分評価を持った時点で引き取る |
| 3D 以外で本機構に乗らないもの | チェッカーボードとチャンネル表示の 2 件だけ。それ以外の可視化はすべてこの trait を通す |

## 検証

- `mise run check`
- 座標変換とヒットテストの優先順位は純粋関数として `ravel-app` の
  ユニットテストで覆う。GPUI テストはドラッグの入力経路に限る
- Field オーバーレイは解析的フィールドで数値検証する（ゴールデン画像を
  増やさない）

## 非対象

- **3D のマニピュレータ**。`3d-scene-plan.md` 待ち
- **属性スプレッドシート**。値の一覧は `attribute-spreadsheet-plan.md` の
  担当。本計画は空間上の可視化に限る
- **ペンツールの直接編集の拡張**。`done/tool-system-plan.md` が担当。
  本計画は path overlay を機構に載せ替えるだけで挙動を変えない
- **オーバーレイのユーザー定義**（スクリプトから追加）。REQ-CODE-001 待ち
- **モーションパスの空間ベジェ**。軌跡の表示とキー位置のドラッグは単位 9 で
  やるが、**空間補間のハンドルは持たない**。`position` は
  `[AnimationChannel; 2]` で各成分が独立した時間カーブを持つ形であり、AE の
  空間補間を入れるには `position` を「2 本の独立チャネル」から「空間キー列」へ
  変える必要がある。それはカーブエディタ・ドープシート・`keyframes` ヘルパ・
  永続化のすべてに波及するので、本計画では扱わない（2026-07-30 の判断）
- **親の設定 UI**。`layer-shell-wiring-plan.md` の `SHELL-5`。本計画は
  リンク線の表示だけを持つ
- **キー入力を取るオーバーレイ**（Viewer 上のテキスト編集）。
  `typography-plan.md` 側の課題
- **ROI 枠**。上記「候補の棚卸し」に見送りとして記録

棚卸し表からは**引受先が決まった候補も消さない** — 消えると同じ空白が
再発見されるだけになる。
