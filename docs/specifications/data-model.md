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
}

enum ParameterValue {
    Float(f32),
    Int(i32),
    Bool(bool),
    String(String),
    // アニメーション可能な値（統一チャネル、REQ-LAYER-004）
    Channel(AnimationChannel),              // スカラー（f32 相当）
    Channel2([AnimationChannel; 2]),        // Vec2
    Channel3([AnimationChannel; 3]),        // Vec3 / RGB
    Channel4([AnimationChannel; 4]),        // Vec4 / RGBA
}
```

- ネットワーク内の**任意のノードパラメータ**がチャネルを持てる（キーフレーム、
  ノード出力バインド、ブレンド。Expression / AudioReactive は placeholder）。
- Int / Bool は v1 では定数のみ（step キーは v2）。
- プロセッサは構築時にパラメータをキャプチャ**しない**。Evaluator が各
  `process()` 呼び出し時にフレーム解決した `ResolvedParams` を渡す
  （アニメーション中のプロセッサ再構築を防ぐ）。
- `NodeTemplate`（registry）の `ParamRange` は UI 用の範囲・デフォルト情報
  として残る。

### サブグラフ (Subgraph)

> **v2 変更**: 旧 `SubgraphKind::Comp` は Composition/Layer モデル（下記）に吸収。
> Subgraph は整理用グループ（Group）のみを扱う。
> 独立コンポジション（独自の解像度/FPS/尺）は Composition として管理する。
>
> **実装状況**: `Subgraph` / `SubgraphId` は未実装（将来拡張）。現行コードに存在しない。
> レイヤーネットワーク内の入れ子グループは **サブネットワーク**
> （REQ-LAYER-003、実装計画 Phase 2）として別途導入される。

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

### Composition / Layer モデル（レイヤーネットワークモデル, v3）

> **v2 からの変更**: 「Layer = LayerSource + ビルトイン Transform + エフェクト
> サブグラフ」および「Composition 全体の平坦化コンパイル（Evaluator 変更不要）」
> を撤回。**1 レイヤー = 殻 + 1 ノードネットワーク**（Houdini 的）に移行。
> 詳細要件は REQ-LAYER、実装計画は
> `docs/implementation/layer-network-model-plan.md` を参照。

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

#### Layer（殻 + ネットワーク）

```rust
struct Layer {
    id: LayerId,
    name: String,
    network: Graph,                // 所有するノードネットワーク（REQ-LAYER-009）
    // 時間配置（AEセマンティクス: start=配置, in/out=トリム）
    start_frame: i64,              // Comp タイムライン上の開始位置（負も可）
    in_frame: u64,                 // ソース内の表示開始フレーム
    out_frame: u64,                // ソース内の表示終了フレーム [in, out)
    // ビルトイン Transform（殻の first-class プロパティ）
    transform: LayerTransform,     // anchor_point/position/scale/rotation
    opacity: AnimationChannel,
    // 合成
    blend_mode: BlendMode,
    adjustment: bool,              // 調整レイヤー（REQ-LAYER-010）
    // 状態
    solo: bool,
    muted: bool,
    locked: bool,
    // 親子
    parent: Option<LayerId>,       // Transform 継承（P/R/S のみ、opacity/blend は継承しない）
    // v2 予約フィールド（評価されない。永続化互換のため存在）
    time_remap: Option<AnimationChannel>,
    track_matte: Option<TrackMatte>,
}
```

- **`LayerSource` enum は廃止**。レイヤー「種類」（Solid / Video / Shape /
  Text / PreComp / Null）は作成時テンプレートに降格し、初期ネットワークを
  生成するだけ（REQ-LAYER-008）。データモデル上、全レイヤーは同一構造。
- **Null レイヤー**は「ネットワークの Out に `frame` ポートが無いレイヤー」
  として再定義。マージチェーンに参加せず、Layer Ref 経由でのみ消費される
  （REQ-LAYER-005）。
- **調整レイヤー**（`adjustment = true`）は、In の `source` ポートに下位
  スタックの合成結果を受け取り、その出力が次の background になる
  （`background' = network(background)`。opacity はエフェクト強度）。

#### ネットワークインターフェース（In / Out ノード, REQ-LAYER-002）

全レイヤーネットワークは `net.in` / `net.out` を1つずつ持つ（型キーで識別）。

- **`net.in`**（殻 → ネットワークの注入点）: 固定出力 `base_geometry`
  （GEOMETRY、レイヤー幅×高さの quad）と `t`（SCALAR、レイヤーローカル時間・
  秒）、調整レイヤーでは `source`（FRAME_BUFFER）、さらにユーザー定義の
  カスタムパラメータポート（Float / Int / Bool / Vec2 / Vec3 / Color）。
  カスタムパラメータは殻の Properties パネルに自動露出しキーフレーム可能。
- **`net.out`**: 入力 `frame`（FRAME_BUFFER、殻が消費する唯一のポート）+
  カスタム出力ポート（任意型。Layer Ref から参照される）。
- 多出力ノードの評価値は `PortRecord`（出力ポート順の値ベクタ）で、
  エッジの `source_port` でインデックスされる。

#### 所有権と ID（REQ-LAYER-009）

ネットワークはオーナーが所有する入れ子構造（Layer → Graph、将来の
サブネットノード → 内部 Graph）。ノード ID は**ドキュメント内でグローバル
一意**とする（`NodeId::next` 採番。永続化は読み込み時にこの不変条件を
維持する）。プロセッサのレジストリはこの不変条件の下で NodeId のみで
索引される。評価キャッシュ・dirty 集合は**所有パス**
（`CompId / LayerId / [SubnetNodeId ...] / NodeId`）をキーとする。
所有パスは ID 衝突のためではなく、同一グラフが複数のオーナー（将来の
共有サブネット・PreComp インスタンス）経由で評価される際の、
評価インスタンス区別のために使う。

#### 殻のコンパイル（REQ-LAYER-007）

殻の合成チェーン（時間変換 → Transform → Opacity → Merge）は synthetic
ノードとして従来通りコンパイルするが、レイヤーネットワークは**平坦化
しない**。旧 `Source → TimeOffset → Effects` の位置には**ネットワーク境界
ノード**（`comp.network`）が1つ入るだけで、境界ノードがレイヤーの
ネットワークを再帰的に pull 評価する。

```
normal layer:     [Network boundary] → Transform → Opacity → Merge
adjustment layer: [Network boundary] → Transform → Merge(adjustment)
                       ▲ source（下位スタック）  ▲ background
```

**決定論的 ID**: 殻の synthetic ノードの ID は `(CompId, LayerId, Role)`
から決定論的に導出（`comp_id << 32 | layer_id << 8 | role`、Role =
Network/Transform/Opacity/Merge）。再コンパイルで ID が安定し、Evaluator
のキャッシュが維持される。Synthetic ノードは `metadata.synthetic = true`
で、永続化除外・ノードエディタ非表示の規約。

**レイヤーローカル時間**（REQ-LAYER-006）: 境界ノードは EvalContext を
ローカル時間（`comp_frame - start_frame + in_frame`、秒ベース）に書き換えて
内部評価に渡す（スコープ付き EvalContext）。表示区間 `[in, out)` の外では
ネットワークを評価せず透明フレームを返す。タイムリマップは v2
（`time_remap` 予約フィールド）。

**Evaluator の変更（v3 で受け入れ）**: Document-aware（境界ノード・
Layer Ref が他レイヤーのネットワークを解決）、スコープ付き再帰評価
（`EvalScope::evaluate_sub`）、評価時パラメータ解決（`ResolvedParams`）。

#### 設計上の注意事項（Fable レビュー指摘）

- **premultiplied alpha**: 全内部処理は premultiplied alpha で統一。入出力時に変換。
  Multiply/Screen/Overlay は premul 前提の数式を使用。
- **solo の扱い**: solo は Comp 全体に影響（any solo → 非 solo を非表示）。展開前のプレパスで処理。
- **PreComp 循環検出**: PreComp ノード（`precomp`、v2）の `comp_id` 参照を
  レイヤーネットワーク走査で検出・拒否（`composition/validate.rs`）。
  Layer Ref の循環も同層で検出する（REQ-LAYER-005）。
- **fps/解像度不一致**: 子 Comp / 異 fps メディアは秒ベースでマッピング（REQ-LAYER-006）。
- **フレーム範囲**: `[in, out)` 半開区間。
- **time remap**: v2 対応。`time_remap: Option<AnimationChannel>` 予約済み。
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
