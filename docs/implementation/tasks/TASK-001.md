# TASK-001: 型システム + ノードグラフデータモデル
- **マイルストーン**: MS1 Foundation
- **関連要件**: REQ-CORE-001, REQ-CORE-003, REQ-CORE-009, REQ-INFRA-008
- **規模**: L
- **依存タスク**: なし

## 概要

Ravelの全処理基盤となるノードグラフDAGのデータモデルと、ノード間を流れるデータの型システムを構築する。Rustのトレイトシステムを活用し、コンパイル時に型互換性を検証可能な階層型データモデルを定義する。併せてCargoワークスペース構成とライセンスヘッダの整備を行い、以降の全タスクの土台とする。

## 実装ステップ

1. **Cargoワークスペース構成**
   - ルート`Cargo.toml`にworkspace定義
   - クレート作成: `ravel-core`（型システム・グラフ・評価）、`ravel-gpu`（wgpu計算パイプライン）、`ravel-ui`（GPUI UI層）、`ravel-media`（FFmpeg・オーディオ）、`ravel-app`（バイナリエントリポイント）
   - 共通依存バージョンを`workspace.dependencies`で管理

2. **NodeDataトレイト階層の定義**
   - 基底トレイト `NodeData: Send + Sync + 'static`（`type_id()`, `type_name()`）
   - カテゴリトレイト:
     - `BufferData`（width, height, pixel_format）
     - `TemporalData`（duration, frame_rate）
     - `GeometricData`（bounds, transform）
     - `NumericData`（as_f32, dimensions）
     - `AudioData`（sample_rate, channels, samples）
     - `TextData`（as_str, style_info）

3. **具体型の実装**
   - `FrameBuffer`（RGBA f32ピクセルバッファ）、`DepthBuffer`、`MultiLayerBuffer`
   - `Clip`（フレーム列+メタデータ）、`TimeRemap`
   - `Shape`（2Dパス）、`Mask`、`Mesh3D`、`ParticleSystem`
   - `Scalar(f32)`、`Vec2(f32, f32)`、`Vec3`、`Vec4`、`Color(f32x4)`、`Curve(KeyframeCurve)`
   - `AudioBuffer`（PCM f32）、`SpectrumData`
   - `PlainText(String)`、`RichText`
   - 全型に `NodeData` + 適切なカテゴリトレイト実装

4. **Node / Edge / Graph構造体の定義**
   - `NodeId(u64)`、`EdgeId(u64)` のnewtypeパターン
   - `Node` 構造体（id, type_key, parameters, inputs, outputs, position, metadata）
   - `Edge` 構造体（id, source: (NodeId, OutputPortIndex), target: (NodeId, InputPortIndex)）
   - `Graph` 構造体（`im::HashMap<NodeId, Arc<Node>>`, `im::HashMap<EdgeId, Edge>`）
   - `Arc<Node>` でイミュータブル共有（TASK-004のアンドゥと連携）

5. **トポロジカルソートの実装**
   - KahnのアルゴリズムでDAG評価順序を算出
   - 循環検出（cycle detection）→ エラー返却
   - 非接続ノードのスキップ（出力ノードから到達不可能なノードは評価しない）

6. **ライセンスヘッダの整備**
   - 全ソースファイルにApache 2.0 / MITデュアルライセンスヘッダを付与
   - ルートに`LICENSE-APACHE`、`LICENSE-MIT`を配置
   - `Cargo.toml`にlicenseフィールド設定

## 対象コンポーネント

- `crates/ravel-core/src/types/mod.rs` — トレイト階層
- `crates/ravel-core/src/types/buffer.rs` — BufferData系具体型
- `crates/ravel-core/src/types/temporal.rs` — TemporalData系具体型
- `crates/ravel-core/src/types/geometric.rs` — GeometricData系具体型
- `crates/ravel-core/src/types/numeric.rs` — NumericData系具体型
- `crates/ravel-core/src/types/audio.rs` — AudioData系具体型
- `crates/ravel-core/src/types/text.rs` — TextData系具体型
- `crates/ravel-core/src/graph/mod.rs` — Graph, Node, Edge構造体
- `crates/ravel-core/src/graph/topo.rs` — トポロジカルソート
- `Cargo.toml` — ワークスペース定義

## 完了条件

- [ ] Cargoワークスペースが5クレート構成で`cargo check`が通る
- [ ] `NodeData`トレイト + 6カテゴリトレイトが定義されている
- [ ] 全具体型（FrameBuffer, Scalar, Vec2, Color等）にトレイト実装がある
- [ ] `NodeId`, `EdgeId` がnewtypeで定義されている
- [ ] `Graph`構造体が`im::HashMap`ベースで定義されている
- [ ] トポロジカルソートが正しい評価順序を返す（ユニットテスト）
- [ ] 循環グラフに対してエラーを返す（ユニットテスト）
- [ ] ダイヤモンド依存（A→B→D, A→C→D）で正しくソートされる（ユニットテスト）
- [ ] 全ソースファイルにApache 2.0 / MITデュアルライセンスヘッダが付与されている
- [ ] `LICENSE-APACHE`, `LICENSE-MIT` がリポジトリルートに存在する
