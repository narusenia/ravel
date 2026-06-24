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

### シーケンスノード (タイムライン表現)

```rust
struct SequenceNode {
    node: Node,                   // 基底ノード
    tracks: Vec<Track>,
    duration: Duration,
    frame_rate: FrameRate,
}

struct Track {
    id: TrackId,
    name: String,
    clips: Vec<Clip>,
    muted: bool,
    locked: bool,
}

struct Clip {
    id: ClipId,
    source: ClipSource,           // メディア参照 or サブグラフ参照
    timeline_range: TimeRange,    // タイムライン上の配置
    source_range: TimeRange,      // ソースメディアの使用範囲
    effect_stack: Vec<NodeId>,    // エフェクトノードチェーン（直列時）
    effect_graph: Option<SubgraphId>, // 分岐時はサブグラフ参照
    transition_in: Option<TransitionRef>,
    transition_out: Option<TransitionRef>,
}

enum ClipSource {
    Media(AssetRef),
    Sequence(NodeId),             // ネストシーケンス
    Generator(NodeId),            // ジェネレータノード
}
```

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
