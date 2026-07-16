# TASK-040: 汎用フィールド型 + ビルトインフィールド
- **マイルストーン**: MS5 Motion Graphics（前提基盤）
- **関連要件**: REQ-CORE-012
- **規模**: L
- **依存タスク**: TASK-038

## 概要
`Field` trait（位置列 + `EvalContext` → 値列のバッチ純関数）と、それを
ノード間で流す `FieldValue` 型を実装する。ビルトインとしてノイズ
（simplex/fbm）、フォールオフ（球/線形/パス距離）、カーブリマップ、画像
サンプラ、Lua 式、フィールド合成（Add/Multiply/Max/Blend）を提供し、
属性変調ノード（`attr = field(P)`）で Geometry に適用する。

## 実装ステップ
1. `Field` trait + `FieldValue`（`Arc<dyn Field>`）の NodeData 統合
2. ノイズフィールド（simplex + fbm、seed/周波数/オクターブ）
3. フォールオフフィールド（球/線形/パス距離、inner/outer + カーブ）
4. カーブリマップ・画像サンプラ（FrameBuffer UV 参照）
5. Lua 式フィールド（位置・属性を引数に取る）
6. 合成ノード（Add/Multiply/Max/Blend）
7. 属性変調ノード（対象属性名 + フィールド入力 + ブレンド量）
8. 統一チャネル（REQ-CORE-007）の値ソースとしての接続
9. ユニットテスト（決定性、合成則、変調適用）

## 対象コンポーネント
- `crates/ravel-core/src/geometry/field.rs`
- `crates/ravel-nodes/src/field/`（processor 群）
- `crates/ravel-core/src/animation/channel.rs`（値ソース統合）

## 完了条件
- [ ] フィールドがノード間を流れ、合成できる
- [ ] ビルトイン5種 + 合成4種が動作する
- [ ] 任意のジオメトリ属性をフィールドで変調できる
- [ ] 統一チャネルの値ソースとして接続できる
- [ ] 同一 seed で決定的（テストで担保）
