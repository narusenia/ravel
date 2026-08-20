# Int / String のキーフレーム 実装計画

> **Status**: `DISK-1`〜`DISK-6` 実装済み — 2026-08-21（`.ravprj` v10 / v11）

対象: `ravel-core` の `ParameterValue` と解決層、`ravel-ui` のキーフレームモデル、
`ravel-app` の Timeline とカーブエディタ。要件は `REQ-CORE-010` の周辺
（属性とアニメーション）。

## 問題

### アニメーションが f32 にしか存在しない

`ravel-core/src/animation/channel.rs:173` の `ChannelSource` は
`Constant(f32)` / `Keyframes(KeyframeCurve)` で、`evaluate` は `f32` を返す。
`ParameterValue`（`graph.rs:205`）で animatable なのは
`Channel` / `Channel2` / `Channel3` / `Channel4` の 4 つだけで、
**`Int` と `String` は素の値**。

`ParameterValue::channels()`（`graph.rs:253`）の doc がそれを明言している:

> `None` for the kinds that carry no float components (`Int`, `Bool`, `String`,
> `PathPoints`, `Curve`).

結果として、繰り返し回数・分割数・列挙の選択・テキストといった
**「モーショングラフィックスで最も動かしたいもの」が動かせない**。

### 総称化は現実的でない

`AnimationChannel` の参照は **382 箇所**（非テスト、計画時点の実測）。
`AnimationChannel<T>` にすると全部が影響を受け、評価器・GPU 経路・
Timeline・カーブエディタが同時に不安定になる。

## 決定事項

### Int は f32 カーブを読み出しで丸める

新しい `ParameterValue::IntChannel(AnimationChannel)` を足す。中身は
**既存の f32 チャンネルそのもの**で、解決時に `round()` して `i32` にする。

これは新機軸ではなく**既にある挙動の一般化**である。`eval.rs:2788` は、
接続されたスカラを Int パラメータへ渡すときに既に
`ResolvedValue::Int(s.0.round() as i32)` としている。ワイヤ経由で
できていたことを、キーフレーム経由でもできるようにするだけ。

副産物として仕様が素直に出る:

- **カーブエディタは階段に見える** — 下地は連続な f32 カーブで、
  描画時に丸めた値をプロットする
- **ベジエは近似** — 制御点は f32 のまま置ける。整数格子に載らない部分は
  丸めが吸収する
- 既存のカーブ編集 UI（`PARAM-2` / `PARAM-5`）が**そのまま使える**

### String は Step 専用の別トラック

f32 に載らないので `ParameterValue::StringSteps(StepCurve<String>)` を足す。
`channels()` は `None` を返し続けるので、**カーブエディタは String を見ない**。
Timeline には行が出て、キーの打ち直しと移動と削除だけができる。

補間は Step のみ。文字列の中間値は定義できない。

### 既存の `Int` / `String` は定数として残す

`Float` と `Channel` が既に「同じ float の 2 つの綴り」として共存しているので、
その対称にする。**移行は実質不要** — 既存の `Int` / `String` はそのまま読め、
ユーザーがキーフレームを打った瞬間だけ再型付けする。
`attribute.set` の `value` が `type` に従って再型付けされるのと同じ既存パターン。

### 差し込み口は `channels()` / `from_channels()`

昇格の対は `ParameterValue::channels()` と `from_channels()` で、
呼び出しは **12 箇所と 5 箇所**（計画時点の実測）。`from_channels` は
今は要素数だけで型を決めているので、**元の値の型を受け取って再型付けする**形に変える。

### 読み出し側は 1 箇所で吸収する

`ParameterValue` → `ResolvedValue` の変換は `eval.rs:2603` の 1 箇所しかない。
ここでフレームを見て `IntChannel` / `StringSteps` を評価すれば、
`ResolvedParams` には今までどおり `ResolvedValue::Int` / `Str` が入る。

**`i32_or` / `str_or` の 41 箇所の呼び出しは 1 つも変わらない。**
ノードプロセッサは自分のパラメータがアニメートされたことを知らない。

## 実装単位

| ID | 単位 | 依存 |
|---|---|---|
| DISK-1 | `IntChannel` と解決層（UI なし、フォーマット上げ） | — |
| DISK-2 | `StepCurve<String>` と `StringSteps` | DISK-1 |
| DISK-3 | Properties のキーフレームトグルと再型付け | DISK-1, DISK-2 |
| DISK-4 | Timeline の行とキーフレーム編集 | DISK-3 |
| DISK-5 | カーブエディタの階段描画（Int のみ） | DISK-4 |
| DISK-6 | ロケール / 文書 | DISK-1〜5 |

### 単位 1: `IntChannel` と解決層

- `ParameterValue::IntChannel(AnimationChannel)` を**末尾に追加**する
  （bincode の位置インデックスを動かさない。`PathPoints` / `Curve` と同じ理由）
- `channels()` が `IntChannel` の中身を返す。`from_channels()` を
  「元の値の型に従って再型付けする」形へ変える
- `eval.rs:2603` でフレーム評価して `ResolvedValue::Int` にする。丸めは
  `round()`（`eval.rs:2788` の既存の綴りに合わせる）
- `.ravprj` フォーマットを 1 つ上げる。**採番はマージ順** — 着手時に
  `manifest.rs` の `CURRENT_FORMAT_VERSION` を見て決める
  （`asset-identity-plan.md` / `parameter-groups-plan.md` の `PGRP-4` / `CM-2` と競る）

**完了条件**

- 定数 `Int` のノードが 1 つも挙動を変えない（既存テスト全通過）
- `IntChannel` にキーフレームを持たせたノードが、フレームごとに違う
  `i32` をプロセッサへ渡すヘッドレステスト
- 丸めの境界（`.5`、負値）のテスト
- 旧 `.ravprj` が読め、ラウンドトリップする

### 単位 2: `StepCurve<String>` と `StringSteps`

- `StepCurve<T>` を `ravel-core/src/animation/` に置く。キーは
  `(frame, T)` の整列済み列で、評価は「そのフレーム以下で最大のキー」
- `ParameterValue::StringSteps(StepCurve<String>)` を末尾に追加
- `channels()` は `None` のまま（カーブエディタへ出さない）

**完了条件**

- フレームをまたいで文字列が切り替わるヘッドレステスト
- 最初のキーより前のフレームは最初のキーの値を返す
- 空の `StepCurve` が既定値へ落ちる

### 単位 3: Properties のキーフレームトグルと再型付け

> **識別子パラメータにトグルを出さないこと**（`DISK-1` のレビューで出た）。
> `layer.ref` の `layer` と `precomp` の `comp_id` は `Int` だが、参照先の
> **生の ID** であって数値ではない。アニメーションさせると
> `Document::id_watermarks` が予約すべき ID を 1 つに決められず（曲線は
> キーの間の値も本物なので有限個に落ちない）、参照先レイヤーの殻が変わった
> ときの無効化対象も決まらない。`DISK-1` は読み出しを
> `ParameterValue::static_identifier` に集約して**定数の `IntChannel` までは
> 正しく扱う**ようにしたが、アニメーションさせない責任はこの単位にある。

- `Int` / `String` の行にキーフレームのトグルを出す
- 打った瞬間に `Int` → `IntChannel`、`String` → `StringSteps` へ再型付けし、
  **最後のキーを消したら定数へ戻す**（f32 側の既存規則と揃える）
- 露出パラメータ宣言（`EXPO-*`）が同じキーを名指している場合の追随を確認する

**完了条件**

- トグルの往復で値が保たれる
- 1 ジェスチャ = 1 undo
- 露出宣言を持つパラメータで再型付けしても宣言が壊れない
- **識別子パラメータ（`layer.ref` の `layer`、`precomp` の `comp_id`）に
  トグルが出ない**

### 単位 4: Timeline の行とキーフレーム編集

- `ravel-ui/src/keyframes.rs` の `property_rows` が `IntChannel` と
  `StringSteps` を拾う。`row_channels` は `IntChannel` を返し、
  `StringSteps` は別の行種別にする（`Vec<&AnimationChannel>` に載らない）
- String の行は Step のみ。補間の切り替えメニューを出さない

**実装**: 行種別は `PropertyRow` のフィールドではなく述語
`keyframes::row_value_kind(layer, id) -> RowValueKind::{Float, Integer,
Steps}` にした。行の形（`channel_names` が 1 要素で `CHANNEL_VALUE`）は
f32 の単成分パラメータと同一で、描画・ヒットテスト・高さ計算はどれも
`channel_names.len()` を数えたままなので、**パネル側に行種別の分岐が
1 つも増えない**。パネルが `ChannelSource::Keyframes` を剥がしていた
箇所は 2 つのアクセサへ集約した — キーの列挙は
`row_key_frames(layer, id, component)`、レーン数は
`row_component_count(layer, id)`。`insert_keyframe` /
`remove_keyframe` / `move_keyframe` / `has_keyframe_at` は
`StringSteps` を内部でディスパッチする（String の insert は
「そのフレームの `sample()` を打ち直す」、最後のキーを消すと
`ParameterValue::String(default_value)` へ戻る）。ドラッグの
プレビューは基準線の型を `RowKeys::{Curve, Steps}` に広げ、
`preview_row_key_moves` が振り分ける。`RowKeys::curve()` が `None` を
返すことがカーブエディタの値軸と接線ジェスチャの出口を閉じ、
`RowValueKind::is_stepped()` が補間切り替え（ツールバーと
コンテキストメニューの両方）の出口を閉じる。識別子パラメータは
`property_rows` の側でも弾く（`is_identifier_parameter`）。

**完了条件**

- Int / String のキーフレームが Timeline に出て、移動・削除・追加ができる
- String の行に補間切り替えが出ない
- 既存の f32 行の挙動が変わらない

### 単位 5: カーブエディタの階段描画

- Int のカーブを**丸めた値**でプロットする。制御点は f32 のまま掴める
- String はカーブエディタに出さない

**実装**: サンプラー（`visit_curve_samples_for_view`）は無改変で、
**頂点列の生成を描画側の純粋関数** `curve_polyline_points(curve,
frame_offset, min_x, max_x, max_samples, integral)` に切り出した。
`integral` が立っているときだけ、丸めた値が変わるフレームで
「前の段を保持する角 → 立ち上がり」の 2 頂点を打つので、隣接フレーム間が
斜線にならず、段の境界が `round()` の境界と一致する。旗は
`CurveSeries::integral` として item が持ち、`TimelineCurveData::integral`
が `row_value_kind(...).is_integral()` から埋める。`integral = false` の
頂点列はサンプル 1 つに頂点 1 つで、f32 の描画は 1 ピクセルも変わらない。

**段の境界の精度はサンプラーが決める（受け入れた上限）。** 可視範囲が
サンプル予算より広いと `visit_curve_samples_for_view` が滑らかな区間を間引くので、
立ち上がりは**サンプルされたフレームにしか置けず、最大 1 サンプル分遅れる**。
これはベジエを折れ線で描くのと同じ近似で、描画側の予算（幅 1 px あたり
2 サンプル）では段の幅が半ピクセルを切るため見えない。不変条件は
「**描かれた立ち上がりは必ずサンプルされたフレーム上にあり、段の値は必ず整数**」で、
`a_decimated_int_staircase_keeps_its_risers_on_sampled_frames` が固定している。
全ての `round()` 境界を必ず描くには値域に比例した頂点が要る（値域は
有界でない）ので、要求が出るまで持たない。

**完了条件**

- Int のカーブが階段に見え、制御点のドラッグで段が動く
- 段の境界が `round()` の境界と一致する

### 単位 6: ロケール / 文書

**実装**: 新しいユーザー可視文字列は無い — Int / String の行は既存の
`timeline.channel.value` を使い、閉じた UI（補間切り替え）は文字列ではなく
出口が消える。したがってロケール資産の変更は無い。

**完了条件**

- `docs/specifications/data-model.md` の `ParameterValue` の記述が追随
- `docs/agent-api-reference.md` の該当表が追随
- `docs/ui-impl-status.md` の Timeline / カーブエディタ行が追随

## 非対象

- **`AnimationChannel<T>` の総称化**。382 箇所に触る割に、得るのは型の綺麗さだけ
- **Bool のキーフレーム**。`StepCurve<T>` が入れば機構としては足りるが、
  要求が出ていない。出たら `StringSteps` と同じ形で足す
- **String の補間**（クロスフェード等）。テキストアニメーションは
  `typography-plan.md` の領分
- **Int の補間モード選択**（丸め方を floor / ceil から選ぶ）。`round()` 固定。
  要求が出てから
