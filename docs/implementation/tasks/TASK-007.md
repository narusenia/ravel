# TASK-007: アニメーションチャネルシステム
- **マイルストーン**: MS1 Foundation
- **関連要件**: REQ-CORE-007
- **規模**: M
- **依存タスク**: TASK-001, TASK-002

## 概要

全パラメータの値ソースを統一的に管理するアニメーションチャネルシステムを実装する。キーフレームカーブ（ベジエ/リニア/ステップ補間）を中核とし、将来的にLuaエクスプレッション、ノード出力接続、オーディオリアクティブ等の多様なソースに差し替え・ブレンド可能な拡張性を持たせる。

## 実装ステップ

1. **ChannelSource enumの定義**
   - `Constant(f32)` — 固定値
   - `Keyframes(KeyframeCurve)` — キーフレームカーブ
   - `Expression(placeholder)` — Luaエクスプレッション（MS6で実装、ここではenum定義のみ）
   - `NodeOutput(NodeId, OutputPort)` — 他ノードの出力値
   - `AudioReactive(placeholder)` — オーディオリアクティブ（MS5で実装、ここではenum定義のみ）
   - `Blend(Box<ChannelSource>, Box<ChannelSource>, BlendMode, f32)` — 2ソースのブレンド

2. **AnimationChannelの実装**
   - `AnimationChannel`構造体: `source: ChannelSource`
   - `evaluate(&self, frame: u64, ctx: &EvalContext) -> f32`
   - ChannelSourceのパターンマッチで各ソースの評価をディスパッチ
   - placeholderソース（Expression, AudioReactive）はデフォルト値を返す

3. **KeyframeCurveの実装**
   - `Keyframe`構造体: `frame: u64`, `value: f32`, `interpolation: Interpolation`, `tangent_in: Vec2`, `tangent_out: Vec2`
   - `Interpolation` enum: `Bezier`, `Linear`, `Step`（Hold）
   - ベジエ補間: 制御点を含む3次ベジエ曲線、De Casteljauアルゴリズムまたはニュートン法でt値を求めサンプリング
   - リニア補間: 前後キーフレーム間の線形補間
   - ステップ補間: 前キーフレームの値を保持（次キーフレームで即座に切り替え）
   - 範囲外アクセス: 最初のキーフレーム前は最初の値、最後のキーフレーム後は最後の値を返す

4. **キーフレーム操作API**
   - `insert(frame, value, interpolation)` — キーフレーム挿入（既存フレームは上書き）
   - `remove(frame)` — キーフレーム削除
   - `modify(frame, new_value, new_tangents)` — キーフレーム値/タンジェント変更
   - `move_keyframe(old_frame, new_frame)` — キーフレームのフレーム位置変更
   - キーフレーム列は常にフレーム順でソート維持
   - 操作はGraphMutation（TASK-004）としてジャーナル対象

5. **ユニットテスト**
   - リニア補間: フレーム0=0.0, フレーム10=1.0 → フレーム5で0.5
   - ベジエ補間: 既知の制御点に対して中間フレームの値が期待範囲内
   - ステップ補間: フレーム0=0.0, フレーム10=1.0 → フレーム5で0.0, フレーム10で1.0
   - 範囲外アクセス: 最初/最後の値が保持される
   - キーフレーム挿入/削除/修正後の評価結果
   - 空カーブの評価（デフォルト値0.0を返す）
   - Constantソースの評価
   - Blendソースの評価（2つのConstantのブレンド）

## 対象コンポーネント

- `crates/ravel-core/src/animation/mod.rs` — アニメーションモジュールエントリ
- `crates/ravel-core/src/animation/channel.rs` — AnimationChannel + ChannelSource
- `crates/ravel-core/src/animation/curve.rs` — KeyframeCurve + Keyframe
- `crates/ravel-core/src/animation/interpolation.rs` — 補間アルゴリズム（ベジエ/リニア/ステップ）
- `crates/ravel-core/src/animation/blend.rs` — BlendMode + ブレンド演算

## 完了条件

- [x] `ChannelSource` enumが全バリアントで定義されている
- [x] `AnimationChannel`がフレーム番号から値を評価できる
- [x] ベジエ補間が正確に動作する（精度テスト: 許容誤差1e-4以内）
- [x] リニア補間が正確に動作する（ユニットテスト）
- [x] ステップ補間が正確に動作する（ユニットテスト）
- [x] 範囲外アクセスで最初/最後の値が返される（ユニットテスト）
- [x] キーフレームの挿入/削除/修正が動作する
- [x] 空カーブの評価がデフォルト値を返す（ユニットテスト）
- [x] Blendソースで2ソースのブレンドが動作する（ユニットテスト）
- [x] placeholderソース（Expression, AudioReactive）がパニックせずデフォルト値を返す

> **実装メモ**: `NodeOutput` の実値解決はグラフ評価コンテキスト拡張（将来MS）待ちで現状 `DEFAULT_VALUE` を返す（order.md要件はenum定義のみのため範囲内）。`Expression`/`AudioReactive` はプレースホルダでデフォルト値返却。36ユニットテストパス、clippy clean（`-D warnings`）。
