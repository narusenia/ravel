# Properties の複合パラメータエディタ 実装計画

> **Status**: Planned — 2026-07-29

対象: カーブとカラーランプを Properties パネルで直接編集できるようにする。
関連要件: REQ-UI-002、REQ-UI-012、REQ-CORE-012、REQ-MOGRAPH-001。

## 問題

`PropertyField`（`crates/ravel-ui/src/properties/mod.rs:23-72`）はスカラー系の
固定バリアントしか持たない。Float / Int / Bool / String / Enum / Color /
Vector / ReadOnly の 8 つで、**行の追加・削除を伴う構造的なパラメータを
編集する手段が無い**。

帰結は既に出ている。

### 1. カーブが文字列で書かれている

`field.curve_remap` は制御点を**文字列パラメータ**として持つ
（`crates/ravel-core/src/registry/builtin.rs:206`）。

```rust
.with_param(string_parameter("points", "0:0,1:1"))
```

Properties では `PropertyField::String`（`properties/node.rs:98`）になり、
`"0:0,0.5:0.8,1:1"` を**手打ちする**ことになる。構文エラーの検出も、
カーブの形の確認も、キーフレームもできない。

`field.curve_remap` は変調の中核ノードで、`per-instance-modulation-plan.md` が
用意する変調層の要になる。ここが手打ちのままでは変調システム全体が使えない。

### 2. カラーランプの置き場所が無い

`style-attributes-plan.md` に追加する `field.ramp`（位置 → 色のランプ）は、
複数ストップ × 色 × 位置という構造を持つ。文字列に押し込む選択肢を取ると
問題 1 を再生産する。

### 3. カーブエディタの資産が Timeline に閉じている

再利用可能なカーブウィジェットは**既にある**。
`crates/ravel-app/src/widgets/curve_editor.rs` が `CurveSeries` / `CurveEdit` /
`CurveTransform` / ヒットテスト / ドラッグ / `curve_editor_canvas_with_x_scale`
（`:691`）を公開しており、Timeline はこれを使っている（`timeline.rs:2836`）。

つまり足りないのは**ウィジェットではなく、Properties 側の受け皿と
パラメータ表現**。

## 決定事項

### 構造的パラメータは文字列にしない

`ParameterValue` に専用バリアントを追加する。前例は `PathPoints`
（`properties/node.rs:150` で ReadOnly 表示されている、ペンツール専用の
構造的パラメータ）。同じ枠組みに乗せる。

```rust
enum ParameterValue {
    // ...
    PathPoints(Vec<PathPoint>),   // 既存
    Curve(CurveParam),            // 追加
    Ramp(RampParam),              // 追加
}
```

`field.curve_remap` の `points` 文字列は**ロード時マイグレーションで
`Curve` に変換する**。旧形式で保存されたプロジェクトは開ける。

`Ramp` は新規ノード（`field.ramp`）のためのものなので互換の問題が無い。

### 編集はポップオーバー、Properties 行にはプレビューを出す

Properties の 1 行にカーブエディタを埋め込むと縦幅を食い、他のパラメータが
見えなくなる。行にはサムネイル（カーブの形 / グラデーションの帯）を出し、
クリックでポップオーバーを開いて編集する。

Timeline のカーブエディタと**同じウィジェットを使う**ので、操作
（点の追加・削除、接線ドラッグ、補間切り替え）は Timeline と一致する。
覚え直しが発生しない。

### キーフレームは v1 では扱わない

カーブとランプ自体をアニメートする（カーブがフレームごとに変わる）のは
v1 では扱わない。ダイヤボタンは出さない。`AnimationChannel` を入れ子に
するかどうかの決着が必要で、それは REQ-CORE-007 の範囲を広げる判断になる。

### 縦方向のビュー状態はウィジェット側の責務にしない

`widgets/curve_editor.rs` は値域を**呼び出し側から受け取る**設計で、
Timeline はそれを `curve_value_range` で持っている（ただし現状 `Some` を
代入する経路が無く、縦ズームは未実装。`issues/medium/` に起票済み）。

Properties 側も同じ形にする。縦ズームの実装は Timeline 側の起票分と
**同じ仕組みを共有する**（ウィジェットに縦ズーム状態を持たせない）。

## 実装単位

### 単位 1: `ParameterValue::Curve` とマイグレーション

- `CurveParam`（制御点列 + 補間種別。既存の `AnimationChannel` のキーフレーム
  表現と型を揃える）
- `field.curve_remap` のテンプレートを `Curve` に切り替える
- ロード時マイグレーション: `points` 文字列 → `Curve`。パース不能な値は
  既定カーブ（0:0, 1:1）にフォールバックし、警告する
- プロセッサ側の読み出しを `Curve` に切り替える

**完了条件**

- 旧形式（文字列）で保存されたプロジェクトが開き、同じカーブとして
  評価されるテスト
- パース不能な文字列で開けて既定カーブになるテスト
- ラウンドトリップ（保存 → ロード）で制御点が保存されるテスト

### 単位 2: カーブエディタのポップオーバー

- `PropertyField::Curve` バリアントと、行のサムネイル描画
- クリックでポップオーバーを開き、`widgets/curve_editor.rs` を配置する
- 編集は 1 ジェスチャ 1 undo（既存のスクラブと同じ規約）

**完了条件**

- `field.curve_remap` を選ぶとカーブ行が出る `ravel-ui` テスト
- 点の追加・移動・削除が Document に反映され、1 ジェスチャ 1 undo になるテスト
- ポップオーバーを閉じても値が保持されるテスト

### 単位 3: `ParameterValue::Ramp` と `field.ramp`

- `RampParam`（位置 + 色のストップ列 + 補間種別）
- `field.ramp` ノード: スカラー入力（または `P` からの座標）→ Color
  出力のフィールド。仕様の詳細は `style-attributes-plan.md` が持つ
- Color を返すフィールドなので、`field.apply` の Color ターゲットに
  そのまま入る（`crates/ravel-core/src/geometry/field.rs:592` の
  `component_arity` は Color を 4 として扱う）

**依存**: `style-attributes-plan.md` の `field.ramp` 単位

**完了条件**

- ランプが位置に応じた色を返すテスト（既知のストップで特定位置の色が期待値）
- `field.apply(target = "Cd")` で色相が変化するテスト（スカラーフィールドでは
  グレースケールにしかならない現状との差分を pin する）

### 単位 4: グラデーションエディタのポップオーバー

- `PropertyField::Ramp` バリアントと、行のグラデーション帯プレビュー
- ポップオーバー: 帯の上のストップをドラッグで移動、ダブルクリックで追加、
  ストップ選択で色を編集（既存の `ColorPicker` を使う。
  `crates/ravel-app/src/panels/properties.rs:262`）
- 補間種別（linear / smooth / constant）の切り替え

**完了条件**

- ストップの追加・移動・削除・色変更が Document に反映されるテスト
- ストップが 1 個以下にならないことのテスト
- 位置が範囲外にならないことのテスト

### 単位 5: 縦ズームの共有

- カーブエディタの値域をビュー状態として持つ仕組みを 1 箇所に置き、
  Properties と Timeline の両方から使う
- Timeline 側の `curve_value_range`（`panels/timeline.rs:241`）を
  その仕組みに載せ替え、`fit_curve_values`（`:948`）を意味のある操作にする

**完了条件**

- ホイール / ピンチで縦方向にズームでき、Fit で自動範囲へ戻るテスト
- Timeline と Properties で同じ操作系になっていることのテスト

### 単位 6: ロケール / 文書

- ポップオーバーのラベルとツールチップ
- `docs/ui-impl-status.md` の Properties 表を更新
- `docs/agent-api-reference.md` に新しい `ParameterValue` バリアントを記載

## 検証

- `mise run check`
- パラメータ表現とマイグレーションは `ravel-core` の headless テストで覆う
- ウィジェットのヒットテストとドラッグは既存の `widgets/curve_editor.rs` の
  テストを流用する。GPUI テストはポップオーバーの開閉に限る

## 非対象

- **カーブ / ランプ自体のアニメーション**。上記の決定事項どおり v1 では扱わない
- **プリセット（イージングライブラリ）**。値の表現が固まってから
- **`PathPoints` のエディタ化**。Viewer のペンツールが担当
  （`done/tool-system-plan.md`）。Properties 側は現状の ReadOnly 表示を維持する
- **LUT の編集**。`effects-library-plan.md` が `.cube` 読み込みとして扱う
