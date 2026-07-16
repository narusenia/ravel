# TASK-042: ジオメトリラスタライズノード
- **マイルストーン**: MS5 Motion Graphics
- **関連要件**: REQ-MOGRAPH-001 (v2), REQ-CORE-009
- **規模**: M
- **依存タスク**: TASK-038

## 概要
`Geometry → FrameBuffer` の明示変換ノード。パスの塗り/ストローク（zeno）、
ポイントのスプライト描画、インスタンスのソースジオメトリ展開描画を行う。
`Cd`/`alpha`/`pscale`/`rot`/`scale` 標準属性を描画に反映する。32bit float
出力（REQ-CORE-009）。

## 実装ステップ
1. パス塗り/ストロークのラスタライズ（zeno、アンチエイリアス）
2. ポイントスプライト描画（pscale/Cd/alpha 反映）
3. インスタンス展開描画（source 参照 + per-instance transform）
4. wgpu 描画パス（storage buffer への属性アップロード）と CPU フォールバック
5. Layer コンパイラ（composition/compile.rs）の Shape ソース展開を
   `ShapeGeometry → Rasterize` チェーンへ変更
6. ゴールデンイメージテスト（既存シェイプ描画との視覚同等性）

## 対象コンポーネント
- `crates/ravel-nodes/src/rasterize/`
- `crates/ravel-gpu/src/`（ポイント/パス描画パイプライン）
- `crates/ravel-core/src/composition/compile.rs`

## 完了条件
- [ ] パス・ポイント・インスタンスがラスタライズされ合成に流れる
- [ ] 標準属性（Cd/alpha/pscale/rot/scale）が描画に反映される
- [ ] 既存 Shape Layer の見た目が維持される（ゴールデンテスト）
- [ ] GPU 経路と CPU フォールバックの結果が一致する
