# Viewer オーバーレイ機構とマニピュレータ 実装計画

> **Status**: Planned — 2026-07-29

対象: Viewer のオーバーレイを拡張可能な機構にし、評価結果を根拠にした
可視化（Field / Geometry）と、パラメータを直接掴めるマニピュレータを載せる。
関連要件: REQ-UI-011、REQ-UI-013、REQ-CORE-010、REQ-CORE-012。

## 問題

### 1. オーバーレイが canvas クロージャに直書きされている

現状 Viewer が描くオーバーレイは 4 種類あり、すべて 1 つの paint クロージャに
並んでいる（`crates/ravel-app/src/panels/viewer.rs:1598-1621`）。

| オーバーレイ | 描画箇所 |
|---|---|
| 比率グリッド | `:1606 paint_proportional_grid` |
| セーフエリア | `:1609 paint_safe_areas` |
| 選択 bbox（ノード / レイヤー） | `:1611-1618 paint_selection_bbox` |
| パス編集ハンドル | `:1619-1620 paint_path_overlay` |

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

## 決定事項

### 機構を先に作り、既存 4 種を載せ替える（挙動不変）

オーバーレイを trait + レジストリにして、既存の 4 種をそれに載せ替える。
このフェーズでは**見た目と操作を一切変えない**。
`backlog.md` の PANEL-1「実効レイアウトの分離（挙動不変のリファクタ）」と
同じ進め方を採る。

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
       ├ FieldOverlay         (Field 評価結果)
       └ ParamManipulator     (ParamRole 宣言 + ハンドル)
             ↓ eval_target() を集約
       EvalRequest (multi-target)
             ↓ 結果はグローバル経由
       OverlayResults グローバル → paint / handles
```

## 実装単位

### 単位 1: オーバーレイ機構の抽出（挙動不変）

- `ViewerOverlay` trait、`OverlayContext`、`OverlayPainter`、`OverlayHandle`、
  `OverlayRegistry` を `crates/ravel-app/src/panels/viewer/overlay.rs` に置く
- 既存 4 種（グリッド / セーフエリア / 選択 bbox / パス編集）を載せ替える。
  `shape_node_bounds` はこの時点では触らず、そのまま bbox オーバーレイの
  データ源にする
- 入力側: ハンドルのヒットテストを優先度順の 1 経路に統合する

**完了条件**

- 4 種すべてについて、載せ替え前後で描画結果が一致するテスト
  （座標変換の性質テスト。ゴールデン画像は使わない）
- ハンドルのヒットテストが優先度順に解決されるテスト
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

### 単位 6: レジストリ / ロケール / 文書

- オーバーレイのトグル項目とマニピュレータのロケール
- `docs/gpui-ui-guide.md` にオーバーレイの追加手順を記載
- `docs/ui-impl-status.md` の Viewer 表を更新
- `docs/specifications/ui-spec.md` にオーバーレイ機構を追記

## 検証

- `mise run check`
- 座標変換とヒットテストの優先順位は純粋関数として `ravel-app` の
  ユニットテストで覆う。GPUI テストはドラッグの入力経路に限る
- Field オーバーレイは解析的フィールドで数値検証する（ゴールデン画像を
  増やさない）

## 非対象

- **3D のマニピュレータ**。`3d-basics-sketch.md` 待ち
- **属性スプレッドシート**。値の一覧は `attribute-spreadsheet-plan.md` の
  担当。本計画は空間上の可視化に限る
- **ペンツールの直接編集の拡張**。`done/tool-system-plan.md` が担当。
  本計画は path overlay を機構に載せ替えるだけで挙動を変えない
- **オーバーレイのユーザー定義**（スクリプトから追加）。REQ-CODE-001 待ち
