# Properties の複合パラメータエディタ 実装計画

> **Status**: In progress — 単位 1（`PARAM-1`）と単位 2（`PARAM-2`）が
> 実装済み（2026-07-31）。他の単位は未着手

対象: カーブとカラーランプを Properties パネルで直接編集できるようにする。
関連要件: REQ-UI-002、REQ-UI-012、REQ-CORE-012、REQ-MOGRAPH-001。

## 問題

`PropertyField`（`crates/ravel-ui/src/properties/mod.rs:23-72`）はスカラー系の
固定バリアントしか持たない。Float / Int / Bool / String / Enum / Color /
Vector / ReadOnly の 8 つで、**行の追加・削除を伴う構造的なパラメータを
編集する手段が無い**。

帰結は既に出ている。

### 1. カーブが文字列で書かれていた（単位 1 で解消）

`field.curve_remap` は制御点を**文字列パラメータ**として持っていた。

```rust
.with_param(string_parameter("points", "0:0,1:1"))
```

Properties では `PropertyField::String` になり、`"0:0,0.5:0.8,1:1"` を
**手打ちする**ことになっていた。構文エラーの検出も、カーブの形の確認も
できない。

単位 1 で `ParameterValue::Curve` に変わり、旧形式は `.ravprj` v5 → v6 の
ロード時変換が拾う。**残っているのは Properties 側の受け皿**（単位 2）で、
現状は読み取り専用サマリ（`N points`）が出るだけ。

`field.curve_remap` は変調の中核ノードで、`per-instance-modulation-plan.md` が
用意する変調層の要になる。

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

> 単位 2 の実装で分かったこと: 再利用できたのは `CurveTransform` と評価関数
> までで、ヒットテストとドラッグは整数フレーム軸（`CurveHit::frame: u64`）に
> 固く結び付いていて `CurveParam` の f32 軸には持ち越せなかった。詳細は
> 単位 2 の実装ノート。

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

### 編集はインライン展開（アコーディオン）。ポップオーバーにしない

行にサムネイル（カーブの形 / グラデーションの帯）を出し、クリックで
**その行の直下にエディタを展開する**。折り畳むと元の 1 行に戻る。

ポップオーバーを採らない理由が 3 つ:

1. **外側クリックで閉じる**。カーブ点をドラッグして形を決め、隣の数値
   パラメータを見て調整し、また点を動かす、という往復ができない。
   カーブとランプはまさにその往復で追い込むパラメータ
2. **Properties は既に Accordion を使っている**（`docs/ui-impl-status.md`
   「Accordion セクション ✅ Node Info / Parameters をデフォルト展開」）。
   展開・折り畳みが既存の操作語彙と一致する
3. **dispatch ツリーへの挿入タイミング問題を踏まない**。新しく生成した要素へ
   アクションを送る形は、`ScrubInput` の全選択が捨てられている問題
   （`issues/medium/app-shell.md` の MED-APP-18）と同じ罠。インライン展開は
   通常の要素ツリーに入るので、この経路自体が発生しない

縦幅は「既定は折り畳み + 展開時の高さをドラッグで変えられる」で足りる。
複数行を同時に展開できる（アコーディオンの排他にしない）。カーブと
ランプを見比べながら調整する需要があるため。

操作は Timeline のカーブエディタに合わせる（空所のダブルクリックで点を
追加するのは Timeline のグラフエディタと同じ）。ただし**ウィジェットの実装は
共有しない**: 単位 2 の実装ノートのとおり、`widgets/curve_editor.rs` は
整数フレーム軸に固く結び付いている。共有するのは座標変換
（`CurveTransform`）と評価関数で、この 2 つが一致していれば見た目と結果は
ずれない。

### 2 つの型に 6 つの消費者がいる

この 2 型を入れる価値は `field.curve_remap` 単体では測れない。カーブと
ランプはそれぞれ 3 つのドメインに現れ、**同じ表現と同じエディタを共有する**。

|  | 値ドメイン | Field ドメイン | Raster ドメイン |
|---|---|---|---|
| **Curve** | `math.curve`（Scalar→Scalar）<br>本計画 単位 7 | `field.curve_remap`<br>実装済み（文字列） | `comp.curves` トーンカーブ<br>`effects-library-plan.md` 単位 1 |
| **Ramp** | `color.ramp`（Scalar→Color）<br>本計画 単位 8 | `field.ramp`（Field→Color）<br>`style-attributes-plan.md` 単位 6 | `comp.gradient`<br>`effects-library-plan.md` 単位 3 |

6 つが別々の表現とエディタを持つ事態を避けるため、**型とエディタは本計画が
所有し、各ノードはそれを使う**。

インライン展開を選んだ判断はここでも効く。`comp.curves` はチャンネルごとに
`Curve` を持つ（4 本）ので 1 ノードに 4 つのカーブ行が並ぶ。ポップオーバーだと
4 本を見比べられない。

### `color.ramp` は Blender の ColorRamp と同じ位置づけ

スカラー 1 本を色に写す変換は、フィールドとは独立に必要になる。

```text
layer.info(index) → color.ramp → constant.color 相当の Color 出力
  → レイヤーごとに色相がずれる
net.in(t) → color.ramp → 時間で色が変わる
```

`field.ramp` は「要素ごとに位置で色を変える」もので、こちらは
「1 つのスカラーを 1 つの色にする」。出力型が Color フィールドと Color 値で
違うので実装は共有しないが、**ランプの評価関数は共有する**。

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

### 単位 1: `ParameterValue::Curve` とマイグレーション ✅

実装済み。

- `ravel_core::param_curve::{CurveParam, CurvePoint}`。制御点は
  `(x, y)` + 補間種別 + 接線で、`KeyframeCurve` / `Keyframe` と補間種別・
  接線規約・区間規約を共有する（両者とも
  `animation::interpolation::{linear_at, bezier_at}` を通る）。違うのは
  入力軸が整数フレームでなく任意スカラーである点だけ
- **定義域外は両端値にクランプ**（`field.curve_remap` の従来動作）。
  繰り返し / 延長は単位 7 の `math.curve` がノード側のパラメータで持つ。
  制御点が空なら恒等（`evaluate(x) == x`）
- `ParameterValue::Curve` は **`PathPoints` の後ろに追加**（bincode の位置
  索引を壊さないため）。`JOURNAL_FORMAT_VERSION` を 7 に上げた。
  `port_data_type()` は `None`、`ResolvedValue::Curve` として
  `params.curve(key)` でプロセッサに届く
- `field.curve_remap` のテンプレートと `CurveRemapField` を `Curve` に切り替え、
  文字列パーサ（`parse_curve`）と `remap_curve` を削除
- ロード時マイグレーション `Document::upgrade_curve_params`（`.ravprj`
  v5 → v6）。走査は `Document::map_graphs` +
  `composition::graph_walk::map_subnets` を v4 → v5 の畳み込みと共有する。
  パース不能な値は `CurveParam::identity()` にフォールバックし
  `tracing::warn!` を出す（部分パースはしない）
- Properties は読み取り専用サマリ（`PropertyField::ReadOnly` の `N points`）
  だった。単位 2 が `PropertyField::Curve` に置き換えた

**完了条件**

- 旧形式（文字列）で保存されたプロジェクトが開き、同じカーブとして
  評価されるテスト ✅（v5 の読み出し実装をテストに写して全域で突き合わせる）
- パース不能な文字列で開けて既定カーブになるテスト ✅
- ラウンドトリップ（保存 → ロード）で制御点が保存されるテスト ✅
- レイヤーネットワークと subnet 内側も移行されるテスト ✅

### 単位 2: カーブエディタのインライン展開 ✅

**単位 1 の成果物**: 行の値は `ParameterValue::Curve(CurveParam)`。
`CurveParam::points()` が制御点（`x` / `y` / `interpolation` / 接線）を返し、
`insert_point` / `remove_point` / `move_point` が並び順の不変条件を保ったまま
編集する。単位 1 が暫定で出していた `PropertyField::ReadOnly { value:
"N points" }` の行を置き換えた。

実装済み。

- `PropertyField::Curve` バリアントと、行のサムネイル描画（折り畳み時）✅
- クリックで行の直下にカーブエディタを展開する。展開状態はパネルが持つ
  （Document に入れない。ビュー状態なので undo の対象外）✅
- 展開時の高さをドラッグで変えられる ✅
- 複数行を同時に展開できる（排他にしない）✅
- 編集は 1 ジェスチャ 1 undo（既存のスクラブと同じ規約）✅

**エディタは `widgets/curve_editor.rs` そのものではなく、新しい
`widgets/param_curve_editor.rs`** になった。前者は `KeyframeCurve` 専用で、
ヒット識別子（`CurveHit::frame: u64`）・ドラッグの量子化（`to_frame` は
整数フレームに丸める）・サンプル間引きがすべて整数フレーム軸に固く
結び付いており、`CurveParam` の f32 軸へ一般化すると Timeline の
既存挙動とテストが変わる。**軸非依存な部分は共有する**:
`CurveTransform`（データ ↔ ウィジェット変換）をそのまま使い、評価は
`CurveParam::evaluate`（内部で `animation::interpolation::{linear_at,
bezier_at}` を通る = `KeyframeCurve::sample` と同じ関数）に委ねる。
第 2 の評価器は作っていない。縦方向のビュー状態は呼び出し側が渡す形
（`ParamCurveEditorState::value_range`）で、単位 5 の縦ズームはここに載る。

**ジェスチャ**: 点のドラッグで移動、空所のダブルクリックで追加（ポインタ
位置に置く）、点のダブルクリックで削除。制御点は最少 2 点を残す（0〜1 点の
カーブは恒等 / 定数になり、空のエディタと見分けが付かないため）。**接線
ドラッグと補間種別の切り替えは入れていない**（完了条件に無く、既定の
Linear カーブでは接線ハンドルが出ない）。単位 5 と合わせて追加する。

**展開状態の寿命**: パネルのターゲットが変わると展開と高さは捨てる。
`points` のような素のキーはどのノードのものか区別しないので、ターゲットを
跨いで持ち越すと無関係な行が開く。ノード選択を切り替えて戻ると折り畳んだ
状態で出る。

**完了条件**

- `field.curve_remap` を選ぶとカーブ行が出る `ravel-ui` テスト ✅
- 展開・折り畳みが値に影響しないテスト ✅
- 2 行を同時に展開できるテスト ✅
- 点の追加・移動・削除が Document に反映され、1 ジェスチャ 1 undo になるテスト ✅
- 展開状態が undo に積まれないテスト ✅
- ノード選択を切り替えて戻ったときの展開状態の扱いが定義どおりであるテスト ✅

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

### 単位 4: グラデーションエディタのインライン展開

- `PropertyField::Ramp` バリアントと、行のグラデーション帯プレビュー
- 展開部: 帯の上のストップをドラッグで移動、ダブルクリックで追加、
  ストップ選択で色を編集（既存の `ColorPicker` を使う。
  `crates/ravel-app/src/panels/properties.rs:262`）
- 補間種別（linear / smooth / constant）の切り替え
- 展開の挙動は単位 2 と共有する（高さのドラッグ、複数同時展開、
  ビュー状態は undo 対象外）

**完了条件**

- ストップの追加・移動・削除・色変更が Document に反映されるテスト
- ストップが 1 個以下にならないことのテスト
- 位置が範囲外にならないことのテスト
- 展開の挙動が単位 2 と一致するテスト

### 単位 7: `math.curve`（値ドメインのカーブ remap）

`ParameterValue::Curve` の値ドメインの消費者。既存の `math.remap`
（線形の入出力レンジ変換）に対して、任意形状のカーブで写す。

- `math.curve`: Scalar → Scalar。入力の正規化範囲（`in_min` / `in_max`）と
  出力範囲（`out_min` / `out_max`）をパラメータで持ち、その間を `Curve` で写す
- 範囲外の扱い: クランプ / 繰り返し / 延長（カーブ端の接線を延ばす）を
  パラメータで選ぶ
- `field.curve_remap` とは**出力型が違う**（Field → Field ではなく
  Scalar → Scalar）ので実装は共有しないが、**カーブの評価関数は共有する**。
  同じ制御点で同じ値が出ることをテストで pin する

**完了条件**

- 恒等カーブ（0:0, 1:1）で入力と一致するテスト
- 範囲外の 3 つのモードがそれぞれ定義どおりであるテスト
- 同じ制御点に対して `field.curve_remap` と同じ値を返すテスト
- カーブエディタが単位 2 と同じ行として出るテスト

### 単位 8: `color.ramp`（値ドメインのカラーランプ）

Blender の ColorRamp 相当。`ParameterValue::Ramp` の値ドメインの消費者。

- `color.ramp`: Scalar → Color。入力の正規化範囲（`in_min` / `in_max`）と
  範囲外の扱い（クランプ）を持つ
- `field.ramp`（`style-attributes-plan.md` 単位 6）とは出力型が違う
  （Color フィールド対 Color 値）ので実装は共有しないが、**ランプの評価関数は
  共有する**。同じストップで同じ色が出ることをテストで pin する
- アルファも出す（ストップは RGBA を持つ）。Color 型が α を含むので
  別ポートにはしない

**完了条件**

- 既知のストップで特定入力値の色が期待値になるテスト
- 同じストップに対して `field.ramp` と同じ色を返すテスト
- 入力が範囲外のとき両端にクランプされるテスト
- `layer.info(index) → color.ramp` でレイヤーごとに色が変わる結合テスト
  （`layer.info` は `scene-info-nodes-plan.md` 単位 2 が追加する）
- グラデーションエディタが単位 4 と同じ行として出るテスト

### 単位 5: 縦ズームの共有

- カーブエディタの値域をビュー状態として持つ仕組みを 1 箇所に置き、
  Properties と Timeline の両方から使う
- Timeline 側の `curve_value_range`（`panels/timeline.rs:241`）を
  その仕組みに載せ替え、`fit_curve_values`（`:948`）を意味のある操作にする

**完了条件**

- ホイール / ピンチで縦方向にズームでき、Fit で自動範囲へ戻るテスト
- Timeline と Properties で同じ操作系になっていることのテスト

### 単位 6: ロケール / 文書

**単位 1〜5 と 7〜8 のすべてを対象にする**（最後に実施する）。

- 展開部のラベルとツールチップ
- `docs/ui-impl-status.md` の Properties 表を更新
- `docs/agent-api-reference.md` に新しい `ParameterValue` バリアントを記載
  （`Curve` は単位 1 で記載済み。残りは `Ramp`）
- `effects-library-plan.md` 単位 1 のトーンカーブと単位 3 のグラデーションが
  本計画の `Curve` / `Ramp` を使うことを両計画に明記する

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
