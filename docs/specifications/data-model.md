# Ravel — データモデル仕様書

## 概要

Ravelのデータモデルは3層で構成される: (1) ノードグラフ（DAG）の構造定義、(2) ノード間を流れるデータ型、(3) プロジェクトファイルの永続化形式。

## ノードグラフモデル

### ノード (Node)

```rust
struct Node {
    id: NodeId,
    type_key: NodeTypeKey,        // "blur", "color_correct", "sequence" 等
    parameters: Vec<Parameter>,
    inputs: Vec<InputPort>,
    outputs: Vec<OutputPort>,
    position: Vec2,               // エディタ上の位置
    metadata: NodeMetadata,
}

struct NodeMetadata {
    label: Option<String>,        // ユーザー定義ラベル
    color: Option<Color>,         // エディタ上のノード色
    collapsed: bool,
}
```

### エッジ (Edge)

```rust
struct Edge {
    id: EdgeId,
    source: (NodeId, OutputPortIndex),
    target: (NodeId, InputPortIndex),
}
```

### パラメータ (Parameter)

```rust
struct Parameter {
    key: String,
    value: ParameterValue,
    channel: AnimationChannel,    // 統一アニメーションチャネル
    metadata: ParameterMetadata,
}

enum ParameterValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    Vec2(f32, f32),
    Vec3(f32, f32, f32),
    Vec4(f32, f32, f32, f32),
    Color(f32, f32, f32, f32),
    String(String),
    Enum(u32),
    Curve(KeyframeCurve),
}

struct ParameterMetadata {
    display_name: String,         // i18nキー
    range: Option<(f32, f32)>,
    default: ParameterValue,
    ui_hint: UiHint,              // Slider, Angle, ColorPicker 等
}
```

### サブグラフ (Subgraph)

> **v2 変更**: 旧 `SubgraphKind::Comp` は Composition/Layer モデル（下記）に吸収。
> Subgraph は整理用グループ（Group）のみを扱う。
> 独立コンポジション（独自の解像度/FPS/尺）は Composition として管理する。

```rust
struct Subgraph {
    id: SubgraphId,
    name: String,
    graph: Graph,                 // 内包するグラフ
    exposed_params: Vec<ExposedParameter>,
    inputs: Vec<SubgraphInput>,
    outputs: Vec<SubgraphOutput>,
}

struct ExposedParameter {
    internal_node: NodeId,
    internal_param: String,
    external_name: String,
    external_metadata: ParameterMetadata,
}
```

親グラフでは入出力ポートを持つ1ノードとして表示される。
ダブルクリックで中に潜る（ブレッドクラム表示）。Ctrl+G で選択ノードをGroup化。

### Composition / Layer モデル（AEモデル, v2）

> **v1 からの変更**: SequenceNode/Track/Clip（NLEモデル）を廃止し、
> Composition/Layer（AEモデル）に全面移行。

#### Composition

```rust
struct Composition {
    id: CompId,
    name: String,
    resolution: (u32, u32),
    frame_rate: FrameRate,
    duration_frames: u64,
    layers: im::Vector<Layer>,     // 下から上への合成順序
    background_color: Color,
}
```

Composition はドキュメント層に `im::HashMap<CompId, Arc<Composition>>` として保持し、
Graph と同様にイミュータブル操作 + 構造共有で undo 対応。

#### Layer

```rust
struct Layer {
    id: LayerId,
    name: String,
    source: LayerSource,
    // 時間配置（AEセマンティクス: start=配置, in/out=トリム）
    start_frame: i64,              // Comp タイムライン上の開始位置（負も可）
    in_frame: u64,                 // ソース内の表示開始フレーム
    out_frame: u64,                // ソース内の表示終了フレーム [in, out)
    // ビルトイン Transform
    position: AnimChannel<Vec2>,
    scale: AnimChannel<Vec2>,
    rotation: AnimChannel<f32>,
    opacity: AnimChannel<f32>,
    anchor_point: AnimChannel<Vec2>,
    // 合成
    blend_mode: BlendMode,
    // 状態
    solo: bool,
    muted: bool,
    locked: bool,
    // 親子
    parent: Option<LayerId>,       // Transform 継承（P/R/S のみ、opacity/blend は継承しない）
    // エフェクト
    effect_graph: Option<SubgraphId>,  // ノードサブグラフ（直列=スタックUI、分岐=ノードグラフUI）
}

enum LayerSource {
    Media { asset_id: String },
    Solid { color: Color, width: u32, height: u32 },
    Shape { node_id: NodeId },      // プロシージャルシェイプノード
    Text { node_id: NodeId },       // テキストノード
    PreComp { comp_id: CompId },    // 別 Composition への参照
    Generator { node_id: NodeId },  // ジェネレータノード
    Null,                           // 親子制御用（描画なし）
}

enum BlendMode {
    Normal,
    Add,
    Multiply,
    Screen,
    Overlay,
}
```

#### CompNode（DAG上の特殊ノード）

CompNode は DAG 上に存在し、パラメータとして `comp_id: CompId` を持つ。
**コンパイラ方式**: CompNode は Composition 内の Layer 群を通常の DAG ノード列に
展開（flatten/lower）する。各 Layer が `Source → EffectChain → Transform → Merge`
のチェーンに展開され、既存の Evaluator でそのまま評価される。

```
Composition (3 layers) が展開されると:

Source_L1 → Effects_L1 → Transform_L1 ─┐
Source_L2 → Effects_L2 → Transform_L2 ─┤─ Merge ─ Merge → CompOutput
Source_L3 → Effects_L3 → Transform_L3 ─┘
```

**決定論的 ID 割り当て**: 展開で生成されるノードの ID は `(CompId, LayerId, Role)` から
決定論的に導出する。`NodeId::new(comp_id.raw() << 32 | layer_id.raw() << 8 | role)`。
再展開時に同一 ID が再利用される → Evaluator のキャッシュが維持される。

**Synthetic ノード**: 展開で生成されたノードは `Node.metadata.synthetic = true` でマーク。
- 永続化(.ravprj)時に除外
- ノードエディタUI では非表示
- Undo は Graph + CompMap を統一 Document スナップショットで管理

展開は構造変更（Layer 追加/削除/順序変更）時にのみ実行。
キーフレーム変更は dirty 通知のみで再展開不要（ID が安定しているためキャッシュ有効）。

**TimeOffset ノード**: 各 Layer に TimeOffset ノードを挿入し、`start_frame` オフセットと
`[in, out)` トリムを処理する。PreComp の場合は子 Comp の fps/解像度への変換も行う。

**Parenting の評価時解決**: Parent の Transform ノード出力を子の Transform ノード入力に
エッジ接続として展開する。コンパイル時の行列計算ではなく、DAG の依存関係として表現し
Evaluator が自然に解決する。

#### 設計上の注意事項（Fable レビュー指摘）

- **premultiplied alpha**: 全内部処理は premultiplied alpha で統一。入出力時に変換。
  Multiply/Screen/Overlay は premul 前提の数式を使用。
- **solo の扱い**: solo は Comp 全体に影響（any solo → 非 solo を非表示）。展開前のプレパスで処理。
- **PreComp 循環検出**: 編集時に `comp_id` 参照グラフの循環を検出・拒否。評価時にも depth guard。
- **fps/解像度不一致**: 子 Comp は自身の fps/解像度で評価（TimeOffset ノードで変換）。
- **フレーム範囲**: `[in, out)` 半開区間。
- **time remap**: 将来対応。Layer に `time_remap: Option<AnimChannel<f64>>` を追加。
- **muted Layer と Parenting**: muted Layer の子が parent 参照する場合、Transform のみ残す。
- **negative start_frame**: Layer の start_frame は i64（負も可）。Comp 先頭より前に配置可能。

## データ型ヒエラルキー

```
NodeData (trait)
├── BufferData (trait)
│   ├── FrameBuffer          # RGBA f32 ピクセルバッファ
│   ├── DepthBuffer          # 単チャネル f32
│   └── MultiLayerBuffer     # マルチレイヤーEXR
├── TemporalData (trait)
│   ├── Clip                 # フレーム列 + メタデータ
│   └── TimeRemap            # タイムリマップカーブ
├── GeometricData (trait)
│   ├── Shape                # 2Dパスデータ
│   ├── Mask                 # マスクデータ
│   ├── Mesh3D               # 3Dメッシュ（基本機能用）
│   └── ParticleSystem       # パーティクル群
├── NumericData (trait)
│   ├── Scalar(f32)
│   ├── Vec2(f32, f32)
│   ├── Vec3(f32, f32, f32)
│   ├── Vec4(f32, f32, f32, f32)
│   ├── Color(f32, f32, f32, f32)
│   └── Curve(KeyframeCurve)
├── AudioData (trait)
│   ├── AudioBuffer          # PCM f32 バッファ
│   └── SpectrumData         # FFT解析結果
└── TextData (trait)
    ├── PlainText(String)
    └── RichText             # スタイル情報付き
```

## アセット参照モデル

```rust
struct AssetRef {
    id: AssetId,
    path: AssetPath,              // 相対パス or 変数付きパス
    hash: Option<String>,         // ファイルハッシュ（整合性確認）
    proxy: Option<ProxyInfo>,
    metadata: AssetMetadata,
}

enum AssetPath {
    Relative(String),             // "./footage/clip01.mov"
    Variable(String, String),     // ("${PROJECT_ROOT}", "footage/clip01.mov")
}

struct AssetMetadata {
    width: Option<u32>,
    height: Option<u32>,
    frame_rate: Option<FrameRate>,
    duration: Option<Duration>,
    codec: Option<String>,
    color_space: Option<String>,
    file_size: u64,
}

struct ProxyInfo {
    path: AssetPath,
    resolution_factor: f32,       // 0.5 = half, 0.25 = quarter
    status: ProxyStatus,
}
```

## 永続化形式

### manifest.json

```json
{
  "format_version": 2,
  "ravel_version": "0.1.0",
  "project_name": "My Lyric Video",
  "created_at": "2026-06-22T10:00:00Z",
  "modified_at": "2026-06-22T15:30:00Z",
  "frame_rate": { "num": 30, "den": 1 },
  "resolution": { "width": 1920, "height": 1080 },
  "color_config": "aces_1.2"
}
```

### graph/main.ron (RON形式)

`GraphDoc`（`ravel-app::project::graph_doc`）として永続化。ライブ`Graph`から
`NodeId`/`EdgeId`昇順でソートした`Vec`に射影し、決定的出力でgit diffを有効化。
ノードは入出力ポート (`inputs`/`outputs`) とエディタ用メタデータ (`metadata`) を保持。

```ron
GraphDoc(
  nodes: [
    Node(
      id: NodeId(1),
      type_key: "read_media",
      inputs: [],
      outputs: [
        OutputPort(name: "out", data_type: DataTypeId(1)),
      ],
      metadata: NodeMetadata(label: None, position: (100.0, 200.0), collapsed: false),
    ),
    Node(
      id: NodeId(2),
      type_key: "color_correct",
      inputs: [
        InputPort(name: "in", accepted_types: [DataTypeId(1)]),
      ],
      outputs: [
        OutputPort(name: "out", data_type: DataTypeId(1)),
      ],
      metadata: NodeMetadata(label: None, position: (300.0, 200.0), collapsed: false),
    ),
    Node(
      id: NodeId(3),
      type_key: "sequence",
      inputs: [
        InputPort(name: "in", accepted_types: [DataTypeId(1)]),
      ],
      outputs: [],
      metadata: NodeMetadata(label: None, position: (500.0, 200.0), collapsed: false),
    ),
  ],
  edges: [
    Edge(id: EdgeId(1), source: NodeId(1), source_port: OutputPortIndex(0), target: NodeId(2), target_port: InputPortIndex(0)),
    Edge(id: EdgeId(2), source: NodeId(2), source_port: OutputPortIndex(0), target: NodeId(3), target_port: InputPortIndex(0)),
  ],
)
```

> **未対応**: ノードパラメータ（`gain`/`gamma`等の値・アセットパス変数）は現行
> `ravel-core::Node`モデル未保持。パラメータ永続化はパラメータ/アニメーションチャネル
> システム統合時（TASK-016以降）に`Node`へ追加し、本RON形式を拡張する。

### assets/refs.json

```json
{
  "assets": [
    {
      "id": "asset_001",
      "path": { "type": "variable", "var": "${PROJECT_ROOT}", "rel": "footage/bg.mov" },
      "hash": "sha256:abcdef...",
      "metadata": {
        "width": 1920,
        "height": 1080,
        "frame_rate": { "num": 30, "den": 1 },
        "codec": "h264",
        "color_space": "sRGB",
        "file_size": 104857600
      }
    }
  ]
}
```

### settings.toml (プロジェクトオーバーライド)

```toml
[color]
ocio_config = "./ocio/config.ocio"
working_space = "ACEScg"
display_space = "sRGB"

[playback]
frame_rate = "30"
proxy_mode = "auto"
proxy_resolution = 0.5

[auto_save]
enabled = true
interval_seconds = 120
```

## 制約・前提条件

- 全内部処理は32bit float
- RON形式はRustネイティブでパース/シリアライズが高速
- プロジェクトファイルはgit diffが有効なテキスト形式
- アセットはプロジェクト内に埋め込まず参照のみ保持
- 関連要件: REQ-CORE-001, REQ-CORE-003, REQ-CORE-007, REQ-PROJ-001
