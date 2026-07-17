# Ravel — アーキテクチャ仕様書

## 概要

Ravelは「ノードグラフファースト」のアーキテクチャ。全てのデータフロー、エフェクト、合成処理がDAG（有向非巡回グラフ）上のノード接続として表現される。タイムラインはこのDAG上の糖衣表現（シーケンスノード）として実装。UI層と処理層は明確に分離され、GPUIによるUI描画とwgpuベースのGPU計算パイプラインがGPUコンテキストを共有する。

## レイヤー構成

```
┌─────────────────────────────────────────────────────────┐
│                    UI Layer (GPUI)                       │
│  ┌──────────┐ ┌──────────┐ ┌────────┐ ┌─────────────┐  │
│  │ Timeline │ │NodeGraph │ │ Viewer │ │ Properties  │  │
│  │  Editor  │ │  Editor  │ │+Scopes │ │  Inspector  │  │
│  └────┬─────┘ └────┬─────┘ └───┬────┘ └──────┬──────┘  │
│       └─────────────┴───────────┴─────────────┘         │
│                         │ Commands / Queries             │
├─────────────────────────┼───────────────────────────────┤
│                  Application Layer                       │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────┐  │
│  │  Undo    │ │Workspace │ │  i18n    │ │  Config   │  │
│  │ Manager  │ │ Manager  │ │  System  │ │  Manager  │  │
│  └──────────┘ └──────────┘ └──────────┘ └───────────┘  │
├─────────────────────────┼───────────────────────────────┤
│                    Core Engine                           │
│  ┌──────────────────────────────────────────────────┐   │
│  │              DAG Evaluation Engine                │   │
│  │  ┌────────┐ ┌──────────┐ ┌───────────────────┐  │   │
│  │  │ Graph  │ │  Node    │ │  Cache Manager    │  │   │
│  │  │Manager │ │Evaluator │ │ (VRAM/RAM/Disk)   │  │   │
│  │  └────────┘ └──────────┘ └───────────────────┘  │   │
│  └──────────────────────────────────────────────────┘   │
│  ┌─────────┐ ┌──────────┐ ┌──────────┐ ┌───────────┐   │
│  │  Type   │ │Animation │ │  Lua     │ │  Plugin   │   │
│  │ System  │ │ Channel  │ │ Runtime  │ │  Host     │   │
│  └─────────┘ └──────────┘ └──────────┘ └───────────┘   │
├─────────────────────────────────────────────────────────┤
│                   Media Layer                            │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────┐  │
│  │  FFmpeg  │ │HW Decode │ │  Audio   │ │   OCIO    │  │
│  │ Backend  │ │ Backend  │ │  Engine  │ │  Backend  │  │
│  └──────────┘ └──────────┘ └──────────┘ └───────────┘  │
├─────────────────────────────────────────────────────────┤
│                    GPU Layer                              │
│  ┌──────────────────────────────────────────────────┐   │
│  │              wgpu Compute Pipeline                │   │
│  │  ┌────────┐ ┌──────────┐ ┌───────────────────┐  │   │
│  │  │Shader  │ │ Texture  │ │  Native API       │  │   │
│  │  │Manager │ │  Pool    │ │  Fallthrough      │  │   │
│  │  └────────┘ └──────────┘ └───────────────────┘  │   │
│  └──────────────────────────────────────────────────┘   │
├─────────────────────────────────────────────────────────┤
│                  Platform Layer                           │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────┐  │
│  │  macOS   │ │ Windows  │ │  Linux   │ │   File    │  │
│  │ (Metal)  │ │ (D3D11)  │ │(Vulkan)  │ │  System   │  │
│  └──────────┘ └──────────┘ └──────────┘ └───────────┘  │
└─────────────────────────────────────────────────────────┘
```

## コアエンジン詳細設計

### DAG評価エンジン

**評価モデル: Hybrid Pull + Dirty Notification**

```
パラメータ変更
    │
    ▼
 Dirty伝播 (Push)
    │ 下流ノードのdirtyフラグをON
    ▼
 出力ノードからPull評価要求
    │
    ▼
 各ノードを再帰的に評価
    │ dirtyフラグがOFF → キャッシュ返却
    │ dirtyフラグがON  → 再評価 → キャッシュ更新
    ▼
 結果をビューアに表示
```

**ノード評価の疑似コード**（レイヤーネットワークモデル v3、REQ-LAYER-007）:
```rust
// 実装シグネチャ:
//   NodeProcessor::process(&self, node, ctx, inputs, params, scope)
//     inputs: &[Option<Arc<dyn NodeData>>]  — 入力ポート順スロット（未接続は None）
//     params: &ResolvedParams               — フレーム解決済みパラメータ（REQ-LAYER-004）
//     scope:  &mut dyn EvalScope            — サブグラフ再帰評価・Document 参照
fn evaluate(&self, path: &[PathSegment], node_id: NodeId, frame: Frame, ctx: &EvalContext)
    -> Arc<dyn NodeOutput>
{
    // キャッシュチェック（キーは所有パス + NodeId。REQ-LAYER-009）
    let key = (path, node_id);
    if let Some(cached) = ctx.cache.get(key, frame) {
        if !ctx.dirty_set.contains(key) {
            return cached;
        }
    }

    // 入力の再帰評価（target入力ポート index 昇順で整列、多出力ノードは
    // PortRecord から source_port で抽出）
    let inputs: Vec<Option<Arc<dyn NodeOutput>>> = self.graph
        .inputs(node_id)
        .map(|(input_id, source_port)| self.evaluate(path, input_id, frame, ctx).extract(source_port))
        .collect();

    // パラメータの評価時解決（定数・キーフレーム・ノード出力バインド）
    let params = self.resolve_params(node_id, frame, ctx);

    // ノード処理実行（プロセッサは Evaluator がノードごとに登録・保持）
    let result = self.processor(node_id).process(node, ctx, &inputs, &params, scope);

    // キャッシュ更新 & dirtyクリア
    ctx.cache.put(key, frame, result.clone());
    ctx.dirty_set.remove(key);

    result
}
```

**ネットワークスコープ（v3）**: レイヤーネットワーク・サブネットワークの
評価は `EvalScope::evaluate_sub(segment, graph, output, ctx, bindings)` で
再帰する。`segment`（`Layer(comp, layer)` / `Subnet(node)` / 予約 `Comp`）
が評価パスに積まれ、キャッシュ/dirty はパスで名前空間化される。境界
ノード（`comp.network`）は EvalContext をレイヤーローカル時間に書き換えて
渡し、Evaluator は Document を保持してレイヤーのネットワークを解決する
（Document-aware）。スコープの無効化（`invalidate_scope`）はオーナー
ノードのキャッシュも道連れにし、ネットワーク編集が殻チェーンへ自動伝播する。

### 型システム

```rust
// 基本トレイト
trait NodeData: Send + Sync + 'static {
    fn data_type_id(&self) -> DataTypeId;
    fn as_any(&self) -> &dyn std::any::Any; // 入力を具体型へ downcast するため
}

// カテゴリトレイト
trait BufferData: NodeData {
    fn width(&self) -> u32;
    fn height(&self) -> u32;
    fn pixel_format(&self) -> PixelFormat;
}

trait TemporalData: NodeData {
    fn duration(&self) -> Duration;
    fn frame_rate(&self) -> FrameRate;
}

trait GeometricData: NodeData {
    fn bounds(&self) -> Rect;
    fn transform(&self) -> Transform2D;
}

// 具体型
struct FrameBuffer { /* RGBA f32 */ }
struct Clip { /* フレーム列 + メタデータ */ }
struct Shape { /* 2Dパスデータ */ }
struct Scalar(f32);
struct Vec2(f32, f32);
struct Color(f32, f32, f32, f32);
struct AudioBuffer { /* PCM f32 */ }
struct ParticleSystem { /* パーティクル群 */ }
// ...

impl BufferData for FrameBuffer { /* ... */ }
impl TemporalData for Clip { /* ... */ }
impl GeometricData for Shape { /* ... */ }
```

### イミュータブルグラフ + アンドゥ

```rust
struct GraphVersion {
    nodes: im::HashMap<NodeId, Arc<Node>>,
    edges: im::HashMap<EdgeId, Edge>,
    // im クレートのpersistent data structureで構造共有
}

struct UndoStack {
    versions: Vec<Arc<GraphVersion>>,
    current: usize,
}

impl UndoStack {
    fn push(&mut self, new_version: Arc<GraphVersion>) {
        self.versions.truncate(self.current + 1);
        self.versions.push(new_version);
        self.current += 1;
    }

    fn undo(&mut self) -> Option<&Arc<GraphVersion>> {
        if self.current > 0 {
            self.current -= 1;
            Some(&self.versions[self.current])
        } else {
            None
        }
    }
}
```

### 統一アニメーションチャネル

```rust
enum ChannelSource {
    Constant(f32),
    Keyframes(KeyframeCurve),       // ベジエ/リニア/ステップ
    Expression(LuaExpression),       // Luaスクリプト
    NodeOutput(NodeId, OutputPort),  // 他ノードの出力
    AudioReactive(AudioAnalysisRef), // オーディオ解析
    Blend(Box<ChannelSource>, Box<ChannelSource>, BlendMode, f32),
}

struct AnimationChannel {
    source: ChannelSource,
}

impl AnimationChannel {
    fn evaluate(&self, frame: Frame, ctx: &EvalContext) -> f32 {
        match &self.source {
            ChannelSource::Constant(v) => *v,
            ChannelSource::Keyframes(curve) => curve.sample(frame),
            ChannelSource::Expression(expr) => expr.eval(frame, ctx),
            ChannelSource::NodeOutput(id, port) => ctx.get_output(*id, *port, frame),
            ChannelSource::AudioReactive(r) => r.sample(frame, ctx),
            ChannelSource::Blend(a, b, mode, factor) => {
                mode.blend(a.evaluate(frame, ctx), b.evaluate(frame, ctx), *factor)
            }
        }
    }
}
```

## スレッディングモデル

```
┌──────────────┐
│  UI Thread   │ ← GPUIメインループ、入力処理、描画
│  (GPUI)      │
└──────┬───────┘
       │ crossbeam-channel (ロックフリー)
┌──────┴───────┐
│  Eval Pool   │ ← ノードグラフ評価、エフェクト処理
│  (rayon)     │   CPU並列はrayonのwork-stealing
└──────┬───────┘
       │ GPUコマンドバッファ投入
┌──────┴───────┐
│  GPU Thread  │ ← wgpuコマンド投入、シェーダディスパッチ
└──────────────┘
┌──────────────┐
│ Decode Pool  │ ← FFmpegデコード、HWデコーダ制御
└──────────────┘
┌──────────────┐
│ Audio Prep   │ ← ミキシング、SRC、エフェクト処理
│  Thread      │   crossbeam-channelでCPALコールバックへchunk送信
└──────────────┘
┌──────────────┐
│ Audio CPAL   │ ← リアルタイム優先度、CPAL callback
│  Callback    │   ※ 絶対にブロックしない（try_recvのみ）
└──────────────┘
┌──────────────┐
│ Tokio Runtime│ ← ファイルI/O、ネットワーク、プラグインホスト
└──────────────┘
┌──────────────┐
│ OFX Process  │ ← 子プロセス（プラグイン隔離実行）
│  (separate)  │
└──────────────┘
```

## キャッシュアーキテクチャ

```
┌─────────────────────────────────────────┐
│         VRAM Cache (GPU Textures)       │
│  最速アクセス / 容量: VRAM依存           │
│  用途: プレビュー表示、GPU処理結果保持    │
│  エビクション: LRU                       │
├────────────────────┬────────────────────┤
│                    │ GPU→CPU転送         │
│                    ▼                    │
│         RAM Cache (System Memory)       │
│  中速アクセス / 容量: 設定可能上限       │
│  用途: 再評価回避、スクラブ時の即応       │
│  エビクション: LRU                       │
├────────────────────┬────────────────────┤
│                    │ シリアライズ         │
│                    ▼                    │
│         Disk Cache (.ravprj/.cache/)    │
│  低速アクセス / 容量: ディスク依存       │
│  用途: セッション跨ぎキャッシュ          │
│  zip化時に除外可能                       │
└─────────────────────────────────────────┘
```

## プロジェクトファイル構造

```
project.ravprj (zip)
├── manifest.json            # フォーマットバージョン、メタデータ
├── graph/
│   ├── main.ron             # ルートノードグラフ定義
│   └── subgraphs/
│       ├── color_grade.ron  # サブグラフ定義 (Group or Comp)
│       └── intro_effect.ron # Comp: 独自解像度/FPS/尺を持つ
├── assets/
│   └── refs.json            # アセット参照（相対パス、ハッシュ、変数）
├── presets/
│   └── node_presets.ron     # ノード単位プリセット
├── settings.toml            # プロジェクト固有設定オーバーライド
├── .journal/                # 操作ジャーナル（正常終了時コンパクション）
└── .cache/                  # キャッシュ（zip化時除外可）
    ├── thumbnails/
    └── render/
```

> **実装状況**: 現行実装（`ravel-app/src/project/container.rs`）が読み書きするのは
> `manifest.json` / `document/main.ron` / `assets/refs.json` / `settings.toml`
> （フォーマット v3）。`document/main.ron` は Composition/Layer・各レイヤーネットワーク・
> メディアアセット（絶対パス）を含む `Document` 全体の RON。
> v1–v2 の `graph/main.ron`（レガシー平坦グラフ）は読み込み時のマイグレーション専用。
> `subgraphs/`・`presets/`・`.journal/`・`.cache/` は将来拡張。

## OpenFX統合アーキテクチャ

```
┌──────────────────────────────────┐
│         Ravel Main Process       │
│                                  │
│  ┌────────────────────────────┐  │
│  │      OFX Host Shim        │  │
│  │  (Rust → C/C++ FFI)       │  │
│  │                            │  │
│  │  ┌──────────────────────┐  │  │
│  │  │   Suite Registry     │  │  │
│  │  │  - Image Effect ✓    │  │  │
│  │  │  - Parameter ✓       │  │  │
│  │  │  - GPU Render ✓      │  │  │
│  │  │  - Multi-clip (将来) │  │  │
│  │  │  - Temporal (将来)   │  │  │
│  │  │  - Interact (将来)   │  │  │
│  │  └──────────────────────┘  │  │
│  └──────────┬─────────────────┘  │
│             │ IPC (shared mem)   │
├─────────────┼────────────────────┤
│             ▼                    │
│  ┌────────────────────────────┐  │
│  │    OFX Plugin Process     │  │ ← 子プロセス（隔離）
│  │  - プラグインDLLロード      │  │
│  │  - renderAction実行        │  │
│  │  - クラッシュ時自動再起動    │  │
│  └────────────────────────────┘  │
└──────────────────────────────────┘
```

## カラーマネジメントパイプライン

```
入力メディア → [入力カラースペース変換 (OCIO)] → 作業空間 (32bit float リニア)
                                                          │
                                                  ノード評価
                                                          │
                                                          ▼
作業空間 → [表示カラースペース変換 (GPU 3D LUT)] → ビューア表示
                                                          │
作業空間 → [出力カラースペース変換 (OCIO)] → エンコード → ファイル出力
```

- 全内部処理は32bit floatリニア空間
- OCIO `.ocio`設定でカラースペース変換を定義
- ビューア表示用はGPU 3D LUTにベイクしwgpuシェーダで適用
- LUT再生成は設定変更時のみ（フレーム毎ではない）

## 制約・前提条件

- GPUIのwgpuカスタムフォーク依存（Zed upstream追従が必要）
- FFmpegはLGPLダイナミックリンク（静的リンク不可）
- OCIOはC++ライブラリ（FFIコスト、ビルド複雑度）
- OFXプラグインはC ABI（型安全性なし、プロセス分離で安全性確保）
- オーディオスレッドはリアルタイム制約（ヒープアロケーション/ロック禁止）
- macOSリード開発のため、Windowsでの動作確認は設計段階からCI含めて行う
