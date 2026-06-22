# タスク仕様

## 目的

Ravelノードグラフの基盤となる型システムとデータモデルを定義し、Cargoワークスペース構成を確立する。全後続タスクの依存先となるコア型・構造体・トレイトを提供する。

## 要件

- [ ] NodeDataトレイト階層の定義（BufferData, TemporalData, GeometricData, NumericData, AudioData, TextData）
- [ ] 具象型の実装（FrameBuffer<f32>, Scalar, Vec2, Vec3, Vec4, Color, TimeCode等）
- [ ] Node / Edge / Graph構造体の定義（Arc共有）
- [ ] NodeId / EdgeIdの型定義（型安全なnewtypeパターン）
- [ ] グラフのトポロジカルソートアルゴリズム実装
- [ ] Cargoワークスペース構成（ravel-core, ravel-gpu, ravel-ui, ravel-media, ravel-app）
- [ ] 全ソースファイルにライセンスヘッダ付与

## 受け入れ基準

- NodeDataトレイトを実装した各具象型がコンパイル・テスト通過
- Graph構造体にノード追加・エッジ接続・トポロジカルソートが動作
- NodeId/EdgeIdが型レベルで混同不可能
- `cargo build --workspace` が全クレートで成功
- ユニットテストカバレッジ：型変換・グラフ操作・ソート結果検証

## 参考情報

- docs/specifications/architecture.md
- docs/specifications/data-model.md
- REQ-CORE-001（ノードグラフモデル）
- REQ-CORE-003（型システム）
- REQ-CORE-009（ワークスペース構成）
