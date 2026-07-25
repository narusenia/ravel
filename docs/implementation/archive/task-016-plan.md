# TASK-016: ビルトインノード実装 (基本セット)

## Context

TASK-014/015 でノードエディタの UI が完成。TASK-016 では既存の 5 テンプレート (constant, merge, blur, transform, color_correct) に対して実際の処理ロジック (NodeProcessor) と GPU シェーダを実装し、DAG 評価パイプラインを E2E で動作可能にする。

依存: TASK-002 (DAG 評価エンジン ✅), TASK-005 (wgpu GPU 計算パイプライン ✅)

## 前提（実装済みインフラ）

- `NodeProcessor` trait (`ravel-core/src/eval.rs`): `process(&self, ctx, inputs) -> Result<Box<dyn NodeData>>`
- `Evaluator` (`ravel-core/src/eval.rs`): register/evaluate/cache/dirty
- `NodeData` trait + 具象型 (`ravel-core/src/types.rs`): FrameBuffer, Scalar, Vec2-4, Color 等
- `GpuContext` + `ComputePipeline` + `ShaderManager` + `TexturePool` (`ravel-gpu/src/`)
- 参照実装: `invert.wgsl` シェーダ, `PassThrough` プロセッサ (bench)

## スコープ

### 1. 新規クレート `ravel-nodes` 作成

```
crates/ravel-nodes/
├── Cargo.toml          # ravel-core + ravel-gpu に依存
├── src/
│   ├── lib.rs          # pub mod + register_all_processors()
│   ├── constant.rs     # ConstantProcessor (CPU)
│   ├── merge.rs        # MergeProcessor (GPU)
│   ├── blur.rs         # BlurProcessor (GPU)
│   ├── transform.rs    # TransformProcessor (GPU)
│   ├── color_correct.rs # ColorCorrectProcessor (GPU)
│   └── shaders/
│       ├── merge.wgsl
│       ├── blur.wgsl
│       ├── transform.wgsl
│       └── color_correct.wgsl
```

### 2. 各ノードプロセッサの実装

#### constant (CPU, Generator)
- 入力: なし
- パラメータ: `value: Float`
- 出力: `Scalar(value)`
- GPU 不要、純粋な CPU 処理

#### blur (GPU, Filter)
- 入力: `image: FrameBuffer`
- パラメータ: `radius: Float`
- 出力: `FrameBuffer`
- シェーダ: 2パス Gaussian blur (水平→垂直の分離フィルタ)
- GpuContext + TexturePool 使用

#### color_correct (GPU, Color)
- 入力: `image: FrameBuffer`
- パラメータ: `brightness`, `contrast`, `saturation: Float`
- 出力: `FrameBuffer`
- シェーダ: per-pixel HSL 調整

#### transform (GPU, Transform)
- 入力: `image: FrameBuffer`
- パラメータ: `translate_x`, `translate_y`, `rotation`, `scale: Float`
- 出力: `FrameBuffer`
- シェーダ: アフィン変換 (逆行列サンプリング + bilinear)

#### merge (GPU, Compositor)
- 入力: `A`, `B: FrameBuffer`
- パラメータ: `operation: String` (over/add/multiply), `mix: Float`
- 出力: `FrameBuffer`
- シェーダ: alpha compositing ブレンドモード

### 3. register_all_processors() 関数

`Evaluator` に全プロセッサを一括登録する関数。Graph の各ノードの type_key に基づいてプロセッサをマッチング。

```rust
pub fn register_all_processors(
    evaluator: &mut Evaluator,
    graph: &Graph,
    gpu: &GpuContext,
) { ... }
```

### 4. テスト

各プロセッサに対して:
- CPU テスト: constant の出力値検証
- GPU テスト: blur/color_correct/transform/merge の入出力検証 (小さなテスト画像)
- 統合テスト: Graph + Evaluator でデモグラフを評価

### 5. ワークスペース統合

- `Cargo.toml` に `ravel-nodes` をメンバー追加
- `ravel-app/Cargo.toml` に `ravel-nodes` 依存追加
- アプリ起動時に `register_all_processors()` を呼ぶ (将来的にプレビューパネルで使用)

## コミット計画

| # | 内容 |
|---|------|
| 1 | chore: create ravel-nodes crate with workspace integration |
| 2 | feat: implement ConstantProcessor (CPU) with tests |
| 3 | feat: implement ColorCorrectProcessor with WGSL shader |
| 4 | feat: implement BlurProcessor with 2-pass Gaussian shader |
| 5 | feat: implement TransformProcessor with affine shader |
| 6 | feat: implement MergeProcessor with blend mode shader |
| 7 | feat: add register_all_processors and integration test |
| 8 | docs: update plan.md and ui-impl-status |

## 検証

- `cargo build` — 全クレート警告なし
- `cargo test -p ravel-nodes` — 全プロセッサテスト通過 (16テスト)
- `cargo test -p ravel-core` — 既存テスト全通過 (153テスト)
- `RUSTFLAGS="-D warnings" cargo clippy -p ravel-nodes` — clean
- `cargo fmt -p ravel-nodes -- --check` — clean
- GPU テストは CI の macOS runner で実行 (Metal バックエンド)

## 実装メモ

- `NodeProcessor::process` はパラメータを直接受け取らない設計のため、プロセッサ構築時 (`from_node` / `new`) に Node のパラメータ値を取り込む方式を採用。パラメータ変更時は `register_all_processors` を再呼び出しして更新。
- GPU ノード共通のヘルパー (`gpu_util.rs`): テクスチャ upload/readback、レイアウトエントリ生成を共通化。
- `register_all_processors` のシグネチャに `ShaderManager` を追加（計画時の `(evaluator, graph, gpu)` から拡張）。GPU プロセッサのパイプライン構築にシェーダコンパイルが必要なため。
- `Node::with_param` ビルダーメソッドを `ravel-core` に追加（テスト・プロセッサ構築の利便性向上）。
